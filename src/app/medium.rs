//! The stick arriving and going, joined to the deck.
//!
//! `src/media.rs` answers "is there a stick, can it be read, what is it
//! called" and answers it about a path. It has never been asked. This is the
//! caller — the piece that turns a change of medium into a browser, a cue
//! store, and where necessary an unload.
//!
//! Until this existed, [`Deck`] took its browser and its cue store at
//! construction and never heard about either again. Both failure modes are
//! silent, which is why they lasted: a stick pushed in after the deck started
//! did nothing at all, and a stick pulled left the deck listing a folder that
//! was no longer mounted and holding a track whose file had gone.
//!
//! # The order, and it is load-bearing twice
//!
//! **Before the presses.** [`crate::app::controls::run`] turns this over
//! first, for the same reason `Controls::turn` services the deck before
//! applying anything: a turn can contain both the removal and the ENTER that
//! arrived with it, and applying the press first loads a file from a stick
//! that is gone. Interpreting a press against what the deck actually is means
//! settling what it is first.
//!
//! **Unload before the listing goes.** `decisions.md` is explicit that a run
//! ends on "unloading, the medium going, or a fault", so the medium going is
//! not a case to invent a policy for. [`Deck::detach`] does it in that order
//! deliberately: dropping the browser first would leave a `Playing` holding
//! threads against a vanished file, and the deck would keep reporting a
//! track it cannot play until the window thread happened to fail.
//!
//! # Every change ejects, including one browsable medium for another
//!
//! The obvious shape is "on `Absent`, eject; on `Browsable`, mount", and it is
//! wrong in a way nothing would report. A stick pulled and another pushed in
//! **between two polls** is one transition, `Browsable` to `Browsable`, and
//! that shape would keep the old browser and the old cue store: a listing of
//! a volume that is no longer there, over the top of a volume that is, with
//! cues keyed to the wrong UUID and written through as if they were right.
//!
//! So a change is a change. Eject, then mount whatever is now there.
//!
//! # Polled at a human rate, not at the loop's
//!
//! The examination is two `stat` calls, which is cheap enough to do on every
//! turn and pointless to: the loop turns over every 10 ms and a stick is
//! pushed in by a hand. [`POLL_EVERY`] paces it, and the first turn is not
//! paced at all so a medium already mounted at start-up is found immediately
//! rather than half a second in.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::app::deck::Deck;
use crate::app::track::Ended;
use crate::browser::{BrowseError, Browser};
use crate::cue::{CueError, CueStore};
use crate::media::{MediaWatch, Medium};
use crate::sink::AudioSink;

/// Where the medium's state comes from.
///
/// The same move [`crate::app::controls::EventSource`] makes: the deck gets
/// [`MediaWatch`], a test gets a script, and the policy below is the same code
/// either way. Without it every one of these transitions would need a real
/// mount to exercise, and a transition nobody can test is a transition nobody
/// has tested.
pub trait MediumSource {
    /// The new state, **only when it changed** — the contract
    /// [`MediaWatch::poll`] already keeps.
    fn poll(&mut self) -> Option<Medium>;

    /// The path the medium is mounted at, which is what the browser opens and
    /// what the cue store keys paths against.
    fn mount_point(&self) -> &Path;
}

impl MediumSource for MediaWatch {
    fn poll(&mut self) -> Option<Medium> {
        MediaWatch::poll(self).cloned()
    }

    fn mount_point(&self) -> &Path {
        MediaWatch::mount_point(self)
    }
}

/// How often the medium is examined.
///
/// Half a second against a hand pushing in a plug. Removal does not need to be
/// quicker: what a pulled stick breaks is the *audio*, and that is caught by
/// the window thread failing rather than by this — see `architecture.md`,
/// "where failures surface, a pulled stick included".
pub const POLL_EVERY: Duration = Duration::from_millis(500);

