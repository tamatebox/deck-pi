//! Adversarial concurrency on the ring, and the reason it needs its own file.
//!
//! **No test in this repository has ever caused `Miss::Overrun` or
//! `Miss::Relocated` to be returned.** `callback_rules.rs`'s overrun test
//! recycles the front *before* the read, so it takes the range check and
//! reports `NotResident`; `window_test.rs`'s race test drops only well below
//! the playhead, exactly as the fill policy does, so the writer never catches
//! the reader. Both are right about what they assert and neither reaches the
//! branch. That is how a memory-ordering defect lived in the re-check path
//! while the module doc called it sound by construction.
//!
//! These are `#[ignore]`d: they need seconds of wall clock and are
//! probabilistic, which is not what the fast suite is for. Run with
//! `cargo test --release --test ring_race_test -- --ignored --nocapture`.
//! Release matters — the reordering this exists to catch is a property of
//! optimised code on a weakly ordered machine.
//!
//! # This detector is weak, and the number is measured
//!
//! **Do not read a clean run as proof.** The detection rate was measured by
//! neutralising *only* the two fences — seqlock left in — and running the
//! pair repeatedly:
//!
//! | Platform | Configuration | Runs detecting the defect |
//! |---|---|---|
//! | macOS/AArch64 | as written | **1 of 5** |
//! | Linux/aarch64 (container) | as written | **1 of 10** |
//! | Linux/aarch64 | `CAP` 1024, `BLOCK` 512 | **0 of 10** |
//!
//! So roughly 10-20% per run, and when it fires it can be a *single* corrupt
//! read in 10.85M. Three consecutive clean runs is about 73% likely with the
//! bug still present — which is to say it is not evidence. An earlier version
//! of this comment claimed those three runs as confirmation; they were a
//! number without a denominator, the same shape as the 4096-slot harness
//! described above, one level up.
//!
//! The third row is why the constants are what they are: lengthening the copy
//! to widen the window between the slot loads and the re-check made detection
//! **worse**, not better. Tuning was tried and did not work; this is as
//! sensitive as this technique gets here.
//!
//! **Consequence worth acting on: these tests are `#[ignore]`d, so the
//! regression guard for the most serious defect in this codebase is opt-in
//! *and* weak.** Someone removing the fences as "redundant with the
//! `Release`/`Acquire`" — the exact reasoning that left them out
//! originally — gets a green default suite on both platforms. A systematic
//! interleaving check (`loom`) is the thing that would actually guard this;
//! until there is one, the module doc in `ring.rs` is the only barrier, and
//! today's lesson is that a doc is not a barrier.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use deck_pi::file::RING_CHANNELS;
use deck_pi::ring::{self, Miss};

/// The sample a given frame must carry. Any other value in a frame the reader
/// was told it could have is corruption, and the point of the whole file.
fn sample_for(frame: u64) -> i32 {
    // Keep it inside the 24-bit range the ring actually holds, and make
    // neighbouring frames differ in the low bits so an off-by-one slot is not
    // mistaken for a clean read.
    ((frame as i32).wrapping_mul(2_654_435) & 0x7f_ffff) - 0x40_0000
}

struct Counts {
    ok: u64,
    overrun: u64,
    relocated: u64,
    not_resident: u64,
    corrupt: u64,
}

