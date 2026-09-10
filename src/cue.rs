//! The cue store — the only state this application persists.
//!
//! `architecture.md`: cues are punched on the deck and stored **on the Pi's SD
//! card**, keyed by the volume's UUID plus the file's relative path. The stick
//! is content and is mounted read-only; the Pi owns what it created. One
//! consequence to accept, recorded there: cues do not travel between two
//! decks, because they are two machines.
//!
//! One cue point per track, in frames, and **frame zero until set** — the
//! CDJ-350 behaviour `hardware.md` transcribes. A cue deliberately set at
//! frame zero is therefore indistinguishable from an unset one, which is fine
//! because they behave identically; it also means zeros need not be stored.
//!
//! # Why this is a hand-written format
//!
//! There is no database — `decisions.md` makes the folder tree the index —
//! and no serialisation dependency. The whole need is a map from a path to a
//! `u64`, which is smaller than the crate that would read it. It is also
//! worth being able to `cat` on an appliance you SSH into, so the format is
//! line-based and leaves Japanese filenames legible.
//!
//! # The trap that decides the format: a filename can contain a newline
//!
//! HFS+ permits almost any Unicode in a name, `:` and `/` excepted, and
//! **that includes U+000A**. Verified on both a Mac and a Linux container:
//! `printf 'two\nlines.wav'` is a legal filename on each. exFAT's spec forbids
//! control characters, so it cannot happen there — but HFS+ is in scope, and
//! `decisions.md` records that the drive in question *is* HFS+.
//!
//! So a naive `frame<TAB>path` line format is not merely untidy on such a
//! name, it is **wrong**: the entry splits across two lines and the second
//! half parses as garbage or, worse, as another entry. Paths are therefore
//! escaped on the way out and unescaped on the way in.
//!
//! # And the key is bytes, not a `String`
//!
//! A filename is bytes on Linux. Converting lossily to `String` maps every
//! invalid sequence to the same replacement character, so two different
//! broken names would **collide on one cue** — silently, and only on the
//! medium that produced them. The key is the raw bytes; the escaping is what
//! lets them live in a text file.

use std::collections::HashMap;
use std::fmt;
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// One line per cue, and a header so a stray file is recognisable.
const HEADER: &str = "# deck-pi cues v1 — frame<TAB>path, path backslash-escaped";

#[derive(Debug)]
pub enum CueError {
    /// The volume UUID cannot be used as a filename.
    ///
    /// Refused rather than sanitised: mangling two different volume names
    /// into one filename would merge their cues, and that is worse than not
    /// saving at all.
    BadVolume { volume: String, reason: &'static str },
    /// A path that is not under the medium. The key is the path *relative to
    /// the mount point*, so an absolute path from somewhere else has no key.
    OutsideMedium { path: PathBuf },
    Io { path: PathBuf, source: std::io::Error },
}

impl fmt::Display for CueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CueError::BadVolume { volume, reason } => {
                write!(f, "volume {:?} cannot name a cue file: {}", volume, reason)
            }
            CueError::OutsideMedium { path } => {
                write!(f, "{} is not on the medium", path.display())
            }
            CueError::Io { path, source } => write!(f, "{}: {}", path.display(), source),
        }
    }
}

impl std::error::Error for CueError {}

/// Where the deck keeps its own state.
///
/// `$XDG_STATE_HOME/deck-pi`, else `$HOME/.local/state/deck-pi`. State rather
/// than config or cache: the deck created it and cannot regenerate it, which
/// is exactly what that directory is for. Not `/var/lib`, because the audio
/// process runs as a plain user — `decisions.md` records that it needs only
/// `rtprio` and `memlock` and no root.
pub fn default_state_dir() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_STATE_HOME") {
        if !x.is_empty() {
            return Some(PathBuf::from(x).join("deck-pi"));
        }
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/deck-pi"))
}

/// The cues for one volume, held in memory and written through on every
/// change.
pub struct CueStore {
    file: PathBuf,
    mount_point: PathBuf,
    /// Keyed by the path relative to the mount point, as raw bytes.
    cues: HashMap<Vec<u8>, u64>,
}

