//! The file layer.
//!
//! Shared between the browser (which needs length, rate and depth, and which
//! must refuse an unplayable file *before* PLAY is pressed) and the window
//! thread (which fills the ring). Both are off the deadline.
//!
//! The scope is exactly what the Digi2 Pro can send, so there is no second
//! rule: 44.1/88.2/176.4 and 48/96/192 kHz, int16 or int24
//! (docs/architecture.md, "What plays").

use std::fmt;
use std::path::{Path, PathBuf};

use crate::sndfile::{ffi, SndFile, SndFileError};

/// The six rates, and nothing else. Both oscillator families are exact here,
/// so no fractional division ever happens (docs/hardware.md).
pub const SUPPORTED_RATES: [u32; 6] = [44_100, 88_200, 176_400, 48_000, 96_000, 192_000];

/// Samples per frame in the ring, always. Mono is duplicated on the way in,
/// which is lossless and leaves the callback with one layout.
pub const RING_CHANNELS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    Int16,
    Int24,
}

impl Depth {
    /// How far `sf_readf_int` left-justifies this depth. Kept as a fact about
    /// the library rather than a magic number at the call site.
    pub const fn left_justify_shift(self) -> u32 {
        match self {
            Depth::Int16 => 16,
            Depth::Int24 => 8,
        }
    }
}

impl fmt::Display for Depth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Depth::Int16 => "16 bit",
            Depth::Int24 => "24 bit",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Wav,
    WavEx,
    Aiff,
    Rf64,
    Wave64,
}

impl Container {
    fn from_format(format: i32) -> Option<Self> {
        match format & ffi::SF_FORMAT_TYPEMASK {
            ffi::SF_FORMAT_WAV => Some(Container::Wav),
            ffi::SF_FORMAT_WAVEX => Some(Container::WavEx),
            ffi::SF_FORMAT_AIFF => Some(Container::Aiff),
            ffi::SF_FORMAT_RF64 => Some(Container::Rf64),
            ffi::SF_FORMAT_W64 => Some(Container::Wave64),
            _ => None,
        }
    }

    /// True for the two containers whose chunk sizes are 32-bit, so a track
    /// past the 2 GiB compatible ceiling may carry a wrapped size field. The
    /// browser uses this to mark an implausible declared length as suspect.
    pub const fn has_32_bit_size_fields(self) -> bool {
        matches!(self, Container::Wav | Container::WavEx | Container::Aiff)
    }
}

impl fmt::Display for Container {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Container::Wav => "WAV",
            Container::WavEx => "WAVE-EX",
            Container::Aiff => "AIFF",
            Container::Rf64 => "RF64",
            Container::Wave64 => "Wave64",
        })
    }
}

/// Why a file will not play. Every one of these is decidable from the header
/// alone, which is what lets the browser refuse on highlight. The UI's
/// obligation is to say *which* — "say why, not just that"
/// (docs/decisions.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reject {
    /// Not WAV, WAVE-EX, AIFF, RF64 or Wave64. DSD and anything libsndfile
    /// does not recognise fail at `open` instead and arrive as `Unreadable`.
    Container,
    /// Not PCM int16 or int24. This subsumes the compressed subtypes (MP3,
    /// FLAC, Vorbis, Opus, ADPCM), 8-bit, 32-bit int, float and double: all
    /// of them are a single `SF_FORMAT_SUBMASK` value.
    Depth { found: i32 },
    /// Outside the DDC's six rates.
    Rate { found: u32 },
    /// More than stereo. Mono and stereo both play; mono is duplicated.
    Channels { found: u32 },
}

impl fmt::Display for Reject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reject::Container => write!(f, "container not WAV / AIFF / RF64 / Wave64"),
            Reject::Depth { found } => write!(f, "{} — needs 16 or 24 bit PCM", describe_subtype(*found)),
            Reject::Rate { found } => write!(f, "{} Hz — outside 44.1-192 kHz", found),
            Reject::Channels { found } => write!(f, "{} channels — needs mono or stereo", found),
        }
    }
}

