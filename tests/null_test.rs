//! The null test — the software half of the two-sided check in
//! docs/implementation.md.
//!
//! It proves the read path, the ring layout and the left-justification: the
//! samples the ring holds must be the source's samples, and re-encoding them
//! must reproduce the source's data chunk byte for byte. The hardware half —
//! `/proc/asound/.../hw_params` while playing — needs the Pi and is not here.
//!
//! Every supported conversion is a pure arithmetic shift, so there is nothing
//! in scope for which this test may legitimately fail.

mod fixtures;

use fixtures::{decode_samples, signal, Bits, Kind, Scratch};

use deck_pi::file::{Container, Depth, OpenError, Reject, Track, RING_CHANNELS};

const RATES: [u32; 6] = [44_100, 88_200, 176_400, 48_000, 96_000, 192_000];
const FRAMES: usize = 512;

/// What the ring must hold for a given source sample: the left-justified
/// int32 `sf_readf_int` returns, shifted right 8 into `S24_LE`.
///
/// Written as the composition the design describes rather than as a single
/// simplified shift, so the test states the contract instead of restating the
/// implementation.
fn expected_ring_value(v: i32, depth: Depth) -> i32 {
    let left_justified = v << depth.left_justify_shift();
    left_justified >> 8
}

/// Which container the file layer must report. Asserted so an RF64 file
/// silently parsed as plain WAV — which would reinstate the 2 GiB ceiling
/// without any visible symptom — fails here.
fn container_of(kind: Kind) -> Container {
    match kind {
        Kind::Wav => Container::Wav,
        Kind::Aiff | Kind::AiffcSowt => Container::Aiff,
        Kind::Rf64 => Container::Rf64,
    }
}

fn depth_of(bits: Bits) -> Depth {
    match bits {
        Bits::S16 => Depth::Int16,
        Bits::S24 => Depth::Int24,
    }
}

/// Reads a whole track through the file layer in one go.
fn drain(track: &mut Track, frames: usize) -> Vec<i32> {
    let mut ring = vec![0i32; frames * RING_CHANNELS];
    let mut filled = 0usize;
    // Read in short blocks, as the window thread will, so a per-call
    // off-by-one at a block boundary shows up here rather than never.
    while filled < frames {
        let block = (frames - filled).min(97); // deliberately not a power of two
        let got = track
            .read_into_ring(&mut ring[filled * RING_CHANNELS..(filled + block) * RING_CHANNELS])
            .expect("read into ring");
        if got == 0 {
            break;
        }
        filled += got;
    }
    assert_eq!(filled, frames, "short read");
    ring
}

#[test]
fn ring_holds_the_source_samples_for_all_twelve_combinations() {
    let scratch = Scratch::new("combinations");

    for kind in [Kind::Wav, Kind::Aiff, Kind::AiffcSowt, Kind::Rf64] {
        for bits in [Bits::S16, Bits::S24] {
            for rate in RATES {
                let source = signal(bits, 2, FRAMES);
                let bytes = fixtures::build(kind, &source, bits, rate, 2);
                let name = format!("{:?}-{:?}-{}", kind, bits, rate);
                let path = fixtures::write(&scratch.dir, &name, kind, &bytes);

                let mut track = Track::open(&path).unwrap_or_else(|e| {
                    panic!("{} should play, got: {}", name, e);
                });
                let info = track.info().clone();
                assert_eq!(info.rate, rate, "{}: rate", name);
                assert_eq!(info.depth, depth_of(bits), "{}: depth", name);
                assert_eq!(info.channels, 2, "{}: channels", name);
                assert_eq!(info.frames, FRAMES as u64, "{}: frame count", name);
                assert_eq!(info.container, container_of(kind), "{}: container", name);
                assert_eq!(
                    info.container.has_32_bit_size_fields(),
                    kind != Kind::Rf64,
                    "{}: 2 GiB ceiling applicability",
                    name
                );

                let ring = drain(&mut track, FRAMES);

                for (i, (&got, &src)) in ring.iter().zip(source.iter()).enumerate() {
                    let want = expected_ring_value(src, info.depth);
                    assert_eq!(
                        got, want,
                        "{}: sample {} — source {:#x}, ring {:#x}, expected {:#x}",
                        name, i, src, got, want
                    );
                }
            }
        }
    }
}

