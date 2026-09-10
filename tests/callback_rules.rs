//! The callback rules, machine-enforced.
//!
//! CLAUDE.md requires the discipline to hold from the first commit, "while
//! the load is still trivial enough to get away with breaking it". That is a
//! statement about intent unless something checks it. This is the check.
//!
//! The rules are wider than "no malloc, no lock, no I/O": also excluded is
//! anything worse than O(1), anything whose working set varies, and any
//! third-party call that does not promise realtime behaviour
//! (docs/implementation.md, "Process setup").

use assert_no_alloc::assert_no_alloc;
use deck_pi::no_alloc;

use deck_pi::engine::{Engine, Outcome};
use deck_pi::file::RING_CHANNELS;
use deck_pi::ring::{self, Miss};
use deck_pi::sink::{AudioSink, CaptureSink};
use deck_pi::transport::Transport;

/// Everything the audio callback does, on a resident window.
#[test]
fn the_read_path_allocates_nothing() {
    let (mut w, r) = ring::new(4096);
    let block: Vec<i32> = (0..2048u64).flat_map(|f| [f as i32, f as i32]).collect();
    w.append(&block);

    // Buffers exist before the region begins, as they will in the engine:
    // the ring is allocated at track load and the period buffer by ALSA.
    let mut out = vec![0i32; 256 * RING_CHANNELS];

    no_alloc(|| {
        for start in [0u64, 256, 512, 1024, 1792] {
            r.read_block(start, &mut out).expect("resident");
        }
        // Backwards, which is what v2's jog does.
        for start in [1792u64, 1024, 512, 0] {
            r.read_block(start, &mut out).expect("resident");
        }
        // Single frames, the fractional read path.
        for f in [0u64, 1, 2047] {
            r.read_frame(f).expect("resident");
        }
        r.resident();
        r.publish_playhead(1234);
    });
}

/// The miss paths matter as much: they run exactly when things are going
/// wrong, which is the worst moment to discover they allocate.
#[test]
fn the_miss_paths_allocate_nothing() {
    let (mut w, r) = ring::new(1024);
    let block: Vec<i32> = (0..512u64).flat_map(|f| [f as i32, f as i32]).collect();
    w.append(&block);
    let mut out = vec![7i32; 128 * RING_CHANNELS];

    no_alloc(|| {
        // Not yet filled.
        assert_eq!(r.read_block(600, &mut out), Err(Miss::NotResident));
        assert!(out.iter().all(|&s| s == 0));
        // Straddling the end of the resident span.
        assert_eq!(r.read_block(500, &mut out), Err(Miss::NotResident));
        // Before the window ever started.
        assert_eq!(r.read_block(u64::MAX - 1, &mut out), Err(Miss::NotResident));
    });
}

/// A read whose frames are recycled underneath it must report `Overrun`
/// without allocating — this is the path a window thread being outrun takes.
#[test]
fn detecting_an_overrun_allocates_nothing() {
    let (mut w, r) = ring::new(256);
    let block: Vec<i32> = (0..256u64).flat_map(|f| [f as i32, f as i32]).collect();
    w.append(&block);
    let mut out = vec![0i32; 64 * RING_CHANNELS];

    // Recycle the front while the reader still thinks frame 0 is there.
    r.read_block(0, &mut out).expect("resident before the drop");
    w.drop_before(200);

    no_alloc(|| {
        assert_eq!(r.read_block(0, &mut out), Err(Miss::NotResident));
        assert!(out.iter().all(|&s| s == 0));
    });
}

/// The engine's `fill` **is** the audio callback. Everything above tests the
/// ring underneath it; this tests the thing ALSA will actually call, on every
/// branch it has — playing, the track's last short period, paused, a silent
/// seek, a miss, and the end of the track.
///
/// The outcomes are collected inside the region and asserted **outside** it.
/// An assertion that fails inside would panic, and formatting the panic
/// message allocates — which aborts on the allocation and hides the actual
/// failure. That cost an hour once; do not put assertions in here.
#[test]
fn the_callback_body_allocates_nothing_on_any_branch() {
    let frames = 1_000u64;
    let (mut w, r) = ring::new(1024);
    let block: Vec<i32> = (0..1000u64).flat_map(|f| [f as i32, f as i32]).collect();
    w.append(&block);

    let t = Transport::new();
    let mut e = Engine::new(frames);
    let mut period = vec![0i32; 128 * RING_CHANNELS];
    let mut seen: [Option<Outcome>; 9] = [None; 9];

    no_alloc(|| {
        t.play();
        seen[0] = Some(e.fill(&t, &r, &mut period)); // playing

        t.request_seek(500);
        seen[1] = Some(e.fill(&t, &r, &mut period)); // a seek consumed inside

        t.pause();
        seen[2] = Some(e.fill(&t, &r, &mut period)); // paused

        t.begin_seek(true);
        seen[3] = Some(e.fill(&t, &r, &mut period)); // silent seek forward
        t.begin_seek(false);
        seen[4] = Some(e.fill(&t, &r, &mut period)); // silent seek back
        t.end_seek(true);

        t.request_seek(frames - 40);
        seen[5] = Some(e.fill(&t, &r, &mut period)); // the last, short period
        seen[6] = Some(e.fill(&t, &r, &mut period)); // the end

        t.request_seek(2_000); // past the track
        seen[7] = Some(e.fill(&t, &r, &mut period));

        // A miss: back to a frame the ring never held.
        t.request_seek(0);
        t.play();
        r.publish_playhead(0);
        seen[8] = Some(e.fill(&t, &r, &mut period));
    });

    // Now it is safe to be wrong.
    assert!(matches!(seen[0], Some(Outcome::Played { .. })), "{:?}", seen[0]);
    assert!(matches!(seen[1], Some(Outcome::Played { .. })), "{:?}", seen[1]);
    assert_eq!(seen[2], Some(Outcome::Paused));
    assert_eq!(seen[3], Some(Outcome::Seeking));
    assert_eq!(seen[4], Some(Outcome::Seeking));
    assert!(
        matches!(seen[5], Some(Outcome::PlayedTail { .. })),
        "{:?}",
        seen[5]
    );
    assert_eq!(seen[6], Some(Outcome::EndOfTrack));
    assert_eq!(seen[7], Some(Outcome::EndOfTrack));
    assert!(seen[8].is_some());
}