/// Names what was found, so the display can be specific rather than saying
/// "unsupported". Only the subtypes plausibly encountered are named.
fn describe_subtype(subtype: i32) -> &'static str {
    match subtype {
        ffi::SF_FORMAT_PCM_S8 | ffi::SF_FORMAT_PCM_U8 => "8 bit",
        ffi::SF_FORMAT_PCM_32 => "32 bit integer",
        ffi::SF_FORMAT_FLOAT => "32 bit float",
        ffi::SF_FORMAT_DOUBLE => "64 bit float",
        _ => "compressed or unsupported sample format",
    }
}

#[derive(Debug)]
pub enum OpenError {
    /// libsndfile could not open or parse the file at all. Covers DSD, a
    /// truncated header, and a permission or I/O failure — including a stick
    /// pulled between the browser's `read_dir` and this open.
    Unreadable(SndFileError),
    /// Opened and parsed; the header says it cannot be played.
    Rejected(Reject),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::Unreadable(e) => write!(f, "unreadable: {}", e),
            OpenError::Rejected(r) => write!(f, "{}", r),
        }
    }
}

/// What the browser shows for one row, and what the transport needs to load a
/// track. Cheap to clone and cache by path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackInfo {
    pub path: PathBuf,
    pub frames: u64,
    pub rate: u32,
    pub channels: u32,
    pub depth: Depth,
    pub container: Container,
    pub seekable: bool,
}

impl TrackInfo {
    pub fn duration_secs(&self) -> f64 {
        self.frames as f64 / self.rate as f64
    }

    /// Bytes the source occupies as audio data, used to spot a wrapped 32-bit
    /// chunk size. Not the file size — headers are excluded.
    pub fn audio_bytes(&self) -> u64 {
        let bytes_per_sample = match self.depth {
            Depth::Int16 => 2,
            Depth::Int24 => 3,
        };
        self.frames * self.channels as u64 * bytes_per_sample
    }

    /// A 32-bit-size container declaring less audio than 2 GiB is normal; one
    /// whose data would have overrun that ceiling was written by a tool that
    /// may have wrapped the field, and the declared length cannot be trusted.
    pub fn declared_length_is_suspect(&self) -> bool {
        self.container.has_32_bit_size_fields() && self.audio_bytes() >= 2 * 1024 * 1024 * 1024
    }
}

/// Decides playability from a parsed header. Separated from `open` so it can
/// be tested against every combination without a file on disk.
pub fn vet(info: &ffi::SF_INFO) -> Result<(Container, Depth, u32, u32), Reject> {
    let container = Container::from_format(info.format).ok_or(Reject::Container)?;

    let depth = match info.format & ffi::SF_FORMAT_SUBMASK {
        ffi::SF_FORMAT_PCM_16 => Depth::Int16,
        ffi::SF_FORMAT_PCM_24 => Depth::Int24,
        found => return Err(Reject::Depth { found }),
    };

    if info.samplerate < 0 {
        return Err(Reject::Rate { found: 0 });
    }
    let rate = info.samplerate as u32;
    if !SUPPORTED_RATES.contains(&rate) {
        return Err(Reject::Rate { found: rate });
    }

    if info.channels < 1 || info.channels > RING_CHANNELS as i32 {
        return Err(Reject::Channels {
            found: info.channels.max(0) as u32,
        });
    }

    Ok((container, depth, rate, info.channels as u32))
}

/// An open, playable track. Reads into the ring's own layout.
pub struct Track {
    file: SndFile,
    info: TrackInfo,
}

impl Track {
    /// Opens and vets in one step. The browser calls this on the highlighted
    /// row and keeps only the `TrackInfo`; the window thread keeps the whole
    /// `Track`.
    pub fn open(path: &Path) -> Result<Self, OpenError> {
        let file = SndFile::open(path).map_err(OpenError::Unreadable)?;
        let raw = *file.info();
        let (container, depth, rate, channels) = vet(&raw).map_err(OpenError::Rejected)?;
        Ok(Track {
            file,
            info: TrackInfo {
                path: path.to_path_buf(),
                frames: raw.frames.max(0) as u64,
                rate,
                channels,
                depth,
                container,
                seekable: raw.seekable != 0,
            },
        })
    }

