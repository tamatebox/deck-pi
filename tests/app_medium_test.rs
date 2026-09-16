//! The stick arriving and going, against the real dispatch.
//!
//! `src/media.rs` has had tests since it was written — they examine real
//! directories and settle "is this a mount point". What had no test, because
//! it had no code, is what the **deck** does when the answer changes. That is
//! the wiring this file covers: a browser and a cue store appearing, a track
//! ending because its medium went, and the transition that looks like nothing
//! happened — one browsable volume replaced by another between two polls.
//!
//! The medium is scripted rather than mounted. Mounting a filesystem needs
//! root, and `tests/` runs as a user on two machines; what is mounted is
//! `media.rs`'s question and is tested there, and what the deck does about the
//! answer is this one. The split is the reason `MediumSource` is a trait.

mod fixtures;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use deck_pi::app::deck::Deck;
use deck_pi::app::medium::{MediumSource, Mount, POLL_EVERY};
use deck_pi::app::track::Config;
use deck_pi::input::{Action, Button};
use deck_pi::media::Medium;
use deck_pi::sink::CaptureSink;
use deck_pi::transport::State;
use fixtures::{Bits, Kind, Scratch};

const WINDOW_BYTES: usize = 8_192 * deck_pi::ring::RING_FRAME_BYTES;
const PERIOD: usize = 128;
const RATE: u32 = 44_100;

/// A medium whose states are a list rather than a filesystem.
///
/// Keeps [`MediaWatch`](deck_pi::media::MediaWatch)'s contract exactly —
/// `poll` answers only when the state changed — because a fake that is easier
/// to satisfy than the real thing tests a loop that does not exist.
struct Script {
    mount_point: PathBuf,
    states: VecDeque<Medium>,
    polls: usize,
}

impl Script {
    fn new(mount_point: impl Into<PathBuf>, states: Vec<Medium>) -> Script {
        Script {
            mount_point: mount_point.into(),
            states: states.into(),
            polls: 0,
        }
    }
}

impl MediumSource for Script {
    fn poll(&mut self) -> Option<Medium> {
        self.polls += 1;
        self.states.pop_front()
    }

    fn mount_point(&self) -> &Path {
        &self.mount_point
    }
}

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
    fn track(&self, folder: &str, name: &str, frames: usize) -> PathBuf {
        let dir = self.medium.join(folder);
        std::fs::create_dir_all(&dir).expect("folder");
        let samples = fixtures::signal(Bits::S24, 2, frames);
        let bytes = fixtures::build(Kind::Wav, &samples, Bits::S24, RATE, 2);
        fixtures::write(&dir, name, Kind::Wav, &bytes)
    }

    /// A deck with **no medium**, which is how one starts: the browser and the
    /// cue store arrive with the stick or not at all.
    fn deck(&self) -> Deck<CaptureSink> {
        Deck::new(
            Box::new(|info| Ok(CaptureSink::new(info.rate, PERIOD, 200_000))),
            None,
            None,
            Config {
                window_bytes: WINDOW_BYTES,
                rt: None,
            },
        )
    }

    fn mount(&self, states: Vec<Medium>) -> Mount<Script> {
        Mount::new(
            Script::new(&self.medium, states),
            Some(self.state.clone()),
        )
        .with_interval(Duration::ZERO)
    }
}

fn browsable(uuid: &str) -> Medium {
    Medium::Browsable {
        uuid: Some(uuid.to_owned()),
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
        if browser
            .view(16)
            .iter()
            .any(|r| r.selected() && r.name() == name)
        {
            return;
        }
        browser.select_next();
    }
    panic!("no row named {name}");
}

fn load(deck: &mut Deck<CaptureSink>, folder: &str, name: &str) {
    select(deck, folder);
    deck.apply(Action::Press(Button::Enter)).expect("descend");
    select(deck, name);
    deck.apply(Action::Press(Button::Enter)).expect("load");
    assert!(deck.loaded().path().is_some(), "{name} did not load");
}

#[test]
fn a_stick_pushed_in_after_the_deck_started_is_what_gives_it_a_browser() {
    // The first of the two silent failures this wiring removes. A deck is
    // switched on with no stick in it far more often than not, and until now
    // that deck stayed browserless for as long as it ran.
    let r = rig("medium-arrives");
    r.track("set", "opener", 2_000);
    let mut deck = r.deck();
    let mut mount = r.mount(vec![browsable("AAAA-BBBB")]);

    assert!(deck.browser().is_none(), "nothing is mounted yet");
    assert!(deck.cues().is_none());

    let change = mount.turn(Duration::ZERO, &mut deck).expect("a change");
    assert!(change.is_clean(), "{change:?}");
    assert_eq!(change.medium, browsable("AAAA-BBBB"));
    assert!(change.ended.is_none(), "nothing was playing to end");

    assert!(deck.browser().is_some(), "the listing arrived with the stick");
    assert!(deck.cues().is_some(), "and so did somewhere to keep cues");

    // And it is the listing of the medium, not of an empty directory.
    load(&mut deck, "set", "opener.wav");
    assert_eq!(deck.transport().state(), State::Paused);

    deck.unload();
    drop(r.scratch);
}

