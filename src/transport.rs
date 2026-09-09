//! The transport: the rate variable, the float64 position, and what the
//! buttons mean.
//!
//! It writes a lock-free slot the callback reads and **never touches the
//! ring** (docs/architecture.md, "Shape of the program"). The position is
//! owned by the callback, which is the only thing that advances it; the
//! transport publishes intent, and reads the position back for the display.
//!
//! # What is deliberately missing
//!
//! **CUE.** `hardware.md` gives GPIO 25 the label `CUE / STOP` and
//! `architecture.md` makes "what PLAY / CUE / FF / REW mean" the transport's
//! job, but no document says what CUE *does* — whether it sets a point,
//! returns to one, or stops. That is a usage decision, not something derivable
//! from the boards, so it is not implemented here rather than guessed at.
//! [`Transport::pause`] covers the STOP half.
//!
//! **Auto-advance at the end of a track.** An open question in
//! `hardware.md`: "whether a track auto-advances when it ends. Stopping is
//! believed to be the usual default on DJ players, but that is recollection,
//! not a checked fact." So the engine *reports* reaching the end and this
//! module does nothing about it.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicU8, Ordering};

/// Unity. At this rate, with output rate matched to the source, the samples
/// reach the DAC untouched — which is the whole point of v1.
pub const RATE_UNITY: f64 = 1.0;

/// Pause. `architecture.md`: "pause is `r = 0`".
pub const RATE_PAUSED: f64 = 0.0;

/// How fast FF and REW move the position while held.
///
/// **Inferred, not stated.** `architecture.md` says that in v2 the silent
/// seek "becomes `r = 4` on the rate variable", so using 4 in v1 too makes
/// the v2 change a pure unmute rather than also a speed change. No document
/// gives v1's seek speed directly.
pub const RATE_SEEK: f64 = 4.0;

/// What the display should say. The callback never reads this; it exists so
/// the UI does not have to reconstruct intent from a float.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    /// Nothing loaded, or stopped.
    Stopped = 0,
    Playing = 1,
    Paused = 2,
    SeekingForward = 3,
    SeekingBack = 4,
}

impl State {
    fn from_u8(v: u8) -> State {
        match v {
            1 => State::Playing,
            2 => State::Paused,
            3 => State::SeekingForward,
            4 => State::SeekingBack,
            _ => State::Stopped,
        }
    }
}

/// The lock-free slot between the control thread and the callback.
///
/// Every field is a single atomic, so there is no torn state to guard and no
/// lock for the callback to take. `f64`s travel as their bit patterns, which
/// is exact — this is a reinterpretation, not a conversion.
pub struct Transport {
    /// The rate variable `r`. One number carries pause, play, and in v2 pitch
    /// and jog, which is why all three ride the same read path.
    rate: AtomicU64,
    /// True while the audio must be muted even though the position is moving.
    /// This is what makes FF and REW a *silent* seek in v1 without giving v1
    /// a second read mode. v2 clears it and the same seek becomes audible.
    silent: AtomicBool,
    /// A one-shot absolute seek. `-1` means none; the callback swaps it out,
    /// so a request is consumed exactly once.
    seek_to: AtomicI64,
    /// Published by the callback for the display and the window thread.
    position: AtomicU64,
    state: AtomicU8,
}

impl Default for Transport {
    fn default() -> Self {
        Transport {
            rate: AtomicU64::new(RATE_PAUSED.to_bits()),
            silent: AtomicBool::new(false),
            seek_to: AtomicI64::new(-1),
            position: AtomicU64::new(0.0f64.to_bits()),
            state: AtomicU8::new(State::Stopped as u8),
        }
    }
}

impl Transport {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- control thread ----

    pub fn play(&self) {
        self.silent.store(false, Ordering::Relaxed);
        self.rate.store(RATE_UNITY.to_bits(), Ordering::Release);
        self.state.store(State::Playing as u8, Ordering::Relaxed);
    }

    /// Also the STOP half of `CUE / STOP`: the position is left where it is.
    /// Whether STOP should return to a cue point is part of the undecided CUE
    /// semantics, so it is not done here.
    pub fn pause(&self) {
        self.silent.store(false, Ordering::Relaxed);
        self.rate.store(RATE_PAUSED.to_bits(), Ordering::Release);
        self.state.store(State::Paused as u8, Ordering::Relaxed);
    }

    /// FF or REW held down. The position moves and the display follows, but
    /// no audio is produced — an audible scan needs the resampler, which
    /// would give v1 a second mode and break its unconditional
    /// bit-perfection.
    pub fn begin_seek(&self, forward: bool) {
        let r = if forward { RATE_SEEK } else { -RATE_SEEK };
        self.silent.store(true, Ordering::Relaxed);
        self.rate.store(r.to_bits(), Ordering::Release);
        self.state.store(
            if forward {
                State::SeekingForward as u8
            } else {
                State::SeekingBack as u8
            },
            Ordering::Relaxed,
        );
    }

    /// FF or REW released. `architecture.md`: "audio resumes on release."
    ///
    /// `was_playing` is the caller's memory of what the transport was doing
    /// before the button went down — releasing FF must not start playback
    /// that was not running.
    pub fn end_seek(&self, was_playing: bool) {
        if was_playing {
            self.play();
        } else {
            self.pause();
        }
    }

    /// Ask the callback to jump. Absolute, in frames.
    ///
    /// Saturated at `i64::MAX` rather than cast, because the slot uses `-1`
    /// as its "no request" sentinel and a bare `as i64` turns `u64::MAX` into
    /// exactly that — a seek that is silently discarded. No real track is
    /// 2^63 frames long, so this only ever fires on a bad input, which is
    /// precisely the case that must not vanish. The engine clamps the
    /// saturated value to the track and reports the end.
    pub fn request_seek(&self, frame: u64) {
        self.seek_to
            .store(frame.min(i64::MAX as u64) as i64, Ordering::Release);
    }