    pub fn info(&self) -> &TrackInfo {
        &self.info
    }

    /// Fills `dst` with interleaved stereo samples in the output's own
    /// layout — `S24_LE`, the 24-bit value right-aligned in a 32-bit word —
    /// and returns the number of *frames* written.
    ///
    /// `dst.len()` must be even; whole frames only. Everything this does is a
    /// pure arithmetic shift, so the chain source -> ring -> DAC is lossless:
    /// `sf_readf_int` hands back the sample left-justified, and `>> 8`
    /// right-aligns it. That shift belongs here and not in the callback
    /// because this thread has no deadline.
    pub fn read_into_ring(&mut self, dst: &mut [i32]) -> Result<usize, SndFileError> {
        assert!(
            dst.len() % RING_CHANNELS == 0,
            "ring writes are whole frames; got {} samples",
            dst.len()
        );
        let frames_wanted = (dst.len() / RING_CHANNELS) as i64;
        if frames_wanted == 0 {
            return Ok(0);
        }

        let mono = self.info.channels == 1;
        let got = if mono {
            // Read into the front of `dst` and expand in place, back to
            // front, so a mono source costs no scratch buffer and no
            // allocation on this path at all.
            // `read_frames` needs room for frames * channels = frames_wanted
            // samples, and `dst` holds twice that, so the mono read lands in
            // dst[..frames_wanted] with the tail untouched.
            let got = self.file.read_frames(dst, frames_wanted)?;
            for i in (0..got as usize).rev() {
                let v = dst[i];
                dst[i * 2] = v;
                dst[i * 2 + 1] = v;
            }
            got
        } else {
            self.file.read_frames(dst, frames_wanted)?
        };

        let samples = got as usize * RING_CHANNELS;
        for s in &mut dst[..samples] {
            // Arithmetic shift: negative samples must stay negative. This is
            // the only conversion the whole path performs.
            *s >>= 8;
        }
        Ok(got as usize)
    }