impl CueStore {
    /// Loads the cues for one volume, or starts empty if there are none.
    ///
    /// A missing file is not an error — it is the first time this stick has
    /// been used. A *corrupt* file is also not an error: unparsable lines are
    /// skipped, because refusing to browse because one line of a cue file is
    /// mangled would be a worse failure than losing that cue.
    pub fn load(
        state_dir: &Path,
        volume: &str,
        mount_point: &Path,
    ) -> Result<CueStore, CueError> {
        let file = state_dir.join(cue_file_name(volume)?);
        let cues = match std::fs::read(&file) {
            Ok(bytes) => parse(&bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => {
                return Err(CueError::Io {
                    path: file,
                    source: e,
                })
            }
        };
        Ok(CueStore {
            file,
            mount_point: mount_point.to_path_buf(),
            cues,
        })
    }

    pub fn path(&self) -> &Path {
        &self.file
    }

    pub fn len(&self) -> usize {
        self.cues.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cues.is_empty()
    }

    /// The cue point for a track, in frames. **Zero when unset**, which is
    /// the CDJ-350 default and is why no "is there one" question is exposed.
    pub fn get(&self, track: &Path) -> Result<u64, CueError> {
        let key = self.key(track)?;
        Ok(self.cues.get(&key).copied().unwrap_or(0))
    }

    /// Sets the cue point and **writes the file immediately.**
    ///
    /// Fallible and eager on purpose. A deck gets switched off at the wall,
    /// so a store that only flushed on shutdown would routinely lose the last
    /// thing set. The write is a few hundred bytes off the audio thread; an SD
    /// card does not care, and the alternative is a caller remembering to
    /// save.
    ///
    /// Setting zero **removes** the entry rather than storing it: zero is the
    /// unset value, so the two are already indistinguishable in behaviour and
    /// keeping the file free of them costs nothing.
    pub fn set(&mut self, track: &Path, frame: u64) -> Result<(), CueError> {
        let key = self.key(track)?;
        if frame == 0 {
            self.cues.remove(&key);
        } else {
            self.cues.insert(key, frame);
        }
        self.save()
    }

    /// Writes the file **atomically** — a temporary beside it, then a rename.
    ///
    /// The failure this prevents is the appliance one: power cut mid-write
    /// leaves a truncated file, and every cue for that stick is gone rather
    /// than one. `rename` within a directory is atomic, so a reader sees
    /// either the old file or the new one.
    pub fn save(&self) -> Result<(), CueError> {
        let dir = self.file.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(|e| CueError::Io {
            path: dir.to_path_buf(),
            source: e,
        })?;

        let tmp = self.file.with_extension("tmp");
        let io = |e: std::io::Error| CueError::Io {
            path: tmp.clone(),
            source: e,
        };

        {
            let mut f = std::fs::File::create(&tmp).map_err(io)?;
            writeln!(f, "{}", HEADER).map_err(io)?;
            // Sorted, so the file is stable across saves and a diff shows
            // what actually changed rather than a rehash of the map.
            let mut entries: Vec<_> = self.cues.iter().collect();
            entries.sort();
            for (path, frame) in entries {
                f.write_all(frame.to_string().as_bytes()).map_err(io)?;
                f.write_all(b"\t").map_err(io)?;
                f.write_all(&escape(path)).map_err(io)?;
                f.write_all(b"\n").map_err(io)?;
            }
            // Flush the contents before the rename publishes them. Without
            // this the rename can be visible while the data is not.
            f.sync_all().map_err(io)?;
        }

        std::fs::rename(&tmp, &self.file).map_err(|e| CueError::Io {
            path: self.file.clone(),
            source: e,
        })?;

        // **And fsync the directory, or the rename itself is not durable.**
        // `sync_all` above puts the *contents* on the card; it says nothing
        // about the directory entry that names them. On ext4 with the default
        // five-second commit interval, `set` can return `Ok` and the previous
        // cue still be there after the power goes — which is the exact
        // scenario `decisions.md` gives as the reason this writes through at
        // all: "a deck gets switched off at the wall". Losing the last cue set
        // is milder than losing the file, but it is silent, and a caller that
        // reads `Ok` as "saved" is entitled to.
        //
        // Best-effort: a filesystem that refuses to open a directory for this
        // is not a reason to report a failed save, because the save succeeded.
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    }

    /// The key: the path relative to the mount point, as raw bytes.
    ///
    /// Accepts a path under the mount point or an already-relative one, and
    /// refuses anything else — an absolute path from elsewhere has no key,
    /// and silently storing it under its full path would make the cue
    /// unfindable once the volume moved.
    fn key(&self, track: &Path) -> Result<Vec<u8>, CueError> {
        let rel = if track.is_absolute() {
            track
                .strip_prefix(&self.mount_point)
                .map_err(|_| CueError::OutsideMedium {
                    path: track.to_path_buf(),
                })?
        } else {
            track
        };
        Ok(rel.as_os_str().as_bytes().to_vec())
    }
}

/// One file per volume, named by the UUID.
///
/// The UUID comes from a filename in `/dev/disk/by-uuid`, so it cannot
/// contain `/` — but it is being used to build a path, so that is checked
/// rather than assumed.
fn cue_file_name(volume: &str) -> Result<String, CueError> {
    let bad = |reason| CueError::BadVolume {
        volume: volume.to_string(),
        reason,
    };
    if volume.is_empty() {
        return Err(bad("empty"));
    }
    if volume == "." || volume == ".." {
        return Err(bad("a relative path component"));
    }
    if volume.contains('/') || volume.contains('\0') {
        return Err(bad("contains a path separator or NUL"));
    }
    if volume.chars().any(|c| c.is_control()) {
        return Err(bad("contains a control character"));
    }
    Ok(format!("{}.cues", volume))
}

/// Escapes a path's bytes so it survives one line of a text file.
///
/// Multi-byte UTF-8 passes through untouched, so a Japanese filename stays
/// readable in the file — the point of a format you can `cat`. Only the bytes
/// that would break the format, or that no terminal renders, are escaped.
fn escape(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            b'\\' => out.extend_from_slice(br"\\"),
            b'\n' => out.extend_from_slice(br"\n"),
            b'\r' => out.extend_from_slice(br"\r"),
            b'\t' => out.extend_from_slice(br"\t"),
            0x00..=0x1f | 0x7f => {
                out.extend_from_slice(format!("\\x{:02x}", b).as_bytes());
            }
            _ => out.push(b),
        }
    }
    out
}