#[test]
fn a_stick_that_goes_ends_the_run_and_takes_the_listing_with_it() {
    // The second, and the worse one: the deck went on holding a track whose
    // file was gone, and on listing a folder that was not mounted, until the
    // window thread happened to fail. `decisions.md` says the medium going is
    // one of the three things that ends a run.
    let r = rig("medium-goes");
    r.track("set", "opener", 40_000);
    let mut deck = r.deck();
    let mut mount = r.mount(vec![browsable("AAAA-BBBB"), Medium::Absent]);

    mount.turn(Duration::ZERO, &mut deck).expect("mounted");
    load(&mut deck, "set", "opener.wav");
    deck.apply(Action::Press(Button::PlayPause)).expect("play");
    assert_eq!(deck.transport().state(), State::Playing);

    let change = mount.turn(Duration::ZERO, &mut deck).expect("a change");
    assert_eq!(change.medium, Medium::Absent);
    assert!(change.ended.is_some(), "the removal ended the run: {change:?}");

    assert!(deck.loaded().path().is_none(), "nothing is loaded now");
    assert_eq!(
        deck.transport().state(),
        State::Stopped,
        "Stopped means nothing loaded, which is what this is"
    );
    assert!(deck.browser().is_none(), "and there is nothing to browse");
    assert!(deck.cues().is_none());

    // **The press that arrived with the removal.** This is why the loop turns
    // the medium over before it applies anything: with the browser gone ENTER
    // has nothing to act on, where a turn in the other order would have
    // loaded a file from a stick that is not there.
    deck.apply(Action::Press(Button::Enter)).expect("no medium");
    assert!(deck.loaded().path().is_none(), "ENTER loaded something");

    drop(r.scratch);
}

#[test]
fn one_browsable_medium_replacing_another_is_still_a_replacement() {
    // The transition that reads as "nothing happened": a stick pulled and
    // another pushed in between two polls is a single `Browsable` to
    // `Browsable` change. Mounting only on a change *from* `Absent` would
    // keep the old listing and — the part that corrupts something — go on
    // writing cues into the previous volume's file.
    let r = rig("medium-swap");
    r.track("set", "opener", 40_000);
    let mut deck = r.deck();
    let mut mount = r.mount(vec![browsable("AAAA-BBBB"), browsable("CCCC-DDDD")]);

    mount.turn(Duration::ZERO, &mut deck).expect("mounted");
    let first = deck.cues().expect("cues").path().to_path_buf();
    load(&mut deck, "set", "opener.wav");
    deck.apply(Action::Press(Button::PlayPause)).expect("play");

    let change = mount.turn(Duration::ZERO, &mut deck).expect("a change");
    assert_eq!(change.medium, browsable("CCCC-DDDD"));
    assert!(
        change.ended.is_some(),
        "the volume under the playing track was replaced: {change:?}"
    );
    assert!(deck.loaded().path().is_none());

    let second = deck.cues().expect("cues").path().to_path_buf();
    assert_ne!(first, second, "cues are still keyed on the old volume");
    assert!(
        second.file_name().unwrap().to_string_lossy().contains("CCCC-DDDD"),
        "{second:?}"
    );

    drop(r.scratch);
}

#[test]
fn a_volume_with_no_uuid_browses_and_plays_and_keeps_no_cues() {
    // `media.rs` calls this "a degradation worth surfacing, not a reason to
    // refuse the medium", so the deck has to actually be in that state rather
    // than treat it as a failed mount.
    let r = rig("medium-no-uuid");
    r.track("set", "opener", 2_000);
    let mut deck = r.deck();
    let mut mount = r.mount(vec![Medium::Browsable { uuid: None }]);

    let change = mount.turn(Duration::ZERO, &mut deck).expect("a change");
    assert!(change.is_clean(), "no UUID is not an error: {change:?}");
    assert!(deck.browser().is_some(), "it browses");
    assert!(deck.cues().is_none(), "and keeps no cues");

    load(&mut deck, "set", "opener.wav");
    // The cue press is refused by nothing and stores nowhere — `Deck::cued`
    // checks for a store, and a deck that panicked here would be one a
    // UUID-less stick could kill with one button.
    deck.apply(Action::Press(Button::Cue)).expect("cue");
    deck.apply(Action::Release(Button::Cue)).expect("cue up");

    deck.unload();
    drop(r.scratch);
}

#[test]
fn a_medium_that_examines_browsable_and_will_not_open_says_so() {
    // The stick going between the examination and the `read_dir`. Folding
    // this into "no medium" would describe a stick that is there as a stick
    // that is not, and the operator would be looking at `No USB` with the
    // thing plugged in.
    let r = rig("medium-unopenable");
    let mut deck = r.deck();
    let gone = r.scratch.dir.join("never-existed");
    let mut mount = Mount::new(
        Script::new(&gone, vec![browsable("AAAA-BBBB")]),
        Some(r.state.clone()),
    )
    .with_interval(Duration::ZERO);

    let change = mount.turn(Duration::ZERO, &mut deck).expect("a change");
    assert!(change.unbrowsable.is_some(), "{change:?}");
    assert!(change.cueless.is_none(), "the cue store was never reached");
    assert!(deck.browser().is_none(), "and the deck has no listing");

    drop(r.scratch);
}

