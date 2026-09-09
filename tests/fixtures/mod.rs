//! Hand-written WAV / AIFF / AIFF-C writers for the null test.
//!
//! Deliberately not generated with `sox` or `ffmpeg`. The whole point of the
//! null test is that the expected bytes are known exactly; routing them
//! through another library would only prove that two libraries agree.
//!
//! Only the chunks libsndfile needs to parse a file are emitted, in the
//! canonical order.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bits {
    S16,
    S24,
}

impl Bits {
    pub fn bytes(self) -> usize {
        match self {
            Bits::S16 => 2,
            Bits::S24 => 3,
        }
    }
    /// The lowest and highest value this depth can hold.
    pub fn range(self) -> (i32, i32) {
        match self {
            Bits::S16 => (i16::MIN as i32, i16::MAX as i32),
            Bits::S24 => (-(1 << 23), (1 << 23) - 1),
        }
    }
}

/// Which byte order the *samples* are in. AIFF is big-endian, except
/// AIFF-C `sowt`, which is little-endian — so this cannot be decided from the
/// container type, which is exactly why it is a separate axis here
/// (docs/architecture.md, "The three problems").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Wav,
    Aiff,
    AiffcSowt,
    /// RF64 — 64-bit sizes, which is what lifts the 2 GiB container ceiling
    /// that caps a 192/24 track at 31 minutes in plain WAV.
    Rf64,
}

impl Kind {
    pub fn sample_endian_is_little(self) -> bool {
        match self {
            Kind::Wav | Kind::AiffcSowt | Kind::Rf64 => true,
            Kind::Aiff => false,
        }
    }
    pub fn extension(self) -> &'static str {
        match self {
            Kind::Wav => "wav",
            Kind::Aiff => "aiff",
            Kind::AiffcSowt => "aifc",
            Kind::Rf64 => "wav",
        }
    }
}

/// A test signal built to catch the mistakes this path can actually make.
///
/// Every value is at a boundary or is channel-distinct, so a sign error, a
/// logical instead of arithmetic shift, a byte-order slip and a left/right
/// swap each produce a different failure rather than all looking alike.
pub fn signal(bits: Bits, channels: usize, frames: usize) -> Vec<i32> {
    let (lo, hi) = bits.range();
    let landmarks = [0, 1, -1, hi, lo, hi - 1, lo + 1, -(hi / 3), hi / 3];
    let mut out = Vec::with_capacity(frames * channels);
    for f in 0..frames {
        for c in 0..channels {
            let v = if f < landmarks.len() {
                // Offset per channel so L and R are never equal here.
                landmarks[(f + c * 3) % landmarks.len()]
            } else {
                // A ramp that walks the full range and changes sign.
                let span = (hi as i64 - lo as i64) as f64;
                let t = (f * channels + c) as f64 / (frames * channels) as f64;
                (lo as f64 + span * t) as i32 + c as i32
            };
            out.push(v.clamp(lo, hi));
        }
    }
    out
}

/// Encodes samples as they appear in the file's data chunk.
pub fn encode_samples(samples: &[i32], bits: Bits, little_endian: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * bits.bytes());
    for &v in samples {
        let b = v.to_le_bytes(); // b[0] = least significant
        let n = bits.bytes();
        if little_endian {
            out.extend_from_slice(&b[..n]);
        } else {
            for i in (0..n).rev() {
                out.push(b[i]);
            }
        }
    }
    out
}

/// Decodes a data chunk back to sample values, sign-extending. Independent of
/// libsndfile — this is the other half of the null test.
pub fn decode_samples(bytes: &[u8], bits: Bits, little_endian: bool) -> Vec<i32> {
    let n = bits.bytes();
    assert_eq!(bytes.len() % n, 0, "data chunk is not a whole number of samples");
    bytes
        .chunks_exact(n)
        .map(|c| {
            let mut w = [0u8; 4];
            let raw = if little_endian {
                w[..n].copy_from_slice(c);
                u32::from_le_bytes(w)
            } else {
                w[4 - n..].copy_from_slice(c);
                u32::from_be_bytes(w)
            };
            // Shift the value up to the top of the word and back down, so the
            // narrower depth sign-extends.
            let pad = 32 - 8 * n as u32;
            ((raw << pad) as i32) >> pad
        })
        .collect()
}

