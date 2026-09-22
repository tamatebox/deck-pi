//! `libsoxr` on the board the deck runs on, thermally soaked — issue #3.
//!
//! # What it measures, and why those two numbers
//!
//! **Realtime ratio** says whether a rate is usable at all: below 1.0 the
//! resampler cannot keep up and v2 falls back to unity for that track, which
//! is a designed path and is bit-perfect — what is lost is the speed control.
//!
//! **Worst-case block time** says whether it is usable *on a deadline*. A mean
//! comfortably above realtime with one block over the period is a dropout, and
//! the mean will not show it. The deck's period is 2 x 128 frames, measured on
//! the real device, so 128 frames is the unit here and the budget is one
//! period: 5.80 ms at 44.1 kHz down to 1.33 ms at 192 kHz.
//!
//! # The soak
//!
//! A run from cold measures a clock the board will not hold for a set and would
//! pass hardware that fails twenty minutes in. Rather than demand a separate
//! warm-up, this reports die temperature and ARM clock beside every row: the
//! matrix takes minutes and heats the board as it goes, so the later rows are
//! the soaked ones and the earlier ones are visibly not.
//! **Read the figures off the rows whose clock has settled.**
//!
//! This began as a 3B+ concern, where a soft limit dropped 1.4 GHz to 1.2 at
//! 60 C and `CLAUDE.md` said to size against the lower figure. **That mechanism
//! is 3A+/3B+ only and the deck now runs on a Pi 4**, which throttles from 80 C
//! and is unharmed by it. The soak survives the move for a different reason: the
//! clock column is also how a run admits that the *supply* gave out rather than
//! the heat, which on this bench is the likelier of the two.
//!
//! # The one configuration that is not a choice
//!
//! `soxr_create` is given the **whole ratio span**, 0.90 to 1.10, not 1:1.
//! `implementation.md` records why in detail: given 1:1 and then handed other
//! ratios, `SOXR_VR` reallocates to grow into the range it was not told about,
//! on whatever thread called it. Declared, it allocates nothing. Getting this
//! wrong here would measure a resampler doing something the deck would never
//! do, and the figures would look entirely reasonable.

use std::ffi::{c_char, c_double, c_uint, c_ulong, c_void};
use std::time::Instant;

// --- libsoxr, hand-written from the installed `soxr.h` ----------------------
//
// The three spec structs are passed by pointer to `soxr_create`, so a wrong
// layout is undefined behaviour rather than a compile error. Each was read
// field by field out of /usr/include/soxr.h; the `e` members are the header's
// own "reserved for internal use" and are filled by the helper functions
// below rather than by this code.

