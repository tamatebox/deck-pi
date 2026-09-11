//! The dispatch: a press in, the modules moved.
//!
//! `tests/input_test.rs` covers gestures against the real dispatch with
//! nothing loaded. This file is the other half — the branches that need a
//! medium, a track and threads: loading from the browser, stepping folders,
//! and the one that is a correctness constraint rather than a preference,
//! **a cue landing on the playing track while the browser is somewhere
//! else.**
//!
//! **What is not covered, said here rather than discovered later.** A Back
//! Cue must send `window::Command::Relocate`, and nothing here observes the
//! send: the command goes down a channel the window thread owns, and its
//! effect — a window rebuilt at the target rather than below it — is visible
//! only as a timing ratio, which `tests/window_test.rs` measures for the
//! command itself. So the three links are each checked and the chain is not:
//! `Transport` returns `Cued::Returned` (transport tests), `Deck` turns that
//! into `Playing::relocate` (read, not run), and the window acts on the
//! command (window tests). Covering the middle link needs the command
//! channel visible from outside `Playing`.

mod fixtures;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use deck_pi::app::deck::Deck;
use deck_pi::app::track::Config;
use deck_pi::browser::Browser;
use deck_pi::cue::CueStore;
use deck_pi::input::{Action, Button};
use deck_pi::sink::CaptureSink;
use deck_pi::transport::{State, RATE_PAUSED};
use fixtures::{Bits, Kind, Scratch};

const WINDOW_BYTES: usize = 8_192 * deck_pi::ring::RING_FRAME_BYTES;
const PERIOD: usize = 128;
const RATE: u32 = 44_100;
const VOLUME: &str = "AAAA-BBBB";

struct Rig {
    scratch: Scratch,
    medium: PathBuf,
    state: PathBuf,
}

fn rig(tag: &str) -> Rig {
    let scratch = Scratch::new(tag);
    let medium = scratch.dir.join("medium");
    let state = scratch.dir.join("state");
    std::fs::create_dir_all(&medium).expect("medium");
    std::fs::create_dir_all(&state).expect("state");
    Rig {
        scratch,
        medium,
        state,
    }
}

impl Rig {
    /// A playable WAV at `medium/<folder>/<name>.wav`.
    fn track(&self, folder: &str, name: &str, frames: usize) -> PathBuf {
        let dir = self.medium.join(folder);
        std::fs::create_dir_all(&dir).expect("folder");
        let samples = fixtures::signal(Bits::S24, 2, frames);
        let bytes = fixtures::build(Kind::Wav, &samples, Bits::S24, RATE, 2);
        fixtures::write(&dir, name, Kind::Wav, &bytes)
    }

    fn deck(&self) -> Deck<CaptureSink> {
        let browser = Browser::open(&self.medium).expect("browser");
        let cues = CueStore::load(&self.state, VOLUME, &self.medium).expect("cues");
        Deck::new(
            Box::new(|info| Ok(CaptureSink::new(info.rate, PERIOD, 200_000))),
            Some(browser),
            Some(cues),
            Config {
                window_bytes: WINDOW_BYTES,
                rt: None,
            },
        )
    }

    fn cues(&self) -> CueStore {
        CueStore::load(&self.state, VOLUME, &self.medium).expect("cues")
    }
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("timed out waiting for {what}");
}

/// Moves the selection to the row with this name, from wherever it is.
fn select(deck: &mut Deck<CaptureSink>, name: &str) {
    let browser = deck.browser().expect("a medium");
    for _ in 0..64 {
        let rows = browser.view(16);
        if rows.iter().any(|r| r.selected() && r.name() == name) {
            return;
        }
        browser.select_next();
    }
    panic!("no row named {name}");
}

#[test]
fn enter_on_a_playable_row_loads_it_and_waits_at_its_head() {
    let r = rig("deck-enter");
    r.track("set", "opener", 2_000);
    let mut deck = r.deck();

    select(&mut deck, "set");
    deck.apply(Action::Press(Button::Enter)).expect("descend");
    select(&mut deck, "opener.wav");
    deck.apply(Action::Press(Button::Enter)).expect("load");

    assert!(deck.loaded().path().is_some(), "the track is loaded");
    assert_eq!(deck.transport().state(), State::Paused, "and waits");
    assert_eq!(deck.transport().position(), 0.0, "at its head");

    // PLAY is now accepted, where on an empty deck it was refused.
    deck.apply(Action::Press(Button::PlayPause)).expect("play");
    assert_eq!(deck.transport().state(), State::Playing);

    deck.unload();
    drop(r.scratch);
}

