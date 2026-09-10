//! The audio callback.
//!
//! It reads the locked int32 ring and nothing else. No malloc, no lock, no
//! I/O, no page fault, nothing worse than O(1) in the period length, and no
//! third-party call that does not promise realtime behaviour. The ring is RAM
//! rather than a file mapping, so a stick pulled mid-set cannot raise SIGBUS
//! here (docs/architecture.md, docs/implementation.md).
//!
//! # Why v1 refuses instead of interpolating
//!
//! Audio is emitted only at exactly unity rate from an exactly integral
//! position. Anything else produces silence and says so. That is not a
//! limitation being worked around — it is what makes
//! `architecture.md`'s claim literal: "*Unconditionally* is now literal".
//! Nearest-neighbour resampling at a fractional position would play, sound
//! roughly right, and quietly stop being bit-perfect, which is the exact
//! failure shape this project keeps running into. v2 adds libsoxr and a unity
//! button to own that decision explicitly.

use crate::file::RING_CHANNELS;
use crate::ring::{Miss, RingReader};
use crate::transport::{Transport, RATE_PAUSED, RATE_UNITY};

/// What one period did. Returned rather than logged, because the callback may
/// not do I/O; the engine's caller decides what to surface.
///
/// Not `Eq`, because `NeedsResampler` carries the offending rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outcome {
    /// Real audio, bit-identical to the source.
    Played { frames: usize },
    /// The last period of the track: `frames` real, the rest silent.
    PlayedTail { frames: usize },
    /// `r = 0`. Silence, and the position does not move.
    Paused,
    /// FF or REW held. The position moves, the display follows, and no audio
    /// is produced — a silent seek, by design in v1.
    Seeking,
    /// The window thread has not reached here, or was outrun, or the window
    /// was relocated under us. Silence.
    Missed(Miss),
    /// The position is at or past the last frame.
    EndOfTrack,
    /// A rate or position v1 cannot serve without a resampler. Silence,
    /// deliberately, rather than an approximation that would sound fine.
    NeedsResampler { rate: f64, fractional: bool },
}

/// The callback's own state. Lives on the audio thread and is touched by
/// nothing else.
pub struct Engine {
    /// **float64.** A float32 accumulator has a 24-bit mantissa, so past 2^23
    /// samples — about 190 s at 44.1 kHz — the spacing between representable
    /// values reaches 1.0, the fractional position is gone and interpolation
    /// quietly stops working. Audible as the back half of a track degrading,
    /// with nothing pointing at the cause.
    position: f64,
    /// The track's length in frames, so the last period can be served short
    /// instead of failing as an underrun.
    track_frames: u64,
}

impl Engine {
    pub fn new(track_frames: u64) -> Self {
        Engine::at(track_frames, 0.0)
    }

    /// Starting from `position` rather than from zero.
    ///
    /// The app loop builds the engine with the transport's own position, so
    /// a load has **one** answer to "where does this start" instead of two
    /// that happen to agree. `Transport::track_loaded` sets zero; this
    /// follows it rather than restating it.
    pub fn at(track_frames: u64, position: f64) -> Self {
        Engine {
            position,
            track_frames,
        }
    }

    pub fn position(&self) -> f64 {
        self.position
    }

    /// Fills one output period.
    ///
    /// `out` is interleaved stereo in `S24_LE` layout — the ring's own
    /// layout, so there is nothing to convert here and no branch on the
    /// source's depth.
    pub fn fill(&mut self, t: &Transport, ring: &RingReader, out: &mut [i32]) -> Outcome {
        debug_assert_eq!(out.len() % RING_CHANNELS, 0, "periods are whole frames");
        let frames = (out.len() / RING_CHANNELS) as u64;

        // A queued seek is absolute and consumed exactly once — **applied,
        // published, and only then retired.** Clearing it first leaves an
        // interval in which no seek is pending and `position` still reads
        // the pre-seek value, and the control thread's end-of-track decision
        // reads exactly that pair. See `Transport::peek_seek`.
        //
        // **No test can see this line's position, and that is worth knowing
        // rather than discovering.** The publish is redundant with the one at
        // the bottom of this function in every single-threaded sense, so
        // moving it after the retire — or deleting it — leaves every
        // observable end state identical and the whole suite green. It earns
        // its keep only in the interleaving, where it is the store a control
        // thread is guaranteed to have seen if it has seen the retire. A
        // covering test would have to catch another thread inside a
        // two-instruction window; the ring's fences, whose window is a whole
        // period's copy, are detected 10-20% of the time. So this is argued
        // from the release on `consumed_seek` and the acquire on
        // `peek_seek`, not measured, and it is said out loud because the
        // catalogue's rule is that an unfalsifiable claim is the kind this
        // project gets wrong.
        if let Some(target) = t.peek_seek() {
            self.position = target.min(self.track_frames) as f64;
            t.publish_position(self.position);
            t.consumed_seek(target);
        }

        // One reading of the pair the branch below depends on, not two
        // loads that can straddle a control-thread write. See
        // `Transport::motion`.
        let (rate, silent) = t.motion();
        let outcome = self.step(ring, out, frames, rate, silent);

        // Publish after the decision, so the display and the window thread
        // see where the audio actually is.
        t.publish_position(self.position);
        ring.publish_playhead(self.position as u64);
        outcome
    }

