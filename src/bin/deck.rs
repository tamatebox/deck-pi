//! The deck. One process: engine, browser, and the loop that joins them.
//!
//! `src/main.rs` is the bring-up CLI — it reports what the file layer makes of
//! a path and can pull one file through the machinery. This is the other
//! binary `src/app/mod.rs` describes, and it is thin on purpose: everything it
//! does is a call into the library, because the library is where the things
//! that can be got wrong are testable.
//!
//! ```sh
//! deck                                   # the deck
//! deck --device=hw:CARD=sndrpihifiberry,DEV=0
//! deck --no-rt                           # a desk, where SCHED_FIFO is refused
//! ```
//!
//! # What this file is responsible for, and it is a short list
//!
//! Assembling five things in an order that matters, installing a signal
//! handler, and printing what the loop reports. No decision about the deck's
//! behaviour is taken here; every one of them is in a module with tests.
//!
//! **`rt::apply` is deliberately not called.** It belongs to the audio thread
//! and is passed down in [`Config::rt`] so that `app::audio::run` can make it
//! its first act on that thread. Calling it here would promote `main`, and
//! glibc's `pthread_create` defaults to `PTHREAD_INHERIT_SCHED` — so the
//! window thread, the control loop and everything else spawned afterwards
//! would inherit `SCHED_FIFO` too. `app::controls::run` asserts it did not.
//!
//! **The control surface is opened before [`Controls`] is built**, because
//! `Controls::new` reads the absolute-axis range out of it. Building the
//! decoder first and attaching devices later would leave `fold_abs` without
//! the span it needs to tell a wrap from a jump, and one wrap of the browse
//! encoder would scroll the length of the axis.
//!
//! **Nothing here fails because a stick or a control surface is missing.** A
//! deck is switched on with no stick in it and finds one later; a surface can
//! be unplugged and plugged back in. Both are ordinary, both are handled in
//! the loop, and a binary that refused to start without them would be a deck
//! that cannot be switched on before the operator is ready.

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "the deck needs Linux: it reads /dev/input and opens an ALSA hw: device.\n\
         On this machine, `deck-pi` is the bring-up CLI and the tests run."
    );
    std::process::ExitCode::FAILURE
}

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(target_os = "linux")]
mod linux {
    use std::process::ExitCode;
    use std::sync::atomic::{AtomicBool, Ordering};

    use deck_pi::app::controls::{self, Controls, Turn};
    use deck_pi::app::deck::Deck;
    use deck_pi::app::medium::{Change, Mount};
    use deck_pi::app::panel::{Panel, Show};
    use deck_pi::app::track::Config;
    use deck_pi::cue;
    use deck_pi::input::Devices;
    use deck_pi::display::text::{Text, CONSOLE};
    use deck_pi::display::Geometry;
    use deck_pi::media::{self, MediaWatch};
    use deck_pi::ring;
    use deck_pi::sink::alsa::AlsaSink;

    /// **Not `hw:0,0`, and that is not a style choice.** On the deck as it
    /// stands, card 0 is `bcm2835 Headphones` — the Pi's own analogue jack —
    /// and the Digi2 Pro is card 1. A number is a probe order, so it moves
    /// when a HAT is added or the HDMI audio device appears; the name does
    /// not. `hw:0,0` would open the wrong output and play, which is the worst
    /// shape of wrong.
    const DEVICE: &str = "hw:CARD=sndrpihifiberry,DEV=0";

    /// `architecture.md` targets 5-10 ms of output latency, which at 44.1 kHz
    /// is 128 frames and not 256. `src/sink/alsa.rs`'s own test is where that
    /// is argued.
    const PERIOD: usize = 128;
    const PERIODS: u32 = 2;

    static STOP: AtomicBool = AtomicBool::new(false);

    /// Async-signal-safe by construction: one relaxed store to a static, and
    /// nothing else. The loop reads it once a turn.
    extern "C" fn on_signal(_: libc::c_int) {
        STOP.store(true, Ordering::Relaxed);
    }