#[repr(C)]
#[derive(Clone, Copy)]
struct SoxrIoSpec {
    itype: c_uint,
    otype: c_uint,
    scale: c_double,
    e: *mut c_void,
    flags: c_ulong,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SoxrQualitySpec {
    precision: c_double,
    phase_response: c_double,
    passband_end: c_double,
    stopband_begin: c_double,
    e: *mut c_void,
    flags: c_ulong,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SoxrRuntimeSpec {
    log2_min_dft_size: c_uint,
    log2_large_dft_size: c_uint,
    coef_size_kbytes: c_uint,
    num_threads: c_uint,
    e: *mut c_void,
    flags: c_ulong,
}

type SoxrT = *mut c_void;
type SoxrError = *const c_char;

extern "C" {
    fn soxr_create(
        input_rate: c_double,
        output_rate: c_double,
        num_channels: c_uint,
        error: *mut SoxrError,
        io_spec: *const SoxrIoSpec,
        quality_spec: *const SoxrQualitySpec,
        runtime_spec: *const SoxrRuntimeSpec,
    ) -> SoxrT;
    fn soxr_io_spec(itype: c_uint, otype: c_uint) -> SoxrIoSpec;
    fn soxr_quality_spec(recipe: c_ulong, flags: c_ulong) -> SoxrQualitySpec;
    fn soxr_runtime_spec(num_threads: c_uint) -> SoxrRuntimeSpec;
    fn soxr_set_io_ratio(resampler: SoxrT, io_ratio: c_double, slew_len: usize) -> SoxrError;
    fn soxr_process(
        resampler: SoxrT,
        input: *const c_void,
        ilen: usize,
        idone: *mut usize,
        output: *mut c_void,
        olen: usize,
        odone: *mut usize,
    ) -> SoxrError;
    fn soxr_delete(resampler: SoxrT);
}

/// `SOXR_INT32_I`. The deck's ring is int32, left-justified, so this is what a
/// resampler in the playback path would actually be handed. Measuring float32
/// would measure a conversion the deck does not do.
///
/// **Two, and it was six for one run.** The enum restarts its numbering
/// halfway — `SOXR_SPLIT = 4` — so the interleaved names are 0..3 and the
/// split ones 4..7. Counting down the list without noticing gives 6, which is
/// `SOXR_INT32_S`: libsoxr then reads the buffer as an array of per-channel
/// pointers, dereferences sample values as addresses, and the process dies on
/// the first block. The header was open at the time; the mistake was arithmetic
/// on an enum, not a guess about the API.
const SOXR_INT32_I: c_uint = 2;
/// `SOXR_VR` — variable rate, which is the whole reason libsoxr was chosen.
const SOXR_VR: c_ulong = 32;

/// The five standard recipes, by the header's own names.
const RECIPES: [(&str, c_ulong); 5] = [
    ("QQ", 0),
    ("LQ", 1),
    ("MQ", 2),
    ("HQ/20bit", 4),
    ("VHQ/28bit", 6),
];

/// `decisions.md` fixes the pitch range at ±10%, supplied rather than
/// inferred, so this is a constant and not a dimension of the matrix.
const RATIO_LO: f64 = 0.90;
const RATIO_HI: f64 = 1.10;

/// Exactly `file.rs`'s `SUPPORTED_RATES`, in the order it lists them.
const RATES: [u32; 6] = [44_100, 88_200, 176_400, 48_000, 96_000, 192_000];

/// The deck's period, measured on the real device: `2 x 128 frames`.
const BLOCK: usize = 128;
const CHANNELS: usize = 2;

fn read_trimmed(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
}

/// Die temperature in °C, or `None` off a Pi.
fn temp_c() -> Option<f64> {
    read_trimmed("/sys/class/thermal/thermal_zone0/temp")?
        .parse::<f64>()
        .ok()
        .map(|milli| milli / 1000.0)
}

/// ARM clock in MHz, from the firmware rather than from the governor.
///
/// **`scaling_cur_freq` is the wrong file and reading it cost one whole run.**
/// It reports what the governor asked for; the firmware's thermal throttle
/// happens underneath and does not appear there. A soaked run showed 1400 MHz
/// in that file while `vcgencmd` said 1200 — which is the difference between
/// figures taken at the clock the board holds and figures labelled with a
/// clock it does not.
fn clock_mhz() -> Option<f64> {
    let out = std::process::Command::new("vcgencmd")
        .args(["measure_clock", "arm"])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let hz: f64 = text.trim().rsplit('=').next()?.parse().ok()?;
    Some(hz / 1e6)
}

/// The firmware's throttle word. Bit 2 is "currently throttled", bit 3
/// "currently capped"; the high bits are the same conditions latched since
/// boot. Printed because a reader looking at these figures in six months
/// needs to know the board was actually throttled when they were taken.
fn throttled() -> Option<String> {
    let out = std::process::Command::new("vcgencmd")
        .arg("get_throttled")
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    Some(text.trim().rsplit('=').next()?.to_owned())
}

struct Case {
    realtime: f64,
    worst_block_us: f64,
    mean_block_us: f64,
    blocks: u64,
}

/// One precision at one rate, for `seconds` of wall clock.
///
/// The ratio moves every block. A fixed ratio would let the resampler settle
/// into a cheap path and would measure the wrong thing entirely: the deck's
/// fader is a hand on a slider, so the interesting cost is the one paid while
/// the ratio is moving.
fn run(recipe: c_ulong, rate: u32, seconds: f64, moving: bool) -> Result<Case, String> {
    run_with(recipe, rate, seconds, moving, SOXR_VR)
}

fn run_with(
    recipe: c_ulong,
    rate: u32,
    seconds: f64,
    moving: bool,
    quality_flags: c_ulong,
) -> Result<Case, String> {
    let io = unsafe { soxr_io_spec(SOXR_INT32_I, SOXR_INT32_I) };
    let quality = unsafe { soxr_quality_spec(recipe, quality_flags) };
    let runtime = unsafe { soxr_runtime_spec(1) };
    let mut err: SoxrError = std::ptr::null();

    // The span, not 1:1. See the module comment.
    let soxr = unsafe {
        soxr_create(
            RATIO_LO,
            RATIO_HI,
            CHANNELS as c_uint,
            &mut err,
            &io,
            &quality,
            &runtime,
        )
    };
    if soxr.is_null() {
        return Err("soxr_create returned null".into());
    }

    // A ramp rather than silence: a resampler fed zeros is still doing the
    // arithmetic, but a signal that moves is what the deck would carry and
    // costs nothing extra to generate once.
    let input: Vec<i32> = (0..BLOCK * CHANNELS)
        .map(|i| ((i as i64 * 0x0001_3579) as i32) >> 8)
        .collect();
    // Room for the widest ratio plus the resampler's own latency.
    let mut output = vec![0i32; BLOCK * CHANNELS * 4];

    let mut worst = 0f64;
    let mut blocks = 0u64;
    let mut frames_in = 0u64;
    let mut ratio_t = 0f64;

    // Unity, set once, so the fixed pass is still a resample rather than a
    // path that might be optimised away as a no-op.
    let variable = quality_flags & SOXR_VR != 0;
    if !moving && variable {
        let rc = unsafe { soxr_set_io_ratio(soxr, 1.0, 0) };
        if !rc.is_null() {
            unsafe { soxr_delete(soxr) };
            return Err("soxr_set_io_ratio failed".into());
        }
    }

    let started = Instant::now();
    while started.elapsed().as_secs_f64() < seconds {
        if moving && variable {
            // A full sweep every ~2 s at any rate, which is a brisk but human
            // hand on the fader.
            ratio_t += 1.0 / (2.0 * rate as f64 / BLOCK as f64);
            let phase = (ratio_t * std::f64::consts::TAU).sin() * 0.5 + 0.5;
            let ratio = RATIO_LO + (RATIO_HI - RATIO_LO) * phase;
            let rc = unsafe { soxr_set_io_ratio(soxr, ratio, 0) };
            if !rc.is_null() {
                unsafe { soxr_delete(soxr) };
                return Err("soxr_set_io_ratio failed".into());
            }
        }

        let mut idone = 0usize;
        let mut odone = 0usize;
        let block_started = Instant::now();
        let rc = unsafe {
            soxr_process(
                soxr,
                input.as_ptr() as *const c_void,
                BLOCK,
                &mut idone,
                output.as_mut_ptr() as *mut c_void,
                output.len() / CHANNELS,
                &mut odone,
            )
        };
        let took = block_started.elapsed().as_secs_f64() * 1e6;
        if !rc.is_null() {
            unsafe { soxr_delete(soxr) };
            return Err("soxr_process failed".into());
        }
        // The first blocks fill the delay line and are not representative;
        // they are still counted as work, just not as the worst case.
        if blocks > 16 && took > worst {
            worst = took;
        }
        blocks += 1;
        frames_in += idone as u64;
    }
    let wall = started.elapsed().as_secs_f64();
    unsafe { soxr_delete(soxr) };

    Ok(Case {
        realtime: frames_in as f64 / wall / rate as f64,
        worst_block_us: worst,
        mean_block_us: wall * 1e6 / blocks.max(1) as f64,
        blocks,
    })
}

/// Is the precision recipe doing anything at all?
///
/// The matrix came out identical across all five recipes, and the recipes do
/// reach `soxr_quality_spec` — the printed specs differ. That leaves the
/// engine. Running the same work with and without `SOXR_VR` separates "the
/// recipe does not matter here" from "the recipe does not matter in variable
/// rate mode", and those are very different things to write down.
fn probe(seconds: f64) {
    println!("mean us per {BLOCK}-frame block, ratio fixed, 2 ch int32\n");
    println!("{:<12} {:>12} {:>12}", "recipe", "with VR", "without VR");
    for (name, recipe) in RECIPES {
        let vr = run_with(recipe, 44_100, seconds, false, SOXR_VR);
        // Without VR the ratio is whatever `soxr_create` was given, so it is
        // created at a real rate pair rather than at the span.
        let plain = run_with(recipe, 44_100, seconds, false, 0);
        let show = |r: &Result<Case, String>| match r {
            Ok(c) => format!("{:.0}", c.mean_block_us),
            Err(e) => e.clone(),
        };
        println!("{:<12} {:>11} {:>11}", name, show(&vr), show(&plain));
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("probe") {
        let seconds: f64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(1.0);
        probe(seconds);
        return;
    }
    let seconds: f64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10.0);

    println!("libsoxr variable-rate, int32 in and out, {CHANNELS} ch, {BLOCK}-frame blocks");
    println!("ratio swept {RATIO_LO} to {RATIO_HI}, declared at soxr_create");
    println!("{seconds:.0} s per case; the board heats as this runs, so read the settled rows\n");
    // What each recipe actually asks for, printed because the matrix below
    // came out identical across all five and the first question is whether
    // the recipe reaches the spec at all.
    println!("what each recipe asks for, with SOXR_VR set:");
    for (name, recipe) in RECIPES {
        let q = unsafe { soxr_quality_spec(recipe, SOXR_VR) };
        println!(
            "  {:<10} precision {:>5.1} bits  passband {:.4}  stopband {:.4}  flags {:#x}",
            name, q.precision, q.passband_end, q.stopband_begin, q.flags
        );
    }
    println!();

    println!(
        "throttle word at start: {}\n",
        throttled().unwrap_or_else(|| "unknown".into())
    );
    println!("block times in us. \"moving\" sets the ratio every block, \"fixed\" sets it once.\n");
    println!(
        "{:<10} {:>8} {:>9} {:>9} {:>8} {:>9} {:>7} {:>7} {:>8}",
        "precision", "rate", "realtime", "mean", "worst", "fixed", "budget", "temp", "clock"
    );

    let mut any_failed = false;
    for (name, recipe) in RECIPES {
        for rate in RATES {
            // One period of the deck's own geometry: 2 x BLOCK frames.
            let budget_us = 2.0 * BLOCK as f64 / rate as f64 * 1e6;
            let moving = run(recipe, rate, seconds, true);
            let fixed = run(recipe, rate, seconds, false);
            match (moving, fixed) {
                (Ok(m), Ok(f)) => {
                    let over = m.worst_block_us > budget_us || m.realtime < 1.0;
                    any_failed |= over;
                    println!(
                        "{:<10} {:>8} {:>8.2}x {:>9.0} {:>8.0} {:>9.0} {:>7.0} {:>6.1}C {:>6.0}MHz{}",
                        name,
                        rate,
                        m.realtime,
                        m.mean_block_us,
                        m.worst_block_us,
                        f.mean_block_us,
                        budget_us,
                        temp_c().unwrap_or(f64::NAN),
                        clock_mhz().unwrap_or(f64::NAN),
                        if over { "  <-- over" } else { "" }
                    );
                    let _ = m.blocks;
                }
                (Err(e), _) | (_, Err(e)) => {
                    any_failed = true;
                    println!("{name:<10} {rate:>8}  {e}");
                }
            }
        }
    }

    println!(
        "\nthrottle word at end: {}",
        throttled().unwrap_or_else(|| "unknown".into())
    );
    println!(
        "\nover = worst block exceeded one period, or throughput fell under realtime.\n\
         Neither is fatal to the deck: v2 falls back to unity for a rate it cannot\n\
         sustain, which is bit-perfect and loses only the speed control."
    );
    if any_failed {
        std::process::exit(1);
    }
}
