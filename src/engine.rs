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
    /// FF or REW held. The position moves, the display follows, and the scan
    /// plays: fragments of the source at unity — [`Engine`]'s `scan`.
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
    /// Frames in one fragment of the scan — [`FRAGMENT_SECS`] of this track.
    fragment: u64,
    /// Frames faded at each end of a fragment — [`FADE_SECS`] of this track.
    fade: u64,
    /// How much of the current fragment the scan has played, or `None` when
    /// no scan is under way.
    into_fragment: Option<u64>,
}

/// How long each fragment of the scan sounds: the user's "about a tenth of a
/// second", 2026-10-26. Long enough to tell where the scan is, short enough
/// that 32x still moves.
pub const FRAGMENT_SECS: f64 = 0.1;

/// [`FRAGMENT_SECS`] in frames at `rate` Hz, never zero.
pub fn fragment_frames(rate: u32) -> u64 {
    ((f64::from(rate) * FRAGMENT_SECS) as u64).max(1)
}

/// How long each end of a scan fragment fades: a raised cosine over 3 ms.
/// Measured offline on 2026-10-26 against three of the user's tracks, hard
/// joins stepped 20-30 dB past anything the music did on its own; the usual
/// splice fix is a 2-5 ms raised-cosine fade, and 3 ms is what the offline
/// renders used.
pub const FADE_SECS: f64 = 0.003;

