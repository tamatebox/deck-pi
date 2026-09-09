//! The browser against a real folder tree on disk.
//!
//! The unit tests in `src/browser.rs` cover the ordering. What needs a
//! filesystem is everything the design documents actually promise about this
//! module: that a refusal is *shown* rather than hidden, that a Mac volume's
//! own droppings are not, that headers are read once per rendered row and
//! never on a mere selection change, and that a medium going away is a
//! `Result` rather than a panic.

mod fixtures;

use fixtures::{signal, Bits, Kind, Scratch};

use deck_pi::browser::{Activation, BrowseError, Browser, Row, Verdict};
use deck_pi::file::Reject;

/// A folder tree with one of everything the browser has to cope with.
///
/// Deliberately built out of the same hand-written writers the null test
/// uses, so the fixtures are not trusting another implementation of the thing
/// under test.
fn tree(tag: &str) -> Scratch {
    let s = Scratch::new(tag);
    let root = &s.dir;

    let put = |dir: &std::path::Path, name: &str, kind: Kind, bits: Bits, rate: u32, ch: usize| {
        let samples = signal(bits, ch, 512);
        let bytes = fixtures::build(kind, &samples, bits, rate, ch);
        fixtures::write(dir, name, kind, &bytes);
    };

    // Files at the root, deliberately created in an order that is not the
    // order they should come out in.
    put(root, "10_ten", Kind::Wav, Bits::S24, 44_100, 2);
    put(root, "2_two", Kind::Wav, Bits::S16, 48_000, 2);
    put(root, "1_one", Kind::Aiff, Bits::S24, 96_000, 2);

    // Two refusals, each a different one of the four reasons.
    put(root, "REFUSED_rate", Kind::Wav, Bits::S16, 32_000, 2);
    put(root, "REFUSED_channels", Kind::Wav, Bits::S24, 48_000, 5);

    // Something libsndfile cannot open at all.
    std::fs::write(root.join("broken.wav"), b"this is not a RIFF file").unwrap();

    // What a Mac leaves behind. `._1_one.aiff` is the one that matters: it
    // would sit directly beside the file it shadows.
    std::fs::write(root.join(".DS_Store"), b"\0\0\0").unwrap();
    std::fs::write(root.join("._1_one.aiff"), b"AppleDouble").unwrap();
    std::fs::create_dir_all(root.join(".Spotlight-V100")).unwrap();
    std::fs::create_dir_all(root.join(".fseventsd")).unwrap();

    // Folders, so "folders first" and descending can both be checked. Named
    // so that alphabetically they would *not* all precede the files.
    std::fs::create_dir_all(root.join("zz_ambient")).unwrap();
    put(&root.join("zz_ambient"), "drone", Kind::Rf64, Bits::S24, 96_000, 2);
    std::fs::create_dir_all(root.join("aa_dance")).unwrap();
    put(&root.join("aa_dance"), "kick", Kind::Wav, Bits::S16, 44_100, 2);

    s
}

fn names(rows: &[Row]) -> Vec<&str> {
    rows.iter().map(|r| r.name()).collect()
}

#[test]
fn folders_come_first_and_files_sort_numerically() {
    let s = tree("browser-order");
    let mut b = Browser::open(&s.dir).expect("opens");

    // Every row at once, so the whole order is visible.
    let rows = b.view(64);
    assert_eq!(
        names(&rows),
        vec![
            // Folders, natural order, before anything else.
            "aa_dance",
            "zz_ambient",
            // Then files. `10_ten` after `2_two`, which plain lexicographic
            // order would have got wrong.
            "1_one.aiff",
            "2_two.wav",
            "10_ten.wav",
            "broken.wav",
            "REFUSED_channels.wav",
            "REFUSED_rate.wav",
        ]
    );
}

#[test]
fn a_mac_volumes_own_files_are_not_shown() {
    // Not cosmetic on this medium: `._1_one.aiff` would appear beside
    // `1_one.aiff` and read as a broken duplicate of it, because libsndfile
    // cannot open an AppleDouble header.
    let s = tree("browser-dotfiles");
    let mut b = Browser::open(&s.dir).expect("opens");
    let rows = b.view(64);
    let shown = names(&rows);
    for hidden in [".DS_Store", "._1_one.aiff", ".Spotlight-V100", ".fseventsd"] {
        assert!(
            !shown.contains(&hidden),
            "{} should be hidden, got {:?}",
            hidden,
            shown
        );
    }
    // And the file it shadows is still there.
    assert!(shown.contains(&"1_one.aiff"));
}

