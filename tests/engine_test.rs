//! The whole v1 software path, end to end: file -> libsndfile -> window
//! thread -> ring -> callback -> output period.
//!
//! This is the null test extended through the engine, and it is the direct
//! check on `architecture.md`'s claim that "v1 is unconditionally
//! bit-perfect". Everything short of handing the buffer to ALSA is here; the
//! hardware half (`hw_params` while playing) needs the Pi.

mod fixtures;

use std::sync::mpsc;
use std::time::{Duration, Instant};

use fixtures::{decode_samples, signal, Bits, Kind, Scratch};

use deck_pi::engine::{Engine, Outcome};
use deck_pi::file::{Depth, RING_CHANNELS};
use deck_pi::ring::{self, Miss};
use deck_pi::sink::{AudioSink, CaptureSink};
use deck_pi::transport::Transport;
use deck_pi::window::{Command, Window};

const WINDOW_BYTES: usize = 2 * 1024 * ring::RING_FRAME_BYTES; // 2048 frames
const PERIOD: usize = 128;

fn depth_of(bits: Bits) -> Depth {
    match bits {
        Bits::S16 => Depth::Int16,
        Bits::S24 => Depth::Int24,
    }
}

/// Plays a whole track through the real arrangement — a window thread filling
/// beside a callback draining into a sink — and returns every sample the sink
/// was handed, in order.
///
/// The sink is where ALSA stands, which is what makes this the null test
/// `implementation.md` describes rather than a parallel one: "play a file,
/// collect the buffers handed to ALSA, and check them against the source".
fn play_to_completion(path: &std::path::Path, frames: u64) -> Vec<i32> {
    let (window, reader, info) = Window::load(path, WINDOW_BYTES).expect("loads");
    assert_eq!(info.frames, frames);

    let (tx, rx) = mpsc::channel();
    let thread = std::thread::spawn(move || window.run(rx, |_| {}));

    let transport = Transport::new();
    transport.track_loaded(0);
    let mut engine = Engine::new(frames);
    transport.play();

    let mut sink = CaptureSink::new(info.rate, PERIOD, frames as usize + PERIOD);
    let mut period = vec![0i32; PERIOD * RING_CHANNELS];
    let deadline = Instant::now() + Duration::from_secs(30);

    loop {
        match engine.fill(&transport, &reader, &mut period) {
            Outcome::Played { frames: n } | Outcome::PlayedTail { frames: n } => {
                // Only the frames that are really the track's go to the
                // device; the silent tail of the last short period does not.
                sink.write_period(&period[..n * RING_CHANNELS])
                    .expect("sink accepted the period");
            }
            Outcome::EndOfTrack => break,
            // **Every miss is transient and none of them is an error.** A real
            // callback emits that period of silence and comes back; the test
            // waits instead, because it is checking content rather than
            // timing.
            //
            // `Relocated` belongs here and used to be treated as a fault. It
            // became common when the ring learned to refuse a read taken
            // while a relocation is in flight — before that the reader could
            // sail through the middle of one and return samples from the
            // window being discarded, which is the bug the seqlock fixed. So
            // the outcome is not new; noticing it is. It arrives at the start
            // of a track, where the window relocates to frame zero.
            Outcome::Missed(_) => std::thread::yield_now(),
            other => panic!("unexpected outcome at frame {}: {:?}", engine.position(), other),
        }
        assert!(Instant::now() < deadline, "playback never completed");
    }

    sink.drain().expect("drain");
    let _ = tx.send(Command::Shutdown);
    let _ = thread.join();

    assert!(sink.was_drained());
    assert_eq!(
        sink.frames_written() as u64,
        frames,
        "the sink was handed the wrong number of frames"
    );
    sink.captured().to_vec()
}

