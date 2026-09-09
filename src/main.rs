//! Placeholder entry point. Prints what the file layer makes of each path
//! given to it, which is the same judgement the browser will show on the
//! highlighted row.

use std::path::PathBuf;

use deck_pi::engine::{Engine, Outcome};
use deck_pi::file::{OpenError, Track, RING_CHANNELS};
use deck_pi::ring::{self, Miss};
use deck_pi::sink::{AudioSink, CaptureSink};
use deck_pi::transport::Transport;
use deck_pi::window::Window;

fn main() {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if args.is_empty() {
        eprintln!("usage: deck-pi [--drain] [--device=hw:...] <file>...");
        eprintln!("  default          reports what the file layer makes of each path");
        eprintln!("  --drain          also pulls every frame through the window thread,");
        eprintln!("                   the ring and the callback into a capture sink");
        eprintln!("  --device=hw:X,Y  plays for real through ALSA, and checks that the");
        eprintln!("                   card exposes no mixer control and that");
        eprintln!("                   /proc/asound reports the rate and format asked for");
        eprintln!("                   (Linux only; hw: devices only, never plughw)");
        std::process::exit(2);
    }

    let drain = args.iter().any(|a| a == std::ffi::OsStr::new("--drain"));
    let device: Option<String> = args.iter().find_map(|a| {
        a.to_string_lossy()
            .strip_prefix("--device=")
            .map(|d| d.to_string())
    });
    let args: Vec<PathBuf> = args
        .into_iter()
        .filter(|a| {
            let s = a.to_string_lossy();
            s != "--drain" && !s.starts_with("--device=")
        })
        .collect();

    for path in args {
        match Track::open(&path) {
            Ok(track) => {
                let i = track.info();
                let secs = i.duration_secs();
                print!(
                    "PLAYS   {}  {} {} Hz {} {}ch  {:.0}:{:05.2}",
                    path.display(),
                    i.container,
                    i.rate,
                    i.depth,
                    i.channels,
                    (secs / 60.0).floor(),
                    secs % 60.0
                );
                print!(
                    "  window ±{:.0}s",
                    ring::half_window_secs(i.rate, ring::WINDOW_BYTES_PLACEHOLDER)
                );
                if i.declared_length_is_suspect() {
                    print!("  [declared length suspect: past the 2 GiB ceiling]");
                }
                println!();
                match device.as_deref() {
                    Some(d) => play_to_device(&path, d),
                    None if drain => drain_through_the_ring(&path),
                    None => {}
                }
            }
            Err(OpenError::Rejected(why)) => println!("REFUSED {}  {}", path.display(), why),
            Err(OpenError::Unreadable(e)) => println!("UNREAD  {}  {}", path.display(), e),
        }
    }
}

/// One period at a time: fill from the ring, hand it to the sink. This is the
/// shape the realtime thread will have, minus `SCHED_FIFO` and `mlockall`.
fn play<S: AudioSink>(
    path: &std::path::Path,
    sink: &mut S,
    label: &str,
) -> Result<(), String> {
    let period_frames = sink.period_frames();
    let (window, reader, info) = Window::load(path, ring::WINDOW_BYTES_PLACEHOLDER)
        .map_err(|e| e.to_string())?;

    let (tx, rx) = std::sync::mpsc::channel();
    let failure = std::sync::Arc::new(std::sync::Mutex::new(None));
    let f = std::sync::Arc::clone(&failure);
    let thread = std::thread::spawn(move || {
        window.run(rx, move |e| {
            if let deck_pi::window::Event::Failed(msg) = e {
                *f.lock().unwrap() = Some(msg);
            }
        })
    });

    let transport = Transport::new();
    let mut engine = Engine::new(info.frames);
    transport.play();

    let started = std::time::Instant::now();
    let mut period = vec![0i32; period_frames * RING_CHANNELS];
    let mut played = 0u64;
    let mut waits = 0u64;
    let mut underruns = 0u64;
    let mut peak: i32 = 0;

    loop {
        match engine.fill(&transport, &reader, &mut period) {
            Outcome::Played { frames } | Outcome::PlayedTail { frames } => {
                for &s in &period[..frames * RING_CHANNELS] {
                    peak = peak.max(s.abs());
                }
                match sink.write_period(&period[..frames * RING_CHANNELS]) {
                    Ok(()) => played += frames as u64,
                    Err(deck_pi::sink::SinkError::Underrun) => underruns += 1,
                    Err(e) => return Err(e.to_string()),
                }
            }
            Outcome::EndOfTrack => break,
            Outcome::Missed(Miss::NotResident) => {
                waits += 1;
                if failure.lock().unwrap().is_some() {
                    break;
                }
                std::thread::yield_now();
            }
            other => {
                println!("        {}: {:?} at frame {}", label, other, engine.position());
                break;
            }
        }
    }

    let _ = sink.drain();
    let _ = tx.send(deck_pi::window::Command::Shutdown);
    let _ = thread.join();

    let elapsed = started.elapsed();
    if let Some(msg) = failure.lock().unwrap().clone() {
        return Err(format!("after {} frames: {}", played, msg));
    }
    let audio_secs = played as f64 / info.rate as f64;
    println!(
        "        {}: {}/{} frames in {:.3} s ({:.1}x realtime), {} waits, \
         {} underruns, peak {:#x}",
        label,
        played,
        info.frames,
        elapsed.as_secs_f64(),
        audio_secs / elapsed.as_secs_f64().max(1e-9),
        waits,
        underruns,
        peak
    );
    Ok(())
}

