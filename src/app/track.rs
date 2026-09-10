//! The track lifecycle: three owned things, created together and joined
//! together.
//!
//! Loading a track builds a window thread, an audio thread and a sink, and
//! nothing else on the deck has that shape — everything else is either
//! process-lifetime (the browser, media watch, the cue store) or a value
//! (`Loaded`, the transport's fields). `decisions.md` is what makes the three
//! per-track rather than per-process:
//!
//! > a track change always contains a pause, which is what lets the audio
//! > thread, window thread and sink be per-track and every drop happen off
//! > the deadline.
//!
//! Read that as a **dependency**, not a description. It holds because an
//! FF/REW tap loads the next track and waits at its head, so a load never
//! lands inside audible playback. If that is ever changed to load-and-play,
//! this module is what has to be revisited, and the pause is the thing that
//! was load-bearing.
//!
//! # Per *track*, not per *play*
//!
//! The threads live as long as the track is loaded, so PAUSE, the end of the
//! track and a Back Cue back into it all happen inside one run — see
//! [`audio::AtEnd`]. What ends a run is unloading, the medium going, or a
//! fault. That is also why [`Playing::finished`] means something worth
//! polling: under [`AtEnd::Idle`] the thread does not exit on its own unless
//! something went wrong.
//!
//! # The medium going away arrives as the run ending
//!
//! The window thread sets a flag the audio loop polls, what is resident plays
//! out, and the loop then stops with [`Stopped::MediumLost`] — so the control
//! thread learns through [`Playing::finished`] and reads the text out of
//! [`Ended::failure`]. That is up to the forward half of the window late,
//! which is fine for a control loop with nothing to do differently and **not**
//! fine for a display that would go on showing a playing deck. The earlier
//! signal is the same flag read directly; it is deliberately not exposed yet,
//! because an accessor with no caller is the shape `implementation.md`
//! catalogues first. Media watch and the display are what will want it.
//!
//! # One obligation this module now owns and does not yet meet
//!
//! `window::Command::Relocate` says "whoever owns the app loop must send this
//! whenever it makes the transport seek outside the resident span, from the
//! control thread". [`Playing`] holds that channel, so the obligation now has
//! an owner — and the owner does not meet it, because deciding to seek is the
//! dispatch stage's job and this one only carries the wiring. What it costs
//! until then is in `src/window.rs`'s module doc, measured: a backwards cue
//! jump is read as a scrub, so the window rebuilds *below* the new position
//! and the cue point — the one frame the callback wants first — arrives last.
//! Stated here rather than left to be re-found, because the sentence that
//! records it lives in another file and addresses a module that did not exist
//! when it was written.
//!
//! # What [`load`] refuses, and what it leaves untouched when it does
//!
//! Nothing outside this function is mutated until every fallible step has
//! passed. A load that fails leaves `Loaded` empty and the transport
//! `Stopped`, rather than a deck that believes it holds a track it could not
//! open. The steps are ordered for that: open the file, open the sink, check
//! the rate, and only then touch the deck.
//!
//! **The rate check is the load-bearing one.** `CLAUDE.md`: "Output sample
//! rate follows the source file, per track. Never resample to a fixed output
//! rate." A sink factory that returns a device opened at some other rate
//! produces audio that is bit-perfect and at the wrong speed — nothing
//! downstream can tell, because `verify_in_force` checks ALSA against *what
//! was asked for*, and the wrong rate was asked for. This is the other half
//! of that check: what was asked for, against the track.
//!
//! **The two are one guarantee in two places, and neither is much use
//! alone**, so they are named together here rather than each looking locally
//! removable: `load` checks the **track against what was asked**, once, before
//! anything is mutated; `AudioSink::verify_in_force` checks **what was asked
//! against what is in force**, once, as soon as the stream is really running.
//! Drop the first and a factory bug plays at the wrong speed with `hw_params`
//! agreeing; drop the second and ALSA can substitute underneath a correct
//! request.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

