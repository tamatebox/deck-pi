//! The output device, behind a trait.
//!
//! Exactly one thing varies between the deck and a test: where a finished
//! period goes. `architecture.md` puts the callback's whole job as "reads the
//! ring and nothing else", so the sink is the only piece below it, and
//! `implementation.md` already implies this seam — it defines the null test as
//! "collect the buffers handed to ALSA".
//!
//! Two implementations:
//!
//! - [`CaptureSink`] — portable. Keeps every period it is given, which *is*
//!   the null test's collector. Available on macOS, so the read path,
//!   the ring, the transport and the callback all stay testable off Linux.
//! - [`AlsaSink`] — Linux only. `hw:` device, exact rate, `S24_LE`, no
//!   software volume, and a write path that allocates nothing.
//!
//! Periods are interleaved stereo `i32` in the ring's own layout — `S24_LE`,
//! the 24-bit value right-aligned in a 32-bit word — so nothing between the
//! file and the DAC converts anything.

pub mod capture;
pub use capture::CaptureSink;

#[cfg(target_os = "linux")]
pub mod alsa;
#[cfg(target_os = "linux")]
pub use alsa::AlsaSink;

use crate::file::RING_CHANNELS;
use std::fmt;

/// Samples per frame on the wire. Stereo, always — mono was duplicated on the
/// way into the ring.
pub const SINK_CHANNELS: usize = RING_CHANNELS;

/// The only sample format in play.
///
/// Not a choice: `WM8804_FORMATS` offers `S16_LE / S20_3LE / S24_LE` and
/// `bcm2835-i2s` offers `S16_LE / S24_LE / S32_LE`, so the usable intersection
/// is `S16_LE / S24_LE` and `S24_LE` is the only useful one
/// (docs/implementation.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    S24Le,
}

impl fmt::Display for SampleFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("S24_LE")
    }
}

/// What is actually in force. Read back from the device rather than echoed
/// from the request, so a substitution shows up here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinkParams {
    pub rate: u32,
    pub channels: u32,
    pub format: SampleFormat,
    /// Frames per period — what the device granted, which may differ from
    /// what was asked for. Unlike the rate, that is fine: period size is a
    /// latency parameter, not a correctness one.
    pub period_frames: usize,
    pub periods: u32,
}

impl SinkParams {
    /// Output latency, in seconds, at the granted period size and count.
    /// `architecture.md` targets 5-10 ms for v2's jog response.
    pub fn latency_secs(&self) -> f64 {
        (self.period_frames * self.periods as usize) as f64 / self.rate as f64
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinkError {
    /// The device name is not a `hw:` device.
    ///
    /// Refused rather than accepted with a warning: `default` brings format
    /// conversion and usually `dmix`, `plughw` resamples, and **neither
    /// reports that it did so**. There is no safe way to notice this later.
    NotHardwareDevice(String),
    /// The device does not support the source's exact rate.
    ///
    /// A hard failure by design. `snd_pcm_hw_params_set_rate_near` would have
    /// picked the closest rate and *succeeded* — one function name apart, no
    /// error, wrong rate. The track fails to load instead.
    RateRefused { asked: u32 },
    /// The device reported something other than what was requested.
    Substituted { field: &'static str, asked: String, got: String },
    /// An underrun: the period was late and the device ran dry.
    Underrun,
    /// The buffer handed in is not a whole number of frames, or is not the
    /// device's period size.
    WrongPeriodLength { given: usize, expected: usize },
    /// Anything the device itself reported, off the audio thread.
    Device(String),
    /// A device error raised **on the audio thread**, carried without
    /// allocating.
    ///
    /// `Device(String)` cannot be used there: `e.to_string()` allocates, and
    /// `write_period`'s own contract two doc comments up says implementations
    /// must not. The error path was doing it anyway — reachable on any
    /// `writei` failure that is not an xrun (`EINTR`, `EBADFD`, `ESTRPIPE`),
    /// which under `assert_no_alloc` aborts the process in a debug build and
    /// buries the actual device error behind the abort.
    ///
    /// `alsa::Error` is a `&'static str` and an errno already, so keeping the
    /// two and formatting them in `Display` costs nothing and loses nothing.
    DeviceRt { func: &'static str, errno: i32 },
}

impl fmt::Display for SinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SinkError::DeviceRt { func, errno } => {
                write!(f, "{} failed on the audio thread: errno {}", func, errno)
            }
            SinkError::NotHardwareDevice(d) => write!(
                f,
                "{:?} is not a hw: device — default mixes and plughw resamples, silently",
                d
            ),
            SinkError::RateRefused { asked } => {
                write!(f, "the device will not take {} Hz exactly", asked)
            }
            SinkError::Substituted { field, asked, got } => {
                write!(f, "device substituted {}: asked {}, got {}", field, asked, got)
            }
            SinkError::Underrun => f.write_str("underrun — the period was late"),
            SinkError::WrongPeriodLength { given, expected } => write!(
                f,
                "period is {} samples, device wants {}",
                given, expected
            ),
            SinkError::Device(m) => write!(f, "device: {}", m),
        }
    }
}

impl std::error::Error for SinkError {}

/// Where a finished period goes.
pub trait AudioSink {
    /// What the device actually settled on.
    fn params(&self) -> SinkParams;

