//! The locked int32 ring around the playhead.
//!
//! The window thread writes it; the audio callback reads it and nothing else.
//! Samples are held in the output's own layout — `S24_LE`, the 24-bit value
//! right-aligned in a 32-bit word — interleaved stereo, so the callback has
//! nothing to convert and no branch on source depth
//! (docs/architecture.md, "Playback").
//!
//! # Why this is not `rtrb`
//!
//! `docs/implementation.md` named `rtrb` during design, for both the ring and
//! the control slot. **It is used for neither.** The control slot needed no
//! queue — it is plain atomics on `Transport` — so the dependency was never
//! added. The reason it could not have served the ring is kept because it is
//! what stops the ring becoming a FIFO later:
//! `architecture.md` requires reads inside the window to be "free in either
//! direction" and the window to be filled "ahead of **and behind** the
//! playhead", and an SPSC FIFO's consumer can only move forward — data behind
//! its read pointer is gone. v1 alone would be satisfied by a FIFO, since
//! playback only reads forward and FF/REW are a silent seek, but then v2's
//! jog would be a rewrite rather than a substitution.
//!
//! # How the callback reads it without a lock
//!
//! Slots are `AtomicI32` accessed `Relaxed`. That is what makes a concurrent
//! read sound *by construction* rather than by argument — and it is free on
//! the target: a relaxed 32-bit atomic load or store on AArch64 is a plain
//! `ldr` / `str` with no barrier and no lock instruction.
//!
//! The resident span is published as three separate atomics rather than one
//! consistent snapshot. Within a generation that understates rather than
//! overstates what is resident, because `end` only grows and `start` only
//! grows: loading `end` **first** gives a lower bound on the true end and
//! `start` **last** an upper bound on the true start. `generation` changes
//! when the writer relocates, which is the only time `start` moves backwards.
//!
//! So the reader loads `generation`, `end`, `start`, reads, then re-loads
//! `start` and `generation`. A miss is reported rather than stale audio. That
//! is one bounded pass with no retry loop, so it stays O(1) — the callback
//! rules exclude anything worse (docs/implementation.md, "Process setup").
//!
//! # Two fences, and why the release/acquire pair is not enough
//!
//! **This section describes a defect that was found by measurement and
//! fixed, and the fences below are load-bearing. Do not remove them as
//! redundant with the `Release`/`Acquire` on `start` — that is precisely the
//! reasoning that left them out.**
//!
//! The acquire/release pair is oriented for *publishing*: the writer fills
//! slots and then stores `end` with `Release`, the reader loads `end` with
//! `Acquire` and then reads slots. That direction is sound.
//!
//! *Invalidating* runs the other way and has no pairing of its own.
//! `drop_before` stores `start` with `Release` before the slots below it are
//! reused — but `Release` orders what comes *before* the store, so the
//! writer's later `Relaxed` slot writes may still become visible ahead of it.
//! Symmetrically the reader's `start.load(Acquire)` orders what comes
//! *after* it, so its earlier `Relaxed` slot loads may complete after it.
//! Either reordering produces the same outcome: the reader reads an
//! overwritten slot, re-checks against a `start` that has not moved yet, and
//! returns `Ok` with a sample from another frame.
//!
//! Measured on AArch64 with a writer recycling right behind the reader:
//! **18 corrupt `Ok`s in 90.6M reads** without the fences, **0 in 52.7M**
//! with them. One `dmb` each, once per block, off the per-sample path.
//!
//! The ordinary fill policy never recycles ahead of the playhead
//! (`window.rs` drops only below `playhead - below_target`), so this is the
//! last line of defence rather than a hot path. That is what let a claim of
//! being "sound by construction" stand here while being untrue.

use std::sync::atomic::{fence, AtomicI32, AtomicU64, Ordering};
use std::sync::Arc;

use crate::file::RING_CHANNELS;

/// Bytes one frame occupies in the ring. The ring is int32 whatever the
/// source depth was, so this does not vary — which is why window size is a
/// function of sample rate alone and has no bit-depth axis.
pub const RING_FRAME_BYTES: usize = RING_CHANNELS * std::mem::size_of::<i32>();

/// The half-window cap, in seconds, from `min(60 s, N MiB)`.
///
/// Applies to each side of the playhead: at 44.1 kHz the byte cap is not
/// reached, and the window is the ±60 s the table in `architecture.md` shows.
pub const HALF_WINDOW_SECS_CAP: f64 = 60.0;

