//! The control loop: a kernel event in, the deck moved.
//!
//! `tests/input_test.rs` drives the real dispatch from the real decoder, and
//! stops there — it builds `RawEvent`s and hands them to `Decoder::feed` by
//! hand. What is new here is the **loop**: who calls `feed`, who calls `tick`
//! when nothing arrives, who notices a device has gone, and in what order all
//! of that happens against `Deck::service`.
//!
//! Every one of those is a placement question, and placement is what this
//! project keeps getting wrong. The two tests that matter most below are not
//! about decoding at all: one asserts that a lost device closes the gestures
//! it was holding, and one asserts that the deck is settled *before* a press
//! is interpreted against it.

mod fixtures;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use deck_pi::app::controls::{Controls, EventSource};
use deck_pi::app::deck::Deck;
use deck_pi::app::track::Config;
use deck_pi::browser::Browser;
use deck_pi::input::{
    Button, RawEvent, ABS_X, EV_ABS, EV_KEY, EV_SYN, SYN_DROPPED,
};
use deck_pi::sink::CaptureSink;
use deck_pi::transport::{RATE_PAUSED, RATE_SEEK, RATE_UNITY};
use fixtures::{Bits, Kind, Scratch};

const WINDOW_BYTES: usize = 8_192 * deck_pi::ring::RING_FRAME_BYTES;
const PERIOD: usize = 128;
const RATE: u32 = 44_100;

/// A source with a script, so the loop can be driven with no `/dev/input`
/// and no sleeping.
///
/// One entry is one turn's worth. An entry with nothing in it is a turn on
/// which `wait` times out, which is the ordinary case and the one where
/// `tick` has to run anyway.
#[derive(Default)]
struct Script {
    turns: VecDeque<(Vec<RawEvent>, usize)>,
    range: Option<(i32, i32)>,
}

impl Script {
    fn new() -> Script {
        Script::default()
    }
    /// What `Devices::abs_range` would report, and the whole reason
    /// `Controls::new` takes the source.
    fn with_range(mut self, min: i32, max: i32) -> Script {
        self.range = Some((min, max));
        self
    }
    fn idle(mut self, turns: usize) -> Script {
        for _ in 0..turns {
            self.turns.push_back((Vec::new(), 0));
        }
        self
    }
    fn events(mut self, evs: &[RawEvent]) -> Script {
        self.turns.push_back((evs.to_vec(), 0));
        self
    }
    /// A turn on which `n` device nodes went away.
    fn lost(mut self, n: usize) -> Script {
        self.turns.push_back((Vec::new(), n));
        self
    }
}

impl EventSource for Script {
    fn wait(&mut self, _timeout: Duration) -> std::io::Result<bool> {
        match self.turns.front() {
            // An idle turn consumes itself here: `read_pending` is not
            // reached when `wait` says nothing is ready, which is exactly the
            // path the real `poll` takes on a timeout.
            Some((evs, lost)) if evs.is_empty() && *lost == 0 => {
                self.turns.pop_front();
                Ok(false)
            }
            Some(_) => Ok(true),
            None => Ok(false),
        }
    }
    fn read_pending(&mut self, out: &mut Vec<RawEvent>) -> std::io::Result<usize> {
        match self.turns.pop_front() {
            Some((evs, lost)) => {
                out.extend(evs);
                Ok(lost)
            }
            None => Ok(0),
        }
    }
    fn abs_range(&self) -> Option<(i32, i32)> {
        self.range
    }
    fn is_empty(&self) -> bool {
        false
    }
}

fn key(button: Button, value: i32) -> RawEvent {
    RawEvent {
        kind: EV_KEY,
        code: button.keycode(),
        value,
    }
}