#[test]
fn the_ring_matches_an_independent_decode_of_the_bytes_on_disk() {
    // The stronger half of the null test. `decode_samples` is our own
    // decoder, written without libsndfile, so this compares libsndfile's
    // output against an independent reading of the same bytes.
    //
    // Note why it is written this way round. The obvious form — re-encode the
    // ring and compare with a re-encoding of the source — is **symmetric**,
    // and a symmetric error cancels: a logical shift instead of an arithmetic
    // one passes that version for int24 material, because the recovery shift
    // undoes exactly what the mistake did. Asserting the ring's absolute
    // value against an independently derived expectation is what actually
    // bites.
    let scratch = Scratch::new("independent-decode");

    for kind in [Kind::Wav, Kind::Aiff, Kind::AiffcSowt, Kind::Rf64] {
        for bits in [Bits::S16, Bits::S24] {
            let source = signal(bits, 2, FRAMES);
            let name = format!("{:?}-{:?}", kind, bits);
            let file_bytes = fixtures::build(kind, &source, bits, 44_100, 2);

            // The data chunk as it literally sits in the file.
            let le = kind.sample_endian_is_little();
            let data = fixtures::encode_samples(&source, bits, le);
            let at = file_bytes
                .windows(data.len())
                .position(|w| w == data.as_slice())
                .unwrap_or_else(|| panic!("{}: data chunk not found in the file", name));
            let on_disk = &file_bytes[at..at + data.len()];

            // Decoded by us, not by libsndfile.
            let independent = decode_samples(on_disk, bits, le);
            assert_eq!(independent, source, "{}: our own decoder disagrees", name);

            let path = fixtures::write(&scratch.dir, &name, kind, &file_bytes);
            let mut track = Track::open(&path).expect("opens");
            let depth = track.info().depth;
            let ring = drain(&mut track, FRAMES);

            for (i, (&got, &want_src)) in ring.iter().zip(independent.iter()).enumerate() {
                assert_eq!(
                    got,
                    expected_ring_value(want_src, depth),
                    "{}: sample {} — bytes on disk decode to {:#x}, ring holds {:#x}",
                    name,
                    i,
                    want_src,
                    got
                );
            }

            // And the round trip in the other direction: the ring is a
            // reversible view of those same bytes. Weaker than the above, but
            // it is the property the DAC ultimately depends on.
            let recovered: Vec<i32> = ring
                .iter()
                .map(|&r| (r << 8) >> depth.left_justify_shift())
                .collect();
            assert_eq!(
                fixtures::encode_samples(&recovered, bits, le),
                on_disk,
                "{}: re-encoded ring is not byte-equal to the source data chunk",
                name
            );
        }
    }
}

#[test]
fn negative_samples_survive_the_shift() {
    // A logical shift instead of an arithmetic one would pass every
    // positive-only signal and silently invert the bottom half of the range.
    let scratch = Scratch::new("negatives");
    for bits in [Bits::S16, Bits::S24] {
        let (lo, _hi) = bits.range();
        let source = vec![lo, lo + 1, -1, -2, lo / 2, -3, -4, -5];
        let bytes = fixtures::build(Kind::Wav, &source, bits, 48_000, 2);
        let path = fixtures::write(&scratch.dir, &format!("neg-{:?}", bits), Kind::Wav, &bytes);

        let mut track = Track::open(&path).expect("opens");
        let depth = track.info().depth;
        let ring = drain(&mut track, source.len() / 2);
        for (i, (&got, &src)) in ring.iter().zip(source.iter()).enumerate() {
            assert!(got < 0, "sample {} lost its sign: {:#x}", i, got);
            assert_eq!(got, expected_ring_value(src, depth), "sample {}", i);
        }
    }
}