#[test]
fn the_medium_is_examined_at_a_human_rate_and_at_once_on_the_first_turn() {
    // Two `stat` calls are cheap, and a hundred a second for ever is still a
    // hundred a second for ever. The first turn is deliberately not paced: a
    // stick already in the deck at switch-on should be found then, not half a
    // second later.
    let r = rig("medium-pace");
    let mut deck = r.deck();
    let mut mount = Mount::new(
        Script::new(&r.medium, vec![browsable("AAAA-BBBB")]),
        Some(r.state.clone()),
    );

    assert!(mount.turn(Duration::ZERO, &mut deck).is_some(), "first turn");
    assert_eq!(mount.watch().polls, 1);

    for ms in [1, 10, 100, 499] {
        assert!(mount
            .turn(Duration::from_millis(ms), &mut deck)
            .is_none());
    }
    assert_eq!(mount.watch().polls, 1, "polled inside the interval");

    assert!(mount.turn(POLL_EVERY, &mut deck).is_none(), "no change left");
    assert_eq!(mount.watch().polls, 2, "and polled once the interval passed");

    drop(r.scratch);
}

/// The one link the scripted source cannot reach: `impl MediumSource for
/// MediaWatch`, two lines of forwarding that nothing else calls.
///
/// Needs a real mount, so it is `--ignored` and takes the path from the
/// environment like the other hardware checks:
///
/// ```sh
/// DECK_PI_MOUNT=/media/stick cargo test --release \
///     --test app_medium_test -- --ignored --nocapture
/// ```
#[test]
#[ignore]
fn the_real_watch_hands_the_deck_a_real_medium() {
    let at = std::env::var("DECK_PI_MOUNT").unwrap_or_else(|_| "/media/stick".into());
    let r = rig("medium-hardware");
    let mut deck = r.deck();
    let mut mount = Mount::new(
        deck_pi::media::MediaWatch::new(&at),
        Some(r.state.clone()),
    );

    let change = mount.turn(Duration::ZERO, &mut deck).expect("a change");
    println!("{at}: {}", change.medium);
    assert!(
        change.medium.is_browsable(),
        "nothing browsable is mounted at {at}"
    );
    assert!(change.is_clean(), "{change:?}");
    assert!(deck.browser().is_some(), "the deck got no listing");

    match (change.medium.uuid(), deck.cues()) {
        (Some(u), Some(store)) => println!("volume {u}, cues in {}", store.path().display()),
        // Not a failure of the wiring, and worth printing rather than
        // asserting away: a volume with no UUID browses and plays.
        (None, None) => println!("no volume UUID — this stick cannot keep cues"),
        (u, c) => panic!("a UUID and a cue store must arrive together: {u:?}, {}", c.is_some()),
    }

    let rows = deck.browser().expect("browser").view(8).len();
    assert!(rows > 0, "the medium browsed as empty");
    println!("{rows} row(s) in the root");

    drop(r.scratch);
}

#[test]
fn a_cue_written_before_a_remount_is_there_after_it() {
    // The point of keying cues on the volume: the stick is the identity, not
    // the session. This also checks the cue file is on the *state* directory
    // and not on the medium — the stick is mounted read-only, so a store that
    // wrote there would fail on the deck and pass in a test that used a
    // writable scratch directory for both.
    let r = rig("medium-remount");
    r.track("set", "opener", 40_000);
    let mut deck = r.deck();
    let mut mount = r.mount(vec![
        browsable("AAAA-BBBB"),
        Medium::Absent,
        browsable("AAAA-BBBB"),
    ]);

    mount.turn(Duration::ZERO, &mut deck).expect("mounted");
    load(&mut deck, "set", "opener.wav");
    deck.apply(Action::Press(Button::PlayPause)).expect("play");
    wait_for("the deck to get somewhere", || {
        deck.transport().position() > 500.0
    });
    // Setting a cue is a press while **paused and away from the point** —
    // `Transport::cue_down`. Paused at frame zero is the Cue Point Sampler
    // instead, which is a preview and stores nothing.
    deck.apply(Action::Press(Button::PlayPause)).expect("pause");
    let at = deck.transport().position() as u64;
    assert!(at > 0, "the fixture must have played something");
    deck.apply(Action::Press(Button::Cue)).expect("cue");
    assert_eq!(deck.cues().expect("cues").len(), 1, "the press stored no cue");

    mount.turn(Duration::ZERO, &mut deck).expect("ejected");
    mount.turn(Duration::ZERO, &mut deck).expect("remounted");

    let store = deck.cues().expect("cues");
    assert_eq!(store.len(), 1, "the cue did not survive the remount");
    let track = r.medium.join("set").join("opener.wav");
    assert_eq!(store.get(&track).expect("get"), at, "and it is the same point");
    assert!(
        store.path().starts_with(&r.state),
        "cues must live on the state directory, not on the read-only medium: {:?}",
        store.path()
    );

    deck.unload();
    drop(r.scratch);
}