fn abs(value: i32) -> RawEvent {
    RawEvent {
        kind: EV_ABS,
        code: ABS_X,
        value,
    }
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// A deck with nothing to play, but a transport that will accept control.
///
/// `track_loaded(0)` is not decoration: `Transport` refuses every control
/// while `State::Stopped`, so without it `begin_seek` is a silent no-op and
/// the seeking assertions below would pass against a deck that did nothing.
fn bare_deck() -> Deck<CaptureSink> {
    let d = Deck::new(
        Box::new(|_| unreachable!("these tests load no track")),
        None,
        None,
        Config::default(),
    );
    d.transport().track_loaded(0);
    d
}

// ---------------------------------------------------------------------------

#[test]
fn a_key_down_reaches_the_transport() {
    // The chain end to end for the first time: a kernel event, the decoder,
    // the dispatch, the transport. Each link had tests; the chain had none.
    let mut source = Script::new().events(&[key(Button::PlayPause, 1)]);
    let mut controls = Controls::new(&source);
    let mut deck = bare_deck();

    assert_eq!(deck.transport().rate(), RATE_PAUSED);
    let turn = controls.turn(&mut source, ms(0), &mut deck).expect("turn");

    assert_eq!(turn.actions, 1, "one press should have come out");
    assert_eq!(deck.transport().rate(), RATE_UNITY, "PLAY did not reach the transport");
}

#[test]
fn a_hold_fires_on_a_turn_with_nothing_to_read() {
    // `Decoder::tick` is called **outside** the `wait` branch, and this is
    // why. A hold is defined by the button *not* coming back up, so the turn
    // that has nothing to read is precisely the turn on which FF starts
    // seeking. Ticking only when an event arrived would mean FF never seeks
    // unless some other button happens to be pressed.
    let mut source = Script::new().events(&[key(Button::Ff, 1)]).idle(1);
    let mut controls = Controls::with_hold(&source, ms(100));
    let mut deck = bare_deck();

    controls.turn(&mut source, ms(0), &mut deck).expect("down");
    assert_eq!(deck.transport().rate(), RATE_PAUSED, "not held long enough yet");

    // Nothing to read on this turn — the source times out.
    let turn = controls.turn(&mut source, ms(150), &mut deck).expect("tick");
    assert_eq!(turn.actions, 1, "the hold should have fired with no event");
    assert_eq!(deck.transport().rate(), RATE_SEEK, "FF did not start seeking");
}

#[test]
fn a_vanished_device_ends_the_gesture_it_was_holding() {
    // **The obligation this stage exists to meet.** `Devices::read_pending`
    // says a caller that gets a non-zero answer must reset its decoder,
    // because the press in flight on a node that has gone can never be
    // released by anything — and a `HoldStart(Ff)` with no `HoldEnd` is a
    // transport stuck in `SeekingForward` until the deck is restarted.
    //
    // It had no caller for three stages. This is what says the caller does
    // its job rather than that the sentence is still there.
    let mut source = Script::new()
        .events(&[key(Button::Ff, 1)])
        .idle(1)
        .lost(1);
    let mut controls = Controls::with_hold(&source, ms(100));
    let mut deck = bare_deck();

    controls.turn(&mut source, ms(0), &mut deck).expect("down");
    controls.turn(&mut source, ms(150), &mut deck).expect("hold");
    assert_eq!(deck.transport().rate(), RATE_SEEK, "the setup is wrong: FF is not seeking");
    assert_eq!(controls.resets(), 0);

    let turn = controls.turn(&mut source, ms(200), &mut deck).expect("loss");

    assert_eq!(turn.lost, 1);
    assert_eq!(controls.resets(), 1, "the decoder was not reset");
    assert_eq!(
        deck.transport().rate(),
        RATE_PAUSED,
        "the deck is still seeking on a button that no longer exists"
    );
}

#[test]
fn a_dropped_queue_closes_the_gestures_it_lost() {
    // The same hazard from the other direction, and the one that is far more
    // likely: evdev buffers 64 events and discards the whole queue when a
    // reader falls behind. `Decoder::feed` handles `SYN_DROPPED` itself, so
    // the loop owes nothing here — this asserts that what the decoder does
    // internally still reaches the transport through the loop, which is the
    // part that was never wired up.
    let mut source = Script::new()
        .events(&[key(Button::Ff, 1)])
        .idle(1)
        .events(&[RawEvent { kind: EV_SYN, code: SYN_DROPPED, value: 0 }]);
    let mut controls = Controls::with_hold(&source, ms(100));
    let mut deck = bare_deck();

    controls.turn(&mut source, ms(0), &mut deck).expect("down");
    controls.turn(&mut source, ms(150), &mut deck).expect("hold");
    assert_eq!(deck.transport().rate(), RATE_SEEK);

    controls.turn(&mut source, ms(200), &mut deck).expect("dropped");

    assert_eq!(
        deck.transport().rate(),
        RATE_PAUSED,
        "a dropped queue left the transport seeking"
    );
    // A drop is not a lost device: nothing was reset by the loop.
    assert_eq!(controls.resets(), 0);
}

// ---------------------------------------------------------------------------

struct Rig {
    _scratch: Scratch,
    medium: PathBuf,
}

fn rig(tag: &str) -> Rig {
    let scratch = Scratch::new(tag);
    let medium = scratch.dir.join("medium");
    std::fs::create_dir_all(&medium).expect("medium");
    Rig {
        _scratch: scratch,
        medium,
    }
}

impl Rig {
    fn track(&self, name: &str, frames: usize) -> PathBuf {
        let samples = fixtures::signal(Bits::S24, 2, frames);
        let bytes = fixtures::build(Kind::Wav, &samples, Bits::S24, RATE, 2);
        fixtures::write(&self.medium, name, Kind::Wav, &bytes)
    }

    fn deck(&self) -> Deck<CaptureSink> {
        let browser = Browser::open(&self.medium).expect("browser");
        Deck::new(
            Box::new(|info| Ok(CaptureSink::new(info.rate, PERIOD, 200_000))),
            Some(browser),
            None,
            Config {
                window_bytes: WINDOW_BYTES,
                rt: None,
            },
        )
    }
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn the_end_of_the_track_is_settled_before_the_press_that_arrives_with_it() {
    // **The ordering, and it is the stage-2 defect one level up.**
    //
    // The end of a track and the press that follows it are discovered in the
    // same turn: the callback publishes the final position, and the operator
    // hits PLAY. `Deck::service` is what derives the end and pauses — nothing
    // else may, because `Transport::reached_end` is the control thread's
    // alone.
    //
    // Apply the press first and the transport still reads rate 1.0, so
    // PLAY/PAUSE takes the *pause* branch; `service` then pauses again for
    // the end of the track and the press has vanished into a state that was
    // about to change anyway. That is exactly the lost PLAY after a Back Cue,
    // moved from the audio thread up into the loop.
    //
    // Servicing first settles the deck, and the press is interpreted against
    // what the deck actually is.
    let rig = rig("controls-end-then-press");
    let path = rig.track("short", 8_000);
    let mut deck = rig.deck();

    deck.load(&path).expect("loads");
    deck.transport().play();

    // Let it play out. Nothing has serviced the deck, so the transport still
    // reads rate 1.0 at the end — which is the state the press arrives in.
    wait_for("the track to play out", || {
        deck.transport().position() >= 8_000.0
    });
    assert_eq!(
        deck.transport().rate(),
        RATE_UNITY,
        "the setup is wrong: the deck should still claim to be playing"
    );

    let mut source = Script::new().events(&[key(Button::PlayPause, 1)]);
    let mut controls = Controls::new(&source);
    controls.turn(&mut source, ms(0), &mut deck).expect("turn");

    assert_eq!(
        deck.transport().rate(),
        RATE_UNITY,
        "the press was spent undoing a state the deck was about to leave"
    );

    deck.unload();
}

#[test]
fn the_axis_range_is_read_at_construction_so_a_wrap_is_one_detent() {
    // **Skipping this does not fail loudly, which is why it is not left to a
    // caller.** `Decoder::fold_abs` needs the span to tell a wrap from a
    // jump; without it, one wrap of the browse encoder scrolls the length of
    // the axis. `Controls::new` takes the source so that a `Controls` cannot
    // exist without having asked for the range.
    //
    // 24 detents, wrapping from 23 to 0: folded that is one step forward,
    // unfolded it is 23 steps back — and 23 steps back from row 0 is row 0,
    // so the two answers are distinguishable rather than merely different.
    let rig = rig("controls-encoder-wrap");
    for n in ["a", "b", "c", "d", "e"] {
        rig.track(n, 4_000);
    }
    let mut deck = rig.deck();
    assert_eq!(deck.browser().expect("browser").selected_index(), 0);

    let mut source = Script::new()
        .with_range(0, 23)
        // The first absolute reading is the baseline and emits nothing.
        .events(&[abs(23)])
        .events(&[abs(0)]);
    let mut controls = Controls::new(&source);

    controls.turn(&mut source, ms(0), &mut deck).expect("baseline");
    assert_eq!(
        deck.browser().expect("browser").selected_index(),
        0,
        "the first absolute reading must establish the baseline, not scroll"
    );
    assert!(
        controls.axis_is_clamped(),
        "the range was not read: `absolute_axis_is_clamped` needs it to answer at all"
    );

    controls.turn(&mut source, ms(10), &mut deck).expect("wrap");
    assert_eq!(
        deck.browser().expect("browser").selected_index(),
        1,
        "the wrap was taken as a 23-detent jump"
    );
}
