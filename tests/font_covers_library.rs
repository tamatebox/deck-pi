//! Does the panel font actually contain the library on the stick?
//!
//! Not a unit test — an audit against real material, run with `--ignored`:
//!
//! ```sh
//! DECK_PI_MUSIC_DIR=/media/stick/Music cargo test --release \
//!     --test font_covers_library -- --ignored --nocapture
//! ```
//!
//! **Why this is worth a file of its own.** `src/display/paint.rs` folds what
//! it can to ASCII and substitutes `?` for the rest, counting both, so a
//! missing glyph can no longer blank a row — but a panel spelling a name
//! differently from the filesystem is still wrong, and the module's own tests
//! can only check the dozen names someone thought to type. The library is
//! 1835 files. The question "would every one of them draw as itself" is about
//! the stick, so it is asked of the stick.
//!
//! It fails on a substitution and only reports a fold, because the two are
//! different sizes of wrong: `Beyonce` for `Beyoncé` is readable, `?瀬元彦`
//! for `濱瀬元彦` is not a name.
//!
//! It walks names, not audio: no file is opened and nothing is decoded, so it
//! runs in well under a second and is safe against a read-only mount.

use deck_pi::display::paint::{Drawn, Face};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        }
        out.push(p);
    }
}

#[test]
#[ignore]
fn every_name_on_the_stick_draws_as_itself() {
    let root = PathBuf::from(
        std::env::var("DECK_PI_MUSIC_DIR").expect("set DECK_PI_MUSIC_DIR, e.g. /media/stick/Music"),
    );
    let mut paths = Vec::new();
    walk(&root, &mut paths);
    assert!(!paths.is_empty(), "nothing under {}", root.display());

    let mut failed = false;
    for face in [Face::Px12, Face::Px16] {
        // Keyed by the character, so the report is "this glyph is missing and
        // here is what it costs", rather than one line per file.
        let mut absent: BTreeMap<char, Vec<String>> = BTreeMap::new();
        for p in &paths {
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                panic!("{} is not UTF-8, which the browser cannot show either", p.display());
            };
            for c in face.missing(name) {
                absent.entry(c).or_default().push(name.to_owned());
            }
        }

        let mut folds: BTreeMap<char, (String, usize)> = BTreeMap::new();
        for p in &paths {
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
            for c in name.chars() {
                if let Drawn::Folded(f) = face.drawn_as(c) {
                    let e = folds.entry(c).or_insert((f, 0));
                    e.1 += 1;
                }
            }
        }
        let affected = paths
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
            .filter(|n| !face.missing(n).is_empty())
            .count();
        println!(
            "\n{face:?}: {} names. {} characters folded to ASCII, {affected} name(s) left with a \
             character that cannot be folded either.",
            paths.len(),
            folds.len()
        );
        for (c, (f, n)) in &folds {
            println!("  fold  U+{:04X} {c:?} -> {f:?} — {n} occurrence(s)", *c as u32);
        }
        for (c, names) in &absent {
            println!(
                "  U+{:04X} {c:?} — {} name(s), e.g. {}",
                *c as u32,
                names.len(),
                names[0]
            );
        }
        if absent.is_empty() {
            println!("  every character present");
        } else {
            failed = true;
        }
    }
    assert!(!failed, "see the list above; each one draws as `?` on the panel");
}
