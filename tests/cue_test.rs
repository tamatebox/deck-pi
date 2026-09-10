//! The cue store against the transport, which is the seam it exists for.
//!
//! `src/cue.rs`'s own tests cover the format and the escaping. What needs
//! putting together is the round trip the deck actually performs: a volume
//! UUID from media watch, a cue restored into the transport on load, the CUE
//! button moving it, and the new point surviving a power cycle.

use std::path::{Path, PathBuf};

use deck_pi::cue::CueStore;
use deck_pi::transport::{State, Transport, RATE_PAUSED};

struct Dir(PathBuf);
impl Dir {
    fn new(tag: &str) -> Dir {
        let d =
            std::env::temp_dir().join(format!("deck-pi-cueint-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("create");
        Dir(d)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A UUID of the shape `/dev/disk/by-uuid` gives for HFS+ — **version 3**,
/// which is not cosmetic: `libblkid` derives it by MD5ing the volume's
/// 8-byte `finder_info.id` behind a fixed seed and stamping `uuid[6] = 0x30`,
/// so a version-4 value cannot come from an HFS+ volume. This one is
/// recomputed from real input bytes; see `src/media.rs`.
const HFSPLUS: &str = "b03e987f-7127-39da-a816-1167109da731";
/// And the shape exFAT gives, which is a different format entirely — hence
/// `decisions.md` storing it as an opaque string rather than parsing it.
const EXFAT: &str = "1A2B-3C4D";

const MOUNT: &str = "/media/stick";

#[test]
fn a_cue_set_on_the_deck_comes_back_after_a_power_cycle() {
    // The whole point of the store. Nothing else in the application
    // persists, so if this does not work there is no state at all.
    let state = Dir::new("cycle");
    let track = Path::new(MOUNT).join("album/piece.wav");

    // Load the track: restore whatever the store has, which is nothing yet.
    let mut store = CueStore::load(&state.0, HFSPLUS, Path::new(MOUNT)).expect("load");
    let t = Transport::new();
    t.set_cue_point(store.get(&track).expect("get"));
    assert_eq!(t.cue_point(), 0, "an unseen track cues at frame zero");

    // Pause somewhere and press CUE — Setting Cue, per the CDJ-350.
    t.pause();
    t.publish_position(90_000.0);
    t.cue_down();
    t.cue_up();
    assert_eq!(t.cue_point(), 90_000);

    // The application persists it. `set` writes through, so nothing has to
    // remember to flush before the wall switch.
    store.set(&track, t.cue_point()).expect("set");

    // Power cycle: everything in memory is gone. `Transport` holds only
    // atomics and has no `Drop`, so dropping it explicitly says nothing —
    // it is simply not used again below.
    drop(store);

    let store = CueStore::load(&state.0, HFSPLUS, Path::new(MOUNT)).expect("reload");
    let t = Transport::new();
    t.set_cue_point(store.get(&track).expect("get"));
    assert_eq!(t.cue_point(), 90_000, "the cue must survive the deck being switched off");
}

#[test]
fn two_sticks_with_the_same_relative_path_keep_separate_cues() {
    // The reason the key is volume UUID *plus* relative path rather than
    // path alone. Two sticks prepared from the same folder structure — which
    // is the normal way a second stick gets made — would otherwise share
    // every cue, and the collision would be silent.
    let state = Dir::new("two-sticks");
    let track = Path::new(MOUNT).join("set/opener.wav");

    let mut a = CueStore::load(&state.0, HFSPLUS, Path::new(MOUNT)).expect("load a");
    let mut b = CueStore::load(&state.0, EXFAT, Path::new(MOUNT)).expect("load b");
    a.set(&track, 111_000).expect("set a");
    b.set(&track, 222_000).expect("set b");

    let a = CueStore::load(&state.0, HFSPLUS, Path::new(MOUNT)).expect("reload a");
    let b = CueStore::load(&state.0, EXFAT, Path::new(MOUNT)).expect("reload b");
    assert_eq!(a.get(&track).expect("get"), 111_000);
    assert_eq!(b.get(&track).expect("get"), 222_000);

    // And they really are two files on the SD card, one per volume, so
    // dropping a stick you no longer use is deleting one file.
    let mut files: Vec<_> = std::fs::read_dir(&state.0)
        .expect("read_dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(
        files,
        vec![
            format!("{}.cues", EXFAT),
            format!("{}.cues", HFSPLUS)
        ]
    );
}

#[test]
fn tracks_on_one_stick_are_independent_and_one_cue_each() {
    // "One cue point per track, and setting a new one cancels the old" is
    // the CDJ-350 rule the transport implements; the store has to agree.
    let state = Dir::new("per-track");
    let first = Path::new(MOUNT).join("a.wav");
    let second = Path::new(MOUNT).join("b.wav");

    let mut store = CueStore::load(&state.0, HFSPLUS, Path::new(MOUNT)).expect("load");
    store.set(&first, 1_000).expect("set");
    store.set(&second, 2_000).expect("set");
    store.set(&first, 3_000).expect("replace the first");

    assert_eq!(store.len(), 2, "two tracks, two cues");
    assert_eq!(store.get(&first).expect("get"), 3_000);
    assert_eq!(store.get(&second).expect("get"), 2_000);
}

#[test]
fn back_cue_returns_to_the_restored_point_rather_than_to_zero() {
    // The behaviour a restored cue is *for*: pressing CUE while playing
    // returns to the point and pauses. If restoring did not reach the
    // transport this would jump to the start of the track instead, which is
    // a silent, plausible-looking wrong answer.
    let state = Dir::new("backcue");
    let track = Path::new(MOUNT).join("long.wav");

    let mut store = CueStore::load(&state.0, HFSPLUS, Path::new(MOUNT)).expect("load");
    store.set(&track, 1_234_567).expect("set");

    let t = Transport::new();
    t.set_cue_point(store.get(&track).expect("get"));
    t.play();
    t.publish_position(2_000_000.0);

    t.cue_down(); // Back Cue: return to the point and pause
    t.cue_up();
    assert_eq!(t.take_seek(), Some(1_234_567));
    // `is_silent` is the FF/REW silent-seek flag, not "paused" — a
    // confusable pair, and the first version of this test used the wrong
    // one. Pausing is the rate going to zero.
    assert_eq!(t.state(), State::Paused, "Back Cue pauses; it does not resume");
    assert_eq!(t.rate(), RATE_PAUSED);
}

#[test]
fn a_stick_remounted_somewhere_else_keeps_its_cues() {
    // The key is the path *relative* to the mount point, and the mount point
    // is still a placeholder — see issue #11. A cue must not be lost because
    // that placeholder changed.
    let state = Dir::new("remount");
    let mut store = CueStore::load(&state.0, HFSPLUS, Path::new("/media/stick")).expect("load");
    store
        .set(Path::new("/media/stick/deep/inside/track.wav"), 4_410_000)
        .expect("set");

    let moved = CueStore::load(&state.0, HFSPLUS, Path::new("/run/media/deck/usb0")).expect("load");
    assert_eq!(
        moved
            .get(Path::new("/run/media/deck/usb0/deep/inside/track.wav"))
            .expect("get"),
        4_410_000
    );
}
