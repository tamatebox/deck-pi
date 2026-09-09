//! Placeholder entry point. Prints what the file layer makes of each path
//! given to it, which is the same judgement the browser will show on the
//! highlighted row.

use std::path::PathBuf;

use deck_pi::engine::{Engine, Outcome};
use deck_pi::file::{OpenError, Track, RING_CHANNELS};
use deck_pi::ring::{self, Miss};
use deck_pi::rt;
use deck_pi::sink::{AudioSink, CaptureSink};
use deck_pi::transport::Transport;
use deck_pi::window::Window;

fn main() {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if let Some(arg) = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .find(|s| s == "--rt-check" || s.starts_with("--rt-check="))
    {
        // `--rt-check=2` also exercises core pinning, which is the one part
        // of the setup with nothing to read it back from except the affinity
        // mask itself.
        let cpu = arg.strip_prefix("--rt-check=").and_then(|c| c.parse().ok());
        std::process::exit(rt_check(cpu));
    }
    if args.is_empty() {
        eprintln!("usage: deck-pi [--drain] [--device=hw:...] <file>...");
        eprintln!("       deck-pi --rt-check");
        eprintln!("  default          reports what the file layer makes of each path");
        eprintln!("  --drain          also pulls every frame through the window thread,");
        eprintln!("                   the ring and the callback into a capture sink");
        eprintln!("  --device=hw:X,Y  plays for real through ALSA, and checks that the");
        eprintln!("                   card exposes no mixer control and that");
        eprintln!("                   /proc/asound reports the rate and format asked for");
        eprintln!("                   (Linux only; hw: devices only, never plughw)");
        eprintln!("  --rt-check[=N]   applies the realtime setup and reads back what the");
        eprintln!("                   kernel actually granted; =N also pins to core N");
        eprintln!("                   (Linux only)");
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

/// Applies the realtime setup and prints what the kernel granted.
///
/// A separate mode rather than something the other paths do, because it
/// changes the process: `mlockall` is process-wide and `SCHED_FIFO` would put
/// the bring-up tool's own bookkeeping at realtime priority. On the deck it
/// belongs at start-up on the audio thread; here it is a bring-up check, in
/// the same spirit as `--device=` reading `hw_params` back.
///
/// Prints the limits first even when the setup succeeds, because "it worked"
/// and "it worked because this user is root" are different answers.
fn rt_check(cpu: Option<usize>) -> i32 {
    match rt::limits() {
        Ok(lim) => {
            println!(
                "limits: memlock {}  rtprio {}",
                match lim.memlock_bytes {
                    None => "unlimited".to_string(),
                    Some(b) => format!("{} bytes ({} MiB)", b, b / (1024 * 1024)),
                },
                lim.rtprio
            );
        }
        Err(e) => {
            println!("limits: unreadable — {}", e);
        }
    }

    // What `mlockall(MCL_FUTURE)` will have to cover: the ring, plus the
    // process as it stands. Sized from the placeholder window because N is
    // not chosen yet (architecture.md), and doubled because the behind and
    // ahead halves are both in the same allocation and a track change
    // overlaps two rings.
    let needed = 2 * ring::WINDOW_BYTES_PLACEHOLDER as u64;
    let req = rt::RtRequest {
        cpu,
        ..rt::RtRequest::default()
    };
    println!(
        "asking:  SCHED_FIFO {}  memlock >= {} MiB  prefault {} KiB  cpu {:?}",
        req.priority,
        needed / (1024 * 1024),
        req.stack_prefault_bytes / 1024,
        req.cpu
    );

    let reached = rt::prefault_stack(req.stack_prefault_bytes);
    println!(
        "prefault: reached {} bytes of stack for a request of {}",
        reached, req.stack_prefault_bytes
    );

    match rt::apply(&req, needed) {
        Ok(got) => {
            println!(
                "in force: {} priority {}  VmLck {} kB  cpus {:?}",
                got.policy, got.priority, got.locked_kib, got.cpus
            );
            println!("rt-check: OK");
            0
        }
        Err(e) => {
            println!("rt-check: FAILED\n{}", e);
            1
        }
    }
}