/// The byte cap, `N`.
///
/// **Not a decision.** `architecture.md` states the value "is not yet chosen"
/// and shows the table at 64 MiB as illustrative. It is also not only a
/// jog-feel parameter: the forward half is how long playback survives a
/// knocked connector, so the grace period varies by rate and is meant to be
/// chosen deliberately (docs/decisions.md). This constant is that illustrative
/// figure, carried so the code has something to run with, and it should be
/// replaced by a measured choice before the enclosure is built.
pub const WINDOW_BYTES_PLACEHOLDER: usize = 64 * 1024 * 1024;

/// Frames the ring should hold for a track at `rate`, from `min(60 s each
/// side, window_bytes total)`.
///
/// Sizing in bytes rather than seconds is what holds RAM flat across the six
/// rates — time alone would swing it 4x — and it degrades the window length
/// instead, so an unexpected hi-res file plays with a shorter window rather
/// than failing.
pub fn capacity_frames(rate: u32, window_bytes: usize) -> usize {
    let by_bytes = window_bytes / RING_FRAME_BYTES;
    let by_time = (HALF_WINDOW_SECS_CAP * 2.0 * rate as f64) as usize;
    by_bytes.min(by_time).max(2)
}

/// Half-window in seconds at `rate` — the figure the display would quote and
/// the grace period after a connector is knocked.
pub fn half_window_secs(rate: u32, window_bytes: usize) -> f64 {
    capacity_frames(rate, window_bytes) as f64 / rate as f64 / 2.0
}

/// The shared allocation. Split into a [`RingWriter`] and a [`RingReader`] at
/// construction; there is exactly one of each.
struct Shared {
    /// `capacity * RING_CHANNELS` samples. Allocated once and never resized.
    slots: Box<[AtomicI32]>,
    capacity: u64,
    /// Track frame index of the oldest resident frame.
    start: AtomicU64,
    /// One past the newest resident frame. `end - start <= capacity` always.
    end: AtomicU64,
    /// Bumped whenever the window is relocated, which is the only time
    /// `start` moves backwards.
    generation: AtomicU64,
    /// Where the reader is, as a whole frame. Published by the reader so the
    /// writer knows which way to fill. A hint, not a synchronisation point.
    playhead: AtomicU64,
}

impl Shared {
    #[inline]
    fn slot_of(&self, frame: u64) -> usize {
        // One division per block, not per sample: the block paths compute
        // this once and then step with a branchless wrap.
        ((frame % self.capacity) as usize) * RING_CHANNELS
    }
}

/// Allocates the ring and returns the two ends.
///
/// The allocation is touched page by page here, deliberately. `mlockall`
/// with `MCL_FUTURE` is not enough on its own — a page touched for the first
/// time inside the callback still faults — and `Box<[AtomicI32]>` is
/// zero-initialised through `calloc`, which the allocator may satisfy with
/// untouched pages (docs/implementation.md, "Process setup"). Calling
/// `mlockall` itself is a process concern and belongs with the rest of the
/// realtime setup, not here.
pub fn new(capacity_frames: usize) -> (RingWriter, RingReader) {
    assert!(capacity_frames >= 2, "ring needs at least two frames");
    let mut v = Vec::with_capacity(capacity_frames * RING_CHANNELS);
    for _ in 0..capacity_frames * RING_CHANNELS {
        v.push(AtomicI32::new(0));
    }
    let slots = v.into_boxed_slice();

    // Pre-fault: read every page back. The pushes above already wrote them,
    // so this is belt and braces against an allocator that maps lazily, and
    // it is cheap — 64 MiB of sequential touches, once per track load.
    let page = 4096 / std::mem::size_of::<AtomicI32>();
    let mut acc = 0i32;
    for i in (0..slots.len()).step_by(page.max(1)) {
        acc = acc.wrapping_add(slots[i].load(Ordering::Relaxed));
    }
    debug_assert_eq!(acc, 0, "a fresh ring must read back as silence");

    let shared = Arc::new(Shared {
        slots,
        capacity: capacity_frames as u64,
        start: AtomicU64::new(0),
        end: AtomicU64::new(0),
        generation: AtomicU64::new(0),
        playhead: AtomicU64::new(0),
    });
    (
        RingWriter {
            shared: Arc::clone(&shared),
        },
        RingReader { shared },
    )
}

