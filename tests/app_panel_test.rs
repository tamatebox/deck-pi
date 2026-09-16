//! The display half: what the deck shows, and when it bothers.
//!
//! `src/display.rs` and `display::paint` have their own tests and they are
//! about cells and pixels. This is the layer above both — the one that reads a
//! deck and produces a [`Screen`] — and the questions here are not about
//! layout at all:
//!
//! - does the status line describe **the playing track** or whatever the
//!   browser happens to be pointing at,
//! - is the position converted with **that track's** sample rate,
//! - and does a turn that changes nothing stay off the filesystem.
//!
//! The first is the same shape as the defect `src/loaded.rs` exists to
//! prevent, one layer up: a cue keyed on the selection was silent and wrong,
//! and a status line keyed on the selection would be too.

mod fixtures;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use deck_pi::app::deck::Deck;
use deck_pi::app::panel::{screen_of, Panel, Show, NO_MEDIUM};
use deck_pi::app::track::Config;
use deck_pi::browser::Browser;
use deck_pi::display::{Geometry, Redraw, Screen};
use deck_pi::input::{Action, Button};
use deck_pi::sink::CaptureSink;
use deck_pi::transport::State;
use fixtures::{Bits, Kind, Scratch};

const WINDOW_BYTES: usize = 8_192 * deck_pi::ring::RING_FRAME_BYTES;
const PERIOD: usize = 128;

/// A display that keeps what it was shown and can change shape underneath the
/// deck, which is what a Pico declaring a different panel looks like.
#[derive(Default)]
struct Capture {
    screens: Vec<Screen>,
    geometry: Option<Geometry>,
}

impl Capture {
    fn grid(&self) -> Geometry {
        self.geometry
            .unwrap_or(Geometry { cols: 40, rows: 8, colour: false })
    }
    fn last(&self) -> &Screen {
        self.screens.last().expect("nothing was drawn")
    }
}

impl Show for Capture {
    fn geometry(&self) -> Geometry {
        self.grid()
    }
    fn show(&mut self, screen: &Screen) -> std::io::Result<()> {
        self.screens.push(screen.clone());
        Ok(())
    }
}

struct Rig {
    scratch: Scratch,
    medium: PathBuf,
}

fn rig(tag: &str) -> Rig {
    let scratch = Scratch::new(tag);
    let medium = scratch.dir.join("medium");
    std::fs::create_dir_all(&medium).expect("medium");
    Rig { scratch, medium }
}

impl Rig {
    fn track(&self, folder: &str, name: &str, rate: u32, bits: Bits, frames: usize) -> PathBuf {
        let dir = self.medium.join(folder);
        std::fs::create_dir_all(&dir).expect("folder");
        let samples = fixtures::signal(bits, 2, frames);
        let bytes = fixtures::build(Kind::Wav, &samples, bits, rate, 2);
        fixtures::write(&dir, name, Kind::Wav, &bytes)
    }

