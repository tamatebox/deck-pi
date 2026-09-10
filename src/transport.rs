//! The transport: the rate variable, the float64 position, and what the
//! buttons mean.
//!
//! It writes a lock-free slot the callback reads and **never touches the
//! ring** (docs/architecture.md, "Shape of the program"). The position is
//! owned by the callback, which is the only thing that advances it; the
//! transport publishes intent, and reads the position back for the display.
//!
//! # CUE is one button with three behaviours
//!
//! Taken from the CDJ-350 operating instructions (Pioneer 389414-01U, p.18)
//! and recorded in `decisions.md`, not from memory:
//!
//! | State | CUE | The manual's name |
//! |---|---|---|
//! | Paused, away from the cue point | **sets** the point there | Setting Cue |
//! | Playing | **returns** to the point and pauses | Back Cue |
//! | Paused **at** the point | **plays while held** | Cue Point Sampler |
//!
//! Four details that matter: one cue point per track, and setting a new one
//! cancels the old; setting it makes no sound; Back Cue **pauses and does not
//! resume**, so PLAY restarts from the point; and the preview is momentary,
//! with no latching.
//!
//! **There is no separate STOP**, because a CDJ has none — returning to the
//! cue point and standing by *is* stopping. So the `CUE / STOP` label on
//! GPIO 25 is one function, and the hold gesture is free for the preview.
//! None of it needs a new mechanism: hold is `r = 1.0`, release is `r = 0`
//! with the position set back to the point.
//!
//! Auto cue is deliberately **not** implemented. The CDJ-350 skips the silent
//! lead-in on load and places the cue where sound starts; `decisions.md`
//! rejects that here, because a long-form piece may open below -78 dB on
//! purpose and letting the deck decide where the music "really" begins is the
//! kind of silent, well-meant alteration this project exists to avoid. The
//! cue point starts at frame zero unless set.

use std::sync::atomic::{fence, AtomicBool, AtomicI64, AtomicU32, AtomicU64, AtomicU8, Ordering};

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
    /// **Nothing loaded** — and *only* that. Not the DJ's "stopped": a CDJ has
    /// no STOP, because returning to the cue point and pausing is what
    /// stopping means (`decisions.md`), so that gesture and a track reaching
    /// its end both land on `Paused`, still loaded and sitting on a frame.
    ///
    /// **A one-way door.** A fresh `Transport` really is `Stopped` — it is
    /// the value the type is constructed with, and the test below asserts it
    /// — but no `state.store` anywhere targets it. The machine can be in this
    /// state and cannot get back to it.
    ///
    /// That is not an oversight, and the reason has changed once already —
    /// which is why it says what it is waiting for rather than just "later".
    /// It was that nothing *unloaded* a track, because no module owned "what
    /// is loaded" ([#14](https://github.com/tamatebox/deck-pi/issues/14)).
    /// That has landed: `Loaded::unload` exists. What is missing now is
    /// smaller and more specific — `unload` clears only its own field, and
    /// the app loop that would call it and store `Stopped` here is not built.
    /// A reader who greps for a writer and finds only the constructor has not
    /// missed one.
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
///
/// # Publishing order
///
/// **"No torn state" is true of each field and was false of the pair the
/// callback decides on.** `Engine::step` reads `rate` and then `is_silent()`,
/// and a control-thread write of the two is two stores: interleave them and
/// the callback sees a combination that never existed. No memory reordering is
/// needed — two adjacent loads against two adjacent stores is enough.
///
/// The combination that mattered was `rate == ±RATE_SEEK` with `silent ==
/// false`, which `step` reads as "a rate other than unity that is not a silent
/// seek" and reports as `Outcome::NeedsResampler`. v1 has no resampler, so the
/// caller treats that as fatal — measured at **10.7M of 35.6M fills** with the
/// control thread pressing and releasing FF, which is a rare event in real use
/// and a stopped deck when it happens.
///
/// The fix is an ordering rule, not a lock and not a wider atomic:
///
/// - **Entering** a silent state, set `silent` **first**, then the rate.
/// - **Leaving** one, set the rate **first**, then `silent`.
///
/// So `silent` is never false while the rate is still a seek rate, and the bad
/// combination cannot be observed. What a badly timed read can still see is
/// `silent == true` with a rate of unity or zero, which costs one period of
/// silence or one period of seeking at 1x instead of 4x. Both are recoverable
/// and neither is reported as an error.
///
/// **Deriving `silent` from the rate would remove the pair entirely and is
/// deliberately not done.** In v1 `silent` holds exactly when `|rate| ==
/// RATE_SEEK`, so the flag looks redundant — but `decisions.md` has FF/REW
/// becoming an **audible** `r = 4` in v2, so the equivalence is a v1-only
/// accident. Encoding it here would work now and have to be unpicked then,
/// which is the same trap as serving the ring with a FIFO.
pub struct Transport {
    /// The rate variable `r`. One number carries pause, play, and in v2 pitch
    /// and jog, which is why all three ride the same read path.
    rate: AtomicU64,
    /// Version around the `(rate, silent)` pair, odd while a write is in
    /// flight. See "Publishing order" on the type — the pair is what the
    /// callback decides on, and it has to be read as one thing.
    motion: AtomicU32,
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
    /// The track's single cue point, in frames. Frame zero until set — not
    /// `None`, because `decisions.md` fixes the unset value at the start of
    /// the file rather than at wherever sound begins.
    cue: AtomicU64,
    /// True while CUE is held down for the momentary preview, so releasing it
    /// knows to stop and return rather than leaving the deck playing.
    previewing: AtomicBool,
}