#[test]
fn the_whole_path_is_bit_perfect_for_every_container_and_depth() {
    let scratch = Scratch::new("engine-bitperfect");
    let frames = 5_000usize; // not a multiple of the period, so the tail runs

    for kind in [Kind::Wav, Kind::Aiff, Kind::AiffcSowt, Kind::Rf64] {
        for bits in [Bits::S16, Bits::S24] {
            let source = signal(bits, 2, frames);
            let name = format!("{:?}-{:?}", kind, bits);
            let file = fixtures::build(kind, &source, bits, 44_100, 2);
            let path = fixtures::write(&scratch.dir, &name, kind, &file);

            // Decoded by us, without libsndfile — the independent side.
            let le = kind.sample_endian_is_little();
            let data = fixtures::encode_samples(&source, bits, le);
            let independent = decode_samples(&data, bits, le);
            assert_eq!(independent, source, "{}: our own decoder disagrees", name);

            let played = play_to_completion(&path, frames as u64);
            assert_eq!(
                played.len(),
                frames * RING_CHANNELS,
                "{}: wrong number of samples reached the output",
                name
            );

            let shift = depth_of(bits).left_justify_shift();
            for (i, (&got, &src)) in played.iter().zip(independent.iter()).enumerate() {
                assert_eq!(
                    got,
                    (src << shift) >> 8,
                    "{}: output sample {} — source {:#x}, got {:#x}",
                    name,
                    i,
                    src,
                    got
                );
            }

            // And the same statement as bytes: re-encoding what reached the
            // output reproduces the source's data chunk exactly.
            let recovered: Vec<i32> = played.iter().map(|&s| (s << 8) >> shift).collect();
            assert_eq!(
                fixtures::encode_samples(&recovered, bits, le),
                data,
                "{}: output is not byte-equal to the source data chunk",
                name
            );
        }
    }
}

#[test]
fn the_whole_path_is_bit_perfect_at_all_six_rates() {
    let scratch = Scratch::new("engine-rates");
    let frames = 3_000usize;

    for rate in [44_100u32, 88_200, 176_400, 48_000, 96_000, 192_000] {
        let source = signal(Bits::S24, 2, frames);
        let file = fixtures::build(Kind::Wav, &source, Bits::S24, rate, 2);
        let path = fixtures::write(&scratch.dir, &format!("r{}", rate), Kind::Wav, &file);

        let played = play_to_completion(&path, frames as u64);
        assert_eq!(played.len(), frames * RING_CHANNELS, "{} Hz", rate);
        for (i, (&got, &src)) in played.iter().zip(source.iter()).enumerate() {
            assert_eq!(got, src, "{} Hz sample {}", rate, i);
        }
    }
}

#[test]
fn a_mono_track_reaches_both_channels_identically_through_the_whole_path() {
    let scratch = Scratch::new("engine-mono");
    let frames = 2_000usize;
    let source = signal(Bits::S16, 1, frames);
    let file = fixtures::build(Kind::Wav, &source, Bits::S16, 48_000, 1);
    let path = fixtures::write(&scratch.dir, "mono", Kind::Wav, &file);

    let played = play_to_completion(&path, frames as u64);
    assert_eq!(played.len(), frames * RING_CHANNELS);
    for (f, &src) in source.iter().enumerate() {
        let want = (src << Depth::Int16.left_justify_shift()) >> 8;
        assert_eq!(played[f * 2], want, "frame {} left", f);
        assert_eq!(played[f * 2 + 1], want, "frame {} right", f);
    }
}

#[test]
fn a_silent_seek_produces_no_audio_and_playback_resumes_where_it_left_off() {
    let scratch = Scratch::new("engine-seek");
    let frames = 20_000u64;
    let source = signal(Bits::S24, 2, frames as usize);
    let file = fixtures::build(Kind::Wav, &source, Bits::S24, 44_100, 2);
    let path = fixtures::write(&scratch.dir, "seek", Kind::Wav, &file);

    let (window, reader, _) = Window::load(&path, WINDOW_BYTES).expect("loads");
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::spawn(move || window.run(rx, |_| {}));

    let t = Transport::new();
    t.track_loaded(0);
    let mut e = Engine::new(frames);
    t.play();
    let mut period = vec![0i32; PERIOD * RING_CHANNELS];

    // Play a bit.
    let deadline = Instant::now() + Duration::from_secs(10);
    while e.position() < 1_000.0 {
        e.fill(&t, &reader, &mut period);
        assert!(Instant::now() < deadline, "never got going");
    }

    // Hold FF. Not one sample may be produced while it is held.
    t.begin_seek(true);
    for _ in 0..20 {
        let outcome = e.fill(&t, &reader, &mut period);
        assert_eq!(outcome, Outcome::Seeking);
        assert!(
            period.iter().all(|&s| s == 0),
            "v1's seek must be silent — audible scan needs the resampler"
        );
    }
    let landed = e.position();
    assert!(landed > 1_000.0);
    assert_eq!(landed.fract(), 0.0, "a 4x seek must leave the position integral");

    // Release: audio resumes from exactly where the seek left it.
    t.end_seek(true);
    let at = landed as usize;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match e.fill(&t, &reader, &mut period) {
            Outcome::Played { frames: n } => {
                for i in 0..n * RING_CHANNELS {
                    assert_eq!(
                        period[i],
                        source[at * RING_CHANNELS + i],
                        "resumed at the wrong sample: offset {} from frame {}",
                        i,
                        at
                    );
                }
                break;
            }
            Outcome::Missed(Miss::NotResident) | Outcome::Missed(Miss::Relocated) => {
                std::thread::yield_now()
            }
            other => panic!("after release: {:?}", other),
        }
        assert!(Instant::now() < deadline, "never resumed");
    }

    let _ = tx.send(Command::Shutdown);
    let _ = thread.join();
}