    fn deck(&self, with_medium: bool) -> Deck<CaptureSink> {
        Deck::new(
            Box::new(|info| Ok(CaptureSink::new(info.rate, PERIOD, 400_000))),
            with_medium.then(|| Browser::open(&self.medium).expect("browser")),
            None,
            Config { window_bytes: WINDOW_BYTES, rt: None },
        )
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

fn select(deck: &mut Deck<CaptureSink>, name: &str) {
    let browser = deck.browser().expect("a medium");
    for _ in 0..64 {
        if browser.view(16).iter().any(|r| r.selected() && r.name() == name) {
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
}

#[test]
fn a_deck_with_no_medium_says_so_rather_than_drawing_nothing() {
    // The state a deck is switched on in. A display that drew only when there
    // was something to draw would show an empty panel, which is what a broken
    // deck also shows.
    let r = rig("panel-empty");
    let mut deck = r.deck(false);
    let g = Geometry { cols: 40, rows: 8, colour: false };

    let s = screen_of(&mut deck, g);
    assert_eq!(s.folder, NO_MEDIUM);
    assert!(s.lines.is_empty());
    assert_eq!(s.status, "NO TRACK", "and the transport says the same thing");
    assert_eq!(deck.transport().state(), State::Stopped);

    drop(r.scratch);
}

#[test]
fn the_status_describes_the_playing_track_and_not_the_browsers_selection() {
    // **The `loaded.rs` defect, one layer up.** Play a 96/24 track, browse to a
    // folder holding a 44.1/16 one, and the status must still say 96k/24: it
    // describes what is coming out of the deck, not what a finger is hovering
    // over. Asking the browser would be the natural wiring and would be wrong
    // in a way nobody would notice from a screenshot.
    let r = rig("panel-two-currents");
    r.track("a", "playing", 96_000, Bits::S24, 200_000);
    r.track("b", "browsed", 44_100, Bits::S16, 2_000);
    let mut deck = r.deck(true);
    let g = Geometry { cols: 40, rows: 8, colour: false };

    load(&mut deck, "a", "playing.wav");
    deck.apply(Action::Press(Button::PlayPause)).expect("play");
    wait_for("the deck to get somewhere", || {
        deck.transport().position() > 96_000.0
    });

    deck.apply(Action::Press(Button::Back)).expect("back");
    load_selection_only(&mut deck, "b");

    let s = screen_of(&mut deck, g);
    assert_eq!(s.folder, "b", "the listing follows the browser");
    assert!(s.status.contains("96k/24"), "the status followed the browser: {}", s.status);
    assert!(s.status.starts_with("PLAY"), "{}", s.status);

    deck.unload();
    drop(r.scratch);
}

/// Descends into `folder` without loading anything out of it.
fn load_selection_only(deck: &mut Deck<CaptureSink>, folder: &str) {
    select(deck, folder);
    deck.apply(Action::Press(Button::Enter)).expect("descend");
}

#[test]
fn the_position_is_converted_with_the_tracks_own_rate() {
    // The transport counts frames and the panel shows a time, so something has
    // to divide — and dividing by the wrong rate gives a clock that runs at
    // the ratio of the two. At 96 kHz against an assumed 44.1 it would run
    // slow by a factor of 2.18, which reads as a plausible time.
    let r = rig("panel-clock");
    r.track("a", "long", 96_000, Bits::S24, 400_000);
    let mut deck = r.deck(true);
    let g = Geometry { cols: 40, rows: 8, colour: false };

    load(&mut deck, "a", "long.wav");
    deck.apply(Action::Press(Button::PlayPause)).expect("play");
    // Two seconds of audio is 192,000 frames at this rate.
    wait_for("two seconds of audio", || {
        deck.transport().position() >= 192_000.0
    });
    deck.apply(Action::Press(Button::PlayPause)).expect("pause");

    let frames = deck.transport().position();
    let s = screen_of(&mut deck, g);
    let expected = deck_pi::display::timecode(Duration::from_secs_f64(frames / 96_000.0));
    assert!(
        s.status.contains(&expected),
        "{} does not carry {expected} for {frames} frames at 96 kHz",
        s.status
    );
    // And the naive reading is visibly different, so the test is not passing
    // by both answers agreeing.
    let wrong = deck_pi::display::timecode(Duration::from_secs_f64(frames / 44_100.0));
    assert_ne!(expected, wrong, "the fixture cannot tell the two apart");

    deck.unload();
    drop(r.scratch);
}

#[test]
fn a_turn_that_changes_nothing_does_not_touch_the_filesystem() {
    // `architecture.md`: header reads ride the redraw budget, driven "from
    // what is actually rendered rather than from each encoder event". The
    // cheap way to get that wrong is to compose a `Screen` every turn to see
    // whether it changed — `Screen` is `PartialEq`, so it reads as free, and
    // `Browser::view` opens a header for every row it is about to show.
    let r = rig("panel-quiet");
    for i in 0..4 {
        r.track("a", &format!("t{i}"), 44_100, Bits::S16, 2_000);
    }
    let mut deck = r.deck(true);
    let mut panel = Panel::new(Capture::default());

    select(&mut deck, "a");
    deck.apply(Action::Press(Button::Enter)).expect("descend");

    assert_eq!(panel.turn(Duration::ZERO, &mut deck).expect("turn"), Redraw::Full);
    let after_first = deck.browser().expect("browser").headers_read();
    assert!(after_first > 0, "the first draw read no headers at all");

    // A hundred turns inside the coalescing window with nothing moving.
    for ms in 1..=100 {
        let drawn = panel.turn(Duration::from_millis(ms), &mut deck).expect("turn");
        assert_eq!(drawn, Redraw::Nothing, "drew at {ms} ms with nothing changed");
    }
    assert_eq!(
        deck.browser().expect("browser").headers_read(),
        after_first,
        "a quiet turn opened a header"
    );
    assert_eq!(panel.show().screens.len(), 1);

    drop(r.scratch);
}

#[test]
fn a_panel_that_has_been_replaced_is_drawn_again_even_though_nothing_moved() {
    // The medium-swap shape, on the display. A reconnected panel is blank, but
    // a `Cadence` carried over has nothing pending and a recent full draw — so
    // it answers `Nothing`, and with a track playing it would only ever answer
    // `Position`. The listing would stay blank until something unrelated
    // changed.
    let r = rig("panel-replaced");
    r.track("a", "one", 44_100, Bits::S16, 2_000);
    let mut deck = r.deck(true);
    let mut panel = Panel::new(Capture::default());

    assert_eq!(panel.turn(Duration::ZERO, &mut deck).expect("turn"), Redraw::Full);
    assert_eq!(panel.turn(Duration::from_millis(1), &mut deck).expect("turn"), Redraw::Nothing);

    panel.forget();
    assert_eq!(
        panel.turn(Duration::from_millis(2), &mut deck).expect("turn"),
        Redraw::Full,
        "a replaced panel was left blank"
    );
    assert_eq!(panel.show().screens.len(), 2);

    drop(r.scratch);
}

#[test]
fn the_grid_is_taken_from_the_display_every_turn_and_never_remembered() {
    // The panel's geometry arrives in the Pico's declaration, so a reconnect
    // can bring a different one. A cached copy would compose a listing for a
    // panel that is no longer there — and the listing is the thing that would
    // look merely odd rather than broken.
    let r = rig("panel-regrid");
    for i in 0..10 {
        r.track("a", &format!("t{i}"), 44_100, Bits::S16, 2_000);
    }
    let mut deck = r.deck(true);
    let mut panel = Panel::new(Capture::default());

    select(&mut deck, "a");
    deck.apply(Action::Press(Button::Enter)).expect("descend");
    panel.turn(Duration::ZERO, &mut deck).expect("turn");
    let tall = panel.show().last().lines.len();
    assert_eq!(tall, 6, "eight rows, less the path and the status");

    // The panel is swapped for a shorter one.
    panel.show_mut().geometry = Some(Geometry { cols: 21, rows: 5, colour: false });
    panel.forget();
    panel.turn(Duration::from_millis(50), &mut deck).expect("turn");
    let short = panel.show().last();
    assert_eq!(short.lines.len(), 3, "the listing did not follow the new panel");
    assert!(
        short.lines.iter().all(|l| deck_pi::display::width(&l.text) <= 20),
        "names were cut to the old panel's width: {short:?}"
    );

    drop(r.scratch);
}

#[test]
fn the_selection_stays_visible_when_the_panel_gets_shorter() {
    // `Browser::view`'s clamp says it pulls the selection back into the
    // viewport "on the next render, always". A twelve-row selection arriving
    // on a three-row panel is the case that makes that claim load-bearing, and
    // it is reachable the moment a panel can be swapped.
    let r = rig("panel-shrink");
    for i in 0..12 {
        r.track("a", &format!("t{i:02}"), 44_100, Bits::S16, 2_000);
    }
    let mut deck = r.deck(true);
    let mut panel = Panel::new(Capture::default());
    panel.show_mut().geometry = Some(Geometry { cols: 40, rows: 14, colour: false });

    select(&mut deck, "a");
    deck.apply(Action::Press(Button::Enter)).expect("descend");
    select(&mut deck, "t11.wav");
    panel.turn(Duration::ZERO, &mut deck).expect("turn");
    assert!(panel.show().last().lines.iter().any(|l| l.selected));

    panel.show_mut().geometry = Some(Geometry { cols: 40, rows: 5, colour: false });
    panel.forget();
    panel.turn(Duration::from_millis(50), &mut deck).expect("turn");
    let s = panel.show().last();
    assert_eq!(s.lines.len(), 3);
    assert!(
        s.lines.iter().any(|l| l.selected && l.text.starts_with("t11")),
        "the selection fell off the shorter panel: {s:?}"
    );

    drop(r.scratch);
}