/// What one change of medium did to the deck.
///
/// **Four fields rather than a `Result`**, because three of these can be true
/// at once and none of them stops the deck. A medium can arrive, fail to
/// browse, and have ended the previous track on its way in.
#[derive(Debug)]
pub struct Change {
    /// What the medium became.
    pub medium: Medium,
    /// The track the eject ended, if one was loaded. The display says this;
    /// it is not an error.
    pub ended: Option<Ended>,
    /// The medium examined as browsable and then would not open.
    ///
    /// **Reported rather than folded into `Absent`.** It means the stick went
    /// between the examination and the `read_dir`, or that something is wrong
    /// with it that `media.rs`'s one directory read did not reach — and a deck
    /// that showed an empty folder for it would be describing a stick that is
    /// there as a stick that is not.
    pub unbrowsable: Option<BrowseError>,
    /// The volume has a UUID and its cue file would not load.
    ///
    /// A degradation, not a refusal: the deck browses and plays, and cues do
    /// not persist — the same state `Medium::Browsable { uuid: None }` is in
    /// for a different reason.
    pub cueless: Option<CueError>,
}

impl Change {
    /// Whether anything about this change needs saying beyond the medium
    /// itself.
    pub fn is_clean(&self) -> bool {
        self.unbrowsable.is_none() && self.cueless.is_none()
    }
}

/// The medium half of the app loop: poll, and move the deck when it moves.
pub struct Mount<M: MediumSource> {
    watch: M,
    /// Where cue files live — the SD card, not the stick, which is mounted
    /// read-only. `None` when there is nowhere to put them, which
    /// `cue::default_state_dir` reports by returning `None` and which is a
    /// deck that plays without keeping cues.
    state_dir: Option<PathBuf>,
    every: Duration,
    next: Duration,
}

impl Mount<MediaWatch> {
    /// The deck's own: the fixed mount point and the user's state directory.
    pub fn at_default_path() -> Mount<MediaWatch> {
        Mount::new(MediaWatch::at_default_path(), crate::cue::default_state_dir())
    }
}

impl<M: MediumSource> Mount<M> {
    pub fn new(watch: M, state_dir: Option<PathBuf>) -> Mount<M> {
        Mount {
            watch,
            state_dir,
            every: POLL_EVERY,
            next: Duration::ZERO,
        }
    }

    /// For tests that would otherwise have to wait out [`POLL_EVERY`].
    pub fn with_interval(mut self, every: Duration) -> Mount<M> {
        self.every = every;
        self
    }

    pub fn watch(&self) -> &M {
        &self.watch
    }

    /// One turn. `None` when the medium has not changed, which is almost
    /// always.
    pub fn turn<S>(&mut self, now: Duration, deck: &mut Deck<S>) -> Option<Change>
    where
        S: AudioSink + Send + 'static,
    {
        if now < self.next {
            return None;
        }
        self.next = now + self.every;
        let medium = self.watch.poll()?;

        // Unconditional, and before anything is opened. See the module doc:
        // one browsable medium replacing another is still a replacement.
        let mut change = Change {
            ended: deck.detach(),
            medium,
            unbrowsable: None,
            cueless: None,
        };

        let Medium::Browsable { uuid } = &change.medium else {
            return Some(change);
        };
        let root = self.watch.mount_point();
        let browser = match Browser::open(root) {
            Ok(b) => b,
            Err(e) => {
                change.unbrowsable = Some(e);
                return Some(change);
            }
        };
        // No UUID and no state directory are both ordinary, and neither is an
        // error to report — the medium itself already says the first, and the
        // second is a property of the machine rather than of the stick.
        let cues = match (uuid.as_deref(), self.state_dir.as_deref()) {
            (Some(volume), Some(dir)) => match CueStore::load(dir, volume, root) {
                Ok(store) => Some(store),
                Err(e) => {
                    change.cueless = Some(e);
                    None
                }
            },
            _ => None,
        };
        deck.attach(browser, cues);
        Some(change)
    }
}
