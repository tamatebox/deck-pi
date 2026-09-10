//! The track lifecycle — load, play, unload — driven the way the control
//! thread will drive it.
//!
//! Stage 1 put the audio loop under test. This is the layer above it: the two
//! threads and the sink being created together, the transport and `Loaded`
//! being moved with them, and both threads being joined again. Everything
//! here is a *caller's* obligation, which is the category every defect the
//! last review found belonged to.
//!
//! **One test here was probabilistic and is not any more, and the change is
//! worth more than the test.** `the_thread_survives_the_end_of_the_track_and_a_cue_back_into_it`
//! found a lost PLAY press after a Back Cue at the end of a track. With the
//! end-of-track pause still being made on the **audio** thread, removing the
//! guard for it turned this test red in **4 runs of 10** — the defect needed
//! a fill in flight across two control-thread writes, so a clean run was an
//! ordinary outcome with the bug present, and the test was evidence only in
//! bulk. Moving the decision to the control thread, where the transport's own
//! documentation always said it belonged, makes the same removal red **10 of
//! 10**: the race became an ordering question on one thread. A defect that
//! can only be caught statistically is usually a defect in the wrong place.
//!
//! **The probe sink is atomics only.** It has to be readable while the audio
//! thread owns it, and a `Mutex` in the period path would be a lock inside
//! `no_alloc` on a thread that may not take one — so what the tests assert on
//! is counters. It also means these tests keep the allocation check they
//! inherit from `app::audio::run`: this file links the crate, so the
//! `#[global_allocator]` is present and a debug build aborts if the loop ever
//! allocates.

mod fixtures;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use deck_pi::app::track::{self, Config, LoadError};
use deck_pi::cue::CueStore;
use deck_pi::loaded::Loaded;
use deck_pi::sink::{AudioSink, SampleFormat, SinkError, SinkParams, SINK_CHANNELS};
use deck_pi::transport::{State, Transport};
use fixtures::{Bits, Kind, Scratch};

/// 8,192 frames of capacity, so a short fixture is resident in one piece and
/// a seek back to the start needs no relocation. Sizing the window is
/// [#9](https://github.com/tamatebox/deck-pi/issues/9) and nothing here
/// depends on the answer.
const WINDOW_BYTES: usize = 8_192 * deck_pi::ring::RING_FRAME_BYTES;
const PERIOD: usize = 128;
const RATE: u32 = 44_100;
const VOLUME: &str = "1A2B-3C4D";

// ---------------------------------------------------------------- the probe

/// What the sink saw, readable from the control thread while the audio thread
/// holds the sink itself.
#[derive(Clone, Default)]
struct Tally {
    frames: Arc<AtomicU64>,
    /// Periods carrying at least one non-zero sample. Silence is not audio,
    /// and a deck that "plays" silence is the failure this distinguishes.
    loud: Arc<AtomicU64>,
    drained: Arc<AtomicBool>,
}

impl Tally {
    fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }
    fn loud(&self) -> u64 {
        self.loud.load(Ordering::Relaxed)
    }
}

struct ProbeSink {
    params: SinkParams,
    tally: Tally,
    panic_on_drain: bool,
}

impl ProbeSink {
    fn at(rate: u32, tally: &Tally) -> ProbeSink {
        ProbeSink {
            params: SinkParams {
                rate,
                channels: SINK_CHANNELS as u32,
                format: SampleFormat::S24Le,
                period_frames: PERIOD,
                periods: 2,
            },
            tally: tally.clone(),
            panic_on_drain: false,
        }
    }
}

impl AudioSink for ProbeSink {
    fn params(&self) -> SinkParams {
        self.params
    }

    /// No device, so nothing runs dry — the same answer `CaptureSink` gives,
    /// and for the same reason. It also keeps these tests from recording the
    /// silence a paused deck produces.
    fn starves_if_not_fed(&self) -> bool {
        false
    }