/// The window thread's end. Blocking, allocating and locking are all fine on
/// this side; it is the thread the deadline does not reach.
pub struct RingWriter {
    shared: Arc<Shared>,
}

impl RingWriter {
    pub fn capacity(&self) -> u64 {
        self.shared.capacity
    }

    /// The span currently readable, as track frame indices.
    pub fn resident(&self) -> std::ops::Range<u64> {
        self.shared.start.load(Ordering::Acquire)..self.shared.end.load(Ordering::Acquire)
    }

    /// The reader's last published position. A hint — it may be a block old.
    pub fn playhead(&self) -> u64 {
        self.shared.playhead.load(Ordering::Relaxed)
    }

    /// Throws the window away and restarts it empty at `frame`.
    ///
    /// This is the only operation that moves `start` backwards, so it is also
    /// the only one that bumps the generation. Used when a seek lands outside
    /// the resident span and when a track is loaded.
    ///
    /// **Bumped twice, so the generation is odd while the relocation is in
    /// progress.** A single bump was not enough, and the hole it left is the
    /// mirror image of the fence one: `start` is stored before `end`, while
    /// the reader loads `end` before `start`. A reader that observed the new
    /// generation, the *old* `end` and the *new* `start` — reachable on a
    /// **backwards** relocate, which is a cue jump or a REW landing outside
    /// the window — computes a span that has **grown**, `new_start..old_end`.
    /// Its re-check then passes, because the generation has not moved again
    /// and the new `start` is below `from`, and it returns slots from the
    /// window that no longer exists. Odd means "do not trust the span", which
    /// the reader can test with one load it was already doing.
    pub fn relocate(&mut self, frame: u64) {
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        self.shared.start.store(frame, Ordering::Release);
        self.shared.end.store(frame, Ordering::Release);
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Discards everything before `frame`, making room ahead.
    ///
    /// Published *before* the slots are reused, which is what lets the reader
    /// detect that a frame it just read has been overwritten.
    pub fn drop_before(&mut self, frame: u64) {
        let start = self.shared.start.load(Ordering::Relaxed);
        let end = self.shared.end.load(Ordering::Relaxed);
        let new_start = frame.clamp(start, end);
        if new_start > start {
            self.shared.start.store(new_start, Ordering::Release);
        }
    }

    /// How many frames can be appended without overrunning the reader's
    /// oldest resident frame.
    pub fn writable(&self) -> u64 {
        let r = self.resident();
        self.shared.capacity - (r.end - r.start)
    }

    /// Appends interleaved stereo frames at the end of the resident span.
    ///
    /// `src.len()` must be a whole number of frames and must fit in
    /// [`writable`]. Writing frame `end` reuses the slot last held by frame
    /// `end - capacity`, and the `end - start <= capacity` invariant is
    /// exactly what guarantees that frame is already below `start` — so a
    /// successful append never clobbers anything the reader may still take.
    pub fn append(&mut self, src: &[i32]) {
        assert_eq!(src.len() % RING_CHANNELS, 0, "appends are whole frames");
        let frames = (src.len() / RING_CHANNELS) as u64;
        if frames == 0 {
            return;
        }
        assert!(
            frames <= self.writable(),
            "append of {} frames overruns the resident span ({} writable)",
            frames,
            self.writable()
        );

        let end = self.shared.end.load(Ordering::Relaxed);
        // Any `start` published by a preceding `drop_before` must be visible
        // before the slots it freed are overwritten. `Release` on that store
        // does not give this — it orders what came before it, not what comes
        // after — so the ordering has to be asked for here. See the module
        // doc; this was measured, not reasoned.
        fence(Ordering::Release);
        let mut slot = self.shared.slot_of(end);
        let limit = self.shared.slots.len();
        for &s in src {
            self.shared.slots[slot].store(s, Ordering::Relaxed);
            slot += 1;
            if slot == limit {
                slot = 0;
            }
        }
        // Publish last: every slot below `end` was written before this store.
        self.shared.end.store(end + frames, Ordering::Release);
    }
}

/// Why a read could not be served in full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Miss {
    /// The window thread has not reached these frames yet, or the track ended.
    /// The callback's answer is silence, not a stall.
    NotResident,
    /// The frames were resident when the read began and were recycled while
    /// it ran. Distinguished from `NotResident` because it means the window
    /// thread is being outrun, which is a different fault to report.
    Overrun,
    /// The window was relocated mid-read — a seek or a track change landed
    /// while the callback was working. The next block will be correct.
    Relocated,
}