    fn install_handlers() {
        // SIGTERM as well as SIGINT: [#8](https://github.com/tamatebox/deck-pi/issues/8)
        // is how the deck starts at boot, and whatever answers it will stop
        // the deck with SIGTERM. A deck that only handled ^C would be killed
        // outright there, with the ALSA device and the threads never let go.
        for sig in [libc::SIGINT, libc::SIGTERM] {
            // SAFETY: installing a handler that only stores to a static.
            unsafe { libc::signal(sig, on_signal as *const () as libc::sighandler_t) };
        }
    }

    struct Args {
        device: String,
        mount: std::path::PathBuf,
        window_bytes: usize,
        rt: bool,
        grid: Geometry,
    }

    fn parse(argv: impl Iterator<Item = String>) -> Result<Option<Args>, String> {
        let mut a = Args {
            device: DEVICE.to_string(),
            mount: std::path::PathBuf::from(media::MOUNT_POINT),
            window_bytes: ring::WINDOW_BYTES_PLACEHOLDER,
            rt: true,
            grid: CONSOLE,
        };
        for arg in argv {
            match arg.as_str() {
                "-h" | "--help" => return Ok(None),
                "--no-rt" => a.rt = false,
                _ if arg.starts_with("--device=") => {
                    a.device = arg["--device=".len()..].to_string()
                }
                _ if arg.starts_with("--mount=") => {
                    a.mount = std::path::PathBuf::from(&arg["--mount=".len()..])
                }
                _ if arg.starts_with("--window-mib=") => {
                    let n = &arg["--window-mib=".len()..];
                    let mib: usize = n
                        .parse()
                        .map_err(|_| format!("--window-mib= wants a number, got {n:?}"))?;
                    a.window_bytes = mib * 1024 * 1024;
                }
                _ if arg.starts_with("--grid=") => {
                    let spec = &arg["--grid=".len()..];
                    let (c, r) = spec
                        .split_once('x')
                        .ok_or_else(|| format!("--grid= wants COLSxROWS, got {spec:?}"))?;
                    let cols = c.parse().map_err(|_| format!("bad columns in {spec:?}"))?;
                    let rows = r.parse().map_err(|_| format!("bad rows in {spec:?}"))?;
                    // Mono, because the console's stand-in for the panel is
                    // the mono one: the marks are what a colour panel would
                    // say with a pen, and seeing them is the point.
                    a.grid = Geometry { cols, rows, colour: false };
                }
                _ => return Err(format!("unknown argument {arg:?}")),
            }
        }
        Ok(Some(a))
    }

    fn usage() {
        eprintln!("usage: deck [--device=hw:...] [--mount=PATH] [--window-mib=N] [--no-rt]");
        eprintln!();
        eprintln!("  --device=     ALSA device, hw: only. Default {DEVICE}");
        eprintln!("  --mount=      where the stick appears. Default {}", media::MOUNT_POINT);
        eprintln!(
            "  --window-mib= the resident window, still issue #9. Default {} MiB",
            ring::WINDOW_BYTES_PLACEHOLDER / (1024 * 1024)
        );
        eprintln!("  --no-rt       do not ask for SCHED_FIFO. For a desk, not for the deck");
        eprintln!(
            "  --grid=CxR    the console display's grid. Default {}x{}, and a panel's own \n\
             \t\tnumbers preview it — 21x5 is the 128x64 at 12 px",
            CONSOLE.cols, CONSOLE.rows
        );
    }

