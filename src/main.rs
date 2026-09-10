//! Placeholder entry point. Prints what the file layer makes of each path
//! given to it, which is the same judgement the browser will show on the
//! highlighted row.

use std::path::PathBuf;

use deck_pi::engine::{Engine, Outcome};
use deck_pi::file::{OpenError, Track, RING_CHANNELS};
use deck_pi::browser::{Browser, Row, Verdict};
use deck_pi::media::{self, Medium};
use deck_pi::ring;
use deck_pi::rt;
use deck_pi::sink::{AudioSink, CaptureSink};
use deck_pi::transport::Transport;
use deck_pi::window::Window;

/// What the CLI was asked to do. Exactly one of these, which is the point:
/// the old parser let `--rt-check` win from any position while silently
/// ignoring everything else on the line, and let `--device=` win over
/// `--drain` without saying so.
#[derive(Debug, PartialEq, Eq)]
enum Mode {
    /// Report what the file layer makes of each path, and stop.
    Report,
    /// Also pull every frame through the window thread, ring and callback.
    Drain,
    /// Play for real through ALSA.
    Device(String),
    RtCheck(Option<usize>),
    MediaCheck(PathBuf),
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    mode: Mode,
    paths: Vec<PathBuf>,
}

/// Hand-rolled, and small enough to stay that way — but **every unrecognised
/// argument is an error**, which is the whole of what was wrong before.
///
/// `deck-pi --drian short.wav` used to print `UNREAD --drian`, then play the
/// file *without* draining, and exit 0. Three failures in one line: the typo
/// became a filename, the mode it asked for was silently not applied, and the
/// exit code said everything was fine. A bring-up tool that reports success
/// for a run that did not happen is worse than no tool.
fn parse<I: IntoIterator<Item = std::ffi::OsString>>(argv: I) -> Result<Args, String> {
    let mut paths = Vec::new();
    let mut drain = false;
    let mut device = None;
    let mut rt_check = None;
    let mut media_check = None;
    let mut only_paths = false;

    for arg in argv {
        let text = arg.to_string_lossy().into_owned();
        if only_paths || !text.starts_with('-') {
            paths.push(PathBuf::from(arg));
            continue;
        }
        match text.as_str() {
            // Everything after `--` is a path, for a file whose name begins
            // with a dash.
            "--" => only_paths = true,
            "--drain" => drain = true,
            "--rt-check" => rt_check = Some(None),
            "--media-check" => media_check = Some(PathBuf::from(media::MOUNT_POINT)),
            _ if text.starts_with("--device=") => {
                device = Some(text["--device=".len()..].to_string())
            }
            _ if text.starts_with("--rt-check=") => {
                let n = &text["--rt-check=".len()..];
                // Parsed rather than `.ok()`-ed away: the old code turned
                // `--rt-check=abc` into an unpinned run, so a typo in the one
                // argument that exercises core pinning meant the pinning
                // silently did not happen.
                let cpu = n
                    .parse::<usize>()
                    .map_err(|_| format!("--rt-check= wants a core number, got {n:?}"))?;
                rt_check = Some(Some(cpu));
            }
            _ if text.starts_with("--media-check=") => {
                media_check = Some(PathBuf::from(&text["--media-check=".len()..]))
            }
            "--device" | "--rt-check-" => {
                return Err(format!("{text} takes its value with '=', as {text}=..."))
            }
            _ => return Err(format!("unknown argument {text:?}")),
        }
    }

    // The two check modes take over the whole run, so anything else on the
    // line was ignored — which used to happen in silence.
    let extras = drain || device.is_some() || !paths.is_empty();
    match (rt_check, media_check) {
        (Some(_), Some(_)) => Err("--rt-check and --media-check are separate runs".into()),
        (Some(_), None) if extras => {
            Err("--rt-check takes nothing else; it is a check, not a mode".into())
        }
        (None, Some(_)) if extras => {
            Err("--media-check takes nothing else; it is a check, not a mode".into())
        }
        (Some(cpu), None) => Ok(Args { mode: Mode::RtCheck(cpu), paths }),
        (None, Some(at)) => Ok(Args { mode: Mode::MediaCheck(at), paths }),
        (None, None) => playback(drain, device, paths),
    }
}

