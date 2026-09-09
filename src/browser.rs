//! The browser: the folder tree, the selection, and the header cache.
//!
//! `architecture.md` calls this "the model" and puts the display opposite it as
//! "the view". The split is load-bearing rather than tidy: which panel the deck
//! gets is open question 1, and the candidates give **two to twelve** browsable
//! rows. So nothing here knows a row count — [`Browser::view`] takes the
//! viewport height as an argument.
//!
//! # `view` is the only thing that reads a header
//!
//! `decisions.md` requires header reads to be "driven by renders, not by
//! encoder events": the browser must open a file to show its length, rate and
//! depth and to apply the four rejections before PLAY, and on a cheap flash
//! controller that open can take several milliseconds. Redraws are already
//! coalesced to 30-50 ms, so if the reads ride the *render* then a fast spin
//! costs one open per redraw instead of one per detent, and the intermediate
//! positions are skipped outright.
//!
//! That is why [`Browser::select_next`] and [`Browser::select_prev`] touch no
//! files at all, and why `view` takes `&mut self` — it is the render, and it is
//! where the reads and the caching happen. Getting this backwards would work
//! perfectly on a desk and fall behind on a spin.
//!
//! # Dotfiles are hidden, and on this medium that is not cosmetic
//!
//! The stick is prepared on a Mac (`architecture.md`), so every folder on it
//! carries `.DS_Store`, and an HFS+ volume additionally carries `._name` twins
//! for files with resource forks, plus `.Spotlight-V100`, `.fseventsd` and
//! `.Trashes` at the top. `._piece.wav` is the one that matters: it would sit
//! directly beside `piece.wav` and read as a broken duplicate of it, because
//! libsndfile cannot open it. One rule removes all of them.
//!
//! # Nothing is filtered by extension
//!
//! A file the deck cannot play must still be *shown*, marked, with the reason —
//! "say why, not just that" (`decisions.md`). Hiding refusals would make a
//! folder look empty when it is full of FLAC.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::file::{OpenError, Reject, Track, TrackInfo};

/// What the browser makes of one file, cached by path.
///
/// Cloneable, unlike [`OpenError`], because the cache hands copies to the
/// view. `Unreadable` keeps the message rather than the error so the display
/// can still say *why* — a truncated header and a pulled stick both land here
/// and are worth distinguishing on screen.
#[derive(Debug, Clone)]
pub enum Verdict {
    Plays(TrackInfo),
    Refused(Reject),
    Unreadable(String),
}

/// One rendered row.
#[derive(Debug, Clone)]
pub enum Row {
    Folder {
        name: String,
        selected: bool,
    },
    File {
        name: String,
        verdict: Verdict,
        selected: bool,
    },
}

impl Row {
    pub fn name(&self) -> &str {
        match self {
            Row::Folder { name, .. } | Row::File { name, .. } => name,
        }
    }

    pub fn selected(&self) -> bool {
        match self {
            Row::Folder { selected, .. } | Row::File { selected, .. } => *selected,
        }
    }
}

/// What pressing ENTER did.
#[derive(Debug, Clone)]
pub enum Activation {
    /// Descended into a folder. The view should be re-rendered.
    Descended,
    /// A playable file. The engine's cue to load it.
    Play(Box<TrackInfo>),
    /// A file that cannot be played, with which of the four reasons applies.
    /// PLAY is never reached, which is the whole point of vetting on
    /// highlight.
    Refused(Reject),
    /// A file libsndfile could not open at all.
    Unreadable(String),
    /// An empty folder, so there is nothing under the selection.
    Nothing,
}

/// Why the browser could not do something. The stick going away is the
/// dominant cause and is not exceptional — `decisions.md` requires the UI to
/// tell "no stick" from "stick I cannot read", and this is the second of
/// those.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowseError {
    /// `read_dir` failed. Almost always the medium being removed.
    Unreadable { path: PathBuf, reason: String },
    /// A path that resolved outside the root. Only reachable through a
    /// symlink on an HFS+ volume, and refused rather than followed.
    OutsideRoot { path: PathBuf },
}

impl std::fmt::Display for BrowseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BrowseError::Unreadable { path, reason } => {
                write!(f, "{}: {}", path.display(), reason)
            }
            BrowseError::OutsideRoot { path } => {
                write!(f, "{} resolves outside the medium", path.display())
            }
        }
    }
}

impl std::error::Error for BrowseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Folder,
    File,
}

#[derive(Debug, Clone)]
struct Entry {
    name: OsString,
    kind: EntryKind,
}

pub struct Browser {
    /// The mount point. Nothing above this is reachable.
    root: PathBuf,
    cwd: PathBuf,
    entries: Vec<Entry>,
    selected: usize,
    /// The top visible row. Held here rather than derived from the selection
    /// so scrolling is stable: stepping down one row scrolls by one, instead
    /// of re-centring and moving every line on the panel.
    first: usize,
    headers: HashMap<PathBuf, Verdict>,
}