/// [`FADE_SECS`] in frames, for a fragment of `fragment` frames — which is
/// [`FRAGMENT_SECS`] long, so the rate need not be asked again. Less than half
/// the fragment, so the two fades never meet.
pub fn fade_frames(fragment: u64) -> u64 {
    ((fragment as f64 * FADE_SECS / FRAGMENT_SECS) as u64).min(fragment.saturating_sub(1) / 2)
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
    ///
    /// The scan's fragments are a tenth of a second at 44.1 kHz until
    /// [`fragments_of`](Self::fragments_of) says what the track's rate is.
    pub fn at(track_frames: u64, position: f64) -> Self {
        Engine {
            position,
            track_frames,
            fragment: fragment_frames(44_100),
            fade: fade_frames(fragment_frames(44_100)),
            into_fragment: None,
        }
    }

    /// Fragments of `frames` each, for a track whose rate makes that a tenth
    /// of a second.
    pub fn fragments_of(mut self, frames: u64) -> Self {
        self.fragment = frames.max(1);
        self.fade = fade_frames(self.fragment);
        self
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
            // Wherever a scan's fragment had got to, it is not here.
            self.into_fragment = None;
            t.publish_position(self.position);
            t.consumed_seek(target);
        }

        // One reading of the pair the branch below depends on, not two
        // loads that can straddle a control-thread write. See
        // `Transport::motion`.
        let (rate, seeking) = t.motion();
        let outcome = self.step(ring, out, frames, rate, seeking);

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
        seeking: bool,
    ) -> Outcome {
        // A seek moves the position at the rate and plays only at unity, so
        // the rate may be anything — including negative — without needing a
        // resampler.
        //
        // **Checked before the end of the track**, deliberately. With the
        // order reversed, a deck that has played to the end reports
        // `EndOfTrack` forever and REW cannot move the position back out of
        // it — the only way off the last frame would be reloading the file.
        // Seeking is a position operation and has to work at the boundaries.
        if seeking && rate != RATE_PAUSED {
            return self.scan(ring, out, frames, rate);
        }
        // Out of a scan, carry on from where its sound had got to rather than
        // from its fragment's start, which was heard a moment ago. Not on a
        // paused reading mid-seek — `Transport::motion`'s safe guess — which
        // is a scan standing still, not one that has ended.
        if !seeking {
            if let Some(into) = self.into_fragment.take() {
                self.advance(into as f64);
            }
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

    /// FF or REW held: **a fragment of the source at unity, then a jump**, so
    /// the operator hears where the scan is — the user's, 2026-10-26.
    ///
    /// Every sample this emits is read from the source at an integral
    /// position, so nothing sounds but unity — and **the first and last
    /// [`FADE_SECS`] of each fragment are faded**, [`fade_edges`], because
    /// the joins clicked. That is a gain stage in the callback, and the only
    /// one: `CLAUDE.md`'s rule against them carries this exception by the
    /// user's decision of 2026-10-26, on the condition that playback never
    /// passes through it. Playback is `step`'s unity branch, which shares
    /// nothing with this function but the ring, and the tests hold every
    /// period played after a scan — let go at every point of a fragment,
    /// fades included — to the source bit for bit.
    ///
    /// **`position` is where the fragment starts**, and moves only between
    /// fragments, by `rate` fragments' worth. So it travels one way at the
    /// seek's speed, which is what the window thread reads its direction
    /// from and what the display shows, and REW plays its fragments forwards
    /// from each step back — the part of the source the window has just
    /// filled on the way down. A fragment the window does not hold is
    /// silence, and the scan moves on regardless: a seek that waited for the
    /// window would stall at 32x on a fast card's worth of hi-res.
    fn scan(&mut self, ring: &RingReader, out: &mut [i32], frames: u64, rate: f64) -> Outcome {
        let mut done = 0u64;
        while done < frames {
            let into = match self.into_fragment {
                Some(i) if i < self.fragment => i,
                Some(_) => {
                    self.advance(rate * self.fragment as f64);
                    0
                }
                None => 0,
            };
            let n = (frames - done).min(self.fragment - into);
            let dst = &mut out[done as usize * RING_CHANNELS..(done + n) as usize * RING_CHANNELS];
            let from = self.position as u64 + into;
            if from >= self.track_frames {
                silence(dst);
            } else if self.track_frames - from < n {
                // The source's last frames, then silence.
                let _ = ring.read_tail(from, dst);
            } else {
                // A miss has already silenced `dst`.
                let _ = ring.read_block(from, dst);
            }
            fade_edges(dst, into, self.fragment, self.fade);
            self.into_fragment = Some(into + n);
            done += n;
        }
        Outcome::Seeking
    }

    /// Moves the position, clamped to the track. Clamping at zero is what
    /// stops REW running the accumulator negative.
    #[inline]
    fn advance(&mut self, by: f64) {
        self.position = (self.position + by).clamp(0.0, self.track_frames as f64);
    }
}

/// Scales the frames of `dst` that lie within `fade` of either end of a
/// fragment by a raised cosine, from zero at the edge to unity `fade` in.
/// `dst` starts `into` frames into a fragment of `fragment` frames.
///
/// **The only place in this file that scales a sample**, and called from
/// [`Engine::scan`] alone — see there for why it exists and what keeps it out
/// of playback. Never above unity, so it cannot overflow the 24-bit sample.
fn fade_edges(dst: &mut [i32], into: u64, fragment: u64, fade: u64) {
    if fade == 0 {
        return;
    }
    for (i, frame) in dst.chunks_exact_mut(RING_CHANNELS).enumerate() {
        let k = into + i as u64;
        let edge = k.min(fragment - 1 - k);
        if edge >= fade {
            continue;
        }
        let g = 0.5 * (1.0 - (std::f64::consts::PI * edge as f64 / fade as f64).cos());
        for s in frame.iter_mut() {
            *s = (f64::from(*s) * g) as i32;
        }
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
        t.track_loaded(0);
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
        t.track_loaded(0);
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

    /// Whether `out` is the source from frame `from` on, sample for sample —
    /// `filled` writes each frame's own index into it.
    fn source_from(out: &[i32], from: u64) -> bool {
        out.iter().enumerate().all(|(i, &s)| {
            let f = from + (i / RING_CHANNELS) as u64;
            s == f as i32 * 2 + (i % RING_CHANNELS) as i32
        })
    }

    /// Whether `out` is a stretch of a scan fragment: the source from frame
    /// `from` on, `into` frames into a fragment of `fragment`. Stated as what
    /// a fade must be rather than as the formula, so it is not the code
    /// checking itself: **exactly the source** away from the ends, and at
    /// them never louder than the source, never of the other sign, and
    /// silent on the edge frame itself.
    fn scanned(out: &[i32], from: u64, into: u64, fragment: u64) -> bool {
        let fade = fade_frames(fragment);
        assert!(fade > 0, "pointless unless the fragment fades");
        out.chunks_exact(RING_CHANNELS).enumerate().all(|(i, frame)| {
            let k = into + i as u64;
            let edge = k.min(fragment - 1 - k);
            frame.iter().enumerate().all(|(c, &got)| {
                let f = from + i as u64;
                let src = f as i32 * 2 + c as i32;
                if edge >= fade {
                    got == src
                } else if edge == 0 {
                    got == 0
                } else {
                    got.abs() <= src.abs() && (got == 0 || got.signum() == src.signum())
                }
            })
        })
    }

    #[test]
    fn a_scan_fragment_fades_in_and_out_and_is_the_source_between() {
        // The fade, alone: one whole fragment from frame 1000 of a source
        // whose samples are large enough for the ramp to show in every step.
        let (mut w, ring) = ring::new(4096);
        let block: Vec<i32> = (0..4096i32).flat_map(|_| [1 << 22, -(1 << 22)]).collect();
        w.append(&block);
        std::mem::forget(w);
        let t = Transport::new();
        t.track_loaded(0);
        let fragment = 400u64;
        let fade = fade_frames(fragment);
        let mut e = Engine::new(4096).fragments_of(fragment);
        t.request_seek(1000);
        t.begin_seek(true);
        let mut out = vec![0i32; fragment as usize * RING_CHANNELS];
        assert_eq!(e.fill(&t, &ring, &mut out), Outcome::Seeking);

        let left: Vec<i32> = out.chunks_exact(RING_CHANNELS).map(|f| f[0]).collect();
        let n = fragment as usize;
        assert_eq!((left[0], left[n - 1]), (0, 0), "silent at both edges");
        let k = fade as usize;
        assert!(left[..k].windows(2).all(|w| w[0] < w[1]), "rises through the fade-in");
        assert!(left[n - k..].windows(2).all(|w| w[0] > w[1]), "falls through the fade-out");
        assert!(left[k..n - k].iter().all(|&s| s == 1 << 22), "untouched between");
        assert!(out.chunks_exact(2).all(|f| f[1] == -f[0]), "both channels, the same gain");
    }

    #[test]
    fn playback_after_a_scan_is_the_source_bit_for_bit_wherever_it_was_let_go() {
        // **The guard on the one gain stage in the callback.** `CLAUDE.md`
        // allows the scan's fade on the condition that playback never passes
        // through it. Let go at every frame of two whole fragments — the
        // fade-ins and fade-outs included — and every period played after
        // has to be the source, sample for sample, from where the sound had
        // got to. Twice, for letting go to play and to pause and then play.
        let (ring, frames) = filled(16_384);
        let fragment = 200u64;
        assert!(fade_frames(fragment) > 0);
        for let_go_after in 1..=2 * fragment {
            for resume_by_play in [false, true] {
                let t = Transport::new();
                t.track_loaded(0);
                let mut e = Engine::new(frames).fragments_of(fragment);
                t.request_seek(1000);
                t.play();
                t.begin_seek(true);
                // One frame at a time, so the scan stops exactly there.
                let mut one = vec![0i32; RING_CHANNELS];
                for _ in 0..let_go_after {
                    e.fill(&t, &ring, &mut one);
                }
                let at = e.position() as u64 + e.into_fragment.unwrap_or(0);

                if resume_by_play {
                    t.end_seek(false);
                    t.play();
                } else {
                    t.end_seek(true);
                }
                let mut out = vec![0i32; 128 * RING_CHANNELS];
                for p in 0..8u64 {
                    assert_eq!(e.fill(&t, &ring, &mut out), Outcome::Played { frames: 128 });
                    assert!(
                        source_from(&out, at + p * 128),
                        "let go after {let_go_after} frames ({}): period {p} is not the source",
                        if resume_by_play { "paused, then PLAY" } else { "playing" }
                    );
                }
            }
        }
    }

    #[test]
    fn a_held_seek_plays_the_source_in_fragments_and_jumps_by_its_rate() {
        // **Inverted, 2026-10-26, by the user's decision** — `decisions.md`.
        // This test asserted that v1's seek was silent and moved the position
        // at 4x. The scan now plays a fragment of the source at unity and
        // jumps by the rate's worth of fragments; the speed is the deck's to
        // raise while the button is held.
        let (ring, frames) = filled(16_384);
        let t = Transport::new();
        t.track_loaded(0);
        let mut e = Engine::new(frames).fragments_of(256);
        t.play();
        let mut out = vec![0i32; 128 * RING_CHANNELS];
        e.fill(&t, &ring, &mut out);
        assert_eq!(e.position(), 128.0);

        // FF at the starting speed: a fragment from where the deck was...
        t.begin_seek(true);
        assert_eq!(e.fill(&t, &ring, &mut out), Outcome::Seeking);
        assert!(scanned(&out, 128, 0, 256), "the scan starts where the deck was");
        assert_eq!(e.fill(&t, &ring, &mut out), Outcome::Seeking);
        assert!(scanned(&out, 256, 128, 256), "the fragment plays on at unity");
        assert_eq!(e.position(), 128.0, "the position moves between fragments, not in one");
        // ...then the rate's worth of fragments on, and the next one.
        assert_eq!(e.fill(&t, &ring, &mut out), Outcome::Seeking);
        assert_eq!(e.position(), 128.0 + RATE_SEEK * 256.0);
        assert!(scanned(&out, 640, 0, 256));

        // Faster: the same, with longer jumps.
        t.set_seek_speed(32.0);
        e.fill(&t, &ring, &mut out);
        assert!(scanned(&out, 768, 128, 256), "a speed change waits for the fragment to end");
        e.fill(&t, &ring, &mut out);
        assert_eq!(e.position(), 640.0 + 32.0 * 256.0);
        assert!(scanned(&out, 8832, 0, 256));

        // REW: steps back by the same rule, and its fragments still play
        // forwards.
        t.begin_seek(false);
        e.fill(&t, &ring, &mut out);
        assert!(scanned(&out, 8960, 128, 256));
        e.fill(&t, &ring, &mut out);
        assert_eq!(e.position(), 8832.0 - RATE_SEEK * 256.0);
        assert!(scanned(&out, 8320, 0, 256));

        // Let go, playing: on from where the sound was, not from the start
        // of the fragment that was just heard.
        t.end_seek(true);
        assert_eq!(e.fill(&t, &ring, &mut out), Outcome::Played { frames: 128 });
        assert!(source_from(&out, 8448));
    }

    #[test]
    fn rew_can_rewind_out_of_the_end_of_a_track() {
        // Regression. The end-of-track check used to run before the seek, so
        // once the position reached the last frame REW did nothing and the
        // deck was stuck there until the file was reloaded.
        let frames = 1_000u64;
        let (ring, _) = filled(frames);
        let t = Transport::new();
        t.track_loaded(0);
        let mut e = Engine::new(frames).fragments_of(128);
        let mut out = vec![0i32; 128 * RING_CHANNELS];

        t.request_seek(frames);
        t.play();
        assert_eq!(e.fill(&t, &ring, &mut out), Outcome::EndOfTrack);
        assert_eq!(e.position(), frames as f64);

        // The first fragment is at the end, where there is nothing to play.
        t.begin_seek(false);
        assert_eq!(e.fill(&t, &ring, &mut out), Outcome::Seeking);
        assert!(out.iter().all(|&s| s == 0));
        // Then a step back, into the track.
        assert_eq!(e.fill(&t, &ring, &mut out), Outcome::Seeking);
        assert_eq!(e.position(), frames as f64 - RATE_SEEK * 128.0);
        assert!(scanned(&out, frames - 256, 0, 128));

        // And audio comes back on release.
        t.end_seek(true);
        assert!(matches!(
            e.fill(&t, &ring, &mut out),
            Outcome::Played { .. }
        ));
    }

    #[test]
    fn a_fragment_the_window_does_not_hold_is_silence_and_the_scan_moves_on() {
        // A seek that waited for the window would stall at 32x on hi-res. One
        // that plays what it has and moves on reaches the end of the track
        // all the same.
        let (ring, _) = filled(512);
        let t = Transport::new();
        t.track_loaded(0);
        // The track is longer than the ring holds.
        let mut e = Engine::new(100_000).fragments_of(128);
        let mut out = vec![9i32; 128 * RING_CHANNELS];
        t.begin_seek(true);
        t.set_seek_speed(32.0);
        e.fill(&t, &ring, &mut out);
        assert!(scanned(&out, 0, 0, 128), "resident: played");
        e.fill(&t, &ring, &mut out);
        assert_eq!(e.position(), 32.0 * 128.0, "past the ring, and still moving");
        assert!(out.iter().all(|&s| s == 0), "not resident: silence");
    }

    #[test]
    fn rew_cannot_drive_the_position_negative() {
        let (ring, frames) = filled(2048);
        let t = Transport::new();
        t.track_loaded(0);
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
        t.track_loaded(0);
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
        t.track_loaded(0);
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
        t.track_loaded(0);
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
        t.track_loaded(0);
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
        t.track_loaded(0);
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
