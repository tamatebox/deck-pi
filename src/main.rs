//! Placeholder entry point. Prints what the file layer makes of each path
//! given to it, which is the same judgement the browser will show on the
//! highlighted row.

use std::path::PathBuf;

use deck_pi::engine::{Engine, Outcome};
use deck_pi::file::{OpenError, Track, RING_CHANNELS};
use deck_pi::ring::{self, Miss};
use deck_pi::transport::Transport;
use deck_pi::window::Window;

fn main() {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if args.is_empty() {
        eprintln!("usage: deck-pi [--drain] <file>...");
        eprintln!("  default   reports what the file layer makes of each path");
        eprintln!("  --drain   also pulls every frame through the window thread");
        eprintln!("            and the ring, as the audio callback would");
        std::process::exit(2);
    }

    let drain = args.iter().any(|a| a == std::ffi::OsStr::new("--drain"));
    let args: Vec<PathBuf> = args
        .into_iter()
        .filter(|a| a != std::ffi::OsStr::new("--drain"))
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
                if drain {
                    drain_through_the_ring(&path);
                }
            }
            Err(OpenError::Rejected(why)) => println!("REFUSED {}  {}", path.display(), why),
            Err(OpenError::Unreadable(e)) => println!("UNREAD  {}  {}", path.display(), e),
        }
    }
}

/// Plays the whole track through the real path — window thread, ring,
/// transport and the callback body — and reports what came out. Everything in
/// v1 short of handing the buffer to ALSA.
///
/// This is the bring-up tool: run it against a real stick to confirm the read
/// path, the ring, the window thread and the callback hold on actual material
/// and actual media, before any of it is wired to a sound card.
fn drain_through_the_ring(path: &std::path::Path) {
    const PERIOD: usize = 256;

    let (window, reader, info) = match Window::load(path, ring::WINDOW_BYTES_PLACEHOLDER) {
        Ok(w) => w,
        Err(e) => {
            println!("        drain: could not load — {}", e);
            return;
        }
    };

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
    let mut period = vec![0i32; PERIOD * RING_CHANNELS];
    let mut played = 0u64;
    let mut misses = 0u64;
    let mut peak: i32 = 0;

    loop {
        match engine.fill(&transport, &reader, &mut period) {
            Outcome::Played { frames } | Outcome::PlayedTail { frames } => {
                for &s in &period[..frames * RING_CHANNELS] {
                    peak = peak.max(s.abs());
                }
                played += frames as u64;
            }
            Outcome::EndOfTrack => break,
            Outcome::Missed(Miss::NotResident) => {
                misses += 1;
                if failure.lock().unwrap().is_some() {
                    break;
                }
                std::thread::yield_now();
            }
            other => {
                println!("        drain: {:?} at frame {}", other, engine.position());
                break;
            }
        }
    }

    let _ = tx.send(deck_pi::window::Command::Shutdown);
    let _ = thread.join();

    let elapsed = started.elapsed();
    let audio_secs = played as f64 / info.rate as f64;
    let failed = failure.lock().unwrap().clone();
    match failed.as_deref() {
        Some(msg) => println!("        drain: FAILED after {} frames — {}", played, msg),
        None => println!(
            "        drain: {}/{} frames in {:.3} s ({:.0}x realtime), {} waits, peak {:#x}",
            played,
            info.frames,
            elapsed.as_secs_f64(),
            audio_secs / elapsed.as_secs_f64().max(1e-9),
            misses,
            peak
        ),
    }
}