/// The ordinary run: report, drain, or play to a device.
fn playback(drain: bool, device: Option<String>, paths: Vec<PathBuf>) -> Result<Args, String> {
    // Both ask for playback and they are different playbacks. The old parser
    // let the device win and dropped `--drain` on the floor.
    if drain && device.is_some() {
        return Err("--drain and --device= are two different runs; pick one".into());
    }
    if paths.is_empty() {
        return Err("no file given".into());
    }
    let mode = match device {
        Some(d) => Mode::Device(d),
        None if drain => Mode::Drain,
        None => Mode::Report,
    };
    Ok(Args { mode, paths })
}

fn usage() {
    eprintln!("usage: deck-pi [--drain | --device=hw:...] [--] <file>...");
    eprintln!("       deck-pi --rt-check[=N]");
    eprintln!("       deck-pi --media-check[=PATH]");
    eprintln!("  default          reports what the file layer makes of each path");
    eprintln!("  --drain          also pulls every frame through the window thread,");
    eprintln!("                   the ring and the callback into a capture sink");
    eprintln!("  --device=hw:X,Y  plays for real through ALSA, and checks that the");
    eprintln!("                   card exposes no volume control and that");
    eprintln!("                   /proc/asound reports the rate and format asked for");
    eprintln!("                   (Linux only; hw: devices only, never plughw)");
    eprintln!("  --media-check[=P] reports the medium's state at P, and lists the root");
    eprintln!("                   folder through the browser if it is browsable");
    eprintln!("  --rt-check[=N]   applies the realtime setup and reads back what the");
    eprintln!("                   kernel actually granted; =N also pins to core N");
    eprintln!("                   (Linux only)");
    eprintln!("  --               everything after this is a path");
    eprintln!();
    eprintln!("exit 0 only if every path was playable and every run succeeded.");
}

fn main() {
    let args = match parse(std::env::args_os().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("deck-pi: {e}");
            eprintln!();
            usage();
            std::process::exit(2);
        }
    };

    match args.mode {
        Mode::MediaCheck(at) => std::process::exit(media_check(&at)),
        Mode::RtCheck(cpu) => std::process::exit(rt_check(cpu)),
        _ => {}
    }

    // **The exit code is a result, not a formality.** Every one of these
    // paths used to end in `exit 0`, so a script could not tell a stick full
    // of playable files from a stick full of FLAC.
    let mut bad = 0usize;
    for path in &args.paths {
        match Track::open(path) {
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
                let played = match &args.mode {
                    Mode::Device(d) => play_to_device(path, d),
                    Mode::Drain => {
                        drain_through_the_ring(path);
                        true
                    }
                    _ => true,
                };
                if !played {
                    bad += 1;
                }
            }
            Err(OpenError::Rejected(why)) => {
                println!("REFUSED {}  {}", path.display(), why);
                bad += 1;
            }
            Err(OpenError::Unreadable(e)) => {
                println!("UNREAD  {}  {}", path.display(), e);
                bad += 1;
            }
        }
    }
    if bad > 0 {
        eprintln!("deck-pi: {bad} of {} did not play", args.paths.len());
        std::process::exit(1);
    }
}

