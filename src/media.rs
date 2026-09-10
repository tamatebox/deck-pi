//! Media watch: is there a stick, can it be read, and what is it called.
//!
//! `architecture.md` gives this module three states — "nothing mounted,
//! mounted but unreadable, browsable" — plus the volume UUID the cue store
//! keys on. It owns one path, the fixed mount point, and never discovers
//! where anything landed: `decisions.md` makes path uniqueness the thing that
//! enforces "one stick", so a second volume finds the path occupied and
//! simply does not mount.
//!
//! # Existence is not the test, and the documents assumed it was
//!
//! `decisions.md` says `--automount=no` means "the path appears only when
//! something is mounted, so 'No USB' versus the browser is one `stat`". The
//! first half is systemd's behaviour; the conclusion depends on systemd also
//! *removing* the directory on unmount, and if it does not — a failed unit, a
//! hard removal, a crash, or anyone's stray `mkdir` — the path lingers.
//!
//! An existence test then reports a stick that is not there, and the browser
//! opens an empty folder. That is a **third** way to confuse "no stick" with
//! "stick I cannot read", in a design whose stated principle is to tell those
//! two apart.
//!
//! Measured in a container: with a tmpfs mounted at the path, the path's
//! `st_dev` is 79 and its parent's is 76; unmounted but with the directory
//! left behind, both read 76. So comparing the two settles it, for the cost of
//! one extra `stat`, and the assumption about systemd's cleanup is removed
//! rather than relied on.
//!
//! Two limits of that technique, neither reachable here: the root of a
//! filesystem is its own parent and so never reads as a mount point, and a
//! bind mount of the *same* filesystem shares `st_dev`. The mount point is a
//! fixed path well below `/` and the medium is a block device, so neither
//! applies — but they are why this is not a general `mountpoint(1)`.

use std::fmt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Where udev is told to put the stick. A placeholder in `implementation.md`
/// and still a placeholder here — chosen in the udev rule, not in the code,
/// which is why it is a constructor argument as well as a default.
pub const MOUNT_POINT: &str = "/media/stick";

/// Where udev's symlink farm names volumes by UUID.
///
/// `decisions.md` requires the UUID to come from here rather than from inside
/// the mount, because neither filesystem exposes its serial through a file
/// API — exFAT keeps it in the boot sector, HFS+ in the volume header.
const BY_UUID: &str = "/dev/disk/by-uuid";

/// The three states, and the one piece of identity that comes with the third.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Medium {
    /// Nothing is mounted at the fixed path. "No USB".
    Absent,
    /// Something is mounted and cannot be read. A formatting mismatch, wrong
    /// ownership from the mount options, or the medium going away mid-read.
    ///
    /// `decisions.md`: showing only `No USB` for this "makes a formatting
    /// mismatch look like broken hardware", so it is a separate state with a
    /// reason attached rather than a variant of `Absent`.
    Unreadable { reason: String },
    /// Mounted and readable.
    Browsable {
        /// The volume UUID, as an **opaque string**. exFAT gives
        /// `XXXX-XXXX`, a 32-bit volume serial; HFS+ gives a standard
        /// 36-character UUID, because `libblkid` does not publish the volume
        /// header's own id — `hfs_set_uuid` MD5s the 8-byte `finder_info.id`
        /// behind a fixed seed and stamps version 3, so what appears in
        /// `/dev/disk/by-uuid/` is `xxxxxxxx-xxxx-3xxx-[89ab]xxx-...` and the
        /// raw id never appears at all. Two unrelated shapes, which is why
        /// `decisions.md` says to store it and never parse it.
        ///
        /// `None` is a real state rather than an error: the stick browses and
        /// plays, but the cue store has nothing to key on, so cues cannot be
        /// persisted for it. That is a degradation worth surfacing, not a
        /// reason to refuse the medium.
        uuid: Option<String>,
    },
}

impl Medium {
    pub fn is_browsable(&self) -> bool {
        matches!(self, Medium::Browsable { .. })
    }

    /// The UUID, if there is one. `None` both when nothing is mounted and
    /// when the volume has no entry in the symlink farm.
    pub fn uuid(&self) -> Option<&str> {
        match self {
            Medium::Browsable { uuid } => uuid.as_deref(),
            _ => None,
        }
    }
}