impl Default for Transport {
    fn default() -> Self {
        Transport {
            rate: AtomicU64::new(RATE_PAUSED.to_bits()),
            motion: AtomicU32::new(0),
            silent: AtomicBool::new(false),
            seek_to: AtomicI64::new(-1),
            position: AtomicU64::new(0.0f64.to_bits()),
            state: AtomicU8::new(State::Stopped as u8),
            cue: AtomicU64::new(0),
            previewing: AtomicBool::new(false),
        }
    }
}

impl Transport {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- control thread ----

    pub fn play(&self) {
        self.publish_motion(RATE_UNITY, false);
        self.state.store(State::Playing as u8, Ordering::Relaxed);
    }

    /// Also the STOP half of `CUE / STOP`: the position is left where it is.
    /// Whether STOP should return to a cue point is part of the undecided CUE
    /// semantics, so it is not done here.
    pub fn pause(&self) {
        self.publish_motion(RATE_PAUSED, false);
        self.state.store(State::Paused as u8, Ordering::Relaxed);
    }

    /// FF or REW held down. The position moves and the display follows, but
    /// no audio is produced — an audible scan needs the resampler, which
    /// would give v1 a second mode and break its unconditional
    /// bit-perfection.
    pub fn begin_seek(&self, forward: bool) {
        let r = if forward { RATE_SEEK } else { -RATE_SEEK };
        self.publish_motion(r, true);
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
    ///
    /// **It does nothing unless the deck is still seeking, and that guard is
    /// not defensive tidiness.** `was_playing` is captured when the button
    /// goes down and is stale the moment anything else changes the state
    /// underneath it. The case that matters is CUE pressed during a held FF:
    /// `hardware.md` chooses Back Cue there, which pauses at the cue point —
    /// and then releasing FF used to hand `was_playing == true` back and
    /// **start playing**, directly against the CDJ-350's "Back Cue pauses; it
    /// does not resume", which `decisions.md` quotes. The release is only
    /// entitled to end a seek it is still in the middle of; if something else
    /// has already decided the state, that decision stands.
    ///
    /// Sound without a lock because every caller of this and of `cue_down` /
    /// `cue_up` is the control thread, so the read and the write below cannot
    /// interleave with another state change.
    pub fn end_seek(&self, was_playing: bool) {
        if !matches!(self.state(), State::SeekingForward | State::SeekingBack) {
            return;
        }
        if was_playing {
            self.play();
        } else {
            self.pause();
        }
    }

    /// CUE pressed. Which of the three behaviours happens is decided from
    /// the current state, exactly as it is on the player.
    ///
    /// The one combination the manual does not cover is CUE pressed while FF
    /// or REW is held. Treated here as Back Cue, on the grounds that anything
    /// other than paused is "moving" and returning to the point is the
    /// predictable answer; noted because it is a reading, not a quotation.
    pub fn cue_down(&self) {
        let paused = self.rate() == RATE_PAUSED;
        let at_cue = self.position() == self.cue_point() as f64;

        if paused && at_cue {
            // Cue Point Sampler: plays while held.
            self.previewing.store(true, Ordering::Relaxed);
            self.play();
        } else if paused {
            // Setting Cue. "No sound is output at this time" — nothing here
            // starts the transport, so that holds by construction.
            self.cue.store(self.position() as u64, Ordering::Release);
        } else {
            // Back Cue: return to the point and pause. It does not resume;
            // PLAY restarts from the point.
            self.back_cue();
        }
    }

    /// CUE released. Only meaningful after a preview, which is momentary.
    pub fn cue_up(&self) {
        if self.previewing.swap(false, Ordering::AcqRel) {
            self.back_cue();
        }
    }

    /// Returns to the cue point and pauses. Both halves already existed: a
    /// one-shot seek request and `r = 0`.
    pub fn back_cue(&self) {
        self.previewing.store(false, Ordering::Relaxed);
        self.pause();
        self.request_seek(self.cue_point());
    }

    /// The track's cue point, in frames.
    pub fn cue_point(&self) -> u64 {
        self.cue.load(Ordering::Acquire)
    }

    /// Sets the cue point directly. For the cue store restoring a saved point
    /// on load; the button goes through [`cue_down`](Self::cue_down).
    pub fn set_cue_point(&self, frame: u64) {
        self.cue.store(frame, Ordering::Release);
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

    /// Publishes the rate and the silent flag as one indivisible change.
    ///
    /// Only the control thread calls this, so the version needs no
    /// compare-and-swap; the odd value is what tells a reader mid-flight.
    fn publish_motion(&self, rate: f64, silent: bool) {
        let v = self.motion.load(Ordering::Relaxed);
        self.motion.store(v.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        self.silent.store(silent, Ordering::Relaxed);
        self.rate.store(rate.to_bits(), Ordering::Relaxed);
        self.motion.store(v.wrapping_add(2), Ordering::Release);
    }

    /// The rate and the silent flag, as one consistent reading.
    ///
    /// **Use this, not `rate()` and `is_silent()` separately, anywhere the two
    /// are decided on together.** Those remain for callers that genuinely want
    /// one — the display wants the rate, a test wants the flag — but the
    /// callback's branch depends on the pair, and two loads across two stores
    /// see combinations that never existed. See "Publishing order" on the type.
    ///
    /// Bounded at two attempts, so it stays O(1) as the callback rules
    /// require. If both race — which needs a control write to land inside a
    /// window of a few instructions, twice — it reports a paused, silent deck:
    /// one period of silence, recovered on the next call. That is the only
    /// reading that is safe to invent, because it neither moves the position
    /// nor emits anything.
    #[inline]
    pub fn motion(&self) -> (f64, bool) {
        for _ in 0..2 {
            let before = self.motion.load(Ordering::Acquire);
            if before & 1 == 0 {
                let silent = self.silent.load(Ordering::Relaxed);
                let rate = f64::from_bits(self.rate.load(Ordering::Relaxed));
                // The two loads above must not sink past this check. The
                // `Acquire` on the load below orders what *follows* it, not
                // what precedes it — the same asymmetry that left `src/ring.rs`
                // unsound, so the fence is not optional here either.
                fence(Ordering::Acquire);
                if self.motion.load(Ordering::Relaxed) == before {
                    return (rate, silent);
                }
            }
            std::hint::spin_loop();
        }
        (RATE_PAUSED, true)
    }

    #[inline]
    pub fn is_silent(&self) -> bool {
        // `Acquire`, pairing with the `Release` on the *clear* in `play`,
        // `pause` and `reached_end`. Seeing `false` here must also mean
        // seeing the rate that was stored before it, or the callback decides
        // on a pair that never existed. See "Publishing order" on the type.
        self.silent.load(Ordering::Acquire)
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
        self.publish_motion(r, self.silent.load(Ordering::Relaxed));
    }

    /// Reports reaching the end of the track. Stops the rate so the position
    /// does not run past the last frame, and advances nothing.
    ///
    /// `decisions.md`: "A track that reaches its end stops. Nothing starts on
    /// its own. In a venue, a next track beginning while attention is
    /// elsewhere is worse than a silence, and PLAY is right there."
    /// Auto-advance is a later addition if wanted, not an omission here.
    ///
    /// **Whoever drives the playback loop must call this when the engine
    /// returns `Outcome::EndOfTrack`, and it must be the control thread.**
    /// Nothing called it for a long time: the engine reported the outcome and
    /// touched the transport not at all, so at the end of a track the deck
    /// read `Playing` at rate 1.0 for ever — a display saying "playing" over
    /// silence, and a PLAY press that pauses. `decisions.md` says the
    /// decision "lands" here; it landed nowhere, and the two tests below
    /// exercised a function with no callers.
    ///
    /// **The engine deliberately does not call it itself**, tempting as that
    /// is when it already holds a `&Transport`. `fill` runs on the audio
    /// thread, and these stores would then race a control-thread `play()` or
    /// `begin_seek()` — the same split-store hazard the rest of this type is
    /// careful about, introduced to save the caller a line.
    pub fn reached_end(&self) {
        self.publish_motion(RATE_PAUSED, false);
        self.previewing.store(false, Ordering::Relaxed);
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
    fn the_cue_point_starts_at_frame_zero() {
        // Not at "where sound starts": auto cue is deliberately not adopted,
        // because a long-form piece may open below -78 dB on purpose.
        let t = Transport::new();
        assert_eq!(t.cue_point(), 0);
    }

    #[test]
    fn cue_while_paused_sets_the_point_and_makes_no_sound() {
        let t = Transport::new();
        t.pause();
        t.publish_position(50_000.0);
        t.cue_down();
        assert_eq!(t.cue_point(), 50_000);
        assert_eq!(t.rate(), RATE_PAUSED, "\"No sound is output at this time\"");
        assert_eq!(t.take_seek(), None, "setting a point must not move the deck");
        t.cue_up();
        assert_eq!(t.rate(), RATE_PAUSED, "release after setting must do nothing");

        // "When a new cue point is set, the previously set cue point is
        // canceled." One point, not a set of hot cues.
        t.publish_position(120_000.0);
        t.cue_down();
        assert_eq!(t.cue_point(), 120_000);
    }

    #[test]
    fn cue_while_playing_returns_to_the_point_and_pauses_without_resuming() {
        let t = Transport::new();
        t.set_cue_point(1_000);
        t.play();
        t.publish_position(80_000.0);

        t.cue_down();
        assert_eq!(t.rate(), RATE_PAUSED, "Back Cue pauses; it does not resume");
        assert_eq!(t.state(), State::Paused);
        assert_eq!(t.take_seek(), Some(1_000), "returns to the cue point");
        assert_eq!(t.cue_point(), 1_000, "Back Cue must not move the point");

        // And PLAY restarts from the point, not from where it was.
        t.play();
        assert_eq!(t.rate(), RATE_UNITY);
        assert_eq!(t.take_seek(), None, "PLAY queues no seek of its own");
    }

    #[test]
    fn cue_held_at_the_point_previews_and_release_returns() {
        // Cue Point Sampler. "Playback continues while the button is held in"
        // — so release means stop and return, with no latching.
        let t = Transport::new();
        t.set_cue_point(4_410);
        t.pause();
        t.publish_position(4_410.0);

        t.cue_down();
        assert_eq!(t.rate(), RATE_UNITY, "plays while held");
        assert_eq!(t.state(), State::Playing);
        assert_eq!(t.take_seek(), None, "already at the point; nothing to seek");
        assert_eq!(t.cue_point(), 4_410, "previewing must not re-set the point");

        // Pretend the callback advanced during the preview.
        t.publish_position(9_000.0);
        t.cue_up();
        assert_eq!(t.rate(), RATE_PAUSED, "release stops");
        assert_eq!(t.take_seek(), Some(4_410), "and returns to the point");
    }

    #[test]
    fn a_second_release_after_a_preview_does_nothing() {
        // The latch is consumed, so a stray release cannot silently re-cue a
        // deck the user has since started playing.
        let t = Transport::new();
        t.pause();
        t.publish_position(0.0);
        t.cue_down();
        t.cue_up();
        assert_eq!(t.take_seek(), Some(0));

        t.play();
        t.cue_up();
        assert_eq!(t.rate(), RATE_UNITY, "a spurious release must not stop playback");
        assert_eq!(t.take_seek(), None);
    }

    #[test]
    fn cue_during_a_held_seek_is_treated_as_back_cue() {
        // The manual does not cover this combination; this is the reading
        // recorded in the module docs, asserted so it cannot drift silently.
        let t = Transport::new();
        t.set_cue_point(2_000);
        t.play();
        t.begin_seek(true);
        t.publish_position(60_000.0);
        t.cue_down();
        assert_eq!(t.rate(), RATE_PAUSED);
        assert!(!t.is_silent(), "back cue leaves the deck ready to play, not muted");
        assert_eq!(t.take_seek(), Some(2_000));
    }

    #[test]
    fn reaching_the_end_clears_a_latched_preview() {
        // A preview that ran into the end of the track must not leave the
        // latch set, or the next release would cue a deck nobody previewed.
        let t = Transport::new();
        t.pause();
        t.publish_position(0.0);
        t.cue_down();
        t.reached_end();
        t.cue_up();
        assert_eq!(t.take_seek(), None, "the latch must have been cleared");
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