#[test]
fn a_track_that_reaches_its_end_leaves_the_deck_stopped() {
    // `decisions.md`: "A track that reaches its end stops. Nothing starts on
    // its own... The engine already reports `EndOfTrack` and decides nothing,
    // **so this is where the decision lands.**"
    //
    // It landed nowhere. `Transport::reached_end` existed, was documented, was
    // named in that decision — and `grep` found its only callers were its own
    // two unit tests. The engine returns the outcome and does not touch the
    // transport, deliberately, because `fill` runs on the audio thread and
    // those stores would race the control thread. So the obligation belongs to
    // whatever drives the loop, and until the app loop exists nothing was
    // discharging it: at the end of a track the deck read `Playing` at rate
    // 1.0 for ever. The display would say "playing" over silence, and the next
    // PLAY press would pause.
    //
    // Written as a loop rather than as a call to `reached_end` because the
    // defect was never in that function — it was in nobody calling it. Same
    // reason `input_test.rs` models the dispatch it is testing, which is how
    // the stale `was_playing` was found.
    let scratch = Scratch::new("engine-endstate");
    let frames = 6_000usize;
    let source = signal(Bits::S24, 2, frames);
    let bytes = fixtures::build(Kind::Wav, &source, Bits::S24, 44_100, 2);
    let path = fixtures::write(&scratch.dir, "short", Kind::Wav, &bytes);

    let (window, reader, info) = Window::load(&path, WINDOW_BYTES).expect("loads");
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::spawn(move || window.run(rx, |_| {}));

    let transport = Transport::new();
    transport.track_loaded(0);
    let mut engine = Engine::new(info.frames);
    transport.play();

    let mut period = vec![0i32; PERIOD * RING_CHANNELS];
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match engine.fill(&transport, &reader, &mut period) {
            Outcome::Played { .. } | Outcome::PlayedTail { .. } => {}
            Outcome::EndOfTrack => {
                transport.reached_end();
                break;
            }
            Outcome::Missed(Miss::NotResident) => std::thread::yield_now(),
            other => panic!("unexpected outcome: {other:?}"),
        }
        assert!(Instant::now() < deadline, "playback never completed");
    }
    let _ = tx.send(Command::Shutdown);
    let _ = thread.join();

    assert_eq!(
        transport.rate(),
        deck_pi::transport::RATE_PAUSED,
        "the deck must not still be running at unity after the last frame"
    );
    // **`Paused`, not `Stopped`** — the third confusable pair this suite has
    // had to pin down. `State::Stopped` means "nothing loaded"; a track that
    // reached its end is still loaded and sitting on its last frame, which is
    // the CDJ's own idea of stopping — `hardware.md`: "returning to the cue
    // point and standing by *is* stopping", which is why there is no separate
    // STOP button. The first version of this assertion expected `Stopped` and
    // was wrong about the design rather than about the code.
    assert_eq!(
        transport.state(),
        deck_pi::transport::State::Paused,
        "and it must say so, or the display reads Playing over silence"
    );
    assert!(
        transport.position() <= info.frames as f64,
        "the position must not run past the last frame"
    );
}