/// Pulls the whole track through window thread, ring and callback into a
/// capture sink — the entire v1 software path with a collector where ALSA
/// would be. Runs anywhere, including a machine with no sound card.
fn drain_through_the_ring(path: &std::path::Path) {
    const PERIOD: usize = 256;
    let rate = match Track::open(path) {
        Ok(t) => t.info().rate,
        Err(e) => {
            println!("        drain: {}", e);
            return;
        }
    };
    // Capacity for the whole track, so nothing reallocates mid-run.
    let frames = Track::open(path).map(|t| t.info().frames).unwrap_or(0) as usize;
    let mut sink = CaptureSink::new(rate, PERIOD, frames + PERIOD);
    if let Err(e) = play(path, &mut sink, "drain") {
        println!("        drain: FAILED — {}", e);
    }
}

/// Plays for real, and runs the two checks `implementation.md` calls the
/// hardware half — with the card's mixer inspected first, because a card with
/// a volume control has its driver scale the stream even on `hw:`.
#[cfg(target_os = "linux")]
fn play_to_device(path: &std::path::Path, device: &str) {
    use deck_pi::sink::alsa::{assert_no_mixer_controls, AlsaSink};

    // architecture.md targets 5-10 ms; at 44.1 kHz that means 128-frame
    // periods, not 256 (see the sink's own test).
    const PERIOD: usize = 128;
    const PERIODS: u32 = 2;

    let rate = match Track::open(path) {
        Ok(t) => t.info().rate,
        Err(e) => {
            println!("        device: {}", e);
            return;
        }
    };

    let mut sink = match AlsaSink::open(device, rate, PERIOD, PERIODS) {
        Ok(s) => s,
        Err(e) => {
            println!("        device: could not open {} at {} Hz — {}", device, rate, e);
            return;
        }
    };
    let p = sink.params();
    println!(
        "        device: {} at {} Hz {} {}ch, {} x {} frames = {:.2} ms",
        device,
        p.rate,
        p.format,
        p.channels,
        p.periods,
        p.period_frames,
        p.latency_secs() * 1000.0
    );

    match assert_no_mixer_controls(sink.card()) {
        Ok(()) => println!("        mixer:  no controls — nothing to scale the stream with"),
        Err(e) => println!("        mixer:  WARNING {}", e),
    }

    let card = sink.card();
    if let Err(e) = play(path, &mut sink, "device") {
        println!("        device: FAILED — {}", e);
        return;
    }
    // Read after playing: the file says `closed` when nothing is running, so
    // this reports what the last stream actually used only if the driver
    // keeps it. During a long track, call it while playing instead.
    match deck_pi::sink::alsa::read_proc_hw_params(card, 0) {
        Ok(hw) => println!("        hw_params: {:?}", hw),
        Err(e) => println!("        hw_params: {}", e),
    }
}

#[cfg(not(target_os = "linux"))]
fn play_to_device(_path: &std::path::Path, device: &str) {
    println!(
        "        device: --device={} needs Linux; ALSA does not exist here. \
         Use --drain for the software path.",
        device
    );
}
