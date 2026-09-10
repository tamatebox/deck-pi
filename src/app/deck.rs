//! The deck: one press in, the modules moved.
//!
//! Stage 2 gave a track its threads. This is the layer that decides *which*
//! track, and what a button means — `Action` in, transport, browser and
//! `Loaded` moved. It is the thing `tests/input_test.rs` had to fake, and
//! faking it is what let the real obligations go unmet for three stages.
//!
//! # Every branch here is a caller of something that stated a rule
//!
//! `implementation.md`'s ninth shape — a precondition the code states and
//! nothing enforces — was found by grepping doc comments for `must` and
//! reading the callers. This module *is* those callers, so the sweep was run
//! before a line of it was written rather than after something broke:
//!
//! | Stated | Where it lands |
//! |---|---|
//! | `window::Command::Relocate` — "whoever owns the app loop must send this" | [`Deck::apply`] on `Cued::Returned` |
//! | `Loaded::set_cue`, never `CueStore::set` with a caller's path | [`Deck::apply`] on `Cued::Set` |
//! | `Transport::reached_end` — "the control thread, and only" | [`Deck::service`] |
//! | `Devices::read_pending` — "a caller that gets a non-zero answer must reset its decoder" | **not here**: this module takes `Action`s, and the device loop that owes that is the next piece |
//!
//! # Two "current" things, still
//!
//! The browser's selection and the playing track move independently, and
//! `src/loaded.rs` exists because of it. So: **ENTER acts on the selection,
//! FF/REW act on the playing track.** That is not a preference — one
//! button's two gestures must not address two objects, and `hardware.md`
//! rejected the same shape when it refused to overload the browse encoder
//! for seeking.

use std::path::Path;
use std::sync::Arc;

use crate::app::track::{self, Ended, LoadError, Playing};
use crate::browser::{Activation, BrowseError, Browser};
use crate::cue::{CueError, CueStore};
use crate::file::TrackInfo;
use crate::input::{Action, Button};
use crate::loaded::{Loaded, Step};
use crate::sink::{AudioSink, SinkError};
use crate::transport::{Cued, Transport, RATE_PAUSED};

/// Why a press could not be carried out. **Not** a refused file: that is a
/// verdict the browser already renders on the row, and pressing ENTER on it
/// does nothing because the reason is on screen before PLAY is reachable.
#[derive(Debug)]
pub enum DeckError {
    Load(LoadError),
    Browse(BrowseError),
    Cue(CueError),
}

impl std::fmt::Display for DeckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeckError::Load(e) => write!(f, "{e}"),
            DeckError::Browse(e) => write!(f, "{e}"),
            DeckError::Cue(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DeckError {}

/// Opens the output for one track. Called per track, because the output rate
/// follows the source and is only known once the header is read.
pub type OpenSink<S> = Box<dyn FnMut(&TrackInfo) -> Result<S, SinkError> + Send>;

pub struct Deck<S: AudioSink + Send + 'static> {
    transport: Arc<Transport>,
    loaded: Loaded,
    /// `None` when there is no medium. `src/media.rs` owns the transitions;
    /// this holds whichever state it last reported.
    browser: Option<Browser>,
    /// `None` when the medium has no UUID to key on — `decisions.md`'s "a
    /// browsable state, not a failure". The deck plays; cues do not persist.
    cues: Option<CueStore>,
    playing: Option<Playing<S>>,
    config: track::Config,
    open_sink: OpenSink<S>,
    /// What FF/REW must restore on release. `Transport::end_seek` takes it as
    /// an argument precisely so this decision is the caller's and visible.
    was_playing: bool,
}