use crate::app::audio::{self, AtEnd, Report, Stopped};
use crate::cue::{CueError, CueStore};
use crate::engine::Engine;
use crate::file::{OpenError, TrackInfo};
use crate::loaded::Loaded;
use crate::ring;
use crate::rt::RtRequest;
use crate::sink::{AudioSink, SinkError};
use crate::transport::Transport;
use crate::window::{Command, Event, Window};

/// The parts of a load that are the deck's rather than the track's.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// `N` from `architecture.md`'s `min(60 s, N MiB)`, still
    /// [#9](https://github.com/tamatebox/deck-pi/issues/9).
    pub window_bytes: usize,
    /// `None` on a developer desk, where a refused promotion is the ordinary
    /// case and `--rt-check` is where it is an error. `Some` on the deck.
    pub rt: Option<RtRequest>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            window_bytes: ring::WINDOW_BYTES_PLACEHOLDER,
            rt: None,
        }
    }
}

/// Why a load did not happen. Nothing was mutated in any of these cases.
#[derive(Debug)]
pub enum LoadError {
    Open(OpenError),
    Sink(SinkError),
    /// The sink was opened at a rate the track is not. See the module doc:
    /// this is the check that no layer below can make.
    WrongRate {
        track: u32,
        sink: u32,
    },
    /// The cue store has no key for this path — it is not under the medium.
    ///
    /// **Fatal on purpose, and it is worth knowing why the softer answer was
    /// not taken.** Playing anyway with the cue point at zero would mean a
    /// cue set later goes nowhere, silently, which is the defect
    /// `src/loaded.rs` exists to prevent, arriving by another door. It is
    /// also unreachable in the wired deck: the browser is rooted at the mount
    /// point and refuses symlinks that leave it, so every path it can produce
    /// has a key. A caller with no store at all passes `None` and never
    /// reaches this.
    Cue(CueError),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Open(e) => write!(f, "{e}"),
            LoadError::Sink(e) => write!(f, "{e}"),
            LoadError::WrongRate { track, sink } => write!(
                f,
                "the sink opened at {sink} Hz for a {track} Hz track — the \
                 output rate follows the source, per track"
            ),
            LoadError::Cue(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LoadError {}

/// What a run turned out to have done. Returned by [`Playing::unload`].
#[derive(Debug, Clone)]
pub struct Ended {
    pub why: Stopped,
    pub report: Report,
    /// What the window thread said when it failed, if it did. The audio side
    /// only learns *that* the medium went — the text is here.
    pub failure: Option<String>,
}

/// A loaded track: its two threads, its sink, and the flags between them.
///
/// **Dropping one shuts it down**, which is not a courtesy. A `Playing` that
/// went out of scope without being unloaded would leave a window thread
/// filling a ring nobody reads and an audio thread writing silence into a
/// real ALSA device for the rest of the process's life, with no handle left
/// to stop either. [`unload`](Self::unload) is the same shutdown with the
/// answer returned and `Loaded` cleared.
pub struct Playing<S: AudioSink> {
    info: TrackInfo,
    transport: Arc<Transport>,
    stop: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
    commands: mpsc::Sender<Command>,
    window: Option<JoinHandle<()>>,
    audio: Option<JoinHandle<(audio::Deck<S>, Stopped, Report)>>,
    /// Filled by the first shutdown, so `unload` and `Drop` can both run.
    ended: Option<Ended>,
}

/// Opens `path` and starts playing it — paused at frame zero, with its stored
/// cue restored.
///
/// `cues` is `None` when the medium has no UUID to key on, which
/// `src/media.rs` encodes as `Browsable { uuid: Option<String> }` and
/// `decisions.md` records as "a browsable state, not a failure": the stick
/// plays, and what it loses is persistence. The cue point is then zero, which
/// is also its unset value, so nothing else has to know.
///
/// `open_sink` is handed the track's `TrackInfo` because the output rate is
/// only knowable once the header is read. Reopening the device per track is
/// free here — one Pi is one deck, so nothing audible is interrupted.
pub fn load<S, F>(
    path: &Path,
    transport: &Arc<Transport>,
    loaded: &mut Loaded,
    cues: Option<&CueStore>,
    config: &Config,
    open_sink: F,
) -> Result<Playing<S>, LoadError>
where
    S: AudioSink + Send + 'static,
    F: FnOnce(&TrackInfo) -> Result<S, SinkError>,
{
    let (window, reader, info) =
        Window::load(path, config.window_bytes).map_err(LoadError::Open)?;
    let sink = open_sink(&info).map_err(LoadError::Sink)?;
    let opened_at = sink.params().rate;
    if opened_at != info.rate {
        return Err(LoadError::WrongRate {
            track: info.rate,
            sink: opened_at,
        });
    }

    // Past here nothing may fail, because past here the deck has changed.
    loaded.load(Box::new(info.clone()));
    let cue = match cues {
        Some(store) => match loaded.cue(store) {
            Ok(frame) => frame,
            Err(e) => {
                loaded.unload();
                return Err(LoadError::Cue(e));
            }
        },
        None => 0,
    };
    transport.track_loaded(cue);

    // The window thread's failure has to reach the audio thread without a
    // lock on the deadline, so the flag is an atomic and the text — which
    // only the control thread reads, after the join — is beside it.
    let lost = Arc::new(AtomicBool::new(false));
    let failure = Arc::new(Mutex::new(None::<String>));
    let (l, w) = (Arc::clone(&lost), Arc::clone(&failure));
    let (commands, rx) = mpsc::channel();
    let window_thread = std::thread::spawn(move || {
        window.run(rx, move |e| {
            if let Event::Failed(msg) = e {
                if let Ok(mut slot) = w.lock() {
                    *slot = Some(msg);
                }
                l.store(true, Ordering::Release);
            }
        })
    });

    let deck = audio::Deck {
        transport: Arc::clone(transport),
        reader,
        // From the transport, so a load has one answer to "where does this
        // start" rather than two that agree today and need not tomorrow.
        engine: Engine::at(info.frames, transport.position()),
        sink,
        at_end: AtEnd::Idle,
    };
    let stop = Arc::new(AtomicBool::new(false));
    // `lost` is not kept here: its two readers are the threads, and a field
    // nothing reads is the accessor-with-no-caller shape wearing a different
    // hat. Media watch will want it back — see the module doc.
    let (s, m) = (Arc::clone(&stop), lost);
    let rt = config.rt;
    // **Spawned after the window thread, and it promotes itself.** `rt::apply`
    // runs as `run`'s first act on this new thread; doing it in the caller
    // would hand `SCHED_FIFO` to every thread spawned afterwards, the window
    // thread included, because glibc's `pthread_create` defaults to
    // `PTHREAD_INHERIT_SCHED`.
    let audio_thread = std::thread::spawn(move || audio::run(deck, &s, &m, rt.as_ref()));

    Ok(Playing {
        info,
        transport: Arc::clone(transport),
        stop,
        failure,
        commands,
        window: Some(window_thread),
        audio: Some(audio_thread),
        ended: None,
    })
}

impl<S: AudioSink> Playing<S> {
    pub fn info(&self) -> &TrackInfo {
        &self.info
    }

    /// The audio thread has exited.
    ///
    /// **Under [`AtEnd::Idle`] this is not the end of the track**, which the
    /// thread survives. It means the medium went, the loop was asked to stop,
    /// or something faulted — in every case the deck cannot make sound again
    /// until it is unloaded and something else loaded. Poll it from the
    /// control loop; there is nothing to block on, and blocking is what a
    /// control loop must not do.
    pub fn finished(&self) -> bool {
        match &self.audio {
            Some(h) => h.is_finished(),
            None => true,
        }
    }

    /// One turn of the control loop's obligations to this track.
    ///
    /// **Call it every iteration.** Today it is one thing: pausing the deck
    /// when the track has played out. That has to happen on the control
    /// thread, because `Transport`'s control methods are read-modify-write
    /// across several atomics and are safe against each other only while one
    /// thread performs them — `Transport::reached_end` carries the full
    /// argument and the scar. The audio thread's part is to publish the
    /// position, which it does every period.
    ///
    /// The lag is one iteration of whatever the control loop blocks on,
    /// which is the input poll. The *audio* is already silent — the engine
    /// serves silence past the last frame regardless — so what lags is the
    /// display's reading, by a poll interval.
    ///
    /// **Its production caller does not exist yet**: the control loop is the
    /// next stage, and until then this module's caller is its tests. That is
    /// true of [`load`] as well, so it is said once here rather than filed
    /// against each function.
    ///
    /// **`&mut self` is a fence, not a signature.** "The control thread and
    /// only the control thread" is the precondition this whole arrangement
    /// turns on, and `&self` would let two threads call it at once while
    /// `&mut self` makes that unavailable. It costs one `mut` in the tests
    /// and no test surface, which is `decisions.md`'s deciding question.
    pub fn service(&mut self) {
        if self.transport.rate() == crate::transport::RATE_PAUSED {
            return;
        }
        // **`settled_position`, not `position`** — the pair has to be read in
        // the mirror of the order the callback writes it, and taking it the
        // wrong way round is exactly the defect this function was built to
        // fix, one layer up. `None` means a seek is in flight and the
        // question is answered next period.
        if let Some(at) = self.transport.settled_position() {
            if at >= self.info.frames as f64 {
                self.transport.reached_end();
            }
        }
    }

    /// Stops both threads and hands back what the run did.
    ///
    /// The transport goes to `Stopped` and `Loaded` is emptied, which is the
    /// pair of transitions the deck had no way to make until this existed.
    pub fn unload(mut self, loaded: &mut Loaded) -> Ended {
        let ended = self.shutdown();
        loaded.unload();
        ended
    }

    /// Idempotent, because [`Drop`] and [`unload`](Self::unload) both run it.
    ///
    /// **And it must stay panic-free.** `Drop` can run during an unwind, and
    /// a panic there is a double panic, which aborts rather than propagates.
    /// It holds today by construction — every fallible step is `let _ =`,
    /// `.ok()`, or a matched `Result`, and there is no `unwrap` — which is
    /// invisible unless said, and one added `.unwrap()` turns a recoverable
    /// panic into an abort with nothing going red.
    fn shutdown(&mut self) -> Ended {
        if let Some(done) = &self.ended {
            return done.clone();
        }

        // Audio first. Shutting the window thread down first would leave the
        // audio loop missing every period until it noticed, which is noise in
        // the report for nothing.
        self.stop.store(true, Ordering::Release);
        let (why, report) = match self.audio.take().map(|h| h.join()) {
            Some(Ok((deck, why, report))) => {
                // **Here, on the control thread.** The ring's last
                // `Arc<Shared>` goes with the reader, so this frees 64 MiB —
                // and doing it on the audio thread is the hazard
                // `implementation.md` names under the language-specific note.
                drop(deck);
                (why, report)
            }
            // The deck went with it and was dropped during the unwind, on
            // that thread. Nothing can be recovered, so the report is empty
            // and says so rather than reading as a run that did nothing.
            Some(Err(_)) => (
                Stopped::Unexpected("the audio thread panicked".into()),
                Report::default(),
            ),
            None => (Stopped::Asked, Report::default()),
        };

        let _ = self.commands.send(Command::Shutdown);
        if let Some(h) = self.window.take() {
            let _ = h.join();
        }

        self.transport.track_unloaded();
        let ended = Ended {
            why,
            report,
            failure: self.failure.lock().ok().and_then(|g| g.clone()),
        };
        self.ended = Some(ended.clone());
        ended
    }
}

/// Hand-written because the sink need not be `Debug` and the join handles
/// say nothing useful. What a reader wants is which track and whether it is
/// still running.
impl<S: AudioSink> std::fmt::Debug for Playing<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Playing")
            .field("track", &self.info.path)
            .field("finished", &self.finished())
            .finish()
    }
}

impl<S: AudioSink> Drop for Playing<S> {
    fn drop(&mut self) {
        self.shutdown();
    }
}