    fn step(
        &mut self,
        ring: &RingReader,
        out: &mut [i32],
        frames: u64,
        rate: f64,
        silent: bool,
    ) -> Outcome {
        // A silent seek moves the position and emits nothing, so the rate may
        // be anything — including negative — without needing a resampler.
        //
        // **Checked before the end of the track**, deliberately. With the
        // order reversed, a deck that has played to the end reports
        // `EndOfTrack` forever and REW cannot move the position back out of
        // it — the only way off the last frame would be reloading the file.
        // Seeking is a position operation and has to work at the boundaries.
        if silent && rate != RATE_PAUSED {
            silence(out);
            self.advance(rate * frames as f64);
            return Outcome::Seeking;
        }

        if self.position >= self.track_frames as f64 {
            silence(out);
            return Outcome::EndOfTrack;
        }

        if rate == RATE_PAUSED {
            silence(out);
            return Outcome::Paused;
        }

        let fractional = self.position.fract() != 0.0;
        if rate != RATE_UNITY || fractional {
            silence(out);
            return Outcome::NeedsResampler { rate, fractional };
        }

        let from = self.position as u64;
        let remaining = self.track_frames - from;
        if remaining < frames {
            // The genuine last period. A short read is expected here rather
            // than a symptom, which is why it uses the tail entry point.
            return match ring.read_tail(from, out) {
                Ok(got) => {
                    self.advance(got as f64);
                    Outcome::PlayedTail { frames: got }
                }
                Err(e) => Outcome::Missed(e),
            };
        }

        match ring.read_block(from, out) {
            Ok(()) => {
                self.advance(frames as f64);
                Outcome::Played {
                    frames: frames as usize,
                }
            }
            // The position deliberately does **not** advance on a miss.
            //
            // Not stated in the design documents, so here is the reasoning:
            // the position is where the *audio* is, not where a clock is. Not
            // advancing means playback resumes exactly where it stopped once
            // the window catches up, and it makes the normal startup case —
            // the window not yet filled at frame 0 — correct rather than a
            // skipped intro. Advancing would keep wall-clock time true at the
            // cost of silently dropping material, which matters if the deck
            // is ever beat-locked to something. Worth revisiting then.
            Err(e) => {
                silence(out);
                Outcome::Missed(e)
            }
        }
    }

    /// Moves the position, clamped to the track. Clamping at zero is what
    /// stops REW running the accumulator negative.
    #[inline]
    fn advance(&mut self, by: f64) {
        self.position = (self.position + by).clamp(0.0, self.track_frames as f64);
    }
}