/// The inverse of [`escape`]. An unrecognised escape is kept verbatim rather
/// than dropped, so a hand-edited file degrades into a wrong name rather than
/// a missing one.
fn unescape(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        match bytes.get(i + 1) {
            Some(b'\\') => {
                out.push(b'\\');
                i += 2;
            }
            Some(b'n') => {
                out.push(b'\n');
                i += 2;
            }
            Some(b'r') => {
                out.push(b'\r');
                i += 2;
            }
            Some(b't') => {
                out.push(b'\t');
                i += 2;
            }
            Some(b'x') if i + 3 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 2..i + 4]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i += 4;
                    }
                    None => {
                        out.push(b'\\');
                        i += 1;
                    }
                }
            }
            _ => {
                out.push(b'\\');
                i += 1;
            }
        }
    }
    out
}

fn parse(bytes: &[u8]) -> HashMap<Vec<u8>, u64> {
    let mut cues = HashMap::new();
    for line in bytes.split(|&b| b == b'\n') {
        if line.is_empty() || line.first() == Some(&b'#') {
            continue;
        }
        let Some(tab) = line.iter().position(|&b| b == b'\t') else {
            continue;
        };
        let Ok(frame) = std::str::from_utf8(&line[..tab]) else {
            continue;
        };
        let Ok(frame) = frame.parse::<u64>() else {
            continue;
        };
        if frame == 0 {
            continue;
        }
        cues.insert(unescape(&line[tab + 1..]), frame);
    }
    cues
}

