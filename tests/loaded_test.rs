//! Who owns "what is loaded" — [#14].
//!
//! The two failures this answers are different in kind and both are in here.
//! One is a **preference settled by argument**: a tap moves within the playing
//! track's folder, because FF/REW's hold already acts there and one button's
//! two gestures must not act on two objects. The other is a **correctness
//! constraint that holds either way**: a cue must be keyed on what is playing,
//! and until this module existed the only thing holding paths was the browser.
//!
//! [#14]: https://github.com/tamatebox/deck-pi/issues/14

mod fixtures;

use std::path::Path;

use deck_pi::browser::{Activation, Browser};
use deck_pi::cue::CueStore;
use deck_pi::file::Track;
use deck_pi::loaded::{Loaded, Step};
use fixtures::{Bits, Kind, Scratch};

/// Two folders, each with tracks, so the selection and the playing track can
/// be somewhere different — which is the whole situation.
fn stick(tag: &str) -> Scratch {
    let s = Scratch::new(tag);
    for (dir, names) in [("A", &["1_one", "2_two", "3_three"][..]), ("B", &["x_other", "y_other"][..])] {
        let d = s.dir.join(dir);
        std::fs::create_dir_all(&d).expect("mkdir");
        for n in names.iter().copied() {
            let samples = fixtures::signal(Bits::S16, 2, 256);
            let bytes = fixtures::build(Kind::Wav, &samples, Bits::S16, 44_100, 2);
            fixtures::write(&d, n, Kind::Wav, &bytes);
        }
    }
    s
}

fn load(l: &mut Loaded, path: &Path) {
    let t = Track::open(path).expect("open");
    l.load(Box::new(t.info().clone()));
}

#[test]
fn a_tap_steps_through_the_playing_tracks_folder_not_the_browsed_one() {
    // The settled answer. The browser is somewhere else entirely, and a tap
    // must not care.
    let s = stick("loaded-tap");
    let mut l = Loaded::nothing();
    load(&mut l, &s.dir.join("A/1_one.wav"));

    // Browse away, into the other folder, as you would while a track plays.
    let mut b = Browser::open(&s.dir).expect("opens");
    descend_into(&mut b, "B");
    assert!(b.path().ends_with("B"), "the browser is in the other folder");

    // The tap is unaffected by where the browser went.
    assert_eq!(
        l.neighbour(Step::Next).expect("read"),
        Some(s.dir.join("A/2_two.wav"))
    );
    assert_eq!(l.neighbour(Step::Previous).expect("read"), None, "at the top");
}

/// Walks the selection to a named row and enters it.
fn descend_into(b: &mut Browser, name: &str) {
    let rows = b.view(64);
    let at = row_named(&rows, name);
    while b.selected_index() < at {
        b.select_next();
    }
    assert!(matches!(b.enter(), Ok(Activation::Descended)), "entering {name}");
}

fn row_named(rows: &[deck_pi::browser::Row], name: &str) -> usize {
    rows.iter()
        .position(|r| match r {
            deck_pi::browser::Row::Folder { name: n, .. } => n == name,
            deck_pi::browser::Row::File { name: n, .. } => n == name,
        })
        .unwrap_or_else(|| panic!("{name} is not listed"))
}

#[test]
fn a_tap_stops_at_a_folder_boundary_rather_than_wrapping() {
    // Issue #12 falls out of this: `None` means the caller does nothing,
    // which is stopping — the same answer `decisions.md` already gives for a
    // track reaching its end, "nothing starts on its own".
    let s = stick("loaded-boundary");
    let mut l = Loaded::nothing();
    load(&mut l, &s.dir.join("A/3_three.wav"));
    assert_eq!(l.neighbour(Step::Next).expect("read"), None, "past the last");
    assert_eq!(
        l.neighbour(Step::Previous).expect("read"),
        Some(s.dir.join("A/2_two.wav"))
    );
}

#[test]
fn a_tap_with_nothing_loaded_does_nothing() {
    // A deck that has not loaded anything is an ordinary state — it is how
    // the deck starts. The type can hold it, so there is no sentinel path to
    // step from.
    let l = Loaded::nothing();
    assert_eq!(l.neighbour(Step::Next).expect("read"), None);
    assert!(l.path().is_none());
}

#[test]
fn a_cue_belongs_to_the_playing_track_even_while_the_browser_is_elsewhere() {
    // **The defect this module exists to prevent**, written as the scenario:
    //
    //   1. play A/1_one.wav
    //   2. browse to B while it plays
    //   3. press CUE to mark a spot in the track you can hear
    //
    // Keyed off the browser's selection, step 3 writes against a file in B —
    // atomically, returning Ok, with nothing to notice. It surfaces weeks
    // later as a cue that went back to the start of its own track, beside a
    // cue on a track nobody set one on.
    let s = stick("loaded-cue");
    let state = Scratch::new("loaded-cue-state");
    let mut store = CueStore::load(&state.dir, "1A2B-3C4D", &s.dir).expect("load");

    let mut l = Loaded::nothing();
    load(&mut l, &s.dir.join("A/1_one.wav"));

    let mut b = Browser::open(&s.dir).expect("opens");
    descend_into(&mut b, "B");

    l.set_cue(&mut store, 90_000).expect("set");

    // The playing track has it.
    assert_eq!(l.cue(&store).expect("get"), 90_000);
    assert_eq!(
        store.get(&s.dir.join("A/1_one.wav")).expect("get"),
        90_000
    );
    // And nothing in the folder the browser wandered into does.
    for n in ["x_other.wav", "y_other.wav"] {
        assert_eq!(
            store.get(&s.dir.join("B").join(n)).expect("get"),
            0,
            "{n} must not have acquired a cue"
        );
    }
}

#[test]
fn setting_a_cue_with_nothing_loaded_writes_nothing_anywhere() {
    // The other half: there is no track for the cue to belong to, so there is
    // no file to guess at.
    let s = stick("loaded-nocue");
    let state = Scratch::new("loaded-nocue-state");
    let mut store = CueStore::load(&state.dir, "1A2B-3C4D", &s.dir).expect("load");
    let l = Loaded::nothing();
    l.set_cue(&mut store, 12_345).expect("set");
    assert_eq!(store.len(), 0, "nothing may be written");
    assert_eq!(l.cue(&store).expect("get"), 0);
}