#[inline]
fn silence(out: &mut [i32]) {
    // Zero is digital silence in `S24_LE`. Not a gain stage: the absence of a
    // sample, not a scaled one.
    for s in out.iter_mut() {
        *s = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring;
    use crate::transport::RATE_SEEK;

    /// A ring pre-filled with frames whose samples encode their own index.
    fn filled(frames: u64) -> (ring::RingReader, u64) {
        let (mut w, r) = ring::new(frames.max(2) as usize);
        let block: Vec<i32> = (0..frames)
            .flat_map(|f| [f as i32 * 2, f as i32 * 2 + 1])
            .collect();
        w.append(&block);
        // Leak the writer: these tests only read, and dropping it would not
        // invalidate the ring anyway.
        std::mem::forget(w);
        (r, frames)
    }

    #[test]
    fn at_unity_the_period_is_the_source_and_the_position_is_exact() {
        let (ring, frames) = filled(4096);
        let t = Transport::new();
        let mut e = Engine::new(frames);
        t.play();

        let mut out = vec![0i32; 256 * RING_CHANNELS];
        for block in 0..16u64 {
            assert_eq!(e.fill(&t, &ring, &mut out), Outcome::Played { frames: 256 });
            let base = block * 256;
            for (i, &got) in out.iter().enumerate() {
                let f = base + (i / RING_CHANNELS) as u64;
                let want = f as i32 * 2 + (i % RING_CHANNELS) as i32;
                assert_eq!(got, want, "block {} sample {}", block, i);
            }
            // Exact, with no accumulated error: 16 blocks of 256 is 4096.
            assert_eq!(e.position(), (base + 256) as f64);
        }
    }

    #[test]
    fn pause_is_silence_and_a_frozen_position() {
        let (ring, frames) = filled(1024);
        let t = Transport::new();
        let mut e = Engine::new(frames);
        t.play();
        let mut out = vec![0i32; 64 * RING_CHANNELS];
        e.fill(&t, &ring, &mut out);
        let held = e.position();

        t.pause();
        for _ in 0..8 {
            assert_eq!(e.fill(&t, &ring, &mut out), Outcome::Paused);
            assert!(out.iter().all(|&s| s == 0));
            assert_eq!(e.position(), held, "pause must not move the position");
        }
    }

    #[test]
    fn a_held_seek_moves_the_position_at_four_times_and_stays_silent() {
        let (ring, frames) = filled(8192);
        let t = Transport::new();
        let mut e = Engine::new(frames);
        t.play();
        let mut out = vec![0i32; 128 * RING_CHANNELS];
        e.fill(&t, &ring, &mut out);
        let from = e.position();

        t.begin_seek(true);
        assert_eq!(e.fill(&t, &ring, &mut out), Outcome::Seeking);
        assert!(out.iter().all(|&s| s == 0), "v1's seek produces no audio");
        assert_eq!(e.position(), from + 128.0 * RATE_SEEK);

        t.begin_seek(false);
        assert_eq!(e.fill(&t, &ring, &mut out), Outcome::Seeking);
        assert_eq!(e.position(), from, "REW retraces exactly");

        // And audio resumes on release, from wherever the seek left it.
        t.end_seek(true);
        assert!(matches!(
            e.fill(&t, &ring, &mut out),
            Outcome::Played { .. }
        ));
    }

    #[test]
    fn rew_can_rewind_out_of_the_end_of_a_track() {
        // Regression. The end-of-track check used to run before the silent
        // seek, so once the position reached the last frame REW did nothing
        // and the deck was stuck there until the file was reloaded.
        let frames = 1_000u64;
        let (ring, _) = filled(frames);
        let t = Transport::new();
        let mut e = Engine::new(frames);
        let mut out = vec![0i32; 128 * RING_CHANNELS];

        t.request_seek(frames);
        t.play();
        assert_eq!(e.fill(&t, &ring, &mut out), Outcome::EndOfTrack);
        assert_eq!(e.position(), frames as f64);

        t.begin_seek(false);
        assert_eq!(e.fill(&t, &ring, &mut out), Outcome::Seeking);
        assert_eq!(e.position(), frames as f64 - 128.0 * RATE_SEEK);

        // And audio comes back on release.
        t.end_seek(true);
        assert!(matches!(
            e.fill(&t, &ring, &mut out),
            Outcome::Played { .. }
        ));
    }

    #[test]
    fn rew_cannot_drive_the_position_negative() {
        let (ring, frames) = filled(2048);
        let t = Transport::new();
        let mut e = Engine::new(frames);
        t.begin_seek(false);
        let mut out = vec![0i32; 256 * RING_CHANNELS];
        for _ in 0..20 {
            e.fill(&t, &ring, &mut out);
        }
        assert_eq!(e.position(), 0.0);
    }

    #[test]
    fn a_non_unity_rate_is_refused_rather_than_approximated() {
        // v1's `Transport` has no public way to set an arbitrary rate, which
        // is itself part of the design — pitch arrives with v2 and the unity
        // button. The engine still has to refuse one if it ever sees it, so
        // the rate is set here through the test-only door.
        let (ring, frames) = filled(2048);
        let t = Transport::new();
        let mut e = Engine::new(frames);
        t.play();
        let mut out = vec![9i32; 64 * RING_CHANNELS];

        for rate in [1.05, 0.95, 2.0, -1.0, 0.5] {
            t.set_rate_for_test(rate);
            match e.fill(&t, &ring, &mut out) {
                Outcome::NeedsResampler { rate: got, fractional } => {
                    assert_eq!(got, rate);
                    assert!(!fractional, "the position was integral; the rate is the problem");
                }
                other => panic!("rate {} gave {:?}, expected a refusal", rate, other),
            }
            assert!(out.iter().all(|&s| s == 0), "rate {} produced audio", rate);
            assert_eq!(e.position(), 0.0, "a refused period must not advance");
        }
    }

    #[test]
    fn a_fractional_position_is_refused_rather_than_approximated() {
        let (ring, frames) = filled(2048);
        let t = Transport::new();
        let mut e = Engine::new(frames);
        t.play();
        let mut out = vec![9i32; 64 * RING_CHANNELS];

        // A jog would leave the position fractional. Restoring bit-perfect
        // output from there requires snapping to an integer sample, which is
        // v2's job; until then the engine must not guess.
        t.begin_seek(true);
        // Half a frame, reachable only through a fractional advance.
        e.position = 100.5;
        t.end_seek(true);
        match e.fill(&t, &ring, &mut out) {
            Outcome::NeedsResampler { rate, fractional } => {
                assert_eq!(rate, RATE_UNITY);
                assert!(fractional);
            }
            other => panic!("expected a refusal, got {:?}", other),
        }
        assert!(out.iter().all(|&s| s == 0));
    }

    #[test]
    fn the_last_period_is_served_short_instead_of_reading_as_an_underrun() {
        // A track whose length is not a multiple of the period. Without the
        // tail path the final frames could never be played at all.
        let frames = 1000u64;
        let (ring, _) = filled(frames);
        let t = Transport::new();
        let mut e = Engine::new(frames);
        t.play();
        let mut out = vec![0i32; 256 * RING_CHANNELS];

        for _ in 0..3 {
            assert_eq!(e.fill(&t, &ring, &mut out), Outcome::Played { frames: 256 });
        }
        assert_eq!(e.position(), 768.0);
        // 232 frames left of a 256-frame period.
        assert_eq!(
            e.fill(&t, &ring, &mut out),
            Outcome::PlayedTail { frames: 232 }
        );
        for (i, &got) in out[..232 * RING_CHANNELS].iter().enumerate() {
            let f = 768 + (i / RING_CHANNELS) as u64;
            assert_eq!(got, f as i32 * 2 + (i % RING_CHANNELS) as i32);
        }
        // The unserved tail of the period is silent, not stale.
        assert!(out[232 * RING_CHANNELS..].iter().all(|&s| s == 0));
        assert_eq!(e.position(), frames as f64);
        assert_eq!(e.fill(&t, &ring, &mut out), Outcome::EndOfTrack);
    }

    #[test]
    fn a_miss_is_silence_and_does_not_move_the_position() {
        let (mut w, r) = ring::new(1024);
        let t = Transport::new();
        let mut e = Engine::new(4096);
        t.play();
        let mut out = vec![7i32; 128 * RING_CHANNELS];

        // Nothing filled yet — the normal startup case at frame 0.
        assert_eq!(
            e.fill(&t, &r, &mut out),
            Outcome::Missed(Miss::NotResident)
        );
        assert!(out.iter().all(|&s| s == 0));
        assert_eq!(e.position(), 0.0, "a startup miss must not skip the intro");

        // Once the window arrives, playback starts from the beginning.
        let block: Vec<i32> = (0..512u64).flat_map(|f| [f as i32, f as i32]).collect();
        w.append(&block);
        assert_eq!(e.fill(&t, &r, &mut out), Outcome::Played { frames: 128 });
        assert_eq!(out[0], 0, "playback resumed at frame 0, not later");
    }

    #[test]
    fn a_seek_request_lands_and_is_clamped_to_the_track() {
        let (ring, frames) = filled(2048);
        let t = Transport::new();
        let mut e = Engine::new(frames);
        t.play();
        let mut out = vec![0i32; 64 * RING_CHANNELS];

        t.request_seek(1000);
        assert_eq!(e.fill(&t, &ring, &mut out), Outcome::Played { frames: 64 });
        assert_eq!(out[0], 2000, "landed on frame 1000");
        assert_eq!(e.position(), 1064.0);

        t.request_seek(u64::MAX);
        assert_eq!(e.fill(&t, &ring, &mut out), Outcome::EndOfTrack);
        assert_eq!(e.position(), frames as f64);
    }

    /// The invariant, stated as a test rather than as a comment: float32
    /// cannot hold this position and float64 can.
    #[test]
    fn float32_would_have_lost_the_fractional_position_and_float64_does_not() {
        // 2^23 samples is ~190 s at 44.1 kHz — the back half of an ordinary
        // track, let alone an 80-minute one.
        let n = (1u64 << 23) as f64;
        assert!(
            (n as f32) + 0.5 == (n as f32),
            "float32 must be unable to represent the half-sample here"
        );
        assert!(n + 0.5 != n, "float64 must still resolve it");

        // And at the length the material actually reaches.
        let three_hours_at_192k = 192_000.0 * 3600.0 * 3.0;
        assert!(
            three_hours_at_192k + 0.5 != three_hours_at_192k,
            "float64 must resolve a half sample three hours into a 192 kHz track"
        );
    }
}