/// The relative path a key came from, for display.
pub fn key_to_path(key: &[u8]) -> PathBuf {
    PathBuf::from(std::ffi::OsString::from_vec(key.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Dir {
            let d = std::env::temp_dir()
                .join(format!("deck-pi-cue-{}-{}", tag, std::process::id()));
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

    const MOUNT: &str = "/media/stick";

    #[test]
    fn a_cue_survives_a_reload() {
        let d = Dir::new("roundtrip");
        let track = Path::new("/media/stick/album/piece.wav");

        let mut s = CueStore::load(&d.0, "1A2B-3C4D", Path::new(MOUNT)).expect("load");
        assert_eq!(s.get(track).expect("get"), 0, "unset reads as frame zero");
        s.set(track, 44_100).expect("set");

        let again = CueStore::load(&d.0, "1A2B-3C4D", Path::new(MOUNT)).expect("reload");
        assert_eq!(again.get(track).expect("get"), 44_100);
    }

    #[test]
    fn a_newline_in_a_filename_survives_the_round_trip() {
        // The trap the format exists for. Legal on HFS+, verified on a real
        // filesystem, and it would otherwise split one entry across two lines
        // — the second half parsing as garbage or as another entry.
        let d = Dir::new("newline");
        let nasty = Path::new(MOUNT).join("two\nlines\tand\\a tab.wav");

        let mut s = CueStore::load(&d.0, "vol", Path::new(MOUNT)).expect("load");
        s.set(&nasty, 12_345).expect("set");

        // The file really is one line per entry, header aside.
        let raw = std::fs::read_to_string(s.path()).expect("read");
        assert_eq!(
            raw.lines().filter(|l| !l.starts_with('#')).count(),
            1,
            "the entry must not have split:\n{}",
            raw
        );

        let again = CueStore::load(&d.0, "vol", Path::new(MOUNT)).expect("reload");
        assert_eq!(again.get(&nasty).expect("get"), 12_345);
    }

    #[test]
    fn a_japanese_filename_stays_legible_in_the_file() {
        // The reason multi-byte UTF-8 is not escaped: this is a format you
        // read over SSH on an appliance with no screen worth using.
        let d = Dir::new("japanese");
        let track = Path::new(MOUNT).join("雨音と遠雷.wav");
        let mut s = CueStore::load(&d.0, "vol", Path::new(MOUNT)).expect("load");
        s.set(&track, 88_200).expect("set");

        let raw = std::fs::read_to_string(s.path()).expect("read");
        assert!(raw.contains("雨音と遠雷.wav"), "not legible:\n{}", raw);

        let again = CueStore::load(&d.0, "vol", Path::new(MOUNT)).expect("reload");
        assert_eq!(again.get(&track).expect("get"), 88_200);
    }

    #[test]
    fn two_names_that_are_not_valid_utf8_do_not_collide() {
        // A lossy `String` conversion maps every invalid sequence to the same
        // replacement character, so these two would have shared one cue —
        // silently, and only on the medium that produced them.
        let d = Dir::new("invalid-utf8");
        let a = PathBuf::from(std::ffi::OsString::from_vec(
            [MOUNT.as_bytes(), b"/", &[0xff], b".wav"].concat(),
        ));
        let b = PathBuf::from(std::ffi::OsString::from_vec(
            [MOUNT.as_bytes(), b"/", &[0xfe], b".wav"].concat(),
        ));
        assert_ne!(a, b);

        let mut s = CueStore::load(&d.0, "vol", Path::new(MOUNT)).expect("load");
        s.set(&a, 111).expect("set a");
        s.set(&b, 222).expect("set b");
        assert_eq!(s.len(), 2, "the two names must be separate keys");

        let again = CueStore::load(&d.0, "vol", Path::new(MOUNT)).expect("reload");
        assert_eq!(again.get(&a).expect("get"), 111);
        assert_eq!(again.get(&b).expect("get"), 222);
    }

    #[test]
    fn setting_a_new_cue_cancels_the_old_one() {
        // The CDJ-350 rule: one cue point per track.
        let d = Dir::new("replace");
        let track = Path::new(MOUNT).join("t.wav");
        let mut s = CueStore::load(&d.0, "vol", Path::new(MOUNT)).expect("load");
        s.set(&track, 1000).expect("set");
        s.set(&track, 2000).expect("set again");
        assert_eq!(s.len(), 1);
        assert_eq!(s.get(&track).expect("get"), 2000);
    }

    #[test]
    fn zero_is_the_unset_value_and_is_not_stored() {
        // A cue deliberately set at frame zero behaves identically to an
        // unset one, so the file need not carry it.
        let d = Dir::new("zero");
        let track = Path::new(MOUNT).join("t.wav");
        let mut s = CueStore::load(&d.0, "vol", Path::new(MOUNT)).expect("load");
        s.set(&track, 5000).expect("set");
        s.set(&track, 0).expect("back to zero");
        assert_eq!(s.len(), 0);
        assert_eq!(s.get(&track).expect("get"), 0);
    }

    #[test]
    fn the_key_is_relative_so_a_different_mount_point_still_finds_it() {
        // Keyed by volume UUID plus *relative* path, per `architecture.md`.
        // The mount point is a placeholder that could change (#11), and a cue
        // must not be lost because of it.
        let d = Dir::new("relative");
        let mut s = CueStore::load(&d.0, "vol", Path::new("/media/stick")).expect("load");
        s.set(Path::new("/media/stick/a/b.wav"), 777).expect("set");

        let moved = CueStore::load(&d.0, "vol", Path::new("/mnt/other")).expect("load");
        assert_eq!(moved.get(Path::new("/mnt/other/a/b.wav")).expect("get"), 777);
    }

    #[test]
    fn a_path_outside_the_medium_has_no_key() {
        let d = Dir::new("outside");
        let s = CueStore::load(&d.0, "vol", Path::new(MOUNT)).expect("load");
        match s.get(Path::new("/etc/passwd")) {
            Err(CueError::OutsideMedium { .. }) => {}
            other => panic!("expected OutsideMedium, got {:?}", other),
        }
    }

    #[test]
    fn a_volume_name_that_would_escape_the_directory_is_refused_not_sanitised() {
        // Mangling two different volume names into one filename would merge
        // their cues, which is worse than not saving at all.
        let d = Dir::new("badvol");
        for bad in ["", "..", ".", "a/b", "with\nnewline"] {
            match CueStore::load(&d.0, bad, Path::new(MOUNT)) {
                Err(CueError::BadVolume { .. }) => {}
                other => panic!("{:?} should be refused, got {:?}", bad, other.is_ok()),
            }
        }
        // And the shapes both filesystems actually produce are accepted.
        for good in ["1A2B-3C4D", "655062ae-6e83-4521-b69b-44b96146a5d7"] {
            CueStore::load(&d.0, good, Path::new(MOUNT)).expect(good);
        }
    }

    #[test]
    fn a_missing_file_is_a_first_use_and_a_mangled_line_is_skipped() {
        let d = Dir::new("corrupt");
        // Missing.
        let s = CueStore::load(&d.0, "vol", Path::new(MOUNT)).expect("load");
        assert!(s.is_empty());

        // Mangled: a good line, a line with no tab, a non-numeric frame, and
        // a comment. Refusing to browse because one line is bad would be a
        // worse failure than losing that cue.
        std::fs::write(
            d.0.join("vol.cues"),
            "# header\n123\tgood.wav\nno tab here\nxyz\tbad.wav\n\n",
        )
        .expect("write");
        let s = CueStore::load(&d.0, "vol", Path::new(MOUNT)).expect("load");
        assert_eq!(s.len(), 1);
        assert_eq!(s.get(Path::new("good.wav")).expect("get"), 123);
    }

    #[test]
    fn the_file_is_sorted_so_saves_are_stable() {
        let d = Dir::new("stable");
        let mut s = CueStore::load(&d.0, "vol", Path::new(MOUNT)).expect("load");
        for name in ["c.wav", "a.wav", "b.wav"] {
            s.set(&Path::new(MOUNT).join(name), 1).expect("set");
        }
        let first = std::fs::read_to_string(s.path()).expect("read");

        // Re-saving without changes must produce the same bytes, or a diff
        // shows a rehash of the map rather than what changed.
        s.save().expect("save");
        assert_eq!(first, std::fs::read_to_string(s.path()).expect("read"));
        let lines: Vec<&str> = first.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(lines, vec!["1\ta.wav", "1\tb.wav", "1\tc.wav"]);
    }

    #[test]
    fn the_write_leaves_no_temporary_behind() {
        let d = Dir::new("atomic");
        let mut s = CueStore::load(&d.0, "vol", Path::new(MOUNT)).expect("load");
        s.set(&Path::new(MOUNT).join("t.wav"), 1).expect("set");
        let left: Vec<_> = std::fs::read_dir(&d.0)
            .expect("read_dir")
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(left, vec![OsStr::new("vol.cues")], "a .tmp survived");
    }

    #[test]
    fn escaping_round_trips_every_byte() {
        // Exhaustive over single bytes, which is the whole alphabet the
        // escaper has to handle.
        for b in 0u8..=255 {
            if b == 0 {
                continue; // NUL cannot appear in a path at all.
            }
            let original = vec![b];
            assert_eq!(
                unescape(&escape(&original)),
                original,
                "byte {:#04x} did not round trip",
                b
            );
        }
    }

    #[test]
    fn the_state_directory_follows_xdg() {
        // Not a hardcoded path: the audio process runs as a plain user, so
        // `/var/lib` is wrong and `$XDG_STATE_HOME` is the convention for
        // state a program created and cannot regenerate.
        let saved = std::env::var_os("XDG_STATE_HOME");
        // Single-threaded test, restored below. The `unsafe` is **not**
        // required at edition 2021 — it compiles without — and is written
        // this way because `set_var` becomes unsafe in 2024, so the edition
        // bump stays a one-line change. Another thread reading the
        // environment concurrently is the hazard; nothing else here does.
        unsafe { std::env::set_var("XDG_STATE_HOME", "/tmp/xdg-state") };
        assert_eq!(
            default_state_dir(),
            Some(PathBuf::from("/tmp/xdg-state/deck-pi"))
        );
        match saved {
            Some(v) => unsafe { std::env::set_var("XDG_STATE_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_STATE_HOME") },
        }
    }
}