/// The audio callback's end.
///
/// Every method here allocates nothing, locks nothing, does no I/O, is O(1)
/// in its output length, and cannot fault: the ring is RAM, not a file
/// mapping, so a stick pulled mid-set cannot raise SIGBUS in the audio thread
/// (docs/architecture.md, "Why a ring and not mmap").
pub struct RingReader {
    shared: Arc<Shared>,
}

impl RingReader {
    pub fn capacity(&self) -> u64 {
        self.shared.capacity
    }

    pub fn resident(&self) -> std::ops::Range<u64> {
        // Note the load order: `end` first and `start` last understates the
        // span rather than overstating it — but only **within a generation**.
        // This accessor does not read `generation`, so across a backwards
        // relocation it can report `new_start..old_end`, wider than either.
        // That is why it is informational only: `copy` does its own
        // generation-checked loads, and it is the one that must not overstate.
        let end = self.shared.end.load(Ordering::Acquire);
        let start = self.shared.start.load(Ordering::Acquire);
        start..end.max(start)
    }

    /// Tells the window thread where the callback is. Called once per block.
    #[inline]
    pub fn publish_playhead(&self, frame: u64) {
        self.shared.playhead.store(frame, Ordering::Relaxed);
    }

    /// Copies `dst.len() / 2` frames starting at track frame `from`.
    ///
    /// **All or nothing.** A partly resident read means the window thread is
    /// behind, and half a period of real audio followed by half a period of
    /// stale samples is worse than a clean dropout and harder to attribute.
    /// The one place a short read is legitimate is the last period of a
    /// track, which is [`read_tail`](Self::read_tail).
    ///
    /// On success every sample in `dst` is the source's, in `S24_LE` layout.
    /// On a miss `dst` is filled with silence — never with stale or torn
    /// samples — and the reason is returned. Reading backwards is the same
    /// call with a lower `from`, which is what v2's jog needs and costs
    /// nothing extra here.
    pub fn read_block(&self, from: u64, dst: &mut [i32]) -> Result<(), Miss> {
        let frames = (dst.len() / RING_CHANNELS) as u64;
        match self.copy(from, dst, frames) {
            Ok(got) if got == frames => Ok(()),
            Ok(_) => {
                self.silence(dst);
                Err(Miss::NotResident)
            }
            Err(e) => Err(e),
        }
    }

    /// The last period of a track: copies what is resident and leaves the
    /// rest silent, returning how many frames were real.
    ///
    /// Short reads are allowed here **only because the caller has established
    /// this is the end of the track**, where a period that runs past the last
    /// frame is expected rather than a symptom. Using it mid-track would hide
    /// exactly the underrun [`read_block`](Self::read_block) exists to
    /// report.
    pub fn read_tail(&self, from: u64, dst: &mut [i32]) -> Result<usize, Miss> {
        let frames = (dst.len() / RING_CHANNELS) as u64;
        let got = self.copy(from, dst, frames)?;
        // Silence whatever was not served, so the tail of the period is not
        // stale ring contents.
        for d in dst[got as usize * RING_CHANNELS..].iter_mut() {
            *d = 0;
        }
        Ok(got as usize)
    }

