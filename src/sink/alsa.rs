//! The ALSA output, Linux only.
//!
//! Bit-perfection is not only a property of our own code: ALSA converts
//! quietly if asked wrongly. Four requirements, all from
//! `implementation.md`, "Holding it on the ALSA side":
//!
//! 1. **`hw:` only.** `default` brings format conversion and usually `dmix`;
//!    `plughw` resamples. Neither reports that it did. So a device name that
//!    is not `hw:` is refused outright rather than accepted with a warning —
//!    there is no later moment at which the mistake becomes visible.
//! 2. **`set_rate`, never `set_rate_near`.** The `_near` variant picks the
//!    closest supported rate and *succeeds*: one function name apart, no
//!    error, wrong rate. The track load fails instead.
//! 3. **`set_rate_resample(false)` explicitly**, and open with
//!    `NO_AUTO_RESAMPLE | NO_AUTO_FORMAT | NO_AUTO_CHANNELS | NO_SOFTVOL`. On
//!    `hw:` there are no plugins to disable, so it is belt and braces — but
//!    `NO_SOFTVOL` also stops alsa-lib inserting a software volume of its
//!    own, which the no-gain invariant wants regardless.
//! 4. **No software volume.** A card with a volume control has its driver
//!    scale the stream even on `hw:`. The Digi2 Pro has none by design;
//!    [`assert_no_mixer_controls`] checks that from the application side.
//!
//! # The write path allocates nothing
//!
//! The obvious call, `pcm.io_i32_s24()`, verifies the format through
//! `hw_params_current()`, which calls `snd_pcm_hw_params_malloc` — an
//! allocation, on the audio thread, once per period. So the format is
//! verified **once at open**, which discharges the safety obligation of
//! `io_unchecked`, and the hot path uses that instead. `assert_no_alloc`
//! covers it in the tests that can reach a device.

use std::ffi::CString;
use std::fs;

use alsa::pcm::{Access, Format, HwParams, State, PCM};
use alsa::{Direction, ValueOr};

use super::{AudioSink, SampleFormat, SinkError, SinkParams, SINK_CHANNELS};

/// Open-mode flags, from `alsa-sys`. Belt and braces on `hw:` except
/// `NO_SOFTVOL`, which is load-bearing.
fn open_flags() -> i32 {
    (alsa_sys::SND_PCM_NO_AUTO_RESAMPLE
        | alsa_sys::SND_PCM_NO_AUTO_CHANNELS
        | alsa_sys::SND_PCM_NO_AUTO_FORMAT
        | alsa_sys::SND_PCM_NO_SOFTVOL) as i32
}

fn dev(e: alsa::Error) -> SinkError {
    SinkError::Device(e.to_string())
}

/// The same, for the audio thread, where `to_string` is not allowed.
fn dev_rt(e: alsa::Error) -> SinkError {
    SinkError::DeviceRt {
        func: e.func(),
        errno: e.errno(),
    }
}

/// True only for a raw hardware device.
///
/// Split out so the rule is testable without a sound card, which is the only
/// way it can be tested at all off the Pi.
///
/// # This tests the name, not the device, and the gap is real
///
/// `hw` is not a reserved word — it is an entry in alsa-lib's configuration
/// tree, and `/etc/asound.conf` or `~/.asoundrc` can redefine it:
///
/// ```text
/// pcm.!hw { type plug slave.pcm "..." }
/// ```
///
/// A name passing this test would then open a plug chain. `SND_PCM_NO_AUTO_*`
/// does not help — those suppress *automatic* insertion, not a chain the
/// configuration asked for by name.
///
/// **What still catches most of it.** `verify_in_force` reads
/// `/proc/asound/.../hw_params`, which reports the hardware side, so any
/// substitution of rate, format or channel count is caught there. And
/// alsa-lib's softvol registers its control on the card, so
/// `assert_no_mixer_controls` sees it. What survives both is a plug chain at
/// the *same* rate and format doing something else to the samples.
///
/// **Not closed, deliberately, and not because of the cost.**
/// `snd_pcm_type` on the open handle is the one-line fact — but the `alsa`
/// crate exposes neither the function nor the raw pointer, so reaching it
/// means opening the device a second time through `alsa-sys` purely to ask,
/// then closing it, before the real open.
///
/// The objection is not that this is expensive. **`hw:` access is exclusive**,
/// so a probe open can fail outright, or race the real one, on the single
/// resource this program exists to hold. Trading a possible failure to play
/// for a guard against someone deliberately redefining `hw` *on the deck* is
/// the wrong way round. If the crate ever exposes the type on the handle we
/// already hold, take it — that version costs nothing and contends with
/// nothing.
pub fn is_hardware_device(name: &str) -> bool {
    name == "hw" || name.starts_with("hw:")
}