/// 80-bit IEEE 754 extended precision, big-endian — the AIFF COMM chunk's
/// sample rate field. Integer rates only, which is all six of ours.
fn ieee754_extended(rate: u32) -> [u8; 10] {
    let mut out = [0u8; 10];
    if rate == 0 {
        return out;
    }
    let leading = 31 - rate.leading_zeros(); // floor(log2(rate))
    let exponent: u16 = 16383 + leading as u16; // bias 16383, no sign bit set
    let mantissa: u64 = (rate as u64) << (63 - leading); // explicit leading 1
    out[..2].copy_from_slice(&exponent.to_be_bytes());
    out[2..].copy_from_slice(&mantissa.to_be_bytes());
    out
}

fn push_chunk_be(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0); // AIFF chunks are word-aligned
    }
}

fn push_chunk_le(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
}

pub fn build_wav(samples: &[i32], bits: Bits, rate: u32, channels: usize) -> Vec<u8> {
    let data = encode_samples(samples, bits, true);
    let block_align = channels * bits.bytes();

    let mut fmt = Vec::new();
    fmt.extend_from_slice(&1u16.to_le_bytes()); // WAVE_FORMAT_PCM
    fmt.extend_from_slice(&(channels as u16).to_le_bytes());
    fmt.extend_from_slice(&rate.to_le_bytes());
    fmt.extend_from_slice(&(rate * block_align as u32).to_le_bytes());
    fmt.extend_from_slice(&(block_align as u16).to_le_bytes());
    fmt.extend_from_slice(&((bits.bytes() * 8) as u16).to_le_bytes());

    let mut body = Vec::new();
    body.extend_from_slice(b"WAVE");
    push_chunk_le(&mut body, b"fmt ", &fmt);
    push_chunk_le(&mut body, b"data", &data);

    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

pub fn build_aiff(
    samples: &[i32],
    bits: Bits,
    rate: u32,
    channels: usize,
    sowt: bool,
) -> Vec<u8> {
    let frames = samples.len() / channels;
    let data = encode_samples(samples, bits, sowt);

    let mut comm = Vec::new();
    comm.extend_from_slice(&(channels as u16).to_be_bytes());
    comm.extend_from_slice(&(frames as u32).to_be_bytes());
    comm.extend_from_slice(&((bits.bytes() * 8) as u16).to_be_bytes());
    comm.extend_from_slice(&ieee754_extended(rate));
    if sowt {
        comm.extend_from_slice(b"sowt");
        // Compression name as a pstring, padded to an even length.
        comm.push(4);
        comm.extend_from_slice(b"sowt");
        comm.push(0);
    }

    let mut ssnd = Vec::new();
    ssnd.extend_from_slice(&0u32.to_be_bytes()); // offset
    ssnd.extend_from_slice(&0u32.to_be_bytes()); // blockSize
    ssnd.extend_from_slice(&data);

    let mut body = Vec::new();
    body.extend_from_slice(if sowt { b"AIFC" } else { b"AIFF" });
    if sowt {
        push_chunk_be(&mut body, b"FVER", &0xA280_5140u32.to_be_bytes());
    }
    push_chunk_be(&mut body, b"COMM", &comm);
    push_chunk_be(&mut body, b"SSND", &ssnd);

    let mut out = Vec::new();
    out.extend_from_slice(b"FORM");
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// RF64, per EBU Tech 3306: the RIFF id becomes `RF64`, the 32-bit size
/// fields are set to `0xFFFFFFFF` as a sentinel, and a `ds64` chunk carries
/// the real 64-bit sizes. The file itself is small here — what is being
/// tested is that the 64-bit path parses, not that a 4 GiB file exists.
pub fn build_rf64(samples: &[i32], bits: Bits, rate: u32, channels: usize) -> Vec<u8> {
    const SENTINEL: u32 = 0xFFFF_FFFF;
    let data = encode_samples(samples, bits, true);
    let frames = (samples.len() / channels) as u64;
    let block_align = channels * bits.bytes();

    let mut ds64 = Vec::new();
    ds64.extend_from_slice(&0u64.to_le_bytes()); // riffSize, patched below
    ds64.extend_from_slice(&(data.len() as u64).to_le_bytes()); // dataSize
    ds64.extend_from_slice(&frames.to_le_bytes()); // sampleCount
    ds64.extend_from_slice(&0u32.to_le_bytes()); // tableLength

    let mut fmt = Vec::new();
    fmt.extend_from_slice(&1u16.to_le_bytes());
    fmt.extend_from_slice(&(channels as u16).to_le_bytes());
    fmt.extend_from_slice(&rate.to_le_bytes());
    fmt.extend_from_slice(&(rate * block_align as u32).to_le_bytes());
    fmt.extend_from_slice(&(block_align as u16).to_le_bytes());
    fmt.extend_from_slice(&((bits.bytes() * 8) as u16).to_le_bytes());

    let mut body = Vec::new();
    body.extend_from_slice(b"WAVE");
    let ds64_riff_size_at = body.len() + 8;
    push_chunk_le(&mut body, b"ds64", &ds64);
    push_chunk_le(&mut body, b"fmt ", &fmt);
    // The data chunk's own size field is the sentinel; ds64 holds the truth.
    body.extend_from_slice(b"data");
    body.extend_from_slice(&SENTINEL.to_le_bytes());
    body.extend_from_slice(&data);
    if data.len() % 2 == 1 {
        body.push(0);
    }

    let riff_size = body.len() as u64;
    body[ds64_riff_size_at..ds64_riff_size_at + 8].copy_from_slice(&riff_size.to_le_bytes());

    let mut out = Vec::new();
    out.extend_from_slice(b"RF64");
    out.extend_from_slice(&SENTINEL.to_le_bytes());
    out.extend_from_slice(&body);
    out
}

pub fn build(kind: Kind, samples: &[i32], bits: Bits, rate: u32, channels: usize) -> Vec<u8> {
    match kind {
        Kind::Wav => build_wav(samples, bits, rate, channels),
        Kind::Aiff => build_aiff(samples, bits, rate, channels, false),
        Kind::AiffcSowt => build_aiff(samples, bits, rate, channels, true),
        Kind::Rf64 => build_rf64(samples, bits, rate, channels),
    }
}

/// Writes a fixture into a scratch directory and returns its path.
pub fn write(dir: &Path, name: &str, kind: Kind, bytes: &[u8]) -> PathBuf {
    std::fs::create_dir_all(dir).expect("create fixture dir");
    let path = dir.join(format!("{}.{}", name, kind.extension()));
    std::fs::write(&path, bytes).expect("write fixture");
    path
}

/// A per-test scratch directory under the target dir, removed on drop.
pub struct Scratch {
    pub dir: PathBuf,
}

impl Scratch {
    pub fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("deck-pi-fixtures-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch { dir }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn extended_precision_matches_known_values() {
    // 44100 Hz is the canonical worked example: 40 0E AC 44 00 ...
    assert_eq!(
        ieee754_extended(44_100),
        [0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]
    );
    assert_eq!(
        ieee754_extended(48_000),
        [0x40, 0x0E, 0xBB, 0x80, 0, 0, 0, 0, 0, 0]
    );
    assert_eq!(
        ieee754_extended(192_000),
        [0x40, 0x10, 0xBB, 0x80, 0, 0, 0, 0, 0, 0]
    );
}

#[test]
fn encode_decode_round_trips_at_both_endiannesses() {
    for bits in [Bits::S16, Bits::S24] {
        let s = signal(bits, 2, 64);
        for le in [true, false] {
            assert_eq!(decode_samples(&encode_samples(&s, bits, le), bits, le), s);
        }
    }
}