impl Browser {
    /// Opens the medium's root folder.
    ///
    /// Whether the mount point *exists* is not this module's question — that
    /// is media watch's `stat`, and `--automount=no` is what makes it a
    /// reliable one (`decisions.md`). By the time this is called, something is
    /// mounted; what can still go wrong is reading it.
    pub fn open(root: &Path) -> Result<Browser, BrowseError> {
        let mut b = Browser {
            root: root.to_path_buf(),
            cwd: root.to_path_buf(),
            entries: Vec::new(),
            selected: 0,
            first: 0,
            headers: HashMap::new(),
        };
        b.reload()?;
        Ok(b)
    }

    /// Re-reads the current folder, keeping the selection where it can.
    ///
    /// The header cache is *not* cleared: it is keyed by path, and a path on a
    /// read-only medium cannot have changed underneath. A different stick
    /// means a new `Browser`.
    pub fn reload(&mut self) -> Result<(), BrowseError> {
        let selected_name = self.entries.get(self.selected).map(|e| e.name.clone());
        self.entries = read_folder(&self.cwd)?;
        self.selected = selected_name
            .and_then(|n| self.entries.iter().position(|e| e.name == n))
            .unwrap_or(0);
        self.first = 0;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.cwd
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn at_root(&self) -> bool {
        self.cwd == self.root
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// The path under the selection, or `None` in an empty folder.
    pub fn selected_path(&self) -> Option<PathBuf> {
        self.entries.get(self.selected).map(|e| self.cwd.join(&e.name))
    }

    /// Moves the selection down one row. **Reads nothing.**
    ///
    /// Does not wrap. A detented encoder gives no feedback that the list
    /// ended, and with two browsable rows on the smallest candidate panel,
    /// wrapping from the end of a long folder to its start would be
    /// indistinguishable from a mis-scroll. Stopping is legible; the top and
    /// bottom of a list are places you can feel.
    pub fn select_next(&mut self) {
        if self.selected + 1 < self.entries.len() {
            self.selected += 1;
        }
    }

    /// Moves the selection up one row. **Reads nothing.**
    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Renders `height` rows, reading and caching the headers of exactly
    /// those rows — the one place a file is opened.
    ///
    /// Scrolls the viewport only as far as it must to keep the selection
    /// inside it.
    pub fn view(&mut self, height: usize) -> Vec<Row> {
        if height == 0 || self.entries.is_empty() {
            return Vec::new();
        }

        // Keep the selection visible, moving the window by the minimum.
        if self.selected < self.first {
            self.first = self.selected;
        } else if self.selected >= self.first + height {
            self.first = self.selected + 1 - height;
        }
        // And never leave blank rows at the bottom while there are entries
        // above that could fill them.
        let max_first = self.entries.len().saturating_sub(height);
        self.first = self.first.min(max_first);

        let last = (self.first + height).min(self.entries.len());
        let mut rows = Vec::with_capacity(last - self.first);
        for i in self.first..last {
            let entry = &self.entries[i];
            let name = entry.name.to_string_lossy().into_owned();
            let selected = i == self.selected;
            rows.push(match entry.kind {
                EntryKind::Folder => Row::Folder { name, selected },
                EntryKind::File => {
                    let path = self.cwd.join(&entry.name);
                    let verdict = match self.headers.get(&path) {
                        Some(v) => v.clone(),
                        None => {
                            let v = read_header(&path);
                            self.headers.insert(path, v.clone());
                            v
                        }
                    };
                    Row::File {
                        name,
                        verdict,
                        selected,
                    }
                }
            });
        }
        rows
    }

    /// The top visible row, for a scrollbar.
    pub fn first_visible(&self) -> usize {
        self.first
    }

    /// How many files in this folder have been read. Diagnostic — it is what
    /// makes "one open per redraw, not one per detent" checkable rather than
    /// asserted.
    pub fn headers_read(&self) -> usize {
        self.headers.len()
    }

    /// ENTER: descend into the selection, or hand a playable file over.
    ///
    /// A file's verdict comes from the cache when the row has been rendered,
    /// which it will have been — the selection is always inside the viewport.
    /// It is read here if not, so ENTER is correct even if nothing has been
    /// drawn yet.
    pub fn enter(&mut self) -> Result<Activation, BrowseError> {
        let Some(entry) = self.entries.get(self.selected).cloned() else {
            return Ok(Activation::Nothing);
        };
        let path = self.cwd.join(&entry.name);
        match entry.kind {
            EntryKind::Folder => {
                let entries = read_folder(&path)?;
                self.cwd = path;
                self.entries = entries;
                self.selected = 0;
                self.first = 0;
                Ok(Activation::Descended)
            }
            EntryKind::File => {
                let verdict = match self.headers.get(&path) {
                    Some(v) => v.clone(),
                    None => {
                        let v = read_header(&path);
                        self.headers.insert(path, v.clone());
                        v
                    }
                };
                Ok(match verdict {
                    Verdict::Plays(info) => Activation::Play(Box::new(info)),
                    Verdict::Refused(r) => Activation::Refused(r),
                    Verdict::Unreadable(m) => Activation::Unreadable(m),
                })
            }
        }
    }

    /// BACK: up one level. Returns false at the root, where there is nowhere
    /// to go — the medium's root is the top of the tree, not the filesystem's.
    ///
    /// Restores the selection onto the folder just left, so BACK after ENTER
    /// returns to where you were rather than to the top of the list.
    pub fn back(&mut self) -> Result<bool, BrowseError> {
        if self.at_root() {
            return Ok(false);
        }
        let left = self.cwd.file_name().map(|n| n.to_os_string());
        let Some(parent) = self.cwd.parent().map(|p| p.to_path_buf()) else {
            return Ok(false);
        };
        let entries = read_folder(&parent)?;
        self.cwd = parent;
        self.entries = entries;
        self.selected = left
            .and_then(|n| self.entries.iter().position(|e| e.name == n))
            .unwrap_or(0);
        self.first = 0;
        Ok(true)
    }
}

/// Opens a file's header and nothing else.
///
/// `Track` is dropped immediately, which closes it: this is one `sf_open` and
/// one `sf_close`. The file layer is deliberately shared with playback here
/// (`architecture.md`) rather than reimplemented for the browser, which is
/// what stops the two disagreeing about whether something will play.
fn read_header(path: &Path) -> Verdict {
    match Track::open(path) {
        Ok(track) => Verdict::Plays(track.info().clone()),
        Err(OpenError::Rejected(r)) => Verdict::Refused(r),
        Err(OpenError::Unreadable(e)) => Verdict::Unreadable(e.to_string()),
    }
}

/// Reads one folder into a sorted list, hiding dotfiles.
///
/// Entries whose type cannot be determined are dropped rather than guessed
/// at: on a read-only medium the only way that happens is the medium going
/// away, and a row that cannot be classified cannot be acted on either.
fn read_folder(dir: &Path) -> Result<Vec<Entry>, BrowseError> {
    let read = std::fs::read_dir(dir).map_err(|e| BrowseError::Unreadable {
        path: dir.to_path_buf(),
        reason: e.to_string(),
    })?;

    let mut entries = Vec::new();
    for item in read {
        // One unreadable entry does not fail the folder. `read_dir`'s
        // iterator yields a `Result` per entry, and on removable media a
        // single one erroring while the rest are fine is exactly the shape of
        // a partial failure.
        let Ok(item) = item else { continue };
        let name = item.file_name();
        if name.as_encoded_bytes().first() == Some(&b'.') {
            continue;
        }
        // `file_type` is the `lstat` type, so a symlink is neither. HFS+ has
        // symlinks; exFAT does not. Follow it with a `metadata` call, and
        // drop it if that fails — a dangling link is not something to show.
        let kind = match item.file_type() {
            Ok(t) if t.is_dir() => EntryKind::Folder,
            Ok(t) if t.is_file() => EntryKind::File,
            Ok(_) => match std::fs::metadata(item.path()) {
                Ok(m) if m.is_dir() => EntryKind::Folder,
                Ok(m) if m.is_file() => EntryKind::File,
                _ => continue,
            },
            Err(_) => continue,
        };
        entries.push(Entry { name, kind });
    }

    entries.sort_by(compare_entries);
    Ok(entries)
}

/// Folders first, then files; each group in natural order.
///
/// **Not specified anywhere in the design documents**, so this is a choice
/// made here. `readdir` order on both filesystems is whatever the directory
/// structure yields, which is neither stable nor meaningful, so *some* order
/// had to be imposed.
///
/// Folders first is the file-manager convention and it matters more here than
/// usual: with two browsable rows on the smallest candidate panel, having the
/// navigable rows collected at the top is the difference between scrolling to
/// find a folder and seeing it.
fn compare_entries(a: &Entry, b: &Entry) -> Ordering {
    match (a.kind, b.kind) {
        (EntryKind::Folder, EntryKind::File) => Ordering::Less,
        (EntryKind::File, EntryKind::Folder) => Ordering::Greater,
        _ => natural_cmp(&a.name.to_string_lossy(), &b.name.to_string_lossy()),
    }
}

/// Case-insensitive, with runs of ASCII digits compared as numbers.
///
/// Numeric-aware because prepared music is commonly numbered, and plain
/// lexicographic order puts `10` before `2`. It costs nothing on unnumbered
/// names, so no assumption about the material is being made — this is safe
/// whether or not anything on the stick is numbered.
///
/// Digit runs are compared by length-then-digits after stripping leading
/// zeros, rather than parsed, so a filename with forty digits in it cannot
/// overflow anything.
///
/// The final tie-break on the raw strings is what makes this a **total**
/// order: without it `A` and `a` compare equal and the sort is free to put
/// them in either order on each reload, which would move rows under the
/// selection for no reason.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.char_indices().peekable();
    let mut bi = b.char_indices().peekable();

    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => break,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some((_, x)), Some((_, y))) => {
                if x.is_ascii_digit() && y.is_ascii_digit() {
                    let na = take_digits(a, &mut ai);
                    let nb = take_digits(b, &mut bi);
                    match compare_digit_runs(na, nb) {
                        Ordering::Equal => {}
                        other => return other,
                    }
                } else {
                    ai.next();
                    bi.next();
                    match lower(x).cmp(&lower(y)) {
                        Ordering::Equal => {}
                        other => return other,
                    }
                }
            }
        }
    }
    // Same by the rules above — order by the raw bytes so the result is
    // stable across reloads.
    a.cmp(b)
}