/// The callback plus the sink, which together are everything that runs under
/// the deadline. `CaptureSink` is pre-allocated to its full size and refuses
/// to grow, so if it ever started reallocating this test would catch it —
/// which matters, because a sink that grows on write would make every other
/// allocation-freedom result here meaningless.
#[test]
fn the_callback_and_the_sink_together_allocate_nothing() {
    let frames = 2_000u64;
    let (mut w, r) = ring::new(2048);
    let block: Vec<i32> = (0..2000u64).flat_map(|f| [f as i32, f as i32]).collect();
    w.append(&block);

    let t = Transport::new();
    let mut e = Engine::new(frames);
    let mut period = vec![0i32; 128 * RING_CHANNELS];
    let mut sink = CaptureSink::new(44_100, 128, frames as usize + 128);
    let mut wrote = 0usize;
    let mut errors = 0usize;

    no_alloc(|| {
        t.play();
        for _ in 0..20 {
            match e.fill(&t, &r, &mut period) {
                Outcome::Played { frames: n } | Outcome::PlayedTail { frames: n } => {
                    if sink.write_period(&period[..n * RING_CHANNELS]).is_err() {
                        errors += 1;
                    } else {
                        wrote += n;
                    }
                }
                _ => {}
            }
        }
        // CUE runs on the control thread, but nothing stops it landing
        // between two periods, so its slot writes are on this budget too.
        t.cue_down();
        t.cue_up();
        let _ = e.fill(&t, &r, &mut period);
        sink.drain().unwrap();
    });

    assert_eq!(errors, 0);
    // 20 periods of 128 is 2560 frames, but the track is 2000: fifteen full
    // periods and an 80-frame tail. The rest come back as EndOfTrack and
    // write nothing, which is the behaviour under test as much as the count.
    assert_eq!(wrote, frames as usize);
    assert_eq!(sink.frames_written(), frames as usize);
    assert!(sink.was_drained());
}

/// Not a test of our code — a test of the harness. Without it, every
/// assertion above could be passing because the check does nothing.
///
/// The two profiles are checked differently because they behave differently
/// on purpose (see the allocator's documentation in `src/lib.rs`): debug
/// aborts, release counts.
#[cfg(debug_assertions)]
#[test]
fn the_enforcement_aborts_in_debug() {
    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(&exe)
        .args(["--exact", "--ignored", "deliberate_violation"])
        .output()
        .expect("run the child");

    assert!(
        !out.status.success(),
        "an allocation inside assert_no_alloc must abort, but the child exited \
         successfully.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        // 6 is SIGABRT, which is what handle_alloc_error raises.
        assert_eq!(
            out.status.signal(),
            Some(6),
            "expected SIGABRT from handle_alloc_error, got {:?}",
            out.status
        );
    }
}

/// In release the violation is counted rather than aborted, so the deck logs
/// a bug instead of killing the audio thread in the middle of a set. That
/// makes it checkable in-process.
#[cfg(not(debug_assertions))]
#[test]
fn the_enforcement_counts_in_release() {
    assert_no_alloc::reset_violation_count();
    assert_eq!(assert_no_alloc::violation_count(), 0);

    let (_w, r) = ring::new(64);
    let mut out = vec![0i32; 8 * RING_CHANNELS];
    assert_no_alloc(|| {
        let _ = r.read_block(0, &mut out);
        let leak: Vec<i32> = Vec::with_capacity(1024);
        std::hint::black_box(&leak);
    });

    assert!(
        assert_no_alloc::violation_count() > 0,
        "the allocation was not counted — enforcement is inert in this profile"
    );
    assert_no_alloc::reset_violation_count();
}

/// Run only by the debug test above, in a child process, where aborting is
/// the expected outcome.
#[cfg(debug_assertions)]
#[test]
#[ignore]
fn deliberate_violation() {
    let (_w, r) = ring::new(64);
    let mut out = vec![0i32; 8 * RING_CHANNELS];
    assert_no_alloc(|| {
        let _ = r.read_block(0, &mut out);
        // This is the line that must abort.
        let leak: Vec<i32> = Vec::with_capacity(1024);
        std::hint::black_box(&leak);
    });
    println!("the allocation was not caught");
}