/// Runs a writer that recycles the ring immediately behind the reader — which
/// the real fill policy never does — for `secs`, and reports what the reader
/// saw. `relocating` adds backwards relocations, the case the generation
/// counter exists for.
fn hammer(secs: u64, relocating: bool) -> Counts {
    // **The capacity has to be barely larger than the block, or this test
    // proves nothing.** At 4096 slots and 128-frame blocks the writer runs 32
    // blocks ahead before wrapping onto the slots the reader is inside, so it
    // never physically overwrites them — `Overrun` still fires in the
    // millions, purely from `start` moving, and the corruption count stays at
    // zero whether the fences are there or not. Measured: that shape passed
    // with the fences removed. At 256 the writer laps every second append,
    // which is what puts a store into a slot the reader is mid-copy on.
    const CAP: usize = 256;
    const BLOCK: u64 = 128;

    let (mut w, r) = ring::new(CAP);
    let stop = Arc::new(AtomicBool::new(false));

    let writer_stop = Arc::clone(&stop);
    let writer = std::thread::spawn(move || {
        let mut next: u64 = 0;
        let mut scratch = vec![0i32; BLOCK as usize * RING_CHANNELS];
        let mut since_relocate = 0u32;
        while !writer_stop.load(Ordering::Relaxed) {
            if relocating && since_relocate > 2_000 {
                since_relocate = 0;
                // Backwards, which is the direction that can inflate the span
                // a reader observes: a cue jump, or REW past the window.
                next = next.saturating_sub(100_000);
                w.relocate(next);
                continue;
            }
            since_relocate += 1;

            // Free space by discarding everything but the last block, so the
            // writer is always overwriting slots the reader may be inside.
            let resident = w.resident();
            if resident.end > BLOCK {
                w.drop_before(resident.end - BLOCK);
            }
            if w.writable() < BLOCK {
                std::hint::spin_loop();
                continue;
            }
            for (i, chunk) in scratch.chunks_mut(RING_CHANNELS).enumerate() {
                let v = sample_for(next + i as u64);
                for s in chunk.iter_mut() {
                    *s = v;
                }
            }
            w.append(&scratch);
            next += BLOCK;
        }
    });

    let mut c = Counts {
        ok: 0,
        overrun: 0,
        relocated: 0,
        not_resident: 0,
        corrupt: 0,
    };
    let mut dst = vec![0i32; BLOCK as usize * RING_CHANNELS];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        for _ in 0..2_000 {
            let span = r.resident();
            if span.end <= span.start {
                continue;
            }
            let from = span.start;
            match r.read_block(from, &mut dst) {
                Ok(()) => {
                    c.ok += 1;
                    // Every frame the reader was *told* it could have must
                    // carry that frame's own sample.
                    for (i, chunk) in dst.chunks(RING_CHANNELS).enumerate() {
                        let want = sample_for(from + i as u64);
                        if chunk.iter().any(|&s| s != want) {
                            c.corrupt += 1;
                            break;
                        }
                    }
                }
                Err(Miss::Overrun) => c.overrun += 1,
                Err(Miss::Relocated) => c.relocated += 1,
                Err(Miss::NotResident) => c.not_resident += 1,
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    writer.join().expect("writer thread");
    c
}

#[test]
#[ignore = "seconds of wall clock; run explicitly, and in release"]
fn a_reader_overtaken_by_the_writer_never_returns_another_frames_sample() {
    let c = hammer(6, false);
    println!(
        "ok {} (corrupt {}), overrun {}, not_resident {}",
        c.ok, c.corrupt, c.overrun, c.not_resident
    );

    // Without this the test could pass by never reaching the branch, which is
    // exactly how the defect survived 167 tests.
    assert!(
        c.overrun > 0,
        "the writer never overtook the reader, so nothing was tested"
    );
    assert!(c.ok > 0, "no read succeeded, so nothing was checked");
    assert_eq!(
        c.corrupt, 0,
        "{} of {} successful reads carried another frame's sample",
        c.corrupt, c.ok
    );
}

#[test]
#[ignore = "seconds of wall clock; run explicitly, and in release"]
fn a_backwards_relocation_is_never_served_as_a_wider_window() {
    let c = hammer(6, true);
    println!(
        "ok {} (corrupt {}), overrun {}, relocated {}, not_resident {}",
        c.ok, c.corrupt, c.overrun, c.relocated, c.not_resident
    );

    assert!(
        c.relocated > 0,
        "no relocation was observed mid-read, so nothing was tested"
    );
    assert_eq!(
        c.corrupt, 0,
        "{} of {} successful reads came from a discarded window",
        c.corrupt, c.ok
    );
}