impl<S: AudioSink + Send + 'static> Deck<S> {
    pub fn new(
        open_sink: OpenSink<S>,
        browser: Option<Browser>,
        cues: Option<CueStore>,
        config: track::Config,
    ) -> Deck<S> {
        Deck {
            transport: Arc::new(Transport::new()),
            loaded: Loaded::nothing(),
            browser,
            cues,
            playing: None,
            config,
            open_sink,
            was_playing: false,
        }
    }

    pub fn transport(&self) -> &Arc<Transport> {
        &self.transport
    }

    pub fn loaded(&self) -> &Loaded {
        &self.loaded
    }

    pub fn browser(&mut self) -> Option<&mut Browser> {
        self.browser.as_mut()
    }

    pub fn playing(&self) -> Option<&Playing<S>> {
        self.playing.as_ref()
    }

    /// One turn of the control loop, independent of any press.
    ///
    /// Two obligations, and both are the control thread's by rule rather than
    /// by convenience. Pausing at the end of a track is
    /// `Transport::reached_end`, which may not be called from the audio
    /// thread. Noticing that the audio thread has *gone* — a pulled stick, a
    /// fault — is this, because the deck must then stop claiming to hold a
    /// track it can no longer play. Returns why, once, for the display.
    pub fn service(&mut self) -> Option<Ended> {
        let playing = self.playing.as_mut()?;
        if playing.finished() {
            // The threads are gone; the track is not playable any more.
            return Some(self.playing.take()?.unload(&mut self.loaded));
        }
        playing.service();
        None
    }

    /// One press.
    pub fn apply(&mut self, action: Action) -> Result<(), DeckError> {
        match action {
            Action::Press(Button::PlayPause) => {
                // **No "is anything loaded" check here.** `Transport` refuses
                // control while `State::Stopped`, which is what "nothing
                // loaded" means, so asking again would be a second copy of
                // the state machine's own rule — and two copies is how the
                // two orders in `Loaded::neighbour` were nearly allowed to
                // drift.
                if self.transport.rate() == RATE_PAUSED {
                    self.transport.play();
                } else {
                    self.transport.pause();
                }
            }

            // **The obligation arrives in the return value.** `Transport`
            // picks one of three behaviours from its own state, and what the
            // deck owes depends on which — so it is returned rather than
            // re-derived here from a state read before the call, which is
            // the pattern that produced two of this project's worst defects.
            Action::Press(Button::Cue) => self.cued(self.transport.cue_down())?,
            Action::Release(Button::Cue) => self.cued(self.transport.cue_up())?,

            Action::HoldStart(b @ (Button::Ff | Button::Rew)) => {
                self.was_playing = self.transport.rate() != RATE_PAUSED;
                self.transport.begin_seek(b == Button::Ff);
            }
            Action::HoldEnd(Button::Ff | Button::Rew) => {
                self.transport.end_seek(self.was_playing);
            }

            // **A tap acts on the playing track's folder position, not on the
            // selection** — `src/loaded.rs`, settled by the user. `None` at a
            // folder boundary is [#12](https://github.com/tamatebox/deck-pi/issues/12)'s
            // answer: do nothing, which is stopping.
            Action::Tap(b @ (Button::Ff | Button::Rew)) => {
                let step = if b == Button::Ff {
                    Step::Next
                } else {
                    Step::Previous
                };
                if let Some(next) = self.loaded.neighbour(step).map_err(DeckError::Browse)? {
                    self.load(&next)?;
                }
            }

            Action::Press(Button::Enter) => self.enter()?,
            Action::Press(Button::Back) => {
                if let Some(b) = self.browser.as_mut() {
                    b.back().map_err(DeckError::Browse)?;
                }
            }
            Action::Browse(detents) => {
                if let Some(b) = self.browser.as_mut() {
                    for _ in 0..detents.unsigned_abs() {
                        if detents > 0 {
                            b.select_next();
                        } else {
                            b.select_prev();
                        }
                    }
                }
            }

            // **Listed rather than swept into a catch-all.** `Button::
            // discipline` is the authority for what the decoder can emit:
            // `Release` only for CUE, tap-or-hold only for FF and REW. A
            // catch-all here would be the arm that made PAUSE fatal in
            // `app::audio::run`, and the check that found that one is asking
            // what the arm actually catches.
            //
            // **The compiler found two I had missed** on the first attempt —
            // a plain `Press` of FF or REW, which are tap-or-hold and emit
            // neither. That is the whole argument in one line: a catch-all
            // compiles, and an enumeration is checked.
            other @ (Action::Press(Button::Ff | Button::Rew)
            | Action::Release(_)
            | Action::Tap(_)
            | Action::HoldStart(_)
            | Action::HoldEnd(_)) => {
                debug_assert!(
                    false,
                    "the decoder cannot emit {other:?} — see Button::discipline"
                );
            }
        }
        Ok(())
    }

    /// What a CUE press or release left the deck owing.
    fn cued(&mut self, what: Cued) -> Result<(), DeckError> {
        match what {
            // Setting Cue. **Through `Loaded`**, so the point cannot be keyed
            // on the browser's selection — the defect `src/loaded.rs` exists
            // to prevent, which was silent and atomic and returned `Ok`.
            Cued::Set(frame) => {
                if let Some(store) = self.cues.as_mut() {
                    self.loaded
                        .set_cue(store, frame)
                        .map_err(DeckError::Cue)?;
                }
            }
            // Back Cue queued a seek. The window has to be told it is a jump.
            Cued::Returned(to) => {
                if let Some(p) = self.playing.as_ref() {
                    p.relocate(to);
                }
            }
            Cued::Previewing | Cued::Nothing => {}
        }
        Ok(())
    }

    fn enter(&mut self) -> Result<(), DeckError> {
        let Some(browser) = self.browser.as_mut() else {
            return Ok(());
        };
        match browser.enter().map_err(DeckError::Browse)? {
            Activation::Play(info) => {
                let path = info.path.clone();
                self.load(&path)
            }
            // The row already carries the reason and the display already
            // shows it, so there is nothing to say that is not on screen.
            // `decisions.md`: say *why*, not just *that* — said on highlight,
            // before PLAY is reachable at all.
            Activation::Descended
            | Activation::Refused(_)
            | Activation::Unreadable(_)
            | Activation::Nothing => Ok(()),
        }
    }

    /// Unloads whatever is playing and loads `path`, paused at frame zero.
    ///
    /// **Unload first, always.** The output device is exclusive, and a load
    /// that opened a second sink before dropping the first would fail on the
    /// real hardware and succeed everywhere it is tested.
    pub fn load(&mut self, path: &Path) -> Result<(), DeckError> {
        self.unload();
        let Deck {
            transport,
            loaded,
            cues,
            config,
            open_sink,
            playing,
            ..
        } = self;
        let started = track::load(path, transport, loaded, cues.as_ref(), config, |info| {
            open_sink(info)
        })
        .map_err(DeckError::Load)?;
        *playing = Some(started);
        Ok(())
    }

    /// Stops and lets go. Idempotent.
    pub fn unload(&mut self) -> Option<Ended> {
        Some(self.playing.take()?.unload(&mut self.loaded))
    }
}
