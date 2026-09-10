//! The control loop: `/dev/input` in, [`Deck::apply`] out.
//!
//! Stage 3 gave a press a destination. This is what produces the press. It is
//! the last piece of the chain `architecture.md` describes — kernel event,
//! [`Decoder`], [`Action`], deck — and it is the piece that has never existed,
//! which is why `Devices::read_pending` has spent three stages stating an
//! obligation to nobody.
//!
//! # The obligation this stage owes
//!
//! `Devices::read_pending` returns how many device nodes went away and says
//! **"a caller that gets a non-zero answer must reset its decoder"**, because
//! the presses in flight on a vanished node can never be released: a
//! `HoldStart(Ff)` with nothing to end it is a transport stuck seeking, and
//! nothing downstream can recover because nothing downstream knows the press
//! is gone. [`Controls::turn`] is that caller.
//!
//! **There is no type-level fence on it, unlike `Cued`.** That was the repair
//! for an obligation no test could observe; this one a test can, so this one
//! has a test — `a_vanished_device_ends_the_gesture_it_was_holding`, and the
//! reset deleted turns it red. Reaching for the same fence twice without
//! asking whether the cheaper check applies is how a fix becomes a habit.
//!
//! # Why the source is a trait
//!
//! `Devices` is `#[cfg(target_os = "linux")]`, so a loop written directly
//! against it is a loop that only exists on the deck — and the whole argument
//! of `src/app/mod.rs` is that the app loop is where the defects are and must
//! therefore be the part that is *most* testable, not the part that is
//! untestable. [`EventSource`] is the same move `AudioSink` makes for the
//! output: the deck gets `Devices`, a test gets a script, and neither knows.
//!
//! It also puts the source *inside* the loop. The reset above is then
//! discharged where the events are read rather than by whoever assembles the
//! program, which is the placement lesson from stage 2 stated once more: a
//! decision is safest in the one place that cannot forget to make it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::app::deck::{Deck, DeckError};
use crate::app::track::Ended;
use crate::input::{Action, Decoder, RawEvent, HOLD_AFTER};
use crate::sink::AudioSink;

/// Where raw events come from.
///
/// Mirrors the three methods of `Devices` exactly rather than improving on
/// them, so that the Linux implementation is a forwarding block with nothing
/// in it to be wrong.
pub trait EventSource {
    /// Waits up to `timeout` for anything to be readable. `Ok(false)` on a
    /// timeout, which is ordinary and is what paces the loop.
    ///
    /// **One wait over all the nodes, never one per node.** `config.txt`
    /// creates seven, and waiting on each in turn spends the full timeout on
    /// each — the round trip becomes seven poll intervals and
    /// [`Decoder::tick`] runs that much later. `implementation.md` records
    /// this as shape 4's wrong repair that looks right.
    fn wait(&mut self, timeout: Duration) -> std::io::Result<bool>;

    /// Appends everything pending. Returns how many devices went away.
    ///
    /// The count is the whole reason this is not `-> io::Result<()>`. See the
    /// module doc.
    fn read_pending(&mut self, out: &mut Vec<RawEvent>) -> std::io::Result<usize>;

    /// The absolute axis range, when a device reported one.
    ///
    /// [`Controls::new`] is what reads it, and reads it there so that a
    /// `Controls` cannot exist without having asked. Skipping it does not
    /// fail loudly: `Decoder::fold_abs` needs the span to tell a wrap from a
    /// jump, and without it one wrap of the browse encoder scrolls the length
    /// of the axis.
    fn abs_range(&self) -> Option<(i32, i32)> {
        None
    }

    /// False once every node has gone. The deck has no controls at all.
    fn is_empty(&self) -> bool {
        false
    }
}