pub struct AlsaSink {
    pcm: PCM,
    params: SinkParams,
    card: i32,
    device: u32,
}

impl AlsaSink {
    /// Opens `device` for one track's rate.
    ///
    /// Reopening per track is free here: one Pi is one deck, so nothing
    /// audible is interrupted (docs/architecture.md, "Sample rate").
    pub fn open(
        device: &str,
        rate: u32,
        period_frames: usize,
        periods: u32,
    ) -> Result<Self, SinkError> {
        if !is_hardware_device(device) {
            return Err(SinkError::NotHardwareDevice(device.to_string()));
        }
        let name = CString::new(device)
            .map_err(|_| SinkError::NotHardwareDevice(device.to_string()))?;

        // SAFETY: `open_with_flags` is unsafe only because the crate does not
        // vet the flags. These four are alsa-lib's own documented open modes.
        let pcm = unsafe { PCM::open_with_flags(&name, Direction::Playback, false, open_flags()) }
            .map_err(dev)?;

        let (granted_period, granted_periods) = {
            let hwp = HwParams::any(&pcm).map_err(dev)?;
            hwp.set_access(Access::RWInterleaved).map_err(dev)?;
            hwp.set_format(Format::S24LE).map_err(dev)?;
            hwp.set_channels(SINK_CHANNELS as u32).map_err(dev)?;
            hwp.set_rate_resample(false).map_err(dev)?;

            // `ValueOr::Nearest` is dir = 0, which for the exact setter means
            // "this rate or fail" — not "the nearest rate". The confusable
            // one is `set_rate_near`, which is never called here.
            hwp.set_rate(rate, ValueOr::Nearest)
                .map_err(|_| SinkError::RateRefused { asked: rate })?;

            // Period size and count *may* be approximated. That asymmetry
            // with the rate is deliberate: latency is a preference, the rate
            // is correctness.
            let p = hwp
                .set_period_size_near(period_frames as alsa::pcm::Frames, ValueOr::Nearest)
                .map_err(dev)?;
            let n = hwp.set_periods_near(periods, ValueOr::Nearest).map_err(dev)?;

            pcm.hw_params(&hwp).map_err(dev)?;
            (p as usize, n)
        };

        // Read back rather than trusting the request. If any of these
        // disagree, ALSA substituted something and the track must not play.
        let in_force = pcm.hw_params_current().map_err(dev)?;
        let got_rate = in_force.get_rate().map_err(dev)?;
        if got_rate != rate {
            return Err(SinkError::Substituted {
                field: "rate",
                asked: rate.to_string(),
                got: got_rate.to_string(),
            });
        }
        let got_format = in_force.get_format().map_err(dev)?;
        if got_format != Format::S24LE {
            return Err(SinkError::Substituted {
                field: "format",
                asked: "S24_LE".to_string(),
                got: format!("{:?}", got_format),
            });
        }
        let got_channels = in_force.get_channels().map_err(dev)?;
        if got_channels != SINK_CHANNELS as u32 {
            return Err(SinkError::Substituted {
                field: "channels",
                asked: SINK_CHANNELS.to_string(),
                got: got_channels.to_string(),
            });
        }
        drop(in_force);

        // Verifies the format once, which is what makes `io_unchecked::<i32>`
        // sound in the write path. Dropped immediately; holding it would
        // block any further `hw_params` call.
        drop(pcm.io_i32_s24().map_err(dev)?);

        let info = pcm.info().map_err(dev)?;
        let card = info.get_card();
        let dev_index = info.get_device();

        pcm.prepare().map_err(dev)?;

        Ok(AlsaSink {
            pcm,
            params: SinkParams {
                rate,
                channels: got_channels,
                format: SampleFormat::S24Le,
                period_frames: granted_period,
                periods: granted_periods,
            },
            card,
            device: dev_index,
        })
    }

    pub fn card(&self) -> i32 {
        self.card
    }

