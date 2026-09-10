//! Who owns "what is loaded".
//!
//! [#14](https://github.com/tamatebox/deck-pi/issues/14). Nothing did.
//! `architecture.md`'s module table said so outright — the browser holds the
//! selection, the transport the position and rate, the audio engine the window
//! and the ALSA setup, and the playing track's *path*, with where it sits in
//! its folder, belonged to none of them.
//!
//! # Two "current" things, and they move independently
//!
//! The browser's selection and the playing track are not the same. Browsing
//! folder B while a track from folder A plays is not an edge case; it is what
//! a browser on a deck is **for**. Every question of the form "which track?"
//! therefore has two possible answers, and the code has to say which.
//!
//! # The one that is a correctness problem, not a preference
//!
//! `CueStore::get` and `set` take the path from their caller, and until this
//! module existed the only thing holding paths was the browser. So the natural
//! wiring — ask the browser — attaches a cue to whatever happened to be
//! highlighted:
//!
//! 1. play `A/track1.wav`
//! 2. browse to `B` while it plays
//! 3. press CUE to mark a spot in the track you can hear
//! 4. the cue is written against `B/something.wav`, atomically, returning `Ok`
//!
//! Nothing reports it. It surfaces weeks later as a cue that went back to the
//! start of the track it belongs to, beside a cue on a track nobody set one
//! on. That is why [`Loaded::cue`] and [`Loaded::set_cue`] exist rather than
//! leaving callers to pass a path: the right source is the easy one, and using
//! the selection means reaching around this type deliberately.
//!
//! # And the one that was a preference until it stopped being one
//!
//! FF/REW *hold* seeks inside the playing track. If *tap* moved the browser,
//! one button's two gestures would act on two different objects — and
//! `hardware.md` already rejected exactly that shape when it refused to
//! overload the browse encoder for seeking: "puts a hidden mode on the most
//! used control. Two dedicated buttons cost two spare pins and no mode."
//! Browser-relative tap would put the mode back on the buttons bought to
//! avoid it. **Settled by the user: a tap acts on the playing track's folder
//! position.**
//!
//! # Neighbours are re-derived, never snapshotted
//!
//! The medium is mounted read-only, so a playing track's folder cannot change
//! underneath the deck. Holding a list would therefore buy nothing and cost
//! staleness, and the only case where re-deriving fails is the medium being
//! gone — which is the same moment loading the next track would fail anyway.
//!
//! It goes through `browser::read_folder`, the browser's own walk, so the two
//! orders cannot drift.

use std::path::{Path, PathBuf};

use crate::browser::{self, BrowseError, EntryKind};
use crate::cue::{CueError, CueStore};
use crate::file::TrackInfo;

/// Which way a tap moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Next,
    Previous,
}

/// The playing track, and where it sits.
///
/// **Able to hold nothing, deliberately.** A deck with no track loaded is an
/// ordinary state — it is how the deck starts, and where it returns when a
/// stick is pulled — so the type says so rather than leaving a caller to
/// remember. `src/media.rs` made the same choice with
/// `Browsable { uuid: Option<String> }`, for the same reason: a state that
/// cannot be represented gets a sentinel later, and a sentinel path is a path
/// that opens something.
pub struct Loaded {
    track: Option<Box<TrackInfo>>,
}

impl Default for Loaded {
    fn default() -> Self {
        Loaded::nothing()
    }
}

impl Loaded {
    pub fn nothing() -> Loaded {
        Loaded { track: None }
    }

    /// A track has been loaded. Takes what the file layer already produced.
    pub fn load(&mut self, info: Box<TrackInfo>) {
        self.track = Some(info);
    }

    /// Nothing is loaded any more — the medium went away, or the deck stopped
    /// and let go.
    ///
    /// This is the transition `Transport::state`'s `Stopped` has no way back
    /// to today, and the reason is the same gap: nothing unloaded a track
    /// because nothing owned one.
    pub fn unload(&mut self) {
        self.track = None;
    }

    pub fn track(&self) -> Option<&TrackInfo> {
        self.track.as_deref()
    }

    pub fn path(&self) -> Option<&Path> {
        self.track.as_ref().map(|t| t.path.as_path())
    }

    /// The file a tap should load, or `None` at a folder boundary and when
    /// nothing is loaded.
    ///
    /// `None` at the ends is the whole of the answer to
    /// [#12](https://github.com/tamatebox/deck-pi/issues/12): the caller does
    /// nothing, which is stopping, which is what `decisions.md` already
    /// decided for a track reaching its end — "nothing starts on its own".
    /// The selection does not wrap either, and for the same reason: on a
    /// two-row panel, arriving back at the top is indistinguishable from a
    /// mis-scroll.
    ///
    /// Folders are skipped. A tap is "next **track**", and stepping onto a
    /// folder would be a load that fails.
    pub fn neighbour(&self, step: Step) -> Result<Option<PathBuf>, BrowseError> {
        let Some(path) = self.path() else {
            return Ok(None);
        };
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return Ok(None);
        };
        let entries = browser::read_folder(dir)?;

        let files: Vec<&std::ffi::OsString> = entries
            .iter()
            .filter(|e| e.kind == EntryKind::File)
            .map(|e| &e.name)
            .collect();
        let Some(at) = files.iter().position(|n| n.as_os_str() == name) else {
            // The playing file is not in its own folder's listing. Reachable:
            // it is hidden by the dotfile rule, or it was removed. Either way
            // there is no "next" to compute from a position that does not
            // exist, and guessing one would move to an arbitrary track.
            return Ok(None);
        };
        let to = match step {
            Step::Next => at.checked_add(1).filter(|i| *i < files.len()),
            Step::Previous => at.checked_sub(1),
        };
        Ok(to.map(|i| dir.join(files[i])))
    }

    /// The cue point for the loaded track, or zero when nothing is loaded.
    ///
    /// **Goes through here rather than taking a path**, so the cue cannot be
    /// keyed on the browser's selection by accident. See the module doc.
    pub fn cue(&self, store: &CueStore) -> Result<u64, CueError> {
        match self.path() {
            Some(p) => store.get(p),
            None => Ok(0),
        }
    }

    /// Records the cue point for the loaded track. Does nothing when nothing
    /// is loaded — there is no track for it to belong to, and writing it
    /// against the browsed file is the defect this type exists to prevent.
    pub fn set_cue(&self, store: &mut CueStore, frame: u64) -> Result<(), CueError> {
        match self.path() {
            Some(p) => store.set(p, frame),
            None => Ok(()),
        }
    }
}