    /// Shared body. Returns the number of frames actually copied, which is
    /// `min(frames, resident.end - from)`, or a miss if `from` itself is not
    /// resident or the copy was invalidated while it ran.
    #[inline]
    fn copy(&self, from: u64, dst: &mut [i32], frames: u64) -> Result<u64, Miss> {
        let gen_before = self.shared.generation.load(Ordering::Acquire);
        let end = self.shared.end.load(Ordering::Acquire);
        let start = self.shared.start.load(Ordering::Acquire);

        if frames == 0 {
            return Ok(0);
        }
        // Odd means a relocation is in flight, so `start` and `end` are not
        // two halves of the same span. Cheaper to refuse than to reason about
        // which of them is stale.
        if gen_before & 1 == 1 {
            self.silence(dst);
            return Err(Miss::Relocated);
        }
        if from < start || from >= end {
            self.silence(dst);
            return Err(Miss::NotResident);
        }
        let serve = frames.min(end - from);

        let mut slot = self.shared.slot_of(from);
        let limit = self.shared.slots.len();
        for d in dst[..serve as usize * RING_CHANNELS].iter_mut() {
            *d = self.shared.slots[slot].load(Ordering::Relaxed);
            slot += 1;
            if slot == limit {
                slot = 0;
            }
        }

        // The slot loads above are `Relaxed` and must not sink past the
        // re-check below, or an overwrite read before `start` moved reads as
        // an overwrite that never happened. `Acquire` on the re-check itself
        // does not give this — it orders what follows it. See the module doc.
        fence(Ordering::Acquire);

        // Re-check in the reverse order of the writer's publishes.
        if self.shared.generation.load(Ordering::Acquire) != gen_before {
            self.silence(dst);
            return Err(Miss::Relocated);
        }
        if self.shared.start.load(Ordering::Acquire) > from {
            self.silence(dst);
            return Err(Miss::Overrun);
        }
        Ok(serve)
    }

    /// One frame, for the fractional read v2's resampler will do. Separate
    /// from [`read_block`] only to keep the per-sample path off the division.
    pub fn read_frame(&self, frame: u64) -> Result<[i32; RING_CHANNELS], Miss> {
        let mut f = [0i32; RING_CHANNELS];
        self.read_block(frame, &mut f)?;
        Ok(f)
    }