    // ---- either thread ----

    #[inline]
    pub fn rate(&self) -> f64 {
        f64::from_bits(self.rate.load(Ordering::Acquire))
    }

    #[inline]
    pub fn is_silent(&self) -> bool {
        self.silent.load(Ordering::Relaxed)
    }

    pub fn state(&self) -> State {
        State::from_u8(self.state.load(Ordering::Relaxed))
    }

    /// The position, as the display should show it.
    #[inline]
    pub fn position(&self) -> f64 {
        f64::from_bits(self.position.load(Ordering::Relaxed))
    }

    // ---- audio callback ----

    /// Takes a pending seek, if any. Wait-free and consumes the request, so a
    /// button press cannot be serviced twice.
    #[inline]
    pub fn take_seek(&self) -> Option<u64> {
        let v = self.seek_to.swap(-1, Ordering::AcqRel);
        if v < 0 {
            None
        } else {
            Some(v as u64)
        }
    }

    #[inline]
    pub fn publish_position(&self, frame: f64) {
        self.position.store(frame.to_bits(), Ordering::Relaxed);
    }

    /// Sets the rate variable directly.
    ///
    /// **Test only, on purpose.** v1 exposes no public setter for an
    /// arbitrary rate, because there is no resampler to serve one and a
    /// control that could ask for 1.05 would be a control that silently
    /// breaks bit-perfection. v2 adds pitch and jog, and with them the unity
    /// button that owns the mode question.
    #[cfg(test)]
    pub(crate) fn set_rate_for_test(&self, r: f64) {
        self.rate.store(r.to_bits(), Ordering::Release);
    }

    /// Reports reaching the end of the track. Stops the rate so the position
    /// does not run past the last frame; **does not** advance to another
    /// track, because whether it should is an open question.
    pub fn reached_end(&self) {
        self.rate.store(RATE_PAUSED.to_bits(), Ordering::Release);
        self.silent.store(false, Ordering::Relaxed);
        self.state.store(State::Paused as u8, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_and_pause_are_the_rate_variable_and_nothing_else() {
        let t = Transport::new();
        assert_eq!(t.rate(), RATE_PAUSED);
        assert_eq!(t.state(), State::Stopped);

        t.play();
        assert_eq!(t.rate(), RATE_UNITY);
        assert!(!t.is_silent());
        assert_eq!(t.state(), State::Playing);

        t.pause();
        assert_eq!(t.rate(), RATE_PAUSED);
        assert_eq!(t.state(), State::Paused);
    }

    #[test]
    fn a_held_seek_mutes_but_keeps_the_position_moving() {
        let t = Transport::new();
        t.play();
        t.begin_seek(true);
        assert!(t.is_silent(), "v1's seek is silent");
        assert_eq!(t.rate(), RATE_SEEK);
        assert_eq!(t.state(), State::SeekingForward);

        t.begin_seek(false);
        assert_eq!(t.rate(), -RATE_SEEK, "REW runs the rate variable negative");
        assert_eq!(t.state(), State::SeekingBack);
    }

    #[test]
    fn releasing_a_seek_resumes_only_what_was_running() {
        let t = Transport::new();
        t.play();
        t.begin_seek(true);
        t.end_seek(true);
        assert_eq!(t.rate(), RATE_UNITY, "audio resumes on release");
        assert!(!t.is_silent());

        // Held from a paused deck: releasing must not start playback.
        t.pause();
        t.begin_seek(true);
        t.end_seek(false);
        assert_eq!(t.rate(), RATE_PAUSED);
        assert_eq!(t.state(), State::Paused);
    }

    #[test]
    fn a_seek_request_is_consumed_exactly_once() {
        let t = Transport::new();
        assert_eq!(t.take_seek(), None);
        t.request_seek(12_345);
        assert_eq!(t.take_seek(), Some(12_345));
        assert_eq!(t.take_seek(), None, "a request must not be serviced twice");
        // Frame 0 is a legitimate target and must not read as "no request".
        t.request_seek(0);
        assert_eq!(t.take_seek(), Some(0));

        // And an absurd target must still arrive as *a* request rather than
        // colliding with the sentinel. `u64::MAX as i64` is -1, which is the
        // sentinel exactly; saturating instead keeps it visible so the engine
        // can clamp it to the track and report the end.
        t.request_seek(u64::MAX);
        assert_eq!(t.take_seek(), Some(i64::MAX as u64));
    }

    #[test]
    fn the_rate_and_position_survive_the_trip_through_atomics_exactly() {
        // f64 travels as its bit pattern, so this is a reinterpretation and
        // not a conversion. If it were ever routed through f32, the position
        // invariant would be silently lost.
        let t = Transport::new();
        for v in [0.0, 1.0, -4.0, 1.0 / 3.0, 8_388_609.5, f64::MAX] {
            t.publish_position(v);
            assert_eq!(t.position(), v, "position {} did not round trip", v);
        }
    }

    #[test]
    fn reaching_the_end_stops_the_rate_and_advances_nothing() {
        let t = Transport::new();
        t.play();
        t.reached_end();
        assert_eq!(t.rate(), RATE_PAUSED);
        assert_eq!(t.state(), State::Paused);
        // Auto-advance is an open question; nothing here may decide it.
        assert_eq!(t.take_seek(), None, "reaching the end must not queue a seek");
    }
}
