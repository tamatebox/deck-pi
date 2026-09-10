//! The audio loop, driven directly.
//!
//! It lived in `src/main.rs` until now, which meant the one place every
//! obligation in this project converges — the caller — was the one place
//! nothing could test. Every defect the last review found was a caller failing
//! to hold up a rule stated elsewhere, so that was the wrong thing to leave
//! untestable.
//!
//! **Every assertion here also checks that the loop allocates nothing**, for
//! free and without saying so: `app::audio::run` wraps its per-period body in
//! `deck_pi::no_alloc`, so a debug build aborts if it ever does. That is the
//! difference between `callback_rules.rs`, which asserts it of the pieces, and
//! this, which exercises the way they are assembled.

mod fixtures;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use deck_pi::app::audio::{self, Deck, Stopped};
use deck_pi::engine::Engine;
use deck_pi::sink::CaptureSink;
use deck_pi::transport::{State, Transport, RATE_PAUSED};
use deck_pi::window::{Command, Event, Window};
use fixtures::{Bits, Kind, Scratch};

const WINDOW_BYTES: usize = 64 * deck_pi::ring::RING_FRAME_BYTES * 64;
const PERIOD: usize = 256;

/// The window thread, its failure flag, and a deck ready to run.
struct Rig {
    scratch: Scratch,
    frames: u64,
    lost: Arc<AtomicBool>,
    tx: mpsc::Sender<Command>,
    thread: std::thread::JoinHandle<()>,
    transport: Arc<Transport>,
}

fn rig(tag: &str, frames: usize) -> (Rig, Deck<CaptureSink>) {
    let scratch = Scratch::new(tag);
    let samples = fixtures::signal(Bits::S24, 2, frames);
    let bytes = fixtures::build(Kind::Wav, &samples, Bits::S24, 44_100, 2);
    let path = fixtures::write(&scratch.dir, "track", Kind::Wav, &bytes);

    let (window, reader, info) = Window::load(&path, WINDOW_BYTES).expect("loads");
    let lost = Arc::new(AtomicBool::new(false));
    let l = Arc::clone(&lost);
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        window.run(rx, move |e| {
            if matches!(e, Event::Failed(_)) {
                l.store(true, Ordering::Release);
            }
        })
    });

    let transport = Arc::new(Transport::new());
    transport.play();
    let deck = Deck {
        transport: Arc::clone(&transport),
        reader,
        engine: Engine::new(info.frames),
        sink: CaptureSink::new(info.rate, PERIOD, frames + PERIOD),
    };
    (
        Rig { scratch, frames: info.frames, lost, tx, thread, transport },
        deck,
    )
}

impl Rig {
    fn finish(self) {
        let _ = self.tx.send(Command::Shutdown);
        let _ = self.thread.join();
        drop(self.scratch);
    }
}

#[test]
fn a_track_plays_out_and_leaves_the_deck_stopped() {
    let (r, deck) = rig("app-audio-plays", 20_000);
    let stop = AtomicBool::new(false);

    let (deck, why, report) = audio::run(deck, &stop, &r.lost, None);
    assert_eq!(why, Stopped::EndOfTrack);
    assert_eq!(report.frames, r.frames, "every frame must reach the sink");
    assert!(report.peak > 0, "the fixture must contain audio");
    assert!(deck.sink.was_drained(), "the device is drained on the way out");

    // **The loop tells the transport.** That wiring is the whole of what was
    // missing when `reached_end` had a doc comment, a design-document sentence
    // and no callers but its own tests.
    assert_eq!(r.transport.rate(), RATE_PAUSED);
    assert_eq!(r.transport.state(), State::Paused);

    drop(deck);
    r.finish();
}

#[test]
fn being_asked_to_stop_ends_the_run_and_hands_everything_back() {
    // The deck is handed back so the **control thread** drops it. The ring's
    // last `Arc<Shared>` goes with the reader, and freeing 64 MiB on the audio
    // thread is the hazard `implementation.md` names — so `run` taking the
    // deck by value and returning it is load-bearing, not a style.
    let (r, deck) = rig("app-audio-stop", 400_000);
    let stop = AtomicBool::new(true);

    let (deck, why, report) = audio::run(deck, &stop, &r.lost, None);
    assert_eq!(why, Stopped::Asked);
    assert_eq!(report.frames, 0, "asked before the first period");
    // Handed back, so it can be dropped here rather than there.
    drop(deck);
    r.finish();
}

#[test]
fn a_medium_that_goes_away_ends_the_run_rather_than_the_track() {
    // What is resident plays out and then the loop reports the medium rather
    // than the end of the track. Distinguishing those two is the whole of the
    // `sf_error` fix — a pulled stick used to arrive as `EndOfTrack`.
    let (r, deck) = rig("app-audio-lost", 400_000);
    let stop = AtomicBool::new(false);
    // Set before the run so it is seen on the first miss, which is
    // deterministic; racing a real removal is `medium_loss_test.rs`'s job.
    r.lost.store(true, Ordering::Release);

    let (deck, why, _report) = audio::run(deck, &stop, &r.lost, None);
    assert_eq!(
        why,
        Stopped::MediumLost,
        "a lost medium is not the track ending"
    );
    drop(deck);
    r.finish();
}