#[cfg(target_os = "linux")]
impl EventSource for crate::input::Devices {
    fn wait(&mut self, timeout: Duration) -> std::io::Result<bool> {
        crate::input::Devices::wait(self, timeout)
    }
    fn read_pending(&mut self, out: &mut Vec<RawEvent>) -> std::io::Result<usize> {
        crate::input::Devices::read_pending(self, out)
    }
    fn abs_range(&self) -> Option<(i32, i32)> {
        crate::input::Devices::abs_range(self)
    }
    fn is_empty(&self) -> bool {
        crate::input::Devices::is_empty(self)
    }
}

/// How long [`Controls::turn`] waits for an event before giving up and
/// ticking anyway.
///
/// **A bound on two latencies, and neither is the button press itself** — a
/// press wakes the `poll` immediately. What this bounds is how late a
/// *non-event* can be noticed:
///
/// - `HoldStart`, which is defined by the button *not* coming back up. 400 ms
///   is the threshold, so 10 ms is 2.5% of it and inaudible on a seek.
/// - The end of the track, which [`Deck::service`] derives from the position
///   the callback publishes. A deck that has played out reads `Playing` for
///   this long before it settles.
///
/// The same 10 ms `window.rs` uses, for the same kind of reason. The cost is
/// 100 wake-ups a second on a core that is otherwise idle; the control thread
/// is not the one with the deadline.
pub const POLL: Duration = Duration::from_millis(10);

/// What one turn produced. For the display, and for bring-up reporting.
#[derive(Debug, Default)]
pub struct Turn {
    /// Actions handed to the deck.
    pub actions: usize,
    /// Devices that went away. Non-zero means the decoder was reset and
    /// whatever it was holding was closed out.
    pub lost: usize,
    /// The track ended, or its threads went. At most one per turn.
    pub ended: Option<Ended>,
    /// Presses that could not be carried out — an unreadable file, a cue
    /// store that would not write. **Not fatal, and not silent**: a refused
    /// press is a thing the display says, which is `decisions.md`'s "say
    /// *why*, not just *that*". Empty `Vec`s do not allocate.
    pub errors: Vec<DeckError>,
}

/// The decoder, its scratch buffers, and the reset obligation.
pub struct Controls {
    decoder: Decoder,
    raw: Vec<RawEvent>,
    actions: Vec<Action>,
    resets: u64,
}

impl Controls {
    /// Takes the source at construction **because that is when the axis range
    /// has to be read**. See [`EventSource::abs_range`].
    pub fn new<E: EventSource + ?Sized>(source: &E) -> Controls {
        Controls::with_hold(source, HOLD_AFTER)
    }

    /// For tests that would otherwise have to wait 400 ms to see a hold.
    pub fn with_hold<E: EventSource + ?Sized>(source: &E, hold_after: Duration) -> Controls {
        let mut decoder = Decoder::new(hold_after);
        if let Some((min, max)) = source.abs_range() {
            decoder.set_abs_range(min, max);
        }
        Controls {
            decoder,
            // Sized once. evdev hands over at most its 64-event buffer before
            // it drops the queue and says `SYN_DROPPED`, so this is the most
            // one read can produce and the loop then allocates nothing.
            raw: Vec::with_capacity(64),
            actions: Vec::with_capacity(16),
            resets: 0,
        }
    }

    /// How many times a lost device forced the decoder to give up its state.
    /// Zero on a healthy deck; anything else is a wiring or power fault.
    pub fn resets(&self) -> u64 {
        self.resets
    }

    /// True when the browse encoder is sitting on a bound of a clamped axis,
    /// so one direction is currently silent. Advisory — see
    /// `Decoder::absolute_axis_is_clamped`, which cannot tell "clamped and
    /// stuck" from "rollover and merely at zero".
    pub fn axis_is_clamped(&self) -> bool {
        self.decoder.absolute_axis_is_clamped()
    }