    pub fn seek(&mut self, frame: u64) -> Result<u64, SndFileError> {
        let at = self.file.seek(frame as i64)?;
        Ok(at.max(0) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(format: i32, samplerate: i32, channels: i32) -> ffi::SF_INFO {
        ffi::SF_INFO {
            frames: 1024,
            samplerate,
            channels,
            format,
            sections: 1,
            seekable: 1,
        }
    }

    /// `vet` takes a parsed header rather than a path precisely so the
    /// subtypes we cannot hand-write a fixture for — float, double, the
    /// compressed families — can still be covered.
    #[test]
    fn every_in_scope_combination_is_accepted() {
        let containers = [
            ffi::SF_FORMAT_WAV,
            ffi::SF_FORMAT_WAVEX,
            ffi::SF_FORMAT_AIFF,
            ffi::SF_FORMAT_RF64,
            ffi::SF_FORMAT_W64,
        ];
        let depths = [
            (ffi::SF_FORMAT_PCM_16, Depth::Int16),
            (ffi::SF_FORMAT_PCM_24, Depth::Int24),
        ];
        let mut accepted = 0;
        for container in containers {
            for (sub, want_depth) in depths {
                for rate in SUPPORTED_RATES {
                    for channels in [1, 2] {
                        let i = info(container | sub, rate as i32, channels);
                        let (_c, depth, got_rate, got_ch) =
                            vet(&i).expect("in-scope header must be accepted");
                        assert_eq!(depth, want_depth);
                        assert_eq!(got_rate, rate);
                        assert_eq!(got_ch, channels as u32);
                        accepted += 1;
                    }
                }
            }
        }
        // 5 containers x 2 depths x 6 rates x 2 channel counts.
        assert_eq!(accepted, 120);
    }

    #[test]
    fn out_of_scope_depths_are_refused_by_depth() {
        // SF_FORMAT_DWVW_16 and IMA_ADPCM stand in for the compressed
        // families; all of them are a single submask value, which is why one
        // check covers the whole list in architecture.md.
        for sub in [
            ffi::SF_FORMAT_PCM_S8,
            ffi::SF_FORMAT_PCM_U8,
            ffi::SF_FORMAT_PCM_32,
            ffi::SF_FORMAT_FLOAT,
            ffi::SF_FORMAT_DOUBLE,
            0x0011, // IMA_ADPCM
            0x0080, // Vorbis
            0x0064, // MPEG layer III
        ] {
            let i = info(ffi::SF_FORMAT_WAV | sub, 44_100, 2);
            assert_eq!(vet(&i), Err(Reject::Depth { found: sub }), "subtype {:#x}", sub);
        }
    }

    #[test]
    fn out_of_scope_containers_are_refused_by_container() {
        for container in [
            0x03_0000, // AU
            0x04_0000, // RAW
            0x0A_0000, // OGG-ish major types
            0x18_0000, // FLAC
            0x20_0000, // OGG
            0x23_0000, // MPEG
        ] {
            let i = info(container | ffi::SF_FORMAT_PCM_16, 44_100, 2);
            assert_eq!(vet(&i), Err(Reject::Container), "container {:#x}", container);
        }
    }

    #[test]
    fn a_rate_between_two_supported_families_is_still_refused() {
        // 64 kHz is an exact division of the 48 kHz crystal and the WM8804
        // driver advertises it, but it is below the board's stated interface
        // floor, so it is out of scope regardless (docs/hardware.md).
        for rate in [32_000, 64_000, 22_050, 11_025, 384_000, 8_000] {
            let i = info(ffi::SF_FORMAT_WAV | ffi::SF_FORMAT_PCM_24, rate, 2);
            assert_eq!(vet(&i), Err(Reject::Rate { found: rate as u32 }));
        }
    }

    #[test]
    fn zero_and_surround_channel_counts_are_refused() {
        for ch in [0, 3, 6, 8] {
            let i = info(ffi::SF_FORMAT_WAV | ffi::SF_FORMAT_PCM_16, 48_000, ch);
            assert_eq!(vet(&i), Err(Reject::Channels { found: ch as u32 }));
        }
    }

    #[test]
    fn container_order_of_checks_reports_the_container_first() {
        // A FLAC file is both the wrong container and the wrong depth. The
        // display should say the more fundamental thing.
        let i = info(0x18_0000 | ffi::SF_FORMAT_PCM_16, 44_100, 2);
        assert_eq!(vet(&i), Err(Reject::Container));
    }

    #[test]
    fn the_2_gib_ceiling_is_only_flagged_on_32_bit_size_containers() {
        // 2 h 15 m at 44.1/24 is the documented ceiling, so just past it.
        let over = TrackInfo {
            path: PathBuf::from("long.wav"),
            frames: 2 * 1024 * 1024 * 1024 / 6 + 1,
            rate: 44_100,
            channels: 2,
            depth: Depth::Int24,
            container: Container::Wav,
            seekable: true,
        };
        assert!(over.declared_length_is_suspect());
        // The same audio in RF64 carries 64-bit sizes, so nothing is suspect.
        let rf64 = TrackInfo { container: Container::Rf64, ..over.clone() };
        assert!(!rf64.declared_length_is_suspect());
        // And a normal-length WAV is not flagged either.
        let short = TrackInfo { frames: 44_100 * 300, ..over.clone() };
        assert!(!short.declared_length_is_suspect());
    }

    #[test]
    fn left_justify_shifts_are_the_ones_sf_readf_int_documents() {
        assert_eq!(Depth::Int16.left_justify_shift(), 16);
        assert_eq!(Depth::Int24.left_justify_shift(), 8);
    }
}
