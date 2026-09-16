//! The whole deck, playing, with everything else running.
//!
//! `README.md` has carried a caveat since the first hardware run: 192 kHz at a
//! 1.33 ms period ran clean **with nothing else on the machine**, and to read
//! that as a floor rather than a verdict, because "the display, the browser,
//! media watch and the input loop were not running". They all exist now, and
//! `src/bin/deck.rs` runs them together — so the caveat is answerable.
//!
//! This is that answer, run with `--ignored` because it needs the deck:
//!
//! ```sh
//! DECK_PI_TRACK="/media/stick/Music/.../01 Chuoda.aiff" \
//!     cargo test --release --test deck_soak -- --ignored --nocapture
//! ```
//!
//! **What it is faithful about**, which is the whole point of not writing a
//! smaller benchmark: the real ALSA device at the track's own rate, the real
//! realtime promotion on the audio thread, the real media watch polling the
//! real mount, the real `/dev/input` nodes, and the real 1 bpp render into the
//! packer — one `Painter` pass per redraw, the same one a panel would get.
//!
//! **What it is not.** Nobody presses anything: there is no switch wired to
//! the Pico yet, so the control loop turns over reading an input surface that
//! never reports. A browse spin is the one load this does not apply, and it is
//! the one that makes `Browser::view` open headers.

#[cfg(not(target_os = "linux"))]
#[test]
#[ignore]
fn the_deck_under_load() {
    eprintln!("the soak needs the deck: ALSA, /dev/input and a mounted stick");
}