/// One period at a time: fill from the ring, hand it to the sink. This is the
/// shape the realtime thread will have, minus `SCHED_FIFO` and `mlockall`.
fn play<S: AudioSink>(
    path: &std::path::Path,
    sink: &mut S,
    label: &str,
    feed_silence_on_miss: bool,
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
    let mut verified = false;

    loop {
        match engine.fill(&transport, &reader, &mut period) {
            Outcome::Played { frames } | Outcome::PlayedTail { frames } => {
                for &s in &period[..frames * RING_CHANNELS] {
                    peak = peak.max(s.abs());
                }
                match sink.write_period(&period[..frames * RING_CHANNELS]) {
                    Ok(()) => {
                        played += frames as u64;
                        // Once, as soon as the stream is actually running.
                        // `hw_params` reads `closed` before that and after
                        // `drain`, which is why this cannot wait until the
                        // end — the previous version read it after playback
                        // and printed it without comparing anything.
                        if !verified {
                            verified = true;
                            if let Err(e) = sink.verify_in_force() {
                                return Err(format!("ALSA substituted something: {e}"));
                            }
                        }
                    }
                    Err(deck_pi::sink::SinkError::Underrun) => underruns += 1,
                    Err(e) => return Err(e.to_string()),
                }
            }
            Outcome::EndOfTrack => {
                // The deck stops here rather than advancing; see
                // `Transport::reached_end`. This loop is the only caller
                // today, which is the whole of what "the app loop" means so
                // far.
                transport.reached_end();
                break;
            }
            // Any miss: the window thread has not got here yet, or it is
            // mid-relocation. Both are transient and both are a period of
            // silence, not a failure — matching `Miss` as a whole rather than
            // naming one variant is deliberate, because `Relocated` used to
            // fall through to the arm below and end playback. It is common at
            // the start of a track, where the window relocates to frame zero.
            Outcome::Missed(_) => {
                waits += 1;
                if failure.lock().unwrap().is_some() {
                    break;
                }
                if feed_silence_on_miss {
                    // **A real device has to be fed.** `engine.rs` fills the
                    // buffer with silence and calls it "silence, not a
                    // stall"; discarding it leaves ALSA to run dry, which is
                    // an xrun, a `prepare()` and a longer gap than the one
                    // period the engine was offering. The position does not
                    // advance, so the track resumes where it was and comes
                    // out one period longer — which is what a dropout is.
                    //
                    // A capture sink is fed nothing and waits instead: it has
                    // no deadline, and the null test wants the track's own
                    // samples rather than a faithful record of how late the
                    // filler was.
                    match sink.write_period(&period) {
                        Ok(()) | Err(deck_pi::sink::SinkError::Underrun) => {}
                        Err(e) => return Err(e.to_string()),
                    }
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
    if let Err(e) = play(path, &mut sink, "drain", false) {
        println!("        drain: FAILED — {}", e);
    }
}

/// Plays for real, and runs the two checks `implementation.md` calls the
/// hardware half — with the card's mixer inspected first, because a card with
/// a volume control has its driver scale the stream even on `hw:`.
#[cfg(target_os = "linux")]
/// Returns whether the device actually played it.
fn play_to_device(path: &std::path::Path, device: &str) -> bool {
    use deck_pi::sink::alsa::{assert_no_mixer_controls, AlsaSink};

    // architecture.md targets 5-10 ms; at 44.1 kHz that means 128-frame
    // periods, not 256 (see the sink's own test).
    const PERIOD: usize = 128;
    const PERIODS: u32 = 2;

    let rate = match Track::open(path) {
        Ok(t) => t.info().rate,
        Err(e) => {
            println!("        device: {}", e);
            return false;
        }
    };

    let mut sink = match AlsaSink::open(device, rate, PERIOD, PERIODS) {
        Ok(s) => s,
        Err(e) => {
            println!("        device: could not open {} at {} Hz — {}", device, rate, e);
            return false;
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

    if let Err(e) = play(path, &mut sink, "device", true) {
        println!("        device: FAILED — {}", e);
        return false;
    }
    // `hw_params` is checked inside `play`, once the stream is running, and a
    // mismatch fails the track rather than being printed. Reading it here
    // instead — which is what this did — reports `closed`, because the device
    // has been drained.
    println!("        hw_params: verified in force while playing");
    true
}

#[cfg(not(target_os = "linux"))]
fn play_to_device(_path: &std::path::Path, device: &str) -> bool {
    println!(
        "        device: --device={} needs Linux; ALSA does not exist here. \
         Use --drain for the software path.",
        device
    );
    // Not a success. Asking a Mac to play through ALSA and getting exit 0
    // would say the run happened.
    false
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

/// Reports the medium's state, and browses it if it is there.
///
/// The bring-up check phase A wants: a stick in a USB port and nothing else
/// on the Pi. It prints the mount-point test in full rather than just the
/// verdict, because "the directory exists but nothing is mounted" is the case
/// an existence test gets wrong and this is where it would be seen.
fn media_check(path: &std::path::Path) -> i32 {
    println!("mount point: {}", path.display());
    match media::is_mount_point(path) {
        Ok(true) => println!("  mount test: something is mounted here"),
        Ok(false) => println!(
            "  mount test: nothing mounted{}",
            if path.exists() {
                " — but the directory exists, which an existence test would call a stick"
            } else {
                ""
            }
        ),
        Err(e) => println!("  mount test: {}", e),
    }

    let state = media::examine(path);
    println!("  state: {}", state);

    match &state {
        Medium::Absent => 1,
        Medium::Unreadable { .. } => 1,
        Medium::Browsable { .. } => {
            let mut b = match Browser::open(path) {
                Ok(b) => b,
                Err(e) => {
                    println!("  browser: {}", e);
                    return 1;
                }
            };
            let rows = b.view(64);
            println!("  root folder: {} entries", rows.len());
            for row in &rows {
                match row {
                    Row::Folder { name, .. } => println!("    [dir]  {}", name),
                    Row::File { name, verdict, .. } => match verdict {
                        Verdict::Plays(i) => println!(
                            "    PLAYS  {}  {} {} Hz {}",
                            name, i.container, i.rate, i.depth
                        ),
                        Verdict::Refused(r) => println!("    refuse {}  {}", name, r),
                        Verdict::Unreadable(m) => println!("    ??     {}  {}", name, m),
                    },
                }
            }
            println!("  headers read: {}", b.headers_read());
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_of(args: &[&str]) -> Result<Args, String> {
        parse(args.iter().map(std::ffi::OsString::from))
    }

    #[test]
    fn a_mistyped_flag_is_an_error_and_not_a_filename() {
        // The whole reason this parser was rewritten. `--drian short.wav`
        // used to print `UNREAD --drian`, then play the file **without**
        // draining, and exit 0 — the typo became a path, the mode silently
        // did not happen, and the exit code said the run was fine.
        let e = parse_of(&["--drian", "short.wav"]).expect_err("must be refused");
        assert!(e.contains("--drian"), "the message must name it: {e}");
    }

    #[test]
    fn a_value_flag_written_with_a_space_says_so_rather_than_eating_the_value() {
        // `--device hw:0,0 f` used to treat `hw:0,0` as a file to open, so
        // the run reported `UNREAD hw:0,0` and then played `f` with no
        // device at all.
        let e = parse_of(&["--device", "hw:0,0", "f"]).expect_err("must be refused");
        assert!(e.contains('='), "the message must say how to write it: {e}");
    }

    #[test]
    fn the_two_playback_modes_are_not_silently_ranked() {
        // `--drain --device=...` used to let the device win and drop
        // `--drain` without a word, so a run that asked for the software
        // path could quietly become a hardware one.
        let e = parse_of(&["--drain", "--device=hw:0,0", "f"]).expect_err("must be refused");
        assert!(e.contains("pick one"), "{e}");
    }

    #[test]
    fn a_check_mode_refuses_to_ignore_the_rest_of_the_line() {
        // `--rt-check` used to win from any position and discard everything
        // else, so `deck-pi track.wav --rt-check` looked like it played the
        // track and did not.
        assert!(parse_of(&["track.wav", "--rt-check"]).is_err());
        assert!(parse_of(&["--rt-check", "--drain"]).is_err());
        assert!(parse_of(&["--rt-check", "--media-check"]).is_err());
        // Alone, it is fine.
        assert_eq!(
            parse_of(&["--rt-check"]).expect("alone"),
            Args { mode: Mode::RtCheck(None), paths: vec![] }
        );
    }

    #[test]
    fn a_core_number_that_is_not_a_number_is_refused_rather_than_dropped() {
        // `--rt-check=abc` used to parse to `None` and run unpinned, so a
        // typo in the one argument that exercises core pinning meant the
        // pinning silently did not happen — and pinning is the part of the
        // realtime setup with nothing but the affinity mask to read it back.
        let e = parse_of(&["--rt-check=abc"]).expect_err("must be refused");
        assert!(e.contains("core number"), "{e}");
        assert_eq!(
            parse_of(&["--rt-check=2"]).expect("a number is fine"),
            Args { mode: Mode::RtCheck(Some(2)), paths: vec![] }
        );
    }

    #[test]
    fn the_ordinary_forms_still_work_and_a_dash_file_is_reachable() {
        assert_eq!(
            parse_of(&["a.wav", "b.wav"]).expect("report"),
            Args {
                mode: Mode::Report,
                paths: vec![PathBuf::from("a.wav"), PathBuf::from("b.wav")]
            }
        );
        assert_eq!(
            parse_of(&["--drain", "a.wav"]).expect("drain").mode,
            Mode::Drain
        );
        assert_eq!(
            parse_of(&["--device=hw:1,0", "a.wav"]).expect("device").mode,
            Mode::Device("hw:1,0".into())
        );
        // A file whose name begins with a dash is reachable, which is what
        // makes "everything starting with - is a flag" safe to enforce.
        assert_eq!(
            parse_of(&["--", "--odd-name.wav"]).expect("after --").paths,
            vec![PathBuf::from("--odd-name.wav")]
        );
    }

    #[test]
    fn no_file_is_an_error_rather_than_a_silent_success() {
        // `deck-pi --drain` alone used to print nothing and exit 0.
        assert!(parse_of(&["--drain"]).is_err());
        assert!(parse_of(&["--device=hw:0,0"]).is_err());
        assert!(parse_of(&[]).is_err());
    }
}