    /// Frames the device wants per write.
    fn period_frames(&self) -> usize {
        self.params().period_frames
    }

    /// Hands one period to the device.
    ///
    /// **Called from the audio thread.** Implementations must not allocate,
    /// take a lock, or do anything worse than O(1) in the period length. The
    /// device write itself is the one syscall allowed here, because it is the
    /// point.
    fn write_period(&mut self, period: &[i32]) -> Result<(), SinkError>;

    /// Blocks until everything queued has been played. Not realtime; called
    /// when a track ends or the deck stops.
    ///
    /// **Terminal for the ALSA sink: the next track needs a new one.**
    /// `snd_pcm_drain` leaves the device in `SETUP`, so a further
    /// `write_period` returns `EBADFD` — and because that is not an xrun, the
    /// recovery path does not call `prepare` and the error surfaces as a bare
    /// device failure. Nothing in this trait said so, and "or the deck stops"
    /// reads as though the sink survives stopping.
    ///
    /// That costs nothing here, because the design already opens a device per
    /// track: the output rate follows the source, so a rate change reopens it
    /// anyway, and `architecture.md` records that as free — the other deck is
    /// a separate Pi, so nothing audible is interrupted. Written down because
    /// it is a contract, not because it is a limitation.
    fn drain(&mut self) -> Result<(), SinkError>;

    /// Checks that the device is running what it was asked for, **while it is
    /// running**, and fails naming the field that differs.
    ///
    /// This is the hardware half of the null test. It is on the trait rather
    /// than only on `AlsaSink` because the caller that needs it is generic
    /// over the sink, and a check that nothing can reach is the failure this
    /// project keeps finding: `AlsaSink::hw_params_in_force` existed, was
    /// cited in `rt.rs`'s doc comment as the model of the read-back
    /// discipline, and had **no callers at all** — while `main.rs` printed
    /// `/proc/asound` with `{:?}` and compared nothing.
    ///
    /// The default is `Ok(())`: a sink with no device has nothing that could
    /// have been substituted underneath it.
    ///
    /// Does file I/O on the ALSA implementation, so **not** from the audio
    /// thread — call it once, after the first period is written.
    fn verify_in_force(&self) -> Result<(), SinkError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(rate: u32, period_frames: usize, periods: u32) -> SinkParams {
        SinkParams {
            rate,
            channels: 2,
            format: SampleFormat::S24Le,
            period_frames,
            periods,
        }
    }

    /// `architecture.md` targets "~5-10 ms output latency for v2 jog
    /// response: 128-256 frame periods, 2-3 periods". Those two statements do
    /// **not** combine freely, and this pins down where they do.
    ///
    /// At 44.1 kHz only the 128-frame periods land inside the target: 256 x 2
    /// is 11.6 ms and 256 x 3 is 17.4 ms. The higher rates have room for the
    /// whole range because the same frame count is less time. So the period
    /// size is not a free choice at the bottom of the rate range — worth
    /// knowing before the jog is tuned against feel.
    #[test]
    fn the_documented_latency_target_constrains_the_period_size_at_44_1_khz() {
        let inside = |p: &SinkParams| (5.0..=10.0).contains(&(p.latency_secs() * 1000.0));

        assert!(inside(&params(44_100, 128, 2)), "128 x 2 at 44.1 kHz");
        assert!(inside(&params(44_100, 128, 3)), "128 x 3 at 44.1 kHz");
        assert!(
            !inside(&params(44_100, 256, 2)),
            "256 x 2 at 44.1 kHz is {:.2} ms, outside the 5-10 ms target",
            params(44_100, 256, 2).latency_secs() * 1000.0
        );
        assert!(!inside(&params(44_100, 256, 3)), "256 x 3 at 44.1 kHz");

        // The formula itself, so a refactor cannot quietly change the meaning.
        assert_eq!(params(48_000, 128, 2).latency_secs(), 256.0 / 48_000.0);

        // And the top of the rate range has room for all four combinations.
        for period in [128usize, 256] {
            for count in [2u32, 3] {
                let ms = params(192_000, period, count).latency_secs() * 1000.0;
                assert!(ms < 10.0, "{} x {} at 192 kHz is {:.2} ms", period, count, ms);
            }
        }
    }

    #[test]
    fn a_non_hw_device_names_the_reason_in_full() {
        // The message has to say *why*, because the failure it prevents is
        // invisible: plughw would have played, and resampled.
        let m = SinkError::NotHardwareDevice("plughw:0,0".into()).to_string();
        assert!(m.contains("plughw"), "{}", m);
        assert!(m.contains("resamples"), "{}", m);
    }
}