impl fmt::Display for Medium {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Medium::Absent => f.write_str("no medium"),
            Medium::Unreadable { reason } => write!(f, "unreadable: {}", reason),
            Medium::Browsable { uuid: Some(u) } => write!(f, "browsable, volume {}", u),
            Medium::Browsable { uuid: None } => {
                f.write_str("browsable, no volume UUID — cues cannot be saved")
            }
        }
    }
}

/// Watches one path.
///
/// Polled rather than event-driven, and deliberately: the check is two
/// `stat` calls, the UI already redraws on a 30-50 ms cadence with nothing
/// under a deadline, and the alternatives each carry a way to be silently
/// wrong. `inotify` on the parent needs the parent to exist and reports
/// directory creation rather than mounting — which is the very distinction
/// above. `/proc/self/mountinfo` is the canonical watch and is Linux-only,
/// where the `st_dev` comparison is not.
pub struct MediaWatch {
    mount_point: PathBuf,
    state: Medium,
}

impl MediaWatch {
    /// Starts in [`Medium::Absent`] without touching the filesystem, so
    /// construction cannot fail and the first [`MediaWatch::poll`] is what
    /// reports whatever is already there.
    pub fn new(mount_point: impl Into<PathBuf>) -> MediaWatch {
        MediaWatch {
            mount_point: mount_point.into(),
            state: Medium::Absent,
        }
    }

    pub fn at_default_path() -> MediaWatch {
        MediaWatch::new(MOUNT_POINT)
    }

    pub fn mount_point(&self) -> &Path {
        &self.mount_point
    }

    pub fn state(&self) -> &Medium {
        &self.state
    }

    /// Re-examines the path. Returns the new state **only when it changed**,
    /// which is what the display's "update on state change" rule wants.
    ///
    /// The expensive halves — a `read_dir` and the symlink-farm scan — run
    /// only when something is mounted, so a poll with no medium present is
    /// two `stat` calls and nothing else.
    pub fn poll(&mut self) -> Option<&Medium> {
        let next = examine(&self.mount_point);
        if next == self.state {
            return None;
        }
        self.state = next;
        Some(&self.state)
    }
}

/// One examination of a path, with no state of its own.
///
/// Separated from [`MediaWatch`] so the states can be produced in a test
/// against real directories without driving a watcher.
pub fn examine(mount_point: &Path) -> Medium {
    match is_mount_point(mount_point) {
        Ok(false) | Err(_) => Medium::Absent,
        Ok(true) => match std::fs::read_dir(mount_point) {
            // The listing itself is not consumed here. Whether individual
            // entries read is the browser's problem, and it reports that per
            // row; what this settles is whether the volume can be opened at
            // all, which is the state the UI shows instead of a folder.
            Ok(_) => Medium::Browsable {
                uuid: volume_uuid(mount_point),
            },
            Err(e) => Medium::Unreadable {
                reason: e.to_string(),
            },
        },
    }
}

/// Whether something is mounted at `path`, by comparing its device with its
/// parent's.
///
/// A missing path is `Ok(false)` rather than an error: "not there" is a state
/// this design expects, not a failure.
pub fn is_mount_point(path: &Path) -> std::io::Result<bool> {
    let here = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if !here.is_dir() {
        // A file at the mount point is not a medium, and treating it as one
        // would hand the browser something `read_dir` refuses.
        return Ok(false);
    }
    let Some(parent) = path.parent() else {
        return Ok(false);
    };
    let above = std::fs::metadata(parent)?;
    Ok(here.dev() != above.dev())
}

