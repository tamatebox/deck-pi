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
    /// **It was a one-way door and is not any more.**
    /// [`Transport::track_unloaded`] stores it, `app::track::Playing::unload`
    /// calls that after joining the audio thread, and `tests/app_track_test.rs`
    /// asserts the round trip. Recorded because the history is the useful
    /// part: the state was constructed and never stored for as long as
    /// nothing *owned* "what is loaded"
    /// ([#14](https://github.com/tamatebox/deck-pi/issues/14)), then for as
    /// long as `Loaded::unload` existed with no app loop to call it. Both
    /// halves read as finished from inside, and neither was.
    ///
    /// **Note which check missed it, because that is the transferable part.**
    /// Grepping for construction sites — `implementation.md`'s cheap sweep
    /// for a declared-but-unreached mechanism — reports this state healthy,
    /// since the constructor is right there in `Default`. What finds a
    /// one-way door is asking which transitions lead *into* each state.
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
/// What a CUE press or release did, and therefore what the caller now owes.
///
/// **Returned rather than inferred, and that is the point.** `cue_down` picks
/// one of three behaviours from the deck's own state, and a caller that needs
/// to know which — the cue store must be written when the point is *set*, the
/// window must be told when the deck *returns* — used to have to read that
/// state before the call and reason about which branch it implied. Two of
/// this project's worst defects are that pattern: a decision made from a
/// value somebody else could change, and a pair of loads taken in the wrong
/// order. The branch is known exactly at the point it is taken, so it is
/// returned from there.
///
/// `#[must_use]` on the methods is the enforcement. `implementation.md`'s
/// ninth shape is a precondition the code states and nothing checks; an
/// obligation carried in a return value the compiler will not let you drop is
/// the one form of it that cannot go unmet by omission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cued {
    /// Nothing to do. Nothing was loaded, or a release that was not a
    /// preview.
    Nothing,
    /// **Setting Cue.** The point is now this frame, and the caller owns
    /// persisting it — `Loaded::set_cue`, never `CueStore::set` with a path
    /// of the caller's own choosing.
    Set(u64),
    /// **Back Cue.** A seek to this frame is queued and the deck is paused.
    /// The caller owns telling the window it is a *jump* and not motion —
    /// `window::Command::Relocate`, via `app::track::Playing::relocate`.
    /// Without it the window infers a scrub and rebuilds below the target, so
    /// the one frame wanted first arrives last.
    Returned(u64),
    /// **Cue Point Sampler.** Playing while the button is held; the release
    /// will be a `Returned`.
    Previewing,
}

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
    /// A one-shot absolute seek. `-1` means none; the callback peeks it,
    /// applies it, publishes the position, and only then retires it — see
    /// [`Transport::peek_seek`] for why that order matters.
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
        if self.nothing_loaded() {
            return;
        }
        self.publish_motion(RATE_UNITY, false);
        self.state.store(State::Playing as u8, Ordering::Relaxed);
    }

    /// **Nothing loaded, so nothing to control.**
    ///
    /// `Stopped` means no track (the type's own doc), so every control that
    /// would move a playhead has to be a no-op here — PLAY on an empty deck
    /// would otherwise set rate 1.0 and `State::Playing`, and the display
    /// would read "playing" over a deck holding nothing.
    ///
    /// **This became reachable and was not audited in the same change**,
    /// which is `implementation.md`'s own rule about a dormant branch: for
    /// as long as `Stopped` was constructed and never stored, no deck was
    /// ever in it after the first press, so nothing that reads it could be
    /// wrong. `Playing::unload` stores it now. The branch going live is what
    /// made these guards necessary, and the audit that should have come with
    /// it is this.
    #[inline]
    fn nothing_loaded(&self) -> bool {
        self.state() == State::Stopped
    }

    /// Also the STOP half of `CUE / STOP`: the position is left where it is.
    /// Whether STOP should return to a cue point is part of the undecided CUE
    /// semantics, so it is not done here.
    pub fn pause(&self) {
        if self.nothing_loaded() {
            return;
        }
        self.publish_motion(RATE_PAUSED, false);
        self.state.store(State::Paused as u8, Ordering::Relaxed);
    }

    /// FF or REW held down. The position moves and the display follows, but
    /// no audio is produced — an audible scan needs the resampler, which
    /// would give v1 a second mode and break its unconditional
    /// bit-perfection.
    pub fn begin_seek(&self, forward: bool) {
        if self.nothing_loaded() {
            return;
        }
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
    /// `controls.md` chooses Back Cue there, which pauses at the cue point —
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
    #[must_use = "a CUE press carries an obligation — see `Cued`"]
    pub fn cue_down(&self) -> Cued {
        if self.nothing_loaded() {
            return Cued::Nothing;
        }
        let paused = self.rate() == RATE_PAUSED;
        let at_cue = self.position() == self.cue_point() as f64;

        if paused && at_cue {
            // Cue Point Sampler: plays while held.
            self.previewing.store(true, Ordering::Relaxed);
            self.play();
            Cued::Previewing
        } else if paused {
            // Setting Cue. "No sound is output at this time" — nothing here
            // starts the transport, so that holds by construction.
            let at = self.position() as u64;
            self.cue.store(at, Ordering::Release);
            Cued::Set(at)
        } else {
            // Back Cue: return to the point and pause. It does not resume;
            // PLAY restarts from the point.
            Cued::Returned(self.back_cue())
        }
    }

    /// CUE released. Only meaningful after a preview, which is momentary.
    #[must_use = "a CUE release carries an obligation — see `Cued`"]
    pub fn cue_up(&self) -> Cued {
        if self.previewing.swap(false, Ordering::AcqRel) {
            Cued::Returned(self.back_cue())
        } else {
            Cued::Nothing
        }
    }

    /// Returns to the cue point and pauses. Both halves already existed: a
    /// one-shot seek request and `r = 0`.
    pub fn back_cue(&self) -> u64 {
        self.previewing.store(false, Ordering::Relaxed);
        self.pause();
        let to = self.cue_point();
        self.request_seek(to);
        to
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

    /// The position, or `None` while a seek is in flight.
    ///
    /// **Use this, not [`position`](Self::position), for anything you are
    /// about to act on.** The callback's protocol is apply, publish, retire
    /// ([`peek_seek`](Self::peek_seek)), so a reader has to observe those in
    /// the mirror order — retire first, position second — and a reader that
    /// takes them the other way round reads a pre-seek position and then asks
    /// a question that has already been answered. That is not a smaller
    /// version of the same mistake; it is the same mistake. The end-of-track
    /// pause was written with the loads in the wrong order on the day the
    /// protocol was introduced, by the same hand, one file away.
    ///
    /// **The `Acquire` on the peek is what makes it work, and only in this
    /// order.** Acquire orders what *follows* it, so peeking first puts the
    /// position load after the retire it is paired with; peeking second
    /// orders nothing that has already happened. `src/ring.rs`'s module doc
    /// has the same asymmetry costing 18 corrupt reads in 90.6M, and
    /// [`motion`](Self::motion) cites it — this is that argument's third
    /// appearance in this codebase, which is the reason it is spelled once
    /// here instead of at each call site.
    ///
    /// **Measured: no test in this repository can see the order, which is
    /// why the pair is behind one function rather than left to call sites.**
    /// Swapping the two lines below turns the whole suite red in **0 runs of
    /// 10**; so does giving the caller the two loads and letting it take them
    /// in the wrong order — which is the state that was committed in
    /// `3492ea4`, green, and found by review rather than by the suite. A
    /// defect no check can catch is not made safe by care; it is made safe by
    /// being unavailable. That is what this function is: not a convenience
    /// over `peek_seek` plus `position`, but the only spelling of the pair
    /// that a caller cannot get wrong.
    ///
    /// `None` means "ask again next period", which is 2.9 ms at 44.1 kHz with
    /// 128-frame periods. A caller that cannot wait — `cue_down` must do
    /// *something* with every press, because `controls.md` refuses controls
    /// that sometimes do nothing — reads `position` directly and accepts a
    /// staleness bounded by one period against a 30-50 ms debounce.
    #[inline]
    pub fn settled_position(&self) -> Option<f64> {
        if self.peek_seek().is_some() {
            return None;
        }
        Some(self.position())
    }

    // ---- audio callback ----

    /// A queued seek, **without** consuming it. Pair with
    /// [`consumed_seek`](Self::consumed_seek).
    ///
    /// # Why this is two calls and not a swap
    ///
    /// It was a swap, and the split is what lets the callback **publish the
    /// position it lands on before the request is cleared**. That ordering is
    /// load-bearing for a control thread deciding anything about where the
    /// deck is: with the clear first, there is an interval in which no seek is
    /// pending *and* `position` still reads the pre-seek value, and any reader
    /// that treats "no seek pending" as "the position is current" is wrong
    /// inside it. It is a few instructions wide, and it was wide enough — the
    /// end-of-track pause read exactly that pair and stopped a deck the
    /// operator had just started. See [`reached_end`](Self::reached_end).
    ///
    /// Peek, apply, publish, then retire, and the interval does not exist: a
    /// reader that sees no pending seek has necessarily seen the store that
    /// came before the retire.
    #[inline]
    pub fn peek_seek(&self) -> Option<u64> {
        let v = self.seek_to.load(Ordering::Acquire);
        if v < 0 {
            None
        } else {
            Some(v as u64)
        }
    }

    /// Retires the request `target`, which the caller has peeked and applied.
    ///
    /// **A newer request that arrived meanwhile is left pending rather than
    /// dropped.** It is then serviced on the next period — one period late,
    /// 2.9 ms at 44.1 kHz with 128-frame periods, against losing a cue jump
    /// outright, which is a button that did nothing.
    ///
    /// **The compare-exchange has an ABA and it is benign for one reason
    /// only.** Request X, peek X, apply, request X again, and this retires
    /// the *second* X believing it is the first — the request is swallowed.
    /// That is harmless because applying a seek is idempotent in its target:
    /// it sets `position = min(target, frames)` and does nothing else, so a
    /// second request for X wanted exactly where the deck already is. **It
    /// stops being harmless the moment servicing a seek acquires a side
    /// effect**, and there is a named candidate — `window::Command::Relocate`,
    /// which the app loop owes on any seek leaving the resident span. Send
    /// that per *request* rather than per *applied seek*, or make this a
    /// counter rather than a value.
    #[inline]
    pub fn consumed_seek(&self, target: u64) {
        let _ = self.seek_to.compare_exchange(
            target as i64,
            -1,
            Ordering::Release,
            Ordering::Relaxed,
        );
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
    /// **The control thread calls this, and only the control thread.**
    ///
    /// It is not a suggestion and it is not about tidiness. Every other
    /// method here that changes control state does a read-modify-write across
    /// more than one atomic — `cue_down` reads the rate and the position and
    /// then decides which of three things to do — and those are safe against
    /// each other only because one thread performs them. Calling this from
    /// the audio thread makes a second writer, and then `reached_end` can
    /// land inside `cue_down`: the preview latches while the pause it was
    /// deciding against has already happened.
    ///
    /// **Stage 1 of the app loop called it from the audio thread**, and the
    /// visible cost was one lost PLAY press after a Back Cue at the end of a
    /// track, found by a test that timed out. The invisible cost was the
    /// paragraph above. The end of the track is now *derived* by the control
    /// thread from the position the callback publishes — `app::track::
    /// Playing::service` — so the callback reports and never decides.
    ///
    /// **Whoever drives the playback loop must call this when the position
    /// reaches the end**, or a deck that has played out reads `Playing` at
    /// rate 1.0 for ever: a display saying "playing" over silence, and a PLAY
    /// press that pauses.
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
    /// careful about, introduced to save the caller a line. That was written
    /// before the app loop existed and then contradicted by the first thing
    /// that called it. Read it as a rule with a scar.
    pub fn reached_end(&self) {
        // **A queued seek makes the report stale, and acting on it loses the
        // operator's PLAY.** Back Cue at the end of a track followed by PLAY
        // queues a seek and sets the rate; a report computed before those and
        // acted on after them concludes, from the *old* position, that the
        // deck should be paused — and pauses one the operator has just
        // started. It sits at the cue point having been told to play, and in
        // a venue that reads as a dead PLAY button after every Back Cue.
        //
        // **Exact about the stores, and about nothing else.** Because this
        // runs on the control thread — the only writer of what it checks and
        // then changes — nothing can queue a seek between this guard and the
        // three stores below; on the audio thread that gap was merely narrow,
        // which is where this started. What the guard cannot vouch for is the
        // *premise*: the caller decided the track had ended by reading a
        // position, and `seek_to` is written by both threads, so a guard
        // placed after that read cannot make it fresh. Freshness is
        // [`settled_position`](Self::settled_position)'s job, at the read.
        // Reading this as closing the whole question is what let the caller
        // take the pair in the wrong order.
        //
        // It belongs here rather than in the caller for the reason `end_seek`
        // guards itself: a pending seek means the position this conclusion
        // rests on is about to be replaced, and the next caller cannot be
        // expected to know that.
        if self.peek_seek().is_some() {
            return;
        }
        self.publish_motion(RATE_PAUSED, false);
        self.previewing.store(false, Ordering::Relaxed);
        self.state.store(State::Paused as u8, Ordering::Relaxed);
    }

    /// A track has been loaded: **paused at frame zero, with `cue_point`
    /// restored.**
    ///
    /// `decisions.md` settles the position — "Loading a track waits at frame
    /// zero, not at its stored cue" — and names the cost, which is that the
    /// first CUE press then takes the paused-and-not-at-cue branch and
    /// overwrites the point the operator did not choose. That is
    /// `cue_down` behaving as specified and is deliberately not special-cased
    /// here: a mode that exists only just after a load is invisible in the
    /// code and unlearnable at the panel.
    ///
    /// **Every field is reset, not only the two a load obviously touches.**
    /// This type outlives the track — one `Transport` per deck, so the
    /// display and the input dispatch hold one thing rather than re-reading a
    /// pointer that changes under them — which means a field left alone is a
    /// field carrying the *previous* track's value. A latched `seek_to` would
    /// fire into the new track on its first period, and a latched
    /// `previewing` would make the first CUE release stop a deck that was
    /// never previewing.
    ///
    /// **The cue point is not clamped to the track here**, deliberately: the
    /// engine clamps a seek request to `track_frames` when it consumes one,
    /// so a stored cue past the end of a file that has since been replaced
    /// lands on the last frame rather than off the end. Clamping in two
    /// places would mean two answers to keep agreeing.
    ///
    /// Called with **no audio thread running** — `app::track::load` spawns it
    /// after this returns, and `Playing::unload` joins it before
    /// [`track_unloaded`](Self::track_unloaded) — which is why these are
    /// plain stores with no ordering discipline beyond the motion pair's own.
    pub fn track_loaded(&self, cue_point: u64) {
        self.seek_to.store(-1, Ordering::Relaxed);
        self.position.store(0.0f64.to_bits(), Ordering::Relaxed);
        self.cue.store(cue_point, Ordering::Relaxed);
        self.previewing.store(false, Ordering::Relaxed);
        self.publish_motion(RATE_PAUSED, false);
        self.state.store(State::Paused as u8, Ordering::Relaxed);
    }

    /// Nothing is loaded any more.
    ///
    /// **This is the transition into [`State::Stopped`], and until the app
    /// loop existed there was none** — the state was constructed and never
    /// stored, which `implementation.md` catalogues as a one-way door and
    /// says is found by asking which transitions lead *into* each state
    /// rather than by grepping for construction sites.
    ///
    /// The cue point goes with the track. It belongs to the file, not to the
    /// deck, and `CueStore` is where it persists — leaving it here would mean
    /// the next track loaded without a stored cue inherits this one's.
    pub fn track_unloaded(&self) {
        self.seek_to.store(-1, Ordering::Relaxed);
        self.position.store(0.0f64.to_bits(), Ordering::Relaxed);
        self.cue.store(0, Ordering::Relaxed);
        self.previewing.store(false, Ordering::Relaxed);
        self.publish_motion(RATE_PAUSED, false);
        self.state.store(State::Stopped as u8, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the callback does with a queued seek, minus applying it: peek,
    /// then retire. The two-call protocol exists so the position can be
    /// published in between — `peek_seek` says why — and these tests are
    /// about cue and seek *semantics*, so they use this and the ordering has
    /// its own test.
    fn take(t: &Transport) -> Option<u64> {
        let seek = t.peek_seek();
        if let Some(frame) = seek {
            t.consumed_seek(frame);
        }
        seek
    }

    #[test]
    fn an_empty_deck_refuses_every_control_rather_than_pretending() {
        // **This became reachable in the change that made `Stopped` a state
        // the deck is actually in.** While it was constructed and never
        // stored, no deck was ever empty after the first press and nothing
        // that reads the state could be wrong; `Playing::unload` stores it
        // now. PLAY on an empty deck would set rate 1.0 and `Playing`, and
        // the display would read "playing" over a deck holding nothing.
        let t = Transport::new();
        assert_eq!(t.state(), State::Stopped);

        t.play();
        assert_eq!(t.rate(), RATE_PAUSED, "PLAY must not start an empty deck");
        assert_eq!(t.state(), State::Stopped);

        t.begin_seek(true);
        assert_eq!(t.state(), State::Stopped, "and there is nothing to seek");
        assert_eq!(t.cue_down(), Cued::Nothing, "nor anything to cue");
        assert_eq!(take(&t), None, "no seek may be queued");

        // Loading is the only way out, which is the state machine's shape:
        // `Stopped` is left by loading a track and entered by unloading one.
        t.track_loaded(0);
        t.play();
        assert_eq!(t.state(), State::Playing);
    }

    #[test]
    fn play_and_pause_are_the_rate_variable_and_nothing_else() {
        let t = Transport::new();
        t.track_loaded(0);
        assert_eq!(t.rate(), RATE_PAUSED);
        assert_eq!(t.state(), State::Paused, "loaded and waiting");

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
        t.track_loaded(0);
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
        t.track_loaded(0);
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
        t.track_loaded(0);
        assert_eq!(take(&t), None);
        t.request_seek(12_345);
        assert_eq!(take(&t), Some(12_345));
        assert_eq!(take(&t), None, "a request must not be serviced twice");
        // Frame 0 is a legitimate target and must not read as "no request".
        t.request_seek(0);
        assert_eq!(take(&t), Some(0));

        // And an absurd target must still arrive as *a* request rather than
        // colliding with the sentinel. `u64::MAX as i64` is -1, which is the
        // sentinel exactly; saturating instead keeps it visible so the engine
        // can clamp it to the track and report the end.
        t.request_seek(u64::MAX);
        assert_eq!(take(&t), Some(i64::MAX as u64));
    }

    #[test]
    fn the_rate_and_position_survive_the_trip_through_atomics_exactly() {
        // f64 travels as its bit pattern, so this is a reinterpretation and
        // not a conversion. If it were ever routed through f32, the position
        // invariant would be silently lost.
        let t = Transport::new();
        t.track_loaded(0);
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
        t.track_loaded(0);
        assert_eq!(t.cue_point(), 0);
    }

    #[test]
    fn cue_while_paused_sets_the_point_and_makes_no_sound() {
        let t = Transport::new();
        t.track_loaded(0);
        t.pause();
        t.publish_position(50_000.0);
        assert_eq!(t.cue_down(), Cued::Set(50_000), "Setting Cue");
        assert_eq!(t.cue_point(), 50_000);
        assert_eq!(t.rate(), RATE_PAUSED, "\"No sound is output at this time\"");
        assert_eq!(take(&t), None, "setting a point must not move the deck");
        assert_eq!(t.cue_up(), Cued::Nothing, "a release that was not a preview");
        assert_eq!(t.rate(), RATE_PAUSED, "release after setting must do nothing");

        // "When a new cue point is set, the previously set cue point is
        // canceled." One point, not a set of hot cues.
        t.publish_position(120_000.0);
        let _ = t.cue_down();
        assert_eq!(t.cue_point(), 120_000);
    }

    #[test]
    fn cue_while_playing_returns_to_the_point_and_pauses_without_resuming() {
        let t = Transport::new();
        t.track_loaded(0);
        t.set_cue_point(1_000);
        t.play();
        t.publish_position(80_000.0);

        assert_eq!(t.cue_down(), Cued::Returned(1_000), "Back Cue");
        assert_eq!(t.rate(), RATE_PAUSED, "Back Cue pauses; it does not resume");
        assert_eq!(t.state(), State::Paused);
        assert_eq!(take(&t), Some(1_000), "returns to the cue point");
        assert_eq!(t.cue_point(), 1_000, "Back Cue must not move the point");

        // And PLAY restarts from the point, not from where it was.
        t.play();
        assert_eq!(t.rate(), RATE_UNITY);
        assert_eq!(take(&t), None, "PLAY queues no seek of its own");
    }

    #[test]
    fn cue_held_at_the_point_previews_and_release_returns() {
        // Cue Point Sampler. "Playback continues while the button is held in"
        // — so release means stop and return, with no latching.
        let t = Transport::new();
        t.track_loaded(0);
        t.set_cue_point(4_410);
        t.pause();
        t.publish_position(4_410.0);

        assert_eq!(t.cue_down(), Cued::Previewing, "Cue Point Sampler");
        assert_eq!(t.rate(), RATE_UNITY, "plays while held");
        assert_eq!(t.state(), State::Playing);
        assert_eq!(take(&t), None, "already at the point; nothing to seek");
        assert_eq!(t.cue_point(), 4_410, "previewing must not re-set the point");

        // Pretend the callback advanced during the preview.
        t.publish_position(9_000.0);
        assert_eq!(t.cue_up(), Cued::Returned(4_410), "release returns");
        assert_eq!(t.rate(), RATE_PAUSED, "release stops");
        assert_eq!(take(&t), Some(4_410), "and returns to the point");
    }

    #[test]
    fn a_second_release_after_a_preview_does_nothing() {
        // The latch is consumed, so a stray release cannot silently re-cue a
        // deck the user has since started playing.
        let t = Transport::new();
        t.track_loaded(0);
        t.pause();
        t.publish_position(0.0);
        let _ = t.cue_down();
        let _ = t.cue_up();
        assert_eq!(take(&t), Some(0));

        t.play();
        assert_eq!(t.cue_up(), Cued::Nothing, "the second release does nothing");
        assert_eq!(t.rate(), RATE_UNITY, "a spurious release must not stop playback");
        assert_eq!(take(&t), None);
    }

    #[test]
    fn cue_during_a_held_seek_is_treated_as_back_cue() {
        // The manual does not cover this combination; this is the reading
        // recorded in the module docs, asserted so it cannot drift silently.
        let t = Transport::new();
        t.track_loaded(0);
        t.set_cue_point(2_000);
        t.play();
        t.begin_seek(true);
        t.publish_position(60_000.0);
        assert_eq!(t.cue_down(), Cued::Returned(2_000), "a held seek is not paused");
        assert_eq!(t.rate(), RATE_PAUSED);
        assert!(!t.is_silent(), "back cue leaves the deck ready to play, not muted");
        assert_eq!(take(&t), Some(2_000));
    }

    #[test]
    fn reaching_the_end_clears_a_latched_preview() {
        // A preview that ran into the end of the track must not leave the
        // latch set, or the next release would cue a deck nobody previewed.
        let t = Transport::new();
        t.track_loaded(0);
        t.pause();
        t.publish_position(0.0);
        let _ = t.cue_down();
        t.reached_end();
        let _ = t.cue_up();
        assert_eq!(take(&t), None, "the latch must have been cleared");
    }

    #[test]
    fn reaching_the_end_stops_the_rate_and_advances_nothing() {
        let t = Transport::new();
        t.track_loaded(0);
        t.play();
        t.reached_end();
        assert_eq!(t.rate(), RATE_PAUSED);
        assert_eq!(t.state(), State::Paused);
        // Auto-advance is an open question; nothing here may decide it.
        assert_eq!(take(&t), None, "reaching the end must not queue a seek");
    }

    #[test]
    fn a_queued_seek_makes_reaching_the_end_stale_and_it_is_ignored() {
        // Back Cue at the end of a track, then PLAY: two control-thread
        // writes, with a fill in flight that decided against the old
        // position. Without the guard the deck ends up paused at the cue
        // point having just been told to play — a PLAY button that does
        // nothing after every Back Cue.
        let t = Transport::new();
        t.track_loaded(0);
        t.track_loaded(0);
        let _ = t.back_cue();
        t.play();

        t.reached_end();
        assert_eq!(t.rate(), RATE_UNITY, "the operator's PLAY must survive");
        assert_eq!(t.state(), State::Playing);

        // Once the seek has been consumed the report is current again.
        assert_eq!(take(&t), Some(0));
        t.reached_end();
        assert_eq!(t.rate(), RATE_PAUSED);
        assert_eq!(t.state(), State::Paused);
    }

    #[test]
    fn a_seek_queued_while_one_is_being_serviced_is_kept_rather_than_dropped() {
        // `consumed_seek` retires *the request it was given*, so a press that
        // lands between the peek and the retire survives to the next period.
        // A plain swap would have discarded it: a cue jump that did nothing.
        let t = Transport::new();
        t.track_loaded(0);
        t.request_seek(100);
        let target = t.peek_seek().expect("queued");

        t.request_seek(200); // the operator again, mid-service
        t.consumed_seek(target);

        assert_eq!(t.peek_seek(), Some(200), "the newer request must survive");
    }

    #[test]
    fn a_position_read_while_a_seek_is_in_flight_has_no_settled_answer() {
        // The reader's half of the protocol. A caller about to *act* on the
        // position must observe the retire before the position, and the only
        // way to offer that is to answer "not yet" while a seek is pending —
        // the alternative is a pair of loads a caller can take in either
        // order, and the first caller took them in the wrong one.
        let t = Transport::new();
        t.track_loaded(0);
        t.publish_position(2_000.0);
        assert_eq!(t.settled_position(), Some(2_000.0));

        t.request_seek(0);
        assert_eq!(
            t.settled_position(),
            None,
            "2,000 is the pre-seek answer and acting on it is the defect"
        );

        // The callback's order: apply, publish, retire.
        let target = t.peek_seek().expect("queued");
        t.publish_position(target as f64);
        t.consumed_seek(target);
        assert_eq!(t.settled_position(), Some(0.0));
    }

    #[test]
    fn no_pending_seek_means_the_published_position_is_the_new_one() {
        // The pair a control thread reads to decide whether the deck has run
        // out. The callback's protocol is peek, apply, publish, retire — so
        // the state "nothing pending, position still the old one" does not
        // occur, and a reader that treats the first as implying the second is
        // right. With the retire first it is wrong for a few instructions,
        // which is what pauses a deck the operator has just started.
        let t = Transport::new();
        t.track_loaded(0);
        t.publish_position(2_000.0);
        t.request_seek(50);

        let target = t.peek_seek().expect("queued");
        t.publish_position(target as f64);
        assert!(
            t.peek_seek().is_some(),
            "still pending, so a reader must conclude nothing yet"
        );
        t.consumed_seek(target);

        assert_eq!(t.peek_seek(), None);
        assert_eq!(t.position(), 50.0);
    }

    #[test]
    fn a_load_restores_the_cue_point_and_still_waits_at_frame_zero() {
        let t = Transport::new();
        t.track_loaded(0);
        t.track_loaded(120_000);
        assert_eq!(t.cue_point(), 120_000, "the stored point must be restored");
        assert_eq!(t.position(), 0.0, "decisions.md: waits at zero, not at the cue");
        assert_eq!(t.state(), State::Paused);
        assert_eq!(t.rate(), RATE_PAUSED);
    }

    #[test]
    fn a_load_clears_what_the_previous_track_latched() {
        // One `Transport` per deck, so every field not reset here is the
        // previous track's. A latched seek would fire into the new track on
        // its first period; a latched preview would make the first CUE
        // release stop a deck nobody previewed.
        let t = Transport::new();
        t.track_loaded(0);
        t.track_loaded(0);
        t.play();
        t.request_seek(900);
        t.pause();
        let _ = t.cue_down();

        t.track_loaded(50);
        assert_eq!(take(&t), None, "a seek must not survive a load");
        let _ = t.cue_up();
        assert_eq!(
            take(&t),
            None,
            "the preview latch must not survive a load"
        );
    }

    #[test]
    fn an_unload_is_the_transition_into_stopped_that_this_type_lacked() {
        let t = Transport::new();
        t.track_loaded(0);
        t.track_loaded(4_410);
        t.play();
        assert_eq!(t.state(), State::Playing);

        t.track_unloaded();
        assert_eq!(t.state(), State::Stopped, "nothing loaded is Stopped");
        assert_eq!(t.rate(), RATE_PAUSED);
        assert_eq!(t.position(), 0.0);
        assert_eq!(
            t.cue_point(),
            0,
            "the cue belongs to the file; leaving it would give the next \
             track this one's point"
        );
    }
}