fn lower(c: char) -> char {
    // `to_lowercase` can yield several chars; the first is enough for
    // ordering and keeps this a `char` comparison. Non-ASCII is unaffected
    // for the scripts in play — Japanese has no case.
    c.to_lowercase().next().unwrap_or(c)
}

fn take_digits<'a>(
    s: &'a str,
    it: &mut std::iter::Peekable<std::str::CharIndices<'a>>,
) -> &'a str {
    let start = it.peek().map(|(i, _)| *i).unwrap_or(s.len());
    let mut end = start;
    while let Some(&(i, c)) = it.peek() {
        if !c.is_ascii_digit() {
            break;
        }
        end = i + c.len_utf8();
        it.next();
    }
    &s[start..end]
}

fn compare_digit_runs(a: &str, b: &str) -> Ordering {
    let ta = a.trim_start_matches('0');
    let tb = b.trim_start_matches('0');
    // Both all zeros: equal in value, so fall through to the caller's
    // tie-break rather than deciding on width here.
    match ta.len().cmp(&tb.len()) {
        Ordering::Equal => ta.cmp(tb),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_sort_as_numbers_not_as_text() {
        let mut names = vec!["10.wav", "2.wav", "1.wav", "20.wav", "3.wav"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, vec!["1.wav", "2.wav", "3.wav", "10.wav", "20.wav"]);
    }

    #[test]
    fn leading_zeros_do_not_change_the_value() {
        assert_eq!(natural_cmp("track007", "track7"), Ordering::Less);
        assert_eq!(natural_cmp("07", "8"), Ordering::Less);
        assert_eq!(natural_cmp("007", "07"), Ordering::Less);
    }

    #[test]
    fn a_forty_digit_run_does_not_overflow() {
        // The reason digit runs are compared as strings rather than parsed.
        // `u64` would have wrapped or the parse would have failed, and either
        // way the order would be wrong rather than loud.
        let big = "9".repeat(40);
        let bigger = "9".repeat(41);
        assert_eq!(natural_cmp(&big, &bigger), Ordering::Less);
        assert_eq!(natural_cmp(&bigger, &big), Ordering::Greater);
    }

    #[test]
    fn the_order_is_total_so_a_reload_cannot_reshuffle_rows() {
        // Case-insensitive comparison makes these equal by the main rules;
        // without the raw tie-break the sort could return either order on
        // each reload, moving rows under the selection.
        assert_eq!(natural_cmp("Aa", "aA"), Ordering::Less);
        assert_eq!(natural_cmp("aA", "Aa"), Ordering::Greater);
        assert_eq!(natural_cmp("same", "same"), Ordering::Equal);
    }

    #[test]
    fn japanese_names_order_deterministically() {
        // Not linguistically meaningful — code-point order — but stable, and
        // the composed form is what arrives, because the HFS+ mount must not
        // pass `nodecompose` (docs/decisions.md). A decomposed name would
        // sort somewhere else entirely.
        let mut names = vec!["雨音と遠雷.wav", "あめ.wav", "第三楽章.wav", "01_intro.wav"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names[0], "01_intro.wav", "digits sort before kana");
        // And the same input sorts the same way twice.
        let mut again = vec!["第三楽章.wav", "01_intro.wav", "雨音と遠雷.wav", "あめ.wav"];
        again.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, again);
    }

    #[test]
    fn case_is_ignored_for_ordering() {
        let mut names = vec!["beta.wav", "Alpha.wav", "gamma.wav", "Delta.wav"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            vec!["Alpha.wav", "beta.wav", "Delta.wav", "gamma.wav"]
        );
    }
}
