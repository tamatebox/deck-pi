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
/// beside a callback draining — and returns every sample handed to the
/// output, in order.
fn play_to_completion(path: &std::path::Path, frames: u64) -> Vec<i32> {
    let (window, reader, info) = Window::load(path, WINDOW_BYTES).expect("loads");
    assert_eq!(info.frames, frames);

    let (tx, rx) = mpsc::channel();
    let thread = std::thread::spawn(move || window.run(rx, |_| {}));

    let transport = Transport::new();
    let mut engine = Engine::new(frames);
    transport.play();

    let mut collected = Vec::with_capacity(frames as usize * RING_CHANNELS);
    let mut period = vec![0i32; PERIOD * RING_CHANNELS];
    let deadline = Instant::now() + Duration::from_secs(30);

    loop {
        match engine.fill(&transport, &reader, &mut period) {
            Outcome::Played { frames: n } | Outcome::PlayedTail { frames: n } => {
                collected.extend_from_slice(&period[..n * RING_CHANNELS]);
            }
            Outcome::EndOfTrack => break,
            // The filler has not reached here yet. A real callback would have
            // emitted this period of silence and moved on; the test waits,
            // because it is checking content rather than timing.
            Outcome::Missed(Miss::NotResident) => std::thread::yield_now(),
            other => panic!("unexpected outcome at frame {}: {:?}", engine.position(), other),
        }
        assert!(Instant::now() < deadline, "playback never completed");
    }

    let _ = tx.send(Command::Shutdown);
    let _ = thread.join();
    collected
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