#[test]
fn a_refused_file_is_shown_with_which_of_the_four_reasons() {
    // `decisions.md`: say *why*, not just *that*. Hiding refusals would make
    // a folder full of FLAC look empty.
    let s = tree("browser-refusals");
    let mut b = Browser::open(&s.dir).expect("opens");
    let rows = b.view(64);

    let verdict = |name: &str| {
        rows.iter()
            .find_map(|r| match r {
                Row::File { name: n, verdict, .. } if n == name => Some(verdict.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{} not rendered", name))
    };

    match verdict("REFUSED_rate.wav") {
        Verdict::Refused(Reject::Rate { found }) => assert_eq!(found, 32_000),
        other => panic!("expected a rate refusal, got {:?}", other),
    }
    match verdict("REFUSED_channels.wav") {
        Verdict::Refused(Reject::Channels { found }) => assert_eq!(found, 5),
        other => panic!("expected a channel refusal, got {:?}", other),
    }
    // Unopenable is a different state from refused, and the display has to be
    // able to tell them apart — one says "this format is out of scope", the
    // other "this file is damaged or the stick just went away".
    match verdict("broken.wav") {
        Verdict::Unreadable(msg) => assert!(!msg.is_empty(), "the reason must survive"),
        other => panic!("expected unreadable, got {:?}", other),
    }
    match verdict("1_one.aiff") {
        Verdict::Plays(info) => {
            assert_eq!(info.rate, 96_000);
            assert_eq!(info.channels, 2);
        }
        other => panic!("expected playable, got {:?}", other),
    }
}

#[test]
fn scrolling_reads_one_header_per_new_row_and_selection_alone_reads_nothing() {
    // The property `decisions.md` requires: "header reads are driven by
    // renders, not by encoder events". A fast spin must cost one open per
    // redraw, not one per detent.
    let s = tree("browser-reads");
    let mut b = Browser::open(&s.dir).expect("opens");
    assert_eq!(b.headers_read(), 0, "opening a folder reads no headers");

    // Two rows, which is the smallest candidate panel's browsable count.
    let rows = b.view(2);
    assert_eq!(names(&rows), vec!["aa_dance", "zz_ambient"]);
    assert_eq!(
        b.headers_read(),
        0,
        "two folders were rendered, and a folder has no header"
    );

    // Spin the encoder six detents with no render in between — what a fast
    // scroll looks like when redraws are coalesced to 30-50 ms.
    for _ in 0..6 {
        b.select_next();
    }
    assert_eq!(
        b.headers_read(),
        0,
        "six detents with no render must read nothing"
    );

    // Now render once. Only the two rows actually visible are read.
    let rows = b.view(2);
    assert_eq!(names(&rows), vec!["broken.wav", "REFUSED_channels.wav"]);
    assert_eq!(
        b.headers_read(),
        2,
        "one render of two rows must read exactly two headers"
    );

    // Rendering the same rows again is free — the cache is keyed by path.
    b.view(2);
    b.view(2);
    assert_eq!(b.headers_read(), 2, "a re-render must not re-read");

    // And stepping back onto a row already read costs nothing either.
    b.select_prev();
    b.view(2);
    assert_eq!(b.headers_read(), 2);
}

#[test]
fn the_viewport_scrolls_by_the_minimum_rather_than_re_centring() {
    // With two rows on the panel, re-centring on every step would move both
    // lines every detent. Scrolling by one keeps the row you were looking at
    // where it was.
    let s = tree("browser-scroll");
    let mut b = Browser::open(&s.dir).expect("opens");

    assert_eq!(names(&b.view(3)), vec!["aa_dance", "zz_ambient", "1_one.aiff"]);
    b.select_next(); // zz_ambient — already visible, so nothing moves
    assert_eq!(names(&b.view(3)), vec!["aa_dance", "zz_ambient", "1_one.aiff"]);
    b.select_next(); // 1_one — still visible
    assert_eq!(names(&b.view(3)), vec!["aa_dance", "zz_ambient", "1_one.aiff"]);
    b.select_next(); // 2_two — off the bottom, so scroll by exactly one
    assert_eq!(
        names(&b.view(3)),
        vec!["zz_ambient", "1_one.aiff", "2_two.wav"]
    );
    assert_eq!(b.first_visible(), 1);

    // At the end of the list the viewport stops rather than leaving blanks.
    for _ in 0..20 {
        b.select_next();
    }
    let rows = b.view(3);
    assert_eq!(rows.len(), 3, "no blank rows at the bottom");
    assert_eq!(
        names(&rows),
        vec!["broken.wav", "REFUSED_channels.wav", "REFUSED_rate.wav"]
    );
    assert!(rows[2].selected(), "the last row is the selected one");
}

#[test]
fn the_selection_does_not_wrap_at_either_end() {
    // A detented encoder gives no feedback that the list ended, and wrapping
    // from the end of a long folder to its start is indistinguishable from a
    // mis-scroll on a two-row panel.
    let s = tree("browser-bounds");
    let mut b = Browser::open(&s.dir).expect("opens");

    b.select_prev();
    b.select_prev();
    assert_eq!(b.selected_index(), 0, "up at the top stays at the top");

    for _ in 0..100 {
        b.select_next();
    }
    assert_eq!(b.selected_index(), b.len() - 1, "down at the bottom stays");
}

#[test]
fn enter_descends_and_back_returns_to_the_folder_it_left() {
    let s = tree("browser-navigate");
    let mut b = Browser::open(&s.dir).expect("opens");
    assert!(b.at_root());

    // Select the *second* folder, so returning to index 0 would be wrong in a
    // way the test can see.
    b.select_next();
    assert_eq!(names(&b.view(64))[b.selected_index()], "zz_ambient");

    match b.enter().expect("enter") {
        Activation::Descended => {}
        other => panic!("expected to descend, got {:?}", other),
    }
    assert!(!b.at_root());
    assert_eq!(b.path().file_name().unwrap(), "zz_ambient");
    assert_eq!(names(&b.view(64)), vec!["drone.wav"]);

    assert!(b.back().expect("back"), "back from a subfolder succeeds");
    assert!(b.at_root());
    assert_eq!(
        names(&b.view(64))[b.selected_index()],
        "zz_ambient",
        "BACK must land on the folder just left, not on the top of the list"
    );
}

#[test]
fn back_at_the_root_goes_nowhere_and_the_medium_is_the_top_of_the_tree() {
    // The mount point is the root of the browsable world; the filesystem
    // above it is not the deck's business.
    let s = tree("browser-root");
    let mut b = Browser::open(&s.dir).expect("opens");
    assert!(!b.back().expect("back at root is not an error"));
    assert!(b.at_root());
    assert_eq!(b.path(), s.dir.as_path());
}

#[test]
fn enter_on_a_playable_file_hands_over_the_header_and_a_refusal_never_reaches_play() {
    let s = tree("browser-enter");
    let mut b = Browser::open(&s.dir).expect("opens");

    // Walk to `1_one.aiff` — index 2, after the two folders.
    b.select_next();
    b.select_next();
    match b.enter().expect("enter") {
        Activation::Play(info) => {
            assert_eq!(info.rate, 96_000);
            assert_eq!(info.path.file_name().unwrap(), "1_one.aiff");
        }
        other => panic!("expected to play, got {:?}", other),
    }
    // And the browser has not moved: activating a file is not navigation.
    assert!(b.at_root());

    // The last row is a refusal. ENTER must report it rather than hand a
    // path to the engine.
    for _ in 0..100 {
        b.select_next();
    }
    match b.enter().expect("enter") {
        Activation::Refused(Reject::Rate { found }) => assert_eq!(found, 32_000),
        other => panic!("expected a refusal, got {:?}", other),
    }
}

#[test]
fn an_empty_folder_renders_nothing_and_activates_nothing() {
    let s = Scratch::new("browser-empty");
    let mut b = Browser::open(&s.dir).expect("opens an empty medium");
    assert!(b.is_empty());
    assert!(b.view(8).is_empty());
    match b.enter().expect("enter") {
        Activation::Nothing => {}
        other => panic!("expected nothing under the selection, got {:?}", other),
    }
    assert!(!b.back().expect("back"));
}

#[test]
fn a_medium_that_goes_away_is_a_result_rather_than_a_panic() {
    // `decisions.md`: "the requirement amounts to do not `unwrap()`". The
    // dominant cause is the stick being pulled, which is a normal event in a
    // venue rather than an exceptional one.
    let s = tree("browser-removed");
    let mut b = Browser::open(&s.dir).expect("opens");
    b.view(64);

    std::fs::remove_dir_all(&s.dir).expect("simulate removal");

    match b.reload() {
        Err(BrowseError::Unreadable { path, reason }) => {
            assert_eq!(path, s.dir);
            assert!(!reason.is_empty(), "the reason must reach the UI");
        }
        other => panic!("expected an unreadable medium, got {:?}", other),
    }
    // The rows already read stay readable, so the UI can keep showing what it
    // had while media watch decides the medium is gone — the same shape as
    // the audio side surviving on what is already resident.
    assert!(!b.view(64).is_empty());
}

#[test]
fn a_file_that_vanishes_between_the_listing_and_the_render_reads_as_unreadable() {
    // The race the file layer's `Unreadable` variant documents: `read_dir`
    // saw it, and by the time the header is opened it is gone.
    let s = tree("browser-race");
    let mut b = Browser::open(&s.dir).expect("opens");
    std::fs::remove_file(s.dir.join("1_one.aiff")).expect("remove");

    let rows = b.view(64);
    let verdict = rows
        .iter()
        .find_map(|r| match r {
            Row::File { name, verdict, .. } if name == "1_one.aiff" => Some(verdict.clone()),
            _ => None,
        })
        .expect("the row is still listed");
    match verdict {
        Verdict::Unreadable(msg) => assert!(!msg.is_empty()),
        other => panic!("expected unreadable, got {:?}", other),
    }
}