    /// Everything the loop did that is worth a line. Called once a turn, so it
    /// says nothing on the overwhelming majority of them.
    ///
    /// **Through the display rather than `println!`.** The console draws the
    /// listing by walking the cursor back over the block it last wrote, and a
    /// `println!` landing in the middle of that leaves the arithmetic wrong
    /// for the rest of the run — `Show::note` is the door that exists so this
    /// cannot happen.
    fn report_turn<D: Show>(turn: &Turn, change: Option<&Change>, show: &mut D) {
        let mut say = |line: String| {
            let _ = show.note(&line);
        };
        if let Some(c) = change {
            say(format!("medium: {}", c.medium));
            if let Some(e) = &c.unbrowsable {
                say(format!("  cannot browse it: {e}"));
            }
            if let Some(e) = &c.cueless {
                say(format!("  cues will not load: {e} — the deck plays, cues do not persist"));
            }
            if c.ended.is_some() {
                say("  the track it was playing has been unloaded".to_string());
            }
        }
        if let Some(ended) = &turn.ended {
            say(format!("track ended: {ended:?}"));
        }
        if turn.lost > 0 {
            say(format!("control surface: {} node(s) went away", turn.lost));
        }
        if turn.reopened > 0 {
            say(format!("control surface: {} node(s) came back", turn.reopened));
        }
        for e in &turn.errors {
            say(format!("refused: {e}"));
        }
        if let Some(e) = &turn.draw_error {
            // Not through `note`: the display is the thing that just refused.
            eprintln!("display: {e}");
        }
    }

    pub fn main() -> ExitCode {
        let args = match parse(std::env::args().skip(1)) {
            Ok(Some(a)) => a,
            Ok(None) => {
                usage();
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("{e}");
                usage();
                return ExitCode::FAILURE;
            }
        };

        // Opened first: `Controls::new` reads the axis range out of it, and a
        // surface that is not there yet is ordinary rather than fatal —
        // `open_discovered` is the constructor that says so.
        let mut devices = match Devices::open_discovered() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("control surface: {e}");
                return ExitCode::FAILURE;
            }
        };
        let carries = devices.carries();
        if devices.is_empty() {
            println!("control surface: none yet — the loop will keep looking");
        } else {
            println!(
                "control surface: {} node(s), buttons {}, detents {}",
                devices.len(),
                if carries.buttons { "yes" } else { "no" },
                if carries.detents { "yes" } else { "no" }
            );
        }
        let mut controls = Controls::new(&devices);

        let state_dir = cue::default_state_dir();
        if state_dir.is_none() {
            println!("cues: no state directory — the deck plays, cues do not persist");
        }
        let mut mount = Mount::new(MediaWatch::new(&args.mount), state_dir);

        let device = args.device.clone();
        let mut deck: Deck<AlsaSink> = Deck::new(
            Box::new(move |info| AlsaSink::open(&device, info.rate, PERIOD, PERIODS)),
            // Both arrive with the medium, or not at all.
            None,
            None,
            Config {
                window_bytes: args.window_bytes,
                rt: args.rt.then(Default::default),
            },
        );

        install_handlers();
        println!(
            "deck: {}, window {} MiB, {}",
            args.device,
            args.window_bytes / (1024 * 1024),
            if args.rt { "SCHED_FIFO requested per track" } else { "no realtime" }
        );
        println!(
            "watching {}, showing {}x{} on the console",
            args.mount.display(),
            args.grid.cols,
            args.grid.rows
        );

        // ANSI only when stdout is a terminal: piped to a file or a log, the
        // escape codes would be the output rather than decorate it.
        // SAFETY: `isatty` reads a descriptor and returns a flag.
        let tty = unsafe { libc::isatty(1) } == 1;
        let mut panel = Panel::new(Text::new(std::io::stdout(), args.grid).with_ansi(tty));

        let outcome = controls::run(
            &mut controls,
            &mut devices,
            &mut mount,
            &mut panel,
            &mut deck,
            &STOP,
            report_turn,
        );

        // Let go of the device and the threads before saying anything about
        // the run: `unload` is what stops them, and a report printed first
        // would be printed while the deck was still playing.
        deck.detach();

        match outcome {
            Ok(r) => {
                println!(
                    "\n{} turns, {} actions, {} media changes, {} ends, {} refused, \
                     {} device losses, {} decoder resets",
                    r.turns,
                    r.actions,
                    r.media,
                    r.ends,
                    r.errors,
                    r.lost,
                    controls.resets()
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("the control loop stopped: {e}");
                ExitCode::FAILURE
            }
        }
    }
}