    /// The hardware half of the null test, as a check the program can make.
    ///
    /// `implementation.md` describes reading
    /// `/proc/asound/cardN/pcm0p/sub0/hw_params` while playing to prove ALSA
    /// accepted what was asked for and substituted nothing. That is a manual
    /// step there; here it is an assertion. Does file I/O, so **not** from
    /// the audio thread — call it once after playback starts.
    pub fn hw_params_in_force(&self) -> Result<ProcHwParams, SinkError> {
        let p = read_proc_hw_params(self.card, self.device)?;
        if p.rate != self.params.rate {
            return Err(SinkError::Substituted {
                field: "rate (in force)",
                asked: self.params.rate.to_string(),
                got: p.rate.to_string(),
            });
        }
        if p.format != "S24_LE" {
            return Err(SinkError::Substituted {
                field: "format (in force)",
                asked: "S24_LE".to_string(),
                got: p.format.clone(),
            });
        }
        if p.channels != self.params.channels {
            return Err(SinkError::Substituted {
                field: "channels (in force)",
                asked: self.params.channels.to_string(),
                got: p.channels.to_string(),
            });
        }
        Ok(p)
    }
}

impl AudioSink for AlsaSink {
    fn params(&self) -> SinkParams {
        self.params
    }

    fn verify_in_force(&self) -> Result<(), SinkError> {
        self.hw_params_in_force().map(|_| ())
    }

    fn write_period(&mut self, period: &[i32]) -> Result<(), SinkError> {
        if period.len() % SINK_CHANNELS != 0 {
            return Err(SinkError::WrongPeriodLength {
                given: period.len(),
                expected: self.params.period_frames * SINK_CHANNELS,
            });
        }
        // SAFETY: the device's format was verified as S24_LE at open, and
        // `S24_LE` is a 32-bit word, so `i32` is the correct sample type. No
        // other IO exists — this one is created and dropped inside this call,
        // and nothing else on this sink makes one.
        let io = unsafe { self.pcm.io_unchecked::<i32>() };

        let mut written = 0usize;
        while written < period.len() {
            match io.writei(&period[written..]) {
                Ok(frames) => written += frames * SINK_CHANNELS,
                Err(e) => {
                    // Recovery, not a decision: the alternative to preparing
                    // again is silence for the rest of the set. Reported so
                    // the caller knows a dropout happened.
                    let xrun = self.pcm.state() == State::XRun;
                    drop(io);
                    if xrun {
                        self.pcm.prepare().map_err(dev_rt)?;
                        return Err(SinkError::Underrun);
                    }
                    return Err(dev_rt(e));
                }
            }
        }
        Ok(())
    }

    fn drain(&mut self) -> Result<(), SinkError> {
        self.pcm.drain().map_err(dev)
    }
}

/// What `/proc/asound/cardN/pcmDp/sub0/hw_params` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcHwParams {
    pub access: String,
    pub format: String,
    pub channels: u32,
    pub rate: u32,
    pub period_size: u64,
    pub buffer_size: u64,
}

/// Reads and parses the kernel's own view of the running stream.
pub fn read_proc_hw_params(card: i32, device: u32) -> Result<ProcHwParams, SinkError> {
    let path = format!("/proc/asound/card{}/pcm{}p/sub0/hw_params", card, device);
    let text = fs::read_to_string(&path)
        .map_err(|e| SinkError::Device(format!("{}: {}", path, e)))?;
    parse_proc_hw_params(&text)
        .ok_or_else(|| SinkError::Device(format!("{} did not parse: {:?}", path, text)))
}

/// Split from the read so it can be tested without a sound card.
///
/// The file reads `closed` when nothing is playing, which is a legitimate
/// answer and not a parse failure — but it is also not a set of parameters,
/// so it is reported as one.
pub fn parse_proc_hw_params(text: &str) -> Option<ProcHwParams> {
    let mut access = None;
    let mut format = None;
    let mut channels = None;
    let mut rate = None;
    let mut period_size = None;
    let mut buffer_size = None;

    for line in text.lines() {
        let (key, value) = line.split_once(':')?;
        // `rate: 44100 (44100/1)` — take the first token of the value.
        let first = value.split_whitespace().next().unwrap_or("");
        match key.trim() {
            "access" => access = Some(first.to_string()),
            "format" => format = Some(first.to_string()),
            "channels" => channels = first.parse().ok(),
            "rate" => rate = first.parse().ok(),
            "period_size" => period_size = first.parse().ok(),
            "buffer_size" => buffer_size = first.parse().ok(),
            _ => {}
        }
    }

    Some(ProcHwParams {
        access: access?,
        format: format?,
        channels: channels?,
        rate: rate?,
        period_size: period_size?,
        buffer_size: buffer_size?,
    })
}

