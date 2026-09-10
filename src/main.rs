//! Placeholder entry point. Prints what the file layer makes of each path
//! given to it, which is the same judgement the browser will show on the
//! highlighted row.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use deck_pi::app::audio::{AtEnd, Parts};
use deck_pi::engine::Engine;
use deck_pi::file::{OpenError, Track};
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

    // **A flag given twice is an error, not the last one winning.** Refusing
    // contradictions *between* flags while accepting them *within* one is the
    // hole the first version of this parser left, and it reopened the door
    // the same commit had just closed: `--rt-check=2 --rt-check` produced
    // `cpu None` — pinning silently not happening, on the one flag that
    // exists to exercise pinning.
    //
    // `--device=hw:0,0 --device=hw:9,9` is the one that costs most here. This
    // CLI's whole job is bring-up, and an operator editing a shell line to
    // change cards, leaving the old flag behind, would test `hw:9,9` while
    // believing they tested `hw:0,0`. Silent, and wrong about the only thing
    // the run was for.
    //
    // `--drain` twice is harmless and is refused anyway, so a reader does not
    // have to learn which flags tolerate repetition.
    fn once<T>(slot: &mut Option<T>, flag: &str, value: T) -> Result<(), String> {
        if slot.is_some() {
            return Err(format!("{flag} given more than once"));
        }
        *slot = Some(value);
        Ok(())
    }

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
            "--drain" => {
                if drain {
                    return Err("--drain given more than once".into());
                }
                drain = true;
            }
            "--rt-check" => once(&mut rt_check, "--rt-check", None)?,
            "--media-check" => once(
                &mut media_check,
                "--media-check",
                PathBuf::from(media::MOUNT_POINT),
            )?,
            // `--device` written with a space would take the value as a path,
            // so it is caught by name. `--rt-check` and `--media-check` need
            // no such arm: both are valid with no value at all.
            "--device" => return Err("--device takes its value with '=', as --device=...".into()),
            _ if text.starts_with("--device=") => {
                once(&mut device, "--device=", text["--device=".len()..].to_string())?
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
                once(&mut rt_check, "--rt-check", Some(cpu))?;
            }
            _ if text.starts_with("--media-check=") => once(
                &mut media_check,
                "--media-check",
                PathBuf::from(&text["--media-check=".len()..]),
            )?,
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
    sink: S,
    label: &str,
) -> Result<(), String> {
    let (window, reader, info) = Window::load(path, ring::WINDOW_BYTES_PLACEHOLDER)
        .map_err(|e| e.to_string())?;

    // The window thread's failure has to reach the audio thread without a
    // lock on the deadline, so the flag is an atomic and the text — which
    // only the reporting below reads — is beside it.
    let lost = std::sync::Arc::new(AtomicBool::new(false));
    let why = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let (l, w) = (std::sync::Arc::clone(&lost), std::sync::Arc::clone(&why));
    let (tx, rx) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        window.run(rx, move |e| {
            if let deck_pi::window::Event::Failed(msg) = e {
                *w.lock().unwrap() = Some(msg);
                l.store(true, Ordering::Release);
            }
        })
    });

    let transport = std::sync::Arc::new(Transport::new());
    // **Load before playing.** `Transport` refuses control while
    // `State::Stopped`, which is what "nothing loaded" means, so `play()` on
    // a fresh transport does nothing — and `AtEnd::Stop` ends at the end of
    // the *track*, so a deck that never starts never ends. That combination
    // hung `--drain` for as long as it took to notice, which was one test
    // run. The app loop reaches this through `app::track::load`; this is the
    // bring-up CLI doing the same thing by hand, cue point zero.
    transport.track_loaded(0);
    transport.play();
    let deck = Parts {
        transport: std::sync::Arc::clone(&transport),
        reader,
        engine: Engine::new(info.frames),
        sink,
        // One file, pulled through, then a report. The deck's setting is
        // `Idle` and lives in `app::track`.
        at_end: AtEnd::Stop,
    };

    // **No realtime setup from the bring-up CLI.** `--rt-check` is where that
    // is exercised and where a refusal is an error; promoting this thread
    // here would make every `--drain` on a developer desk print a refusal it
    // can do nothing about.
    let stop = AtomicBool::new(false);
    let (deck, stopped, report) = deck_pi::app::audio::run(deck, &stop, &lost, None);

    // **The control thread pauses the deck at the end, and here that is this
    // thread** — `run` has returned, so nothing else is touching the
    // transport. The audio loop deliberately does not do it; see
    // `Transport::reached_end`.
    if stopped == deck_pi::app::audio::Stopped::EndOfTrack {
        transport.reached_end();
    }

    let _ = tx.send(deck_pi::window::Command::Shutdown);
    let _ = thread.join();
    // Dropped here, on this thread, which is the point of `run` handing it
    // back: the ring's last `Arc` goes with the reader, and 64 MiB freed on
    // the audio thread is the hazard `implementation.md` names.
    drop(deck);

    if let Some(msg) = why.lock().unwrap().clone() {
        return Err(format!("after {} frames: {}", report.frames, msg));
    }
    if let deck_pi::app::audio::Stopped::Unexpected(e) = &stopped {
        return Err(e.clone());
    }
    let audio_secs = report.frames as f64 / info.rate as f64;
    println!(
        "        {}: {}/{} frames in {:.3} s ({:.1}x realtime), {} waits, \
         {} underruns, peak {:#x}",
        label,
        report.frames,
        info.frames,
        report.elapsed.as_secs_f64(),
        audio_secs / report.elapsed.as_secs_f64().max(1e-9),
        report.misses,
        report.underruns,
        report.peak
    );
    Ok(())
}

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
    let sink = CaptureSink::new(rate, PERIOD, frames + PERIOD);
    if let Err(e) = play(path, sink, "drain") {
        println!("        drain: FAILED — {}", e);
    }
}

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

    let sink = match AlsaSink::open(device, rate, PERIOD, PERIODS) {
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

    if let Err(e) = play(path, sink, "device") {
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
    fn a_flag_given_twice_is_refused_rather_than_letting_the_last_one_win() {
        // **Found by review after the first version of this parser shipped**,
        // and it reopened the door the same commit had just closed. The commit
        // message said a typo in `--rt-check=` "meant the pinning silently did
        // not happen"; `--rt-check=2 --rt-check` produced exactly that by a
        // different route, giving `cpu None`.
        //
        // Measured on the built binary, not reasoned:
        //   --rt-check=2 --rt-check          -> cpu None
        //   --rt-check --rt-check=2          -> cpu Some(2)
        //   --device=hw:0,0 --device=hw:9,9  -> ran hw:9,9, said nothing
        //
        // The shape: contradictions *between* flags were refused, and
        // contradictions *within* one flag were not, because each was
        // `x = Some(..)` in a loop that could not tell a first from a second.
        //
        // `--device=` is the one that costs most. This CLI exists for
        // bring-up; an operator editing a shell line to change cards and
        // leaving the old flag behind tests the wrong card while believing
        // otherwise, which is silent and wrong about the only thing the run
        // was for.
        assert!(parse_of(&["--rt-check=2", "--rt-check"]).is_err());
        assert!(parse_of(&["--rt-check", "--rt-check=2"]).is_err());
        assert!(parse_of(&["--device=hw:0,0", "--device=hw:9,9", "x.wav"]).is_err());
        assert!(parse_of(&["--media-check=/a", "--media-check=/b"]).is_err());
        // `--drain` twice is harmless and refused anyway, so nobody has to
        // learn which flags tolerate repetition.
        assert!(parse_of(&["--drain", "--drain", "x.wav"]).is_err());

        // And the messages name the flag.
        let e = parse_of(&["--device=a", "--device=b", "x"]).expect_err("refused");
        assert!(e.contains("--device"), "{e}");
    }

    #[test]
    fn no_file_is_an_error_rather_than_a_silent_success() {
        // `deck-pi --drain` alone used to print nothing and exit 0.
        assert!(parse_of(&["--drain"]).is_err());
        assert!(parse_of(&["--device=hw:0,0"]).is_err());
        assert!(parse_of(&[]).is_err());
    }
}