    /// One turn: settle the deck, read what arrived, act on it.
    ///
    /// `now` is monotonic and supplied by the caller — the decoder holds no
    /// clock, which is what makes tap-versus-hold testable without sleeping
    /// (`src/input.rs`, *The clock is read here, not taken from the event*).
    ///
    /// # The order is the load-bearing part
    ///
    /// **[`Deck::service`] runs before the presses, not after.** Both the end
    /// of the track and the press that arrives with it are discovered in the
    /// same turn, and applying the press first is the stage-2 defect one
    /// level up: the transport still reads `Playing` at rate 1.0, so
    /// PLAY/PAUSE takes the pause branch, and then `service` pauses it again
    /// for the end of the track. The operator's press vanishes into a state
    /// that was about to change anyway. Servicing first settles the deck, and
    /// the press is then interpreted against what the deck actually is.
    pub fn turn<E, S>(&mut self, source: &mut E, now: Duration, deck: &mut Deck<S>) -> std::io::Result<Turn>
    where
        E: EventSource + ?Sized,
        S: AudioSink + Send + 'static,
    {
        let mut turn = Turn {
            ended: deck.service(),
            ..Turn::default()
        };

        self.raw.clear();
        self.actions.clear();

        if source.wait(POLL)? {
            // **The obligation.** `Devices::read_pending`: a caller that gets
            // a non-zero answer must reset its decoder, because the presses
            // in flight on a node that has gone can never be released by
            // anything. `Decoder::reset` closes them on its way out, so the
            // actions it emits go through the deck like any others — a stuck
            // `SeekingForward` ends because a `HoldEnd` really is delivered,
            // not because the state was quietly dropped.
            turn.lost = source.read_pending(&mut self.raw)?;
            if turn.lost > 0 {
                self.resets += 1;
                self.decoder.reset(&mut self.actions);
            }
            for i in 0..self.raw.len() {
                self.decoder.feed(now, self.raw[i], &mut self.actions);
            }
        }

        // **Unconditional, and outside the `wait` branch.** A hold is defined
        // by an event not arriving, so the turn that has nothing to read is
        // exactly the turn in which a hold fires.
        self.decoder.tick(now, &mut self.actions);

        turn.actions = self.actions.len();
        for i in 0..self.actions.len() {
            if let Err(e) = deck.apply(self.actions[i]) {
                turn.errors.push(e);
            }
        }
        Ok(turn)
    }
}

/// What a whole run of the loop did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub turns: u64,
    pub actions: u64,
    pub lost: u64,
    pub errors: u64,
    pub ends: u64,
}

/// Runs the control loop until `stop` is set.
///
/// **Not a realtime thread, and it asserts so.** This blocks in `poll`,
/// allocates on a load, and reaches libsndfile through `Deck::load`. glibc's
/// `pthread_create` defaults to `PTHREAD_INHERIT_SCHED`, so a program that
/// called `rt::apply` before spawning this would put all of that at
/// `SCHED_FIFO` 75 — `rt::is_realtime` exists for exactly this check and
/// `Window::run` makes the same one.
///
/// **A deck whose every input node has gone keeps playing.** `wait` on an
/// empty set sleeps rather than spinning, so the loop turns over at [`POLL`]
/// doing nothing but servicing the deck. Stopping instead would take the
/// audio down over a fault in the buttons; `EventSource::is_empty` is how a
/// caller that wants to say so on the display can see it.
pub fn run<E, S>(
    controls: &mut Controls,
    source: &mut E,
    deck: &mut Deck<S>,
    stop: &AtomicBool,
) -> std::io::Result<Report>
where
    E: EventSource + ?Sized,
    S: AudioSink + Send + 'static,
{
    debug_assert!(
        !crate::rt::is_realtime(),
        "the control thread inherited a realtime policy — call rt::apply on \
         the audio thread, after this one is running (see rt::is_realtime)"
    );
    let started = Instant::now();
    let mut report = Report::default();
    while !stop.load(Ordering::Relaxed) {
        let turn = controls.turn(source, started.elapsed(), deck)?;
        report.turns += 1;
        report.actions += turn.actions as u64;
        report.lost += turn.lost as u64;
        report.errors += turn.errors.len() as u64;
        report.ends += u64::from(turn.ended.is_some());
    }
    Ok(report)
}