/// Fails if the card exposes any mixer control.
///
/// `implementation.md`: "A card with a volume control means the driver scales
/// the stream, even on `hw:`. The Digi2 Pro has none by design... Confirm with
/// `amixer -c N contents` that there is nothing there to scale with." This is
/// that confirmation, in code, so it runs on every start instead of once
/// during bring-up.
pub fn assert_no_mixer_controls(card: i32) -> Result<(), SinkError> {
    let mixer = alsa::mixer::Mixer::new(&format!("hw:{}", card), false).map_err(dev)?;
    let scalers: Vec<String> = mixer
        .iter()
        .filter_map(alsa::mixer::Selem::new)
        .filter(|s| {
            // **The question is whether anything can scale or mute the
            // stream, not whether the card has any control at all**, and the
            // difference is the whole finding. Every WM8804 card carries one:
            // `wm8804.c` declares `SND_SOC_DAPM_MUX("Tx Source", ...)`, a DAPM
            // mux, whose kcontrol is published under the widget's name — and
            // alsa-lib's `simple_add1` registers any `ENUMERATED` control as a
            // simple element even when its name matches none of the volume
            // suffixes. So the previous "any element at all is a failure"
            // test would have called an input selector a volume control, on
            // the real Digi2 Pro, every single time.
            //
            // That failure mode is worse than it sounds in both directions:
            // gated in an app loop it would stop the deck booting, and left as
            // a warning it would teach whoever reads the logs that this
            // warning is normal — so a genuine volume control would arrive
            // looking exactly like the one they had learned to ignore.
            s.has_playback_volume() || s.has_playback_switch() || s.has_capture_volume()
        })
        .map(|s| s.get_id().get_name().unwrap_or("?").to_string())
        .collect();
    if scalers.is_empty() {
        Ok(())
    } else {
        Err(SinkError::Device(format!(
            "card {} exposes volume or mute controls {:?} — the driver scales the \
             stream even on hw:",
            card, scalers
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_raw_hardware_device_names_are_accepted() {
        assert!(is_hardware_device("hw:0,0"));
        assert!(is_hardware_device("hw:CARD=sndrpihifiberry,DEV=0"));
        assert!(is_hardware_device("hw"));
        // Every one of these would have played, and converted, in silence.
        for bad in ["default", "plughw:0,0", "dmix:0", "pulse", "null", "sysdefault"] {
            assert!(!is_hardware_device(bad), "{} must be refused", bad);
        }
    }

    #[test]
    fn open_refuses_a_converting_device_before_touching_alsa() {
        // No card is needed to check this, which is the point: the rule is
        // testable everywhere, and it is the rule that cannot fail loudly
        // later.
        match AlsaSink::open("plughw:0,0", 44_100, 128, 2) {
            Err(SinkError::NotHardwareDevice(d)) => assert_eq!(d, "plughw:0,0"),
            other => panic!("expected a refusal, got {:?}", other.map(|s| s.params())),
        }
    }

    #[test]
    fn the_open_flags_are_the_four_the_design_names() {
        let f = open_flags() as u32;
        assert_ne!(f & alsa_sys::SND_PCM_NO_AUTO_RESAMPLE, 0);
        assert_ne!(f & alsa_sys::SND_PCM_NO_AUTO_CHANNELS, 0);
        assert_ne!(f & alsa_sys::SND_PCM_NO_AUTO_FORMAT, 0);
        assert_ne!(f & alsa_sys::SND_PCM_NO_SOFTVOL, 0);
        // And not nonblocking: the write path is a blocking writei loop.
        assert_eq!(f & alsa_sys::SND_PCM_NONBLOCK, 0);
    }

    #[test]
    fn proc_hw_params_parses_what_the_kernel_writes() {
        let text = "access: RW_INTERLEAVED\n\
                    format: S24_LE\n\
                    subformat: STD\n\
                    channels: 2\n\
                    rate: 44100 (44100/1)\n\
                    period_size: 128\n\
                    buffer_size: 256\n";
        let p = parse_proc_hw_params(text).expect("parses");
        assert_eq!(p.access, "RW_INTERLEAVED");
        assert_eq!(p.format, "S24_LE");
        assert_eq!(p.channels, 2);
        assert_eq!(p.rate, 44_100);
        assert_eq!(p.period_size, 128);
        assert_eq!(p.buffer_size, 256);
    }

    #[test]
    fn a_closed_stream_is_not_mistaken_for_parameters() {
        // The file says `closed` when nothing is playing. Returning a
        // zero-filled struct there would make a not-playing deck look like it
        // had agreed to 0 Hz.
        assert!(parse_proc_hw_params("closed\n").is_none());
        assert!(parse_proc_hw_params("").is_none());
        // A partial file is also not enough.
        assert!(parse_proc_hw_params("format: S24_LE\nrate: 44100\n").is_none());
    }
}
