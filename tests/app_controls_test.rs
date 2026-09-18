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
    /// What each look finds, in order; an exhausted queue finds nothing.
    finds: VecDeque<usize>,
    /// How many times the deck has looked. The point of the backoff test.
    looks: usize,
    empty: bool,
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
    /// A surface with nothing open, which is what a pulled cable leaves.
    fn unplugged(mut self) -> Script {
        self.empty = true;
        self
    }
    /// What the next look finds.
    fn finds(mut self, n: usize) -> Script {
        self.finds.push_back(n);
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
        self.empty
    }
    fn rediscover(&mut self) -> std::io::Result<usize> {
        self.looks += 1;
        let found = self.finds.pop_front().unwrap_or(0);
        if found > 0 {
            self.empty = false;
        }
        Ok(found)
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
fn a_seek_starts_on_the_turn_that_reads_the_press() {
    // **This test asserted the opposite until SEARCH stopped being
    // tap-or-hold.** It existed because a hold is defined by the button *not*
    // coming back up, so the turn with nothing to read was the one on which
    // FF started seeking — which is why `Decoder::tick` was called outside
    // the `wait` branch at all. With TRACK SEARCH carrying the tap meaning,
    // SEARCH has nothing to disambiguate and seeks on the way down. No turn
    // without an event is needed, and none of the seek's start depends on the
    // clock any more.
    let mut source = Script::new().events(&[key(Button::Ff, 1)]).idle(1);
    let mut controls = Controls::new(&source);
    let mut deck = bare_deck();

    let turn = controls.turn(&mut source, ms(0), &mut deck).expect("down");
    assert_eq!(turn.actions, 1, "the press should have come out at once");
    assert_eq!(deck.transport().rate(), RATE_SEEK, "FF did not start seeking");

    // And the idle turn adds nothing, where it used to be the one that
    // mattered.
    let turn = controls.turn(&mut source, ms(150), &mut deck).expect("idle");
    assert_eq!(turn.actions, 0, "nothing is waiting on the clock now");
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
    let mut controls = Controls::new(&source);
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
    let mut controls = Controls::new(&source);
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

#[test]
fn a_lost_node_is_looked_for_again_on_the_same_turn() {
    // The hole this closes: `read_pending` drops a device that goes away and
    // nothing ever put one back. With the controls soldered to the header
    // that was sound, because an overlay's node exists from boot and a
    // vanishing one really is a fault. On USB a vanishing node is a replug,
    // and without this the deck plays on with every control dead.
    let mut source = Script::new().lost(2).finds(2);
    let mut controls = Controls::new(&source);
    let mut deck = bare_deck();

    let turn = controls.turn(&mut source, ms(0), &mut deck).expect("turn");

    assert_eq!(turn.lost, 2, "the script said two went away");
    assert_eq!(turn.reopened, 2, "both should have been found again");
    assert_eq!(source.looks, 1, "one loss, one look");
}

#[test]
fn an_unplugged_surface_is_looked_for_on_a_timer_rather_than_every_turn() {
    // **A retry with no floor is a busy loop wearing the costume of a retry.**
    // Looking means reading `/dev/input` and opening every node on it, and
    // `POLL` is 10 ms, so an unplugged surface would mean a hundred directory
    // scans a second for as long as the cable stayed out — on the machine
    // that is also meeting an audio deadline.
    let mut source = Script::new().unplugged();
    let mut controls = Controls::new(&source);
    let mut deck = bare_deck();

    // Three seconds of turns at the real poll interval.
    let turns = 300;
    for i in 0..turns {
        controls
            .turn(&mut source, ms(i * 10), &mut deck)
            .expect("turn");
    }

    assert!(
        source.looks <= 4,
        "looked {} times in 3 s; the floor is one a second",
        source.looks
    );
    assert!(
        source.looks >= 3,
        "looked only {} times in 3 s; it should keep trying",
        source.looks
    );
}

#[test]
fn a_surface_that_comes_back_stops_being_looked_for() {
    // The other half of the timer: once something is open again the deck must
    // stop scanning, or the floor above would become a permanent background
    // cost rather than a thing that happens while unplugged.
    let mut source = Script::new().unplugged().finds(2);
    let mut controls = Controls::new(&source);
    let mut deck = bare_deck();

    for i in 0..300 {
        controls
            .turn(&mut source, ms(i * 10), &mut deck)
            .expect("turn");
    }

    assert_eq!(source.looks, 1, "one look found it; there was no reason for a second");
    assert!(!source.is_empty(), "the script should have come back");
}

// ---------------------------------------------------------------------------

/// A medium whose states are a list. Same shape as the one in
/// `tests/app_medium_test.rs`, kept local because what is under test here is
/// the loop that drives it rather than the policy it drives.
struct MediumScript {
    mount_point: PathBuf,
    states: VecDeque<deck_pi::media::Medium>,
}

impl deck_pi::app::medium::MediumSource for MediumScript {
    fn poll(&mut self) -> Option<deck_pi::media::Medium> {
        self.states.pop_front()
    }
    fn mount_point(&self) -> &std::path::Path {
        &self.mount_point
    }
}

#[test]
fn the_whole_loop_runs_a_session_from_an_empty_deck_to_a_playing_one() {
    // **What `src/bin/deck.rs` does, with the two sources scripted.** The
    // binary is deliberately thin — it assembles these five pieces and prints
    // — so the thing worth testing is this: `controls::run` itself, driving a
    // deck that starts with no medium and no listing, through a stick
    // arriving and a press, to a track playing.
    //
    // Until this existed, `run` had no caller anywhere. It was the last piece
    // of `src/app/` in the state the media watch had been in for four stages.
    use deck_pi::app::medium::Mount;
    use deck_pi::app::panel::{Panel, Show};
    use deck_pi::display::{Geometry, Screen};
    use deck_pi::media::Medium;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A display that keeps what it was shown, so the loop's last half can be
    /// asserted on rather than assumed.
    #[derive(Default)]
    struct Capture {
        screens: Vec<Screen>,
        notes: Vec<String>,
    }
    impl Show for Capture {
        fn geometry(&self) -> Geometry {
            Geometry { cols: 40, rows: 8, colour: false }
        }
        fn show(&mut self, screen: &Screen) -> std::io::Result<()> {
            self.screens.push(screen.clone());
            Ok(())
        }
        fn note(&mut self, line: &str) -> std::io::Result<()> {
            self.notes.push(line.to_owned());
            Ok(())
        }
    }

    let r = rig("controls-session");
    r.track("opener", 40_000);

    // No browser and no cues: both arrive with the medium or not at all,
    // which is how the binary constructs it.
    let mut deck: Deck<CaptureSink> = Deck::new(
        Box::new(|info| Ok(CaptureSink::new(info.rate, PERIOD, 200_000))),
        None,
        None,
        Config { window_bytes: WINDOW_BYTES, rt: None },
    );
    let mut mount = Mount::new(
        MediumScript {
            mount_point: r.medium.clone(),
            states: VecDeque::from(vec![Medium::Browsable { uuid: None }]),
        },
        None,
    )
    .with_interval(Duration::ZERO);

    // Idle first, so the medium is mounted before the press — the ordering the
    // loop promises. Then ENTER on the only row, then PLAY.
    let mut source = Script::new()
        .idle(2)
        .events(&[key(Button::Enter, 1), key(Button::Enter, 0)])
        .idle(2)
        .events(&[key(Button::PlayPause, 1), key(Button::PlayPause, 0)])
        .idle(2);
    let mut controls = Controls::new(&source);

    let mut panel = Panel::new(Capture::default());
    let stop = AtomicBool::new(false);
    let mut seen_medium = 0;
    let mut turns = 0;
    let report = deck_pi::app::controls::run(
        &mut controls,
        &mut source,
        &mut mount,
        &mut panel,
        &mut deck,
        &stop,
        |_turn, change, show: &mut Capture| {
            if let Some(c) = change {
                seen_medium += 1;
                let _ = show.note(&format!("medium: {}", c.medium));
            }
            turns += 1;
            // The loop runs until something stops it, and in the binary that
            // is a signal. Here it is **the second full draw**, not a turn
            // count: these turns take microseconds, so ten of them fit inside
            // one coalescing window and `Cadence` — correctly — draws once.
            // Stopping there would assert against the screen as it was before
            // the press, which is the display lagging rather than the loop
            // being wrong.
            if turns >= 10 && show.screens.len() >= 2 {
                stop.store(true, Ordering::Relaxed);
            }
        },
    )
    .expect("the loop");

    assert_eq!(seen_medium, 1, "the callback never heard about the stick");
    assert_eq!(report.media, 1);
    assert!(report.turns >= 10, "{report:?}");
    assert_eq!(report.errors, 0, "{report:?}");

    assert!(deck.browser().is_some(), "the medium never reached the deck");
    assert!(
        deck.loaded().path().is_some(),
        "ENTER did not load: the press never reached the deck"
    );
    assert_eq!(
        deck.transport().rate(),
        RATE_UNITY,
        "PLAY did not start it"
    );

    // And it really is making samples, not just claiming a rate.
    wait_for("the deck to get somewhere", || {
        deck.transport().position() > 500.0
    });

    // **The display half ran too, and it showed the deck it ended up with.**
    // The last screen is the one after the press, not the one before it —
    // which is why the panel is turned over last.
    let shown = &panel.show().screens;
    assert!(!shown.is_empty(), "the display was never drawn");
    let last = shown.last().expect("a screen");
    assert_eq!(last.folder, "medium", "the listing is of the mounted folder");
    assert!(
        last.lines.iter().any(|l| l.selected && l.text.starts_with("opener")),
        "{last:?}"
    );
    assert!(last.status.starts_with("PLAY"), "{:?}", last.status);
    assert_eq!(panel.show().notes, vec!["medium: browsable, no volume UUID — cues cannot be saved"]);

    deck.detach();
}