#[test]
fn mono_is_duplicated_to_both_channels_losslessly() {
    let scratch = Scratch::new("mono");
    for bits in [Bits::S16, Bits::S24] {
        let source = signal(bits, 1, FRAMES);
        let bytes = fixtures::build(Kind::Wav, &source, bits, 44_100, 1);
        let path = fixtures::write(&scratch.dir, &format!("mono-{:?}", bits), Kind::Wav, &bytes);

        let mut track = Track::open(&path).expect("mono plays");
        assert_eq!(track.info().channels, 1);
        let depth = track.info().depth;
        let ring = drain(&mut track, FRAMES);

        assert_eq!(ring.len(), FRAMES * RING_CHANNELS);
        for (f, &src) in source.iter().enumerate() {
            let want = expected_ring_value(src, depth);
            assert_eq!(ring[f * 2], want, "frame {} left", f);
            assert_eq!(ring[f * 2 + 1], want, "frame {} right", f);
        }
    }
}

#[test]
fn seek_lands_on_the_frame_asked_for() {
    let scratch = Scratch::new("seek");
    let source = signal(Bits::S24, 2, FRAMES);
    let bytes = fixtures::build(Kind::Aiff, &source, Bits::S24, 96_000, 2);
    let path = fixtures::write(&scratch.dir, "seek", Kind::Aiff, &bytes);

    let mut track = Track::open(&path).expect("opens");
    assert!(track.info().seekable);

    for target in [0u64, 1, 97, 300, (FRAMES - 1) as u64] {
        assert_eq!(track.seek(target).expect("seek"), target);
        let mut buf = [0i32; RING_CHANNELS];
        assert_eq!(track.read_into_ring(&mut buf).expect("read"), 1);
        for c in 0..RING_CHANNELS {
            let src = source[target as usize * RING_CHANNELS + c];
            assert_eq!(
                buf[c],
                expected_ring_value(src, Depth::Int24),
                "frame {} channel {}",
                target,
                c
            );
        }
    }
}

#[test]
fn reading_past_the_end_returns_zero_rather_than_repeating() {
    let scratch = Scratch::new("eof");
    let source = signal(Bits::S16, 2, 8);
    let bytes = fixtures::build(Kind::Wav, &source, Bits::S16, 44_100, 2);
    let path = fixtures::write(&scratch.dir, "eof", Kind::Wav, &bytes);

    let mut track = Track::open(&path).expect("opens");
    let mut buf = vec![0i32; 32 * RING_CHANNELS];
    assert_eq!(track.read_into_ring(&mut buf).expect("read"), 8);
    assert_eq!(track.read_into_ring(&mut buf).expect("read at eof"), 0);
}

#[test]
fn every_out_of_scope_header_is_refused_with_its_own_reason() {
    let scratch = Scratch::new("rejections");

    // Rate outside the six. 32 kHz is an exact division of the 48 kHz
    // crystal and the WM8804 driver even advertises it, but it is below the
    // board's stated interface floor (docs/hardware.md).
    for rate in [8_000u32, 22_050, 32_000, 64_000, 384_000] {
        let source = signal(Bits::S16, 2, 16);
        let bytes = fixtures::build(Kind::Wav, &source, Bits::S16, rate, 2);
        let path = fixtures::write(&scratch.dir, &format!("rate-{}", rate), Kind::Wav, &bytes);
        match Track::open(&path) {
            Err(OpenError::Rejected(Reject::Rate { found })) => assert_eq!(found, rate),
            other => panic!("{} Hz should be refused for its rate, got {:?}", rate, other.map(|t| t.info().clone())),
        }
    }

    // More than stereo.
    let source = signal(Bits::S24, 6, 16);
    let bytes = fixtures::build(Kind::Wav, &source, Bits::S24, 48_000, 6);
    let path = fixtures::write(&scratch.dir, "surround", Kind::Wav, &bytes);
    match Track::open(&path) {
        Err(OpenError::Rejected(Reject::Channels { found })) => assert_eq!(found, 6),
        other => panic!("6 channels should be refused, got {:?}", other.map(|t| t.info().clone())),
    }
}

#[test]
fn a_missing_file_is_unreadable_not_rejected() {
    // This is the shape a stick pulled between `read_dir` and the open takes,
    // and it must not be reported as a format problem.
    match Track::open(std::path::Path::new("/nonexistent/deck-pi/no-such-file.wav")) {
        Err(OpenError::Unreadable(_)) => {}
        other => panic!("expected Unreadable, got {:?}", other.map(|t| t.info().clone())),
    }
}