    fn write_period(&mut self, period: &[i32]) -> Result<(), SinkError> {
        self.tally
            .frames
            .fetch_add((period.len() / SINK_CHANNELS) as u64, Ordering::Relaxed);
        if period.iter().any(|s| *s != 0) {
            self.tally.loud.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    fn drain(&mut self) -> Result<(), SinkError> {
        self.tally.drained.store(true, Ordering::Release);
        assert!(!self.panic_on_drain, "the probe was asked to panic here");
        Ok(())
    }
}

// ----------------------------------------------------------------- fixtures

struct Rig {
    scratch: Scratch,
    medium: PathBuf,
    state: PathBuf,
    transport: Arc<Transport>,
    loaded: Loaded,
}

fn rig(tag: &str) -> Rig {
    let scratch = Scratch::new(tag);
    let medium = scratch.dir.join("medium");
    let state = scratch.dir.join("state");
    std::fs::create_dir_all(&medium).expect("medium");
    std::fs::create_dir_all(&state).expect("state");
    Rig {
        scratch,
        medium,
        state,
        transport: Arc::new(Transport::new()),
        loaded: Loaded::nothing(),
    }
}

impl Rig {
    /// A playable WAV on the medium.
    fn track(&self, name: &str, frames: usize) -> PathBuf {
        let samples = fixtures::signal(Bits::S24, 2, frames);
        let bytes = fixtures::build(Kind::Wav, &samples, Bits::S24, RATE, 2);
        fixtures::write(&self.medium, name, Kind::Wav, &bytes)
    }

    fn cues(&self) -> CueStore {
        CueStore::load(&self.state, VOLUME, &self.medium).expect("cue store")
    }

    fn load(&mut self, path: &Path, tally: &Tally) -> Result<track::Playing<ProbeSink>, LoadError> {
        self.load_with(path, None, tally, RATE, false)
    }

    fn load_with(
        &mut self,
        path: &Path,
        cues: Option<&CueStore>,
        tally: &Tally,
        sink_rate: u32,
        panic_on_drain: bool,
    ) -> Result<track::Playing<ProbeSink>, LoadError> {
        let config = Config {
            window_bytes: WINDOW_BYTES,
            rt: None,
        };
        let tally = tally.clone();
        track::load(path, &self.transport, &mut self.loaded, cues, &config, |_| {
            let mut s = ProbeSink::at(sink_rate, &tally);
            s.panic_on_drain = panic_on_drain;
            Ok(s)
        })
    }
}

/// Polls until `cond` holds. Everything here is another thread's work, and
/// the alternative — sleeping a fixed time and hoping — is what makes a suite
/// slow and flaky at once.
fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("timed out waiting for {what}");
}

// -------------------------------------------------------------------- tests

#[test]
fn loading_a_track_with_a_stored_cue_leaves_it_intact() {
    // `decisions.md`: "Loading a track waits at frame zero, not at its stored
    // cue" — the point is restored into the transport and available to Back
    // Cue, and the playhead still starts at the beginning. Both halves are
    // asserted here, because either one alone is satisfied by a bug: reading
    // no cue at all passes the position check, and jumping to the cue passes
    // the cue check.
    let mut r = rig("track-cue-intact");
    let path = r.track("piece", 4_000);
    let mut cues = r.cues();
    cues.set(&path, 1_234).expect("set");

    let tally = Tally::default();
    let playing = r.load_with(&path, Some(&cues), &tally, RATE, false).expect("loads");

    assert_eq!(r.transport.cue_point(), 1_234, "the stored point is restored");
    assert_eq!(r.transport.position(), 0.0, "and the deck waits at frame zero");
    assert_eq!(r.transport.state(), State::Paused, "loaded, not playing");
    assert_eq!(r.loaded.path(), Some(path.as_path()));
    assert_eq!(playing.info().rate, RATE);

    // And the store still holds it. A load that rewrote the cue — by saving
    // the transport's zero back, say — would pass every assertion above and
    // lose the point on the next power cycle.
    assert_eq!(cues.get(&path).expect("get"), 1_234);

    playing.unload(&mut r.loaded);
    drop(r.scratch);
}

#[test]
fn a_second_load_does_not_inherit_the_first_tracks_cue() {
    // One `Transport` per deck, so its cue field outlives the track. An
    // uncued track loading onto a cued one's point would send Back Cue into
    // the middle of a file nobody marked.
    let mut r = rig("track-cue-leak");
    let first = r.track("first", 2_000);
    let second = r.track("second", 2_000);
    let mut cues = r.cues();
    cues.set(&first, 900).expect("set");

    let tally = Tally::default();
    let a = r.load_with(&first, Some(&cues), &tally, RATE, false).expect("loads a");
    assert_eq!(r.transport.cue_point(), 900);
    a.unload(&mut r.loaded);

    let b = r.load_with(&second, Some(&cues), &tally, RATE, false).expect("loads b");
    assert_eq!(r.transport.cue_point(), 0, "the second track has no cue of its own");
    b.unload(&mut r.loaded);
    drop(r.scratch);
}

#[test]
fn unloading_is_the_transition_into_stopped_the_deck_never_had() {
    // `State::Stopped` was constructed and never stored — a one-way door,
    // catalogued in `implementation.md` as the shape that a grep for
    // construction sites reports healthy. This is the transition, and it is
    // the whole reason the lifecycle owns both objects at once: the transport
    // saying `Stopped` while `Loaded` still holds a path would be two answers
    // to "is anything loaded".
    let mut r = rig("track-unload");
    let path = r.track("piece", 2_000);
    let tally = Tally::default();

    let playing = r.load(&path, &tally).expect("loads");
    r.transport.play();
    assert_eq!(r.transport.state(), State::Playing);
    assert!(r.loaded.path().is_some());

    let ended = playing.unload(&mut r.loaded);
    assert_eq!(r.transport.state(), State::Stopped, "nothing loaded");
    assert_eq!(r.loaded.path(), None, "and `Loaded` agrees");
    assert_eq!(r.transport.cue_point(), 0, "the cue went with the track");
    assert_eq!(ended.failure, None, "the medium was there throughout");
    assert!(tally.drained.load(Ordering::Acquire), "the device is drained");
    drop(r.scratch);
}

#[test]
fn a_paused_deck_keeps_its_audio_thread() {
    // **The regression this pins was live until stage 2.** The loop's last
    // match arm was a catch-all that reported `Unexpected`, so `Outcome::
    // Paused` and `Outcome::Seeking` ended the run — and a track loads
    // paused, so the thread would have died before its first audible period.
    // Nothing caught it because the only caller was a bring-up CLI that plays
    // one file and never touches a control.
    let mut r = rig("track-paused");
    let path = r.track("piece", 2_000);
    let tally = Tally::default();
    let playing = r.load(&path, &tally).expect("loads");

    // Paused from the load, then a held seek, which is the other outcome that
    // was fatal. Neither may end the run.
    std::thread::sleep(Duration::from_millis(20));
    assert!(!playing.finished(), "PAUSE must not end the run");
    r.transport.begin_seek(true);
    std::thread::sleep(Duration::from_millis(20));
    assert!(!playing.finished(), "a held FF must not end the run");
    r.transport.end_seek(false);

    let ended = playing.unload(&mut r.loaded);
    assert_eq!(
        ended.why,
        deck_pi::app::audio::Stopped::Asked,
        "the run ended because it was asked to, not because the deck paused"
    );
    assert_eq!(tally.loud(), 0, "and nothing was emitted while it sat there");
    drop(r.scratch);
}

#[test]
fn the_thread_survives_the_end_of_the_track_and_a_cue_back_into_it() {
    // The audio thread is per *track*, not per *play*. A deck sitting on the
    // last frame is still loaded — `decisions.md` says a track that reaches
    // its end stops and nothing advances on its own — so CUE and PLAY have to
    // work from there, which means the device stays open and the ring stays
    // filled.
    let mut r = rig("track-end");
    let frames = 2_000u64;
    let path = r.track("piece", frames as usize);
    let tally = Tally::default();
    let mut playing = r.load(&path, &tally).expect("loads");

    r.transport.play();
    wait_for("the track to play out", || tally.frames() >= frames);

    // **`service` is the control loop, and this test is standing in for
    // it.** The audio thread publishes the position and decides nothing; the
    // pause at the end is the control thread's, because it is the only
    // thread allowed to write control state.
    wait_for("the control loop to pause the deck", || {
        playing.service();
        r.transport.state() == State::Paused
    });
    assert!(!playing.finished(), "the end of the track is not the end of the run");

    // Pressing PLAY at the end must not leave the deck reading `Playing` over
    // silence: nothing latches, so the next service pauses it again.
    r.transport.play();
    wait_for("PLAY at the end to be refused", || {
        playing.service();
        r.transport.state() == State::Paused
    });

    // Back Cue to the point, then play. The cue is frame zero here, which is
    // its unset value.
    // **Back Cue and PLAY are two writes, and a fill can land between them.**
    // This is the gesture that found the `reached_end` staleness bug: the
    // in-flight fill decided against the old position, and the loop's report
    // paused the deck the operator had just started. Left as two calls,
    // deliberately — it is what the panel does.
    let before = tally.loud();
    r.transport.back_cue();
    r.transport.play();
    wait_for("audio to flow again", || {
        playing.service();
        tally.loud() > before + 2
    });

    playing.unload(&mut r.loaded);
    drop(r.scratch);
}

#[test]
fn a_sink_opened_at_the_wrong_rate_is_refused_and_nothing_is_mutated() {
    // `CLAUDE.md`: the output rate follows the source, per track. A sink at
    // another rate plays the samples untouched and at the wrong speed, and no
    // layer below can tell — `verify_in_force` checks ALSA against what was
    // asked for, and the wrong thing was asked for.
    let mut r = rig("track-wrong-rate");
    let path = r.track("piece", 2_000);
    let tally = Tally::default();

    let err = r
        .load_with(&path, None, &tally, 48_000, false)
        .expect_err("a 48 kHz sink for a 44.1 kHz track");
    assert!(
        matches!(err, LoadError::WrongRate { track: 44_100, sink: 48_000 }),
        "got {err:?}"
    );
    assert_eq!(r.loaded.path(), None, "a refused load loads nothing");
    assert_eq!(r.transport.state(), State::Stopped);
    assert_eq!(tally.frames(), 0, "and the sink was never fed");
    drop(r.scratch);
}

#[test]
fn a_file_that_will_not_open_leaves_the_deck_exactly_as_it_was() {
    let mut r = rig("track-unopenable");
    let missing = r.medium.join("not-here.wav");
    let tally = Tally::default();

    let err = r.load(&missing, &tally).expect_err("no such file");
    assert!(matches!(err, LoadError::Open(_)), "got {err:?}");
    assert_eq!(r.loaded.path(), None);
    assert_eq!(r.transport.state(), State::Stopped, "still nothing loaded");
    drop(r.scratch);
}

#[test]
fn a_track_outside_the_medium_is_refused_rather_than_played_without_cues() {
    // The cue key is the path relative to the mount point, so a path from
    // somewhere else has no key. Playing anyway would mean a cue set later
    // goes nowhere — the silent-wrong-key defect `src/loaded.rs` exists to
    // prevent, arriving by another door.
    let mut r = rig("track-outside");
    let outside = r.scratch.dir.join("elsewhere");
    std::fs::create_dir_all(&outside).expect("elsewhere");
    let samples = fixtures::signal(Bits::S24, 2, 1_000);
    let bytes = fixtures::build(Kind::Wav, &samples, Bits::S24, RATE, 2);
    let path = fixtures::write(&outside, "stray", Kind::Wav, &bytes);

    let cues = r.cues();
    let tally = Tally::default();
    let err = r
        .load_with(&path, Some(&cues), &tally, RATE, false)
        .expect_err("no key for a path off the medium");
    assert!(matches!(err, LoadError::Cue(_)), "got {err:?}");
    assert_eq!(r.loaded.path(), None, "the rollback has to be complete");
    assert_eq!(r.transport.state(), State::Stopped);
    drop(r.scratch);
}

#[test]
fn dropping_a_playing_stops_both_of_its_threads() {
    // Without this, a `Playing` that went out of scope would leave a window
    // thread filling a ring nobody reads and an audio thread writing silence
    // into a real device for the rest of the process's life, with no handle
    // left to stop either.
    let mut r = rig("track-drop");
    let path = r.track("piece", 2_000);
    let tally = Tally::default();

    let playing = r.load(&path, &tally).expect("loads");
    r.transport.play();
    wait_for("playback to start", || tally.loud() > 0);
    drop(playing);

    assert!(
        tally.drained.load(Ordering::Acquire),
        "the audio thread ran to its end and drained the sink"
    );
    assert_eq!(
        r.transport.state(),
        State::Stopped,
        "a dropped track is not a loaded one"
    );
    // `Loaded` is the one thing `Drop` cannot reach — it has no handle on it —
    // so this is the difference between dropping and unloading, stated rather
    // than discovered.
    assert!(r.loaded.path().is_some());
    r.loaded.unload();
    drop(r.scratch);
}

#[test]
fn an_audio_thread_that_panics_is_reported_rather_than_silently_gone() {
    // The panic is raised in `drain`, which runs **outside** the `no_alloc`
    // region. A panic inside it would abort the process rather than unwind —
    // boxing the payload allocates — which is the allocator guard working as
    // intended and is why the probe panics where it does. The branch under
    // test is `join` returning `Err`, and it does not care where the panic
    // came from.
    //
    // The panic message this prints is the test working.
    let mut r = rig("track-panic");
    let path = r.track("piece", 1_000);
    let tally = Tally::default();
    let playing = r
        .load_with(&path, None, &tally, RATE, true)
        .expect("loads");

    let ended = playing.unload(&mut r.loaded);
    assert!(
        matches!(&ended.why, deck_pi::app::audio::Stopped::Unexpected(m) if m.contains("panicked")),
        "got {:?}",
        ended.why
    );
    assert_eq!(r.transport.state(), State::Stopped, "the deck still unloaded");
    assert_eq!(r.loaded.path(), None);
    drop(r.scratch);
}