    #[inline]
    fn silence(&self, dst: &mut [i32]) {
        // Digital silence is zero in S24_LE, and zeroing is not a gain stage:
        // it is the absence of a sample, not a scaled one.
        for d in dst.iter_mut() {
            *d = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table in `architecture.md`, "Size the window in bytes", at the
    /// 64 MiB figure it is shown with. If the sizing formula is changed, this
    /// says so instead of the document quietly going stale.
    #[test]
    fn window_sizes_reproduce_the_architecture_table() {
        let n = WINDOW_BYTES_PLACEHOLDER;
        let expected: [(u32, u32, f64); 6] = [
            // rate, ring fill rate kB/s, half window seconds
            (44_100, 353, 60.0),
            (48_000, 384, 60.0),
            (88_200, 706, 47.0),
            (96_000, 768, 43.0),
            (176_400, 1_411, 23.0),
            (192_000, 1_536, 21.0),
        ];
        for (rate, fill_kb_s, half_secs) in expected {
            let fill = rate as usize * RING_FRAME_BYTES;
            assert_eq!(
                (fill as f64 / 1000.0).round() as u32,
                fill_kb_s,
                "{} Hz fill rate",
                rate
            );
            // The document truncates; allow it to be within a second.
            let got = half_window_secs(rate, n);
            assert!(
                got >= half_secs && got < half_secs + 1.0,
                "{} Hz: table says ±{} s, formula gives ±{:.2} s",
                rate,
                half_secs,
                got
            );
        }
    }

    #[test]
    fn ram_is_flat_across_all_six_rates_and_the_time_cap_only_shortens_it() {
        let n = WINDOW_BYTES_PLACEHOLDER;
        let mut byte_capped = Vec::new();
        for rate in crate::file::SUPPORTED_RATES {
            let frames = capacity_frames(rate, n);
            let bytes = frames * RING_FRAME_BYTES;
            assert!(bytes <= n, "{} Hz exceeds the byte cap", rate);
            if frames == n / RING_FRAME_BYTES {
                byte_capped.push(rate);
            }
        }
        // 44.1 and 48 kHz hit the 60 s cap first; the other four are capped by
        // bytes and so cost exactly the same RAM as each other.
        assert_eq!(byte_capped, vec![88_200, 176_400, 96_000, 192_000]);
    }

    #[test]
    fn a_fresh_ring_is_empty_and_reads_as_a_miss() {
        let (w, r) = new(64);
        assert_eq!(r.resident(), 0..0);
        assert_eq!(w.writable(), 64);
        let mut buf = [1i32; 8];
        assert_eq!(r.read_block(0, &mut buf), Err(Miss::NotResident));
        assert_eq!(buf, [0i32; 8], "a miss must leave silence, not stale data");
    }

    fn frames(from: u64, count: u64) -> Vec<i32> {
        // Encode the frame index in both channels so a wrap, an off-by-one or
        // a channel swap each produce a distinguishable wrong value.
        (from..from + count)
            .flat_map(|f| [f as i32 * 2, f as i32 * 2 + 1])
            .collect()
    }

    #[test]
    fn reads_are_free_in_either_direction_inside_the_span() {
        let (mut w, r) = new(16);
        w.append(&frames(0, 16));
        // Forward, then back to a frame already passed — the property an
        // SPSC FIFO cannot offer and v2's jog depends on.
        for start in [0u64, 4, 8, 12, 6, 2, 0, 9] {
            let mut buf = vec![0i32; 4 * RING_CHANNELS];
            r.read_block(start, &mut buf).expect("resident");
            assert_eq!(buf, frames(start, 4), "reading from frame {}", start);
        }
    }

    #[test]
    fn wrapping_past_the_end_of_the_allocation_is_seamless() {
        let (mut w, r) = new(8);
        w.append(&frames(0, 8));
        // Free the first half and append past the physical end.
        w.drop_before(4);
        w.append(&frames(8, 4));
        assert_eq!(r.resident(), 4..12);
        let mut buf = vec![0i32; 8 * RING_CHANNELS];
        r.read_block(4, &mut buf).expect("spans the wrap");
        assert_eq!(buf, frames(4, 8));
    }

    #[test]
    fn append_beyond_capacity_is_refused_rather_than_clobbering() {
        let (mut w, _r) = new(8);
        w.append(&frames(0, 8));
        assert_eq!(w.writable(), 0);
        let too_much = frames(8, 1);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            w.append(&too_much)
        }))
        .is_err());
    }

    #[test]
    fn a_partly_resident_read_is_a_miss_not_a_short_read() {
        // The callback has a fixed period to fill; half a block of real audio
        // followed by half a block of stale samples would be worse than a
        // clean dropout, and harder to attribute.
        let (mut w, r) = new(16);
        w.append(&frames(0, 4));
        let mut buf = vec![7i32; 8 * RING_CHANNELS];
        assert_eq!(r.read_block(0, &mut buf), Err(Miss::NotResident));
        assert!(buf.iter().all(|&s| s == 0));
    }

    #[test]
    fn read_tail_serves_what_is_there_and_silences_the_rest() {
        // Only legitimate at the end of a track, where a period running past
        // the last frame is expected rather than a symptom.
        let (mut w, r) = new(16);
        w.append(&frames(0, 10));
        let mut buf = vec![9i32; 16 * RING_CHANNELS];
        assert_eq!(r.read_tail(0, &mut buf), Ok(10));
        assert_eq!(&buf[..10 * RING_CHANNELS], &frames(0, 10)[..]);
        assert!(
            buf[10 * RING_CHANNELS..].iter().all(|&s| s == 0),
            "the unserved tail must be silence, not stale ring contents"
        );
        // A start that is not resident at all is still a miss, not a zero.
        assert_eq!(r.read_tail(10, &mut buf), Err(Miss::NotResident));
    }

    #[test]
    fn relocate_invalidates_the_window_and_bumps_the_generation() {
        let (mut w, r) = new(16);
        w.append(&frames(0, 16));
        w.relocate(1_000_000);
        assert_eq!(r.resident(), 1_000_000..1_000_000);
        let mut buf = vec![0i32; 2 * RING_CHANNELS];
        assert_eq!(r.read_block(0, &mut buf), Err(Miss::NotResident));
        w.append(&frames(1_000_000, 4));
        r.read_block(1_000_000, &mut buf).expect("after relocation");
        assert_eq!(buf, frames(1_000_000, 2));
    }

    #[test]
    fn drop_before_is_clamped_to_the_resident_span() {
        let (mut w, r) = new(16);
        w.append(&frames(0, 8));
        w.drop_before(0); // no-op
        assert_eq!(r.resident(), 0..8);
        w.drop_before(100); // past the end — clamps, never inverts the span
        assert_eq!(r.resident(), 8..8);
        assert!(r.resident().start <= r.resident().end);
    }

    #[test]
    fn the_playhead_is_a_hint_the_writer_can_read() {
        let (w, r) = new(16);
        r.publish_playhead(1234);
        assert_eq!(w.playhead(), 1234);
    }
}