#[test]
fn a_cue_set_while_browsing_elsewhere_lands_on_the_playing_track() {
    // **The defect `src/loaded.rs` exists to prevent, now actually wired.**
    // Play a track in `a`, browse to `b`, press CUE to mark a spot in the
    // track you can hear. Asking the browser for the path would key the cue
    // on whatever is highlighted — atomically, returning `Ok`, with nothing
    // to notice until the cue goes to the start of the track it belongs to
    // weeks later.
    let r = rig("deck-cue-key");
    let playing = r.track("a", "one", 40_000);
    let browsed = r.track("b", "two", 2_000);
    let mut deck = r.deck();

    select(&mut deck, "a");
    deck.apply(Action::Press(Button::Enter)).expect("descend");
    select(&mut deck, "one.wav");
    deck.apply(Action::Press(Button::Enter)).expect("load");
    deck.apply(Action::Press(Button::PlayPause)).expect("play");
    wait_for("the deck to get somewhere", || {
        deck.transport().position() > 500.0
    });

    // Browse away while it plays. This is what a browser on a deck is for.
    deck.apply(Action::Press(Button::Back)).expect("back");
    select(&mut deck, "b");
    deck.apply(Action::Press(Button::Enter)).expect("descend");
    select(&mut deck, "two.wav");

    // Pause and mark the spot.
    deck.apply(Action::Press(Button::PlayPause)).expect("pause");
    let at = deck.transport().position() as u64;
    assert!(at > 0, "the fixture must have played something");
    deck.apply(Action::Press(Button::Cue)).expect("set the cue");

    assert_eq!(deck.transport().cue_point(), at);
    let store = r.cues();
    assert_eq!(
        store.get(&playing).expect("get"),
        at,
        "the cue belongs to the track that is playing"
    );
    assert_eq!(
        store.get(&browsed).expect("get"),
        0,
        "and not to the one being browsed"
    );

    deck.unload();
    drop(r.scratch);
}

#[test]
fn a_tap_steps_the_playing_folder_and_stops_at_its_end() {
    // `decisions.md`: a tap loads the next track and waits at its head, even
    // if the deck was playing — nothing starts making sound that PLAY did
    // not start. And `None` at a folder boundary is #12's answer: do
    // nothing, which is stopping.
    let r = rig("deck-tap");
    r.track("set", "1-first", 2_000);
    let second = r.track("set", "2-second", 2_000);
    let mut deck = r.deck();

    select(&mut deck, "set");
    deck.apply(Action::Press(Button::Enter)).expect("descend");
    select(&mut deck, "1-first.wav");
    deck.apply(Action::Press(Button::Enter)).expect("load");
    deck.apply(Action::Press(Button::PlayPause)).expect("play");

    deck.apply(Action::Tap(Button::Ff)).expect("next");
    assert_eq!(deck.loaded().path(), Some(second.as_path()));
    assert_eq!(
        deck.transport().state(),
        State::Paused,
        "a tap waits at the head even from a playing deck"
    );
    assert_eq!(deck.transport().rate(), RATE_PAUSED);

    // The end of the folder: nothing happens, and that is the answer.
    deck.apply(Action::Tap(Button::Ff)).expect("no next");
    assert_eq!(deck.loaded().path(), Some(second.as_path()), "still there");

    deck.unload();
    drop(r.scratch);
}

#[test]
fn the_tap_follows_the_playing_track_not_the_selection() {
    // One button's two gestures must not address two objects — FF/REW's
    // *hold* seeks inside the playing track, so its *tap* has to act on the
    // same track. `controls.md` rejected the same shape when it refused to
    // overload the browse encoder for seeking.
    let r = rig("deck-tap-object");
    r.track("a", "1-a-one", 2_000);
    let a_two = r.track("a", "2-a-two", 2_000);
    r.track("b", "1-b-one", 2_000);
    let mut deck = r.deck();

    select(&mut deck, "a");
    deck.apply(Action::Press(Button::Enter)).expect("descend");
    select(&mut deck, "1-a-one.wav");
    deck.apply(Action::Press(Button::Enter)).expect("load");

    // Browse into the other folder and leave the selection there.
    deck.apply(Action::Press(Button::Back)).expect("back");
    select(&mut deck, "b");
    deck.apply(Action::Press(Button::Enter)).expect("descend");
    select(&mut deck, "1-b-one.wav");

    deck.apply(Action::Tap(Button::Ff)).expect("next");
    assert_eq!(
        deck.loaded().path(),
        Some(a_two.as_path()),
        "the tap moved inside the playing folder, not the browsed one"
    );

    deck.unload();
    drop(r.scratch);
}

#[test]
fn entering_a_file_the_deck_refuses_is_not_an_error_and_changes_nothing() {
    // The row already carries the reason and the display already shows it —
    // `decisions.md`'s "say why, not just that", said on highlight, before
    // PLAY is reachable at all. So ENTER on it is a no-op rather than a
    // failure to report.
    let r = rig("deck-refused");
    std::fs::write(r.medium.join("compressed.flac"), b"not a wav").expect("write");
    let mut deck = r.deck();

    select(&mut deck, "compressed.flac");
    deck.apply(Action::Press(Button::Enter))
        .expect("a refused file is not a dispatch error");
    assert!(deck.loaded().path().is_none(), "nothing was loaded");
    assert_eq!(deck.transport().state(), State::Stopped);
    drop(r.scratch);
}

#[test]
fn the_control_loop_pauses_the_deck_when_the_track_plays_out() {
    let r = rig("deck-service");
    r.track("set", "short", 1_500);
    let mut deck = r.deck();

    select(&mut deck, "set");
    deck.apply(Action::Press(Button::Enter)).expect("descend");
    select(&mut deck, "short.wav");
    deck.apply(Action::Press(Button::Enter)).expect("load");
    deck.apply(Action::Press(Button::PlayPause)).expect("play");

    wait_for("the deck to pause itself at the end", || {
        deck.service();
        deck.transport().state() == State::Paused
    });
    assert!(
        deck.playing().is_some(),
        "the end of a track is not the end of the run — it is still loaded"
    );

    deck.unload();
    assert_eq!(deck.transport().state(), State::Stopped);
    assert!(deck.loaded().path().is_none());
    drop(r.scratch);
}