#[cfg(target_os = "linux")]
#[test]
#[ignore]
fn the_deck_under_load() {
    use deck_pi::app::controls::{self, Controls};
    use deck_pi::app::deck::Deck;
    use deck_pi::app::medium::Mount;
    use deck_pi::app::panel::Panel;
    use deck_pi::app::track::Config;
    use deck_pi::display::packed::Packed;
    use deck_pi::display::wire::Declared;
    use deck_pi::input::Devices;
    use deck_pi::media::MediaWatch;
    use deck_pi::rt::RtRequest;
    use deck_pi::sink::alsa::AlsaSink;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    const PERIOD: usize = 128;
    const PERIODS: u32 = 2;

    let track = std::path::PathBuf::from(
        std::env::var("DECK_PI_TRACK").expect("set DECK_PI_TRACK to a file on the stick"),
    );
    let device = std::env::var("DECK_PI_DEVICE")
        .unwrap_or_else(|_| "hw:CARD=sndrpihifiberry,DEV=0".into());
    let mount_path = std::env::var("DECK_PI_MOUNT").unwrap_or_else(|_| "/media/stick".into());
    let seconds: u64 = std::env::var("DECK_PI_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);

    let say = |what: &str| {
        let temp = std::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp")
            .ok()
            .and_then(|s| s.trim().parse::<f64>().ok())
            .map(|m| m / 1000.0)
            .unwrap_or(f64::NAN);
        let clock = std::process::Command::new("vcgencmd")
            .args(["measure_clock", "arm"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().rsplit('=').next()?.parse::<f64>().ok())
            .map(|hz| hz / 1e6)
            .unwrap_or(f64::NAN);
        let throttled = std::process::Command::new("vcgencmd")
            .arg("get_throttled")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().rsplit('=').next().unwrap_or("?").to_owned())
            .unwrap_or_else(|| "?".into());
        println!("{what:<7} {temp:.1} C  {clock:.0} MHz  throttled={throttled}");
    };

    println!("track  {}", track.display());
    println!("device {device}, {seconds} s, period {PERIOD} x {PERIODS}");
    say("before");

    let dev = device.clone();
    let mut deck: Deck<AlsaSink> = Deck::new(
        Box::new(move |info| AlsaSink::open(&dev, info.rate, PERIOD, PERIODS)),
        None,
        None,
        Config {
            window_bytes: deck_pi::ring::WINDOW_BYTES_PLACEHOLDER,
            // The deck's own setting. `rt::apply` runs on the audio thread as
            // its first act; a test that left this `None` would be measuring
            // the thing README already measured.
            rt: Some(RtRequest::default()),
        },
    );

    let mut mount = Mount::new(MediaWatch::new(&mount_path), deck_pi::cue::default_state_dir());
    let drew = mount.turn(Duration::ZERO, &mut deck).expect("a medium");
    println!("medium {}", drew.medium);
    assert!(deck.browser().is_some(), "nothing mounted at {mount_path}");

    let mut devices = Devices::open_discovered().expect("input");
    println!("input  {} node(s)", devices.len());
    let mut controls = Controls::new(&devices);

    // A real 1 bpp render per redraw, into nowhere. The bytes are what a panel
    // would get; discarding them measures the deck's half and not the link's.
    let mut panel = Panel::new(Packed::new(
        std::io::sink(),
        Declared {
            px_w: 128,
            px_h: 64,
            colour: false,
            partial: false,
            brightness_levels: 0,
            max_payload: 16 * 1024,
        },
    ));

    deck.load(&track).expect("load");
    let info = deck.loaded().track().expect("loaded").clone();
    println!(
        "playing {} Hz {:?} {} ch",
        info.rate, info.depth, info.channels
    );
    deck.apply(deck_pi::input::Action::Press(deck_pi::input::Button::PlayPause))
        .expect("play");

    // **Which threads are realtime, read from the kernel.** `rt::in_force`
    // reports the *calling* thread, so it can say the control thread is not
    // promoted and can say nothing at all about the one that matters. This
    // walks `/proc/self/task` instead, and the assertion below is two claims
    // in one: the audio thread got `SCHED_FIFO`, and **nothing else did** —
    // which is the `PTHREAD_INHERIT_SCHED` leak `app::audio` is arranged to
    // avoid and that no other test can observe.
    fn scheduling() -> Vec<(String, u32, u32)> {
        let mut out = Vec::new();
        let Ok(tasks) = std::fs::read_dir("/proc/self/task") else { return out };
        for t in tasks.flatten() {
            let Ok(stat) = std::fs::read_to_string(t.path().join("stat")) else { continue };
            // `comm` is parenthesised and may contain spaces, so everything is
            // counted from the last ')' rather than by splitting the line.
            let Some(after) = stat.rfind(')').map(|i| &stat[i + 1..]) else { continue };
            let f: Vec<&str> = after.split_whitespace().collect();
            // /proc/pid/stat, 1-based: 40 is rt_priority and 41 is policy.
            let (Some(prio), Some(policy)) = (f.get(37), f.get(38)) else { continue };
            let comm = std::fs::read_to_string(t.path().join("comm"))
                .unwrap_or_default()
                .trim()
                .to_owned();
            out.push((
                comm,
                policy.parse().unwrap_or(u32::MAX),
                prio.parse().unwrap_or(0),
            ));
        }
        out
    }

    const SCHED_FIFO: u32 = 1;
    let threads = scheduling();
    for (comm, policy, prio) in &threads {
        let name = match policy {
            0 => "SCHED_OTHER",
            1 => "SCHED_FIFO",
            2 => "SCHED_RR",
            _ => "?",
        };
        println!("thread  {comm:<16} {name} priority {prio}");
    }
    let realtime: Vec<_> = threads.iter().filter(|(_, p, _)| *p == SCHED_FIFO).collect();
    assert_eq!(
        realtime.len(),
        1,
        "exactly one thread should be realtime — the audio one. Got {realtime:?}"
    );
    assert_eq!(realtime[0].2, 75, "at the priority `RtRequest::default` asks for");

    static STOP: AtomicBool = AtomicBool::new(false);
    STOP.store(false, Ordering::Relaxed);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(seconds));
        STOP.store(true, Ordering::Relaxed);
    });

    let started = Instant::now();
    let report = controls::run(
        &mut controls,
        &mut devices,
        &mut mount,
        &mut panel,
        &mut deck,
        &STOP,
        |turn, change, _show| {
            if let Some(c) = change {
                println!("medium changed: {}", c.medium);
            }
            for e in &turn.errors {
                println!("refused: {e}");
            }
            if let Some(e) = &turn.draw_error {
                println!("display: {e}");
            }
        },
    )
    .expect("the loop");
    let wall = started.elapsed();

    say("after");
    let ended = deck.unload().expect("something was playing");
    let a = ended.report;

    println!();
    println!("audio   {} frames, {} misses, {} underruns, peak {}", a.frames, a.misses, a.underruns, a.peak);
    println!(
        "        {:.1} s of audio in {:.1} s of wall clock",
        a.frames as f64 / f64::from(info.rate),
        wall.as_secs_f64()
    );
    println!(
        "loop    {} turns ({:.0}/s), {} draws ({:.1}/s), {} refused",
        report.turns,
        report.turns as f64 / wall.as_secs_f64(),
        report.draws,
        report.draws as f64 / wall.as_secs_f64(),
        report.errors
    );
    println!(
        "browser {} header(s) read, {} decoder reset(s), {} device loss(es)",
        deck.browser().map(|b| b.headers_read()).unwrap_or(0),
        controls.resets(),
        report.lost
    );
    // Of the *control* thread, which is what `in_force` can see — and which
    // must not be realtime. The audio thread was checked above.
    match deck_pi::rt::in_force() {
        Ok(f) => println!(
            "control thread: {:?} priority {}, {} KiB locked",
            f.policy, f.priority, f.locked_kib
        ),
        Err(e) => println!("control thread: {e}"),
    }

    // **The assertions are about the audio and nothing else.** A turn count is
    // a curiosity; a miss is a hole in the output, and an underrun is the
    // device having run dry. Those are what the caveat in README was about.
    assert_eq!(a.underruns, 0, "the device ran dry with the loop running");
    assert_eq!(a.misses, 0, "the ring could not serve a period");
    assert!(
        a.frames > u64::from(info.rate) * (seconds - 1),
        "played {} frames in {seconds} s at {} Hz — the transport did not keep up",
        a.frames,
        info.rate
    );
    assert_eq!(report.errors, 0, "the loop refused something");
}
