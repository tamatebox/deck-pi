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

    use std::fs::File;
    use std::path::{Path, PathBuf};

    use deck_pi::app::controls::{self, Controls, Turn};
    use deck_pi::app::deck::Deck;
    use deck_pi::app::medium::{Change, Mount};
    use deck_pi::app::panel::{Panel, Show};
    use deck_pi::app::track::Config;
    use deck_pi::cue;
    use deck_pi::input::Devices;
    use deck_pi::display::packed::Packed;
    use deck_pi::display::text::{Text, CONSOLE};
    use deck_pi::display::wire::{self, Declared, Message, Reader};
    use deck_pi::display::{Geometry, Lighting, Screen};
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

    /// What udev names the Pico's serial interface under `/dev/serial/by-id`:
    /// the manufacturer, product and serial number `firmware/src/main.rs`
    /// declares, then the interface number. A prefix, so that number is the
    /// firmware's business.
    const LINK_PREFIX: &str = "usb-deck-pi_deck-pi_control_surface_deck-pico-1-if";

    /// How long the Pico has to answer `Hello`. It brings its panel up before
    /// it listens — about 0.4 s from power, and again on every `Hello`.
    const HANDSHAKE: std::time::Duration = std::time::Duration::from_secs(2);

    /// How often a panel that has gone is looked for. The look is a directory
    /// read; the handshake that follows a find holds the loop while the Pico
    /// brings its panel up, so it runs only when there is a link to answer.
    const LOOK_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

    static STOP: AtomicBool = AtomicBool::new(false);

    /// Where the deck draws: the console, or the panel across the Pico.
    ///
    /// **Chosen once, at start.** A panel that goes away mid-run is looked for
    /// until it comes back ([`Display::listen`]); a deck that started on the
    /// console stays there, which `boot.sh` waiting for the Pico makes rare.
    enum Display {
        Console(Text<std::io::Stdout>),
        Panel {
            packed: Packed<File>,
            /// The same port, for reading what the Pico says back.
            link: File,
            /// Frames sent, for the report at the end.
            frames: u64,
            /// Set when the link fails or hangs up, and cleared when the Pico
            /// answers again — [`Display::listen`].
            gone: bool,
            /// What the deck last asked the glass for, said again to a Pico
            /// that comes back: the deck draws only when something changes.
            last: Option<Screen>,
            lighting: Lighting,
            /// When to look for the Pico next, while it is gone.
            look_at: std::time::Instant,
        },
    }

    /// The Pico has gone: said once, and looked for from the next turn.
    fn lose(gone: &mut bool, look_at: &mut std::time::Instant, e: &std::io::Error) {
        if !*gone {
            println!("panel: the link failed: {e} — looking for the Pico again");
        }
        *gone = true;
        *look_at = std::time::Instant::now();
    }

    /// Whether the kernel has hung the port up — the Pico unplugged, or reset.
    ///
    /// **Asked, because a read cannot tell.** A hung-up tty reads as end of
    /// file, which with `VMIN = VTIME = 0` is also what "nothing to read"
    /// looks like, so `take` alone would see a quiet Pico. A paused deck
    /// writes nothing either, so without this the loss would wait for the
    /// next button press to be noticed.
    fn hung_up(link: &File) -> bool {
        use std::os::fd::AsRawFd;
        let mut p = libc::pollfd { fd: link.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one pollfd, owned here, and no wait.
        let ready = unsafe { libc::poll(&mut p, 1, 0) };
        ready > 0 && p.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
    }

    impl Display {
        /// Whatever the Pico has said since the last turn.
        ///
        /// **Every turn, not before each frame.** `Packed` times the round
        /// trip when it takes the ack, so reading only before the next frame
        /// would time the gap between frames instead — seconds, on a deck
        /// left alone. Every turn makes it good to the loop's 10 ms.
        ///
        /// **And, while the Pico is gone, where it went.** A knocked USB lead
        /// in a venue is ordinary, and before this a deck that lost its panel
        /// kept drawing into a dead port for the rest of the run: the controls
        /// came back on their own and the glass stayed frozen. A Pico that
        /// answers again is handed the last screen and lighting at once.
        fn listen(&mut self) {
            let Display::Panel { packed, link, gone, last, lighting, look_at, .. } = self else {
                return;
            };
            if !*gone {
                if hung_up(link) {
                    let e = std::io::Error::from(std::io::ErrorKind::BrokenPipe);
                    lose(gone, look_at, &e);
                    return;
                }
                match packed.take(link) {
                    Ok(said) => {
                        for m in said {
                            if let Message::Fault { code, reason } = m {
                                println!("panel: fault {code}: {reason}");
                            }
                        }
                    }
                    Err(e) => lose(gone, look_at, &e),
                }
                return;
            }

            let now = std::time::Instant::now();
            if now < *look_at {
                return;
            }
            *look_at = now + LOOK_EVERY;
            let Some(path) = find_link() else { return };
            if let Ok((d, out, new_link)) = connect(&path) {
                if d.has_panel() {
                    *packed = Packed::new(out, d);
                    *link = new_link;
                    *gone = false;
                    println!("panel: back, on {}", path.display());
                    if let Err(e) = packed.lighting(*lighting) {
                        lose(gone, look_at, &e);
                    } else if let Some(s) = last.as_ref() {
                        if let Err(e) = packed.show(s) {
                            lose(gone, look_at, &e);
                        }
                    }
                }
            }
        }
    }

    impl Show for Display {
        fn geometry(&self) -> Geometry {
            match self {
                Display::Console(t) => t.geometry(),
                Display::Panel { packed, .. } => packed.geometry(),
            }
        }

        /// To a gone panel, kept and not written: an error here would be
        /// retried and reported every turn, and `listen` says it once.
        fn show(&mut self, screen: &Screen) -> std::io::Result<()> {
            match self {
                Display::Console(t) => t.show(screen),
                Display::Panel { packed, frames, gone, last, look_at, .. } => {
                    *last = Some(screen.clone());
                    if !*gone {
                        *frames += 1;
                        if let Err(e) = packed.show(screen) {
                            lose(gone, look_at, &e);
                        }
                    }
                    Ok(())
                }
            }
        }

        fn lighting(&mut self, want: Lighting) -> std::io::Result<()> {
            match self {
                Display::Console(t) => t.lighting(want),
                Display::Panel { packed, gone, lighting, look_at, .. } => {
                    *lighting = want;
                    if !*gone {
                        if let Err(e) = packed.lighting(want) {
                            lose(gone, look_at, &e);
                        }
                    }
                    Ok(())
                }
            }
        }

        /// To the terminal either way: a 256x64 panel has no line to spare,
        /// and the person who started the deck is reading stdout.
        fn note(&mut self, line: &str) -> std::io::Result<()> {
            match self {
                Display::Console(t) => Show::note(t, line),
                Display::Panel { .. } => {
                    println!("{line}");
                    Ok(())
                }
            }
        }
    }

    fn find_link() -> Option<PathBuf> {
        std::fs::read_dir("/dev/serial/by-id")
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(LINK_PREFIX))
            })
    }

    /// Opens the Pico's serial port as a byte pipe.
    ///
    /// **Three settings, each of which fails silently when missing** —
    /// `cdj-200.md` found the first two the hard way with `cat`:
    ///
    /// - `CLOCAL`, or a blocking open waits for a carrier a USB port never
    ///   raises. Opened `O_NONBLOCK` so the open itself cannot hang first.
    /// - DTR, which the firmware's `wait_connection` waits on before it reads
    ///   a byte. Raised explicitly rather than trusted to the open.
    /// - Raw mode with `VMIN = VTIME = 0`, so a read with nothing waiting
    ///   returns 0 at once — which is what `wire::Reader::fill_from` takes as
    ///   "nothing to read" — and no byte is translated on its way through.
    fn open_link(path: &Path) -> std::io::Result<File> {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;

        let c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: a NUL-terminated path and flags; the fd is owned below.
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `fd` was just opened and nothing else owns it.
        let file = unsafe { File::from_raw_fd(fd) };
        let fd = file.as_raw_fd();
        let check = |r: libc::c_int| if r < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) };

        // SAFETY: `termios` is plain data, filled by `tcgetattr` before use.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        check(unsafe { libc::tcgetattr(fd, &mut t) })?;
        // SAFETY: `t` is a valid termios.
        unsafe { libc::cfmakeraw(&mut t) };
        t.c_cflag |= libc::CLOCAL | libc::CREAD;
        t.c_cc[libc::VMIN] = 0;
        t.c_cc[libc::VTIME] = 0;
        check(unsafe { libc::tcsetattr(fd, libc::TCSANOW, &t) })?;

        let dtr: libc::c_int = libc::TIOCM_DTR;
        // SAFETY: TIOCMBIS reads one int through the pointer.
        check(unsafe { libc::ioctl(fd, libc::TIOCMBIS, &dtr) })?;

        // Blocking writes from here: a frame is written whole or fails.
        // SAFETY: flag reads and writes on an fd this function owns.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        check(flags)?;
        check(unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) })?;
        // Anything the Pico said before this run asked is not an answer to it.
        // SAFETY: discards the port's queues.
        check(unsafe { libc::tcflush(fd, libc::TCIOFLUSH) })?;
        Ok(file)
    }

    /// Opens the link and asks the Pico what it has.
    fn connect(path: &Path) -> std::io::Result<(Declared, File, File)> {
        let mut link = open_link(path)?;
        let mut out = link.try_clone()?;
        let declared = wire::handshake(&mut out, &mut link, &mut Reader::new(), HANDSHAKE)?;
        Ok((declared, out, link))
    }

    /// The panel if the Pico has one and answers, the console otherwise —
    /// and a line either way saying which, and why.
    fn choose_display(grid: Geometry) -> Display {
        // ANSI only when stdout is a terminal: piped to a file or a log, the
        // escape codes would be the output rather than decorate it.
        // SAFETY: `isatty` reads a descriptor and returns a flag.
        let tty = unsafe { libc::isatty(1) } == 1;
        let console = || Display::Console(Text::new(std::io::stdout(), grid).with_ansi(tty));

        let Some(path) = find_link() else {
            println!("panel: no Pico serial link — showing {}x{} on the console", grid.cols, grid.rows);
            return console();
        };
        match connect(&path) {
            Ok((d, out, link)) if d.has_panel() => {
                println!(
                    "panel: {}x{}, {} brightness levels, on {}",
                    d.px_w,
                    d.px_h,
                    d.brightness_levels,
                    path.display()
                );
                Display::Panel {
                    packed: Packed::new(out, d),
                    link,
                    frames: 0,
                    gone: false,
                    last: None,
                    lighting: Lighting::Full,
                    look_at: std::time::Instant::now(),
                }
            }
            Ok(_) => {
                println!("panel: the Pico declares none — showing the console");
                console()
            }
            Err(e) => {
                println!("panel: {}: {e} — showing the console", path.display());
                console()
            }
        }
    }

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
    fn report_turn(turn: &Turn, change: Option<&Change>, show: &mut Display) {
        // Here because this is the one thing the loop calls every turn with
        // the display in hand; `Display::listen` says why every turn.
        show.listen();
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
            if let Some(e) = &c.first {
                say(format!("  its first track would not load: {e}"));
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
        println!("watching {}", args.mount.display());

        let mut panel = Panel::new(choose_display(args.grid));

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

        // **The link's own measurement**, which `architecture.md` has wanted
        // since the panel moved to the Pico: write to ack, on the last frame.
        if let Display::Panel { packed, frames, .. } = panel.show() {
            println!(
                "panel: {frames} frames, {} unacknowledged, last round trip {}",
                packed.outstanding(),
                packed
                    .last_rtt()
                    .map_or("never measured".to_string(), |d| format!("{:.1} ms", d.as_secs_f64() * 1e3))
            );
        }

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