/// Finds the volume UUID for whatever is mounted at `path`.
///
/// Walks `/dev/disk/by-uuid` and matches on **device number** rather than on
/// a name: `st_dev` of a file on a block-backed filesystem is the block
/// device's `st_rdev`, so the two compare directly and no path or partition
/// name has to be parsed or guessed.
///
/// `None` covers every way this can come up empty, and all of them are
/// legitimate: the farm does not exist (not Linux, or no udev), the volume is
/// not block-backed (a tmpfs has an anonymous device with no entry), or the
/// filesystem carries no UUID at all. No subprocess — `blkid` is what
/// `decisions.md` names, and this reads the same information udev already
/// published.
pub fn volume_uuid(path: &Path) -> Option<String> {
    let want = std::fs::metadata(path).ok()?.dev();
    for entry in std::fs::read_dir(BY_UUID).ok()?.flatten() {
        // `metadata` follows the symlink, which is the point — the target is
        // the block device node and carries the `rdev` to match.
        let Ok(target) = std::fs::metadata(entry.path()) else {
            continue;
        };
        if target.rdev() == want {
            return Some(entry.file_name().to_string_lossy().into_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_path_is_absent_rather_than_an_error() {
        // The dominant state on a deck with no stick in it, so it must not be
        // exceptional.
        let missing = Path::new("/nonexistent-deck-pi-mount-point");
        assert!(!is_mount_point(missing).expect("not an error"));
        assert_eq!(examine(missing), Medium::Absent);
    }

    #[test]
    fn a_directory_that_is_not_a_mount_point_reads_as_absent() {
        // The case an existence test gets wrong: the mount point directory
        // left behind after an unmount, or created by hand. `stat`-for-
        // existence would call this a stick and hand the browser an empty
        // folder.
        let dir = std::env::temp_dir().join(format!("deck-pi-media-{}", std::process::id()));
        let inner = dir.join("stick");
        std::fs::create_dir_all(&inner).expect("create");

        assert!(!is_mount_point(&inner).expect("not an error"));
        assert_eq!(
            examine(&inner),
            Medium::Absent,
            "an ordinary directory must not read as a medium"
        );
        assert!(inner.exists(), "and it really is there — that is the point");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_at_the_mount_point_is_not_a_medium() {
        let dir = std::env::temp_dir().join(format!("deck-pi-media-f-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create");
        let file = dir.join("stick");
        std::fs::write(&file, b"not a directory").expect("write");

        assert!(!is_mount_point(&file).expect("not an error"));
        assert_eq!(examine(&file), Medium::Absent);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_watch_reports_only_changes() {
        // What drives the redraw. Reporting every poll would redraw the panel
        // at the poll rate, and `architecture.md` asks for state changes.
        let mut w = MediaWatch::new("/nonexistent-deck-pi-mount-point");
        assert_eq!(w.state(), &Medium::Absent);
        // Already Absent at construction, so the first poll of an absent
        // medium is not a change.
        assert!(w.poll().is_none());
        assert!(w.poll().is_none());
    }

    #[test]
    fn no_uuid_is_a_browsable_state_not_a_failure() {
        // A volume with no entry in the symlink farm still browses and plays;
        // what it loses is the cue store's key. Encoded in the type rather
        // than left to a caller to remember.
        let with = Medium::Browsable {
            uuid: Some("1234-ABCD".to_string()),
        };
        let without = Medium::Browsable { uuid: None };
        assert!(with.is_browsable() && without.is_browsable());
        assert_eq!(with.uuid(), Some("1234-ABCD"));
        assert_eq!(without.uuid(), None);
        assert!(
            without.to_string().contains("cues cannot be saved"),
            "the degradation must be sayable: {}",
            without
        );
    }

    #[test]
    fn the_uuid_is_opaque_and_both_filesystems_shapes_survive_it() {
        // `decisions.md` says store it as an opaque string rather than
        // parsing it, so this only checks that neither shape is altered on
        // the way through.
        //
        // The HFS+ value is a real one rather than a plausible-looking one:
        // it is what `libblkid`'s `hfs_set_uuid` produces from the
        // `finder_info.id` bytes `a1b2c3d4e5f60718` — MD5 behind the fixed
        // seed, then version 3 and the RFC 4122 variant stamped in. Those
        // bytes used to sit here *as* the UUID, which was the hash's input
        // mistaken for its output; recomputing from them keeps the example
        // reproducible and shows what the old constant actually was.
        for raw in ["1A2B-3C4D", "b03e987f-7127-39da-a816-1167109da731"] {
            let m = Medium::Browsable {
                uuid: Some(raw.to_string()),
            };
            assert_eq!(m.uuid(), Some(raw));
        }
    }

    #[test]
    fn an_unreadable_medium_keeps_its_reason() {
        let m = Medium::Unreadable {
            reason: "Permission denied (os error 13)".to_string(),
        };
        assert!(!m.is_browsable());
        assert!(
            m.to_string().contains("Permission denied"),
            "the reason is the whole difference from Absent: {}",
            m
        );
    }
}
