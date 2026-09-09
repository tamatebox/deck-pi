//! The window thread against real files, and the ring under real contention.

mod fixtures;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fixtures::{signal, Bits, Kind, Scratch};

use deck_pi::file::{Depth, RING_CHANNELS};
use deck_pi::ring::{self, Miss};
use deck_pi::window::{Command, Event, Window};

/// Small enough to make the window's edges reachable in a test, expressed in
/// bytes because that is the unit the design sizes in.
const SMALL_WINDOW_BYTES: usize = 64 * ring::RING_FRAME_BYTES * 16; // 1024 frames

fn expected(v: i32, depth: Depth) -> i32 {
    (v << depth.left_justify_shift()) >> 8
}

/// Drives `fill_step` until it stops making progress, as the real loop does.
fn fill_until_quiet(w: &mut Window) -> usize {
    let mut total = 0;
    for _ in 0..1000 {
        let f = w.fill_step().expect("fill");
        total += f.frames;
        if f.frames == 0 {
            return total;
        }
    }
    panic!("fill_step never settled");
}

#[test]
fn the_ring_receives_the_tracks_samples_through_the_window_thread() {
    let scratch = Scratch::new("window-fill");
    let frames = 4000;
    let source = signal(Bits::S24, 2, frames);
    let bytes = fixtures::build(Kind::Wav, &source, Bits::S24, 44_100, 2);
    let path = fixtures::write(&scratch.dir, "fill", Kind::Wav, &bytes);

    let (mut window, reader, info) =
        Window::load(&path, SMALL_WINDOW_BYTES).expect("loads");
    assert_eq!(info.frames, frames as u64);
    assert_eq!(reader.capacity(), 1024);

    fill_until_quiet(&mut window);

    // The forward half is `ahead_target`; nothing beyond it should be resident.
    let resident = reader.resident();
    assert_eq!(resident.start, 0);
    assert_eq!(resident.end, window.ahead_target());

    let mut buf = vec![0i32; 64 * RING_CHANNELS];
    reader.read_block(0, &mut buf).expect("front of the window");
    for (i, (&got, &src)) in buf.iter().zip(source.iter()).enumerate() {
        assert_eq!(got, expected(src, Depth::Int24), "sample {}", i);
    }
}

#[test]
fn the_window_follows_the_playhead_and_keeps_a_half_behind_it() {
    let scratch = Scratch::new("window-follow");
    let frames = 8000;
    let source = signal(Bits::S16, 2, frames);
    let bytes = fixtures::build(Kind::Wav, &source, Bits::S16, 48_000, 2);
    let path = fixtures::write(&scratch.dir, "follow", Kind::Wav, &bytes);

    let (mut window, reader, _) = Window::load(&path, SMALL_WINDOW_BYTES).expect("loads");
    let ahead = window.ahead_target();
    let behind = window.behind_target();
    fill_until_quiet(&mut window);

    // Walk the playhead forward in callback-sized blocks and check the window
    // tracks it, including that frames already played stay readable.
    let block = 128u64;
    let mut playhead = 0u64;
    while playhead + ahead < frames as u64 {
        reader.publish_playhead(playhead);
        fill_until_quiet(&mut window);
        let r = reader.resident();
        assert!(r.contains(&playhead), "playhead {} fell out of {:?}", playhead, r);
        assert_eq!(r.end, playhead + ahead, "forward half at playhead {}", playhead);
        assert_eq!(
            r.start,
            playhead.saturating_sub(behind),
            "behind half at playhead {}",
            playhead
        );

        // A frame the playhead has already passed is still readable — the
        // property a FIFO cannot provide.
        if playhead >= block {
            let back = playhead - block;
            let mut buf = vec![0i32; 4 * RING_CHANNELS];
            reader.read_block(back, &mut buf).expect("behind the playhead");
            for c in 0..4 * RING_CHANNELS {
                assert_eq!(buf[c], expected(source[back as usize * 2 + c], Depth::Int16));
            }
        }
        playhead += block;
    }
    assert!(playhead > behind, "the test never exercised the behind half");
}

#[test]
fn a_track_shorter_than_the_window_becomes_fully_resident_and_reports_the_end() {
    let scratch = Scratch::new("window-short");
    let frames = 300;
    let source = signal(Bits::S16, 2, frames);
    let bytes = fixtures::build(Kind::Aiff, &source, Bits::S16, 44_100, 2);
    let path = fixtures::write(&scratch.dir, "short", Kind::Aiff, &bytes);

    let (mut window, reader, _) = Window::load(&path, SMALL_WINDOW_BYTES).expect("loads");
    fill_until_quiet(&mut window);
    assert!(window.at_end_of_track());
    assert_eq!(reader.resident(), 0..frames as u64);

    // And reading past the last frame is a clean miss, not a wrap to the top.
    let mut buf = vec![0i32; 8 * RING_CHANNELS];
    assert_eq!(
        reader.read_block(frames as u64 - 4, &mut buf),
        Err(Miss::NotResident)
    );
    assert!(buf.iter().all(|&s| s == 0));
}

#[test]
fn a_seek_out_of_range_relocates_and_refills_from_the_new_position() {
    let scratch = Scratch::new("window-seek");
    let frames = 20_000;
    let source = signal(Bits::S24, 2, frames);
    let bytes = fixtures::build(Kind::Rf64, &source, Bits::S24, 96_000, 2);
    let path = fixtures::write(&scratch.dir, "seek", Kind::Rf64, &bytes);

    let (mut window, reader, _) = Window::load(&path, SMALL_WINDOW_BYTES).expect("loads");
    fill_until_quiet(&mut window);

    let target = 15_000u64;
    window.relocate(target).expect("relocate");
    assert_eq!(reader.resident(), target..target);
    // Anything from before the seek must now read as a miss, not as audio.
    let mut buf = vec![0i32; 8 * RING_CHANNELS];
    assert_eq!(reader.read_block(0, &mut buf), Err(Miss::NotResident));

    reader.publish_playhead(target);
    fill_until_quiet(&mut window);
    reader.read_block(target, &mut buf).expect("after the seek");
    for c in 0..buf.len() {
        assert_eq!(
            buf[c],
            expected(source[target as usize * RING_CHANNELS + c], Depth::Int24),
            "sample {} after seeking to {}",
            c,
            target
        );
    }
}

#[test]
fn a_playhead_that_outran_the_window_is_recovered_rather_than_wedged() {
    // The shape of a severe underrun: the callback has consumed past the end
    // of what was ever filled. The window must restart around it, not sit
    // there filling frames nobody will ask for.
    let scratch = Scratch::new("window-outrun");
    let frames = 20_000;
    let source = signal(Bits::S16, 2, frames);
    let bytes = fixtures::build(Kind::Wav, &source, Bits::S16, 44_100, 2);
    let path = fixtures::write(&scratch.dir, "outrun", Kind::Wav, &bytes);

    let (mut window, reader, _) = Window::load(&path, SMALL_WINDOW_BYTES).expect("loads");
    fill_until_quiet(&mut window);

    reader.publish_playhead(12_345);
    let filled = window.fill_step().expect("fill");
    assert!(filled.relocated, "an outrun playhead must relocate the window");
    fill_until_quiet(&mut window);
    assert!(reader.resident().contains(&12_345));

    let mut buf = vec![0i32; 4 * RING_CHANNELS];
    reader.read_block(12_345, &mut buf).expect("recovered");
    for c in 0..buf.len() {
        assert_eq!(buf[c], expected(source[12_345 * RING_CHANNELS + c], Depth::Int16));
    }
}

#[test]
fn the_thread_serves_a_callback_running_beside_it() {
    // The real arrangement: a window thread filling while a reader drains in
    // block-sized reads. Every sample delivered must be the source's, and the
    // reader must never see a torn value — only a clean miss.
    let scratch = Scratch::new("window-thread");
    let frames = 60_000usize;
    let source = Arc::new(signal(Bits::S24, 2, frames));
    let bytes = fixtures::build(Kind::Wav, &source, Bits::S24, 44_100, 2);
    let path = fixtures::write(&scratch.dir, "threaded", Kind::Wav, &bytes);

    let (window, reader, _) = Window::load(&path, SMALL_WINDOW_BYTES).expect("loads");
    let (tx, rx) = mpsc::channel();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let ev = Arc::clone(&events);
    let handle = std::thread::spawn(move || {
        window.run(rx, move |e| ev.lock().unwrap().push(e));
    });

    let block = 256usize;
    let mut playhead = 0u64;
    let mut misses = 0usize;
    let mut buf = vec![0i32; block * RING_CHANNELS];
    let deadline = Instant::now() + Duration::from_secs(20);

    while (playhead as usize) + block <= frames {
        reader.publish_playhead(playhead);
        match reader.read_block(playhead, &mut buf) {
            Ok(()) => {
                let base = playhead as usize * RING_CHANNELS;
                for (i, &got) in buf.iter().enumerate() {
                    let want = expected(source[base + i], Depth::Int24);
                    assert_eq!(
                        got, want,
                        "torn or wrong sample at frame {} offset {}",
                        playhead, i
                    );
                }
                playhead += block as u64;
            }
            Err(Miss::NotResident) => {
                // The filler has not reached here yet. Real audio would emit
                // silence; the test waits, because it is checking content.
                misses += 1;
                std::thread::yield_now();
            }
            Err(other) => panic!("unexpected miss at frame {}: {:?}", playhead, other),
        }
        assert!(Instant::now() < deadline, "the window thread never caught up");
    }

    tx.send(Command::Shutdown).expect("send shutdown");
    handle.join().expect("window thread");

    let seen = events.lock().unwrap().clone();
    assert!(seen.contains(&Event::Stopped), "events were {:?}", seen);
    assert!(
        seen.iter().filter(|e| **e == Event::EndOfTrack).count() <= 1,
        "EndOfTrack must be latched, got {:?}",
        seen
    );
    println!(
        "drained {} frames in {}-frame blocks; {} misses while waiting on the filler",
        playhead, block, misses
    );
}

#[test]
fn a_reader_racing_a_recycling_writer_never_sees_a_torn_frame() {
    // Straight at the ring, with the writer recycling slots as fast as it
    // can. Every sample of a frame encodes that frame's index, so a value
    // stitched from two different frames is detectable — which is the failure
    // the generation and start re-checks exist to prevent.
    let capacity = 512usize;
    let (mut w, r) = ring::new(capacity);
    let stop = Arc::new(AtomicU64::new(0));
    let reader_playhead = Arc::new(AtomicU64::new(0));

    let stop_w = Arc::clone(&stop);
    let ph = Arc::clone(&reader_playhead);
    let writer = std::thread::spawn(move || {
        let mut next = 0u64;
        let chunk = 64u64;
        while stop_w.load(Ordering::Relaxed) == 0 {
            let playhead = ph.load(Ordering::Relaxed);
            w.drop_before(playhead.saturating_sub(capacity as u64 / 2));
            let room = w.writable().min(chunk);
            if room == 0 {
                std::thread::yield_now();
                continue;
            }
            let block: Vec<i32> = (next..next + room)
                .flat_map(|f| [f as i32, f as i32])
                .collect();
            w.append(&block);
            next += room;
        }
        next
    });

    let mut frame = 0u64;
    let mut buf = [0i32; 16 * RING_CHANNELS];
    let mut hits = 0u64;
    let mut misses = 0u64;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && hits < 200_000 {
        reader_playhead.store(frame, Ordering::Relaxed);
        r.publish_playhead(frame);
        match r.read_block(frame, &mut buf) {
            Ok(()) => {
                for (i, &got) in buf.iter().enumerate() {
                    let f = frame + (i / RING_CHANNELS) as u64;
                    assert_eq!(got, f as i32, "frame {} sample {} was torn", f, i);
                }
                hits += 1;
                frame += 16;
            }
            Err(_) => {
                misses += 1;
                std::thread::yield_now();
            }
        }
    }
    stop.store(1, Ordering::Relaxed);
    let written = writer.join().expect("writer thread");

    assert!(hits > 1000, "only {} successful reads — the test did not race", hits);
    println!(
        "{} verified blocks, {} misses, writer produced {} frames",
        hits, misses, written
    );
}

#[test]
fn what_is_resident_keeps_playing_after_the_window_thread_stops() {
    // The claim in architecture.md: pulling the stick behaves like a CDJ —
    // what is already resident keeps playing, and "the grace period is the
    // forward half of the window".
    //
    // What is simulated here is the window thread being gone. A real pulled
    // stick means EIO from the block device, which needs the hardware; but
    // the half that matters for the callback is that it has no file-backed
    // pages in its path at all, so it cannot fault however the read failed.
    let scratch = Scratch::new("window-pulled");
    let frames = 40_000usize;
    let source = signal(Bits::S24, 2, frames);
    let bytes = fixtures::build(Kind::Wav, &source, Bits::S24, 44_100, 2);
    let path = fixtures::write(&scratch.dir, "pulled", Kind::Wav, &bytes);

    let (mut window, reader, info) = Window::load(&path, SMALL_WINDOW_BYTES).expect("loads");
    let ahead = window.ahead_target();
    let playhead = 5_000u64;
    reader.publish_playhead(playhead);
    fill_until_quiet(&mut window);
    assert_eq!(reader.resident().end, playhead + ahead);

    // The window thread goes away, along with the open file.
    drop(window);
    std::fs::remove_file(&path).expect("remove the file under it");

    // Every frame of the forward half is still deliverable, and correct.
    let block = 64usize;
    let mut buf = vec![0i32; block * RING_CHANNELS];
    let mut served = 0u64;
    let mut at = playhead;
    while reader.read_block(at, &mut buf).is_ok() {
        let base = at as usize * RING_CHANNELS;
        for (i, &got) in buf.iter().enumerate() {
            assert_eq!(got, expected(source[base + i], Depth::Int24), "frame {}", at);
        }
        at += block as u64;
        served += block as u64;
    }

    // The grace period is the forward half, to within one block.
    assert!(
        served >= ahead - block as u64 && served <= ahead,
        "served {} frames, forward half is {}",
        served,
        ahead
    );
    let grace_secs = served as f64 / info.rate as f64;
    let expected_grace = ring::half_window_secs(info.rate, SMALL_WINDOW_BYTES);
    assert!(
        (grace_secs - expected_grace).abs() < 0.01,
        "grace {:.3} s vs the window's forward half {:.3} s",
        grace_secs,
        expected_grace
    );

    // And past it, silence — never stale samples.
    assert_eq!(reader.read_block(at, &mut buf), Err(Miss::NotResident));
    assert!(buf.iter().all(|&s| s == 0));
    println!(
        "grace period after the filler stopped: {} frames = {:.3} s at {} Hz",
        served, grace_secs, info.rate
    );
}

#[test]
fn the_thread_fills_before_it_waits_on_the_channel() {
    // Regression. `run` used to call `recv_timeout` first, which put the
    // whole poll interval of latency in front of every track load and every
    // relocation — precisely the two moments the window is empty, so it was a
    // guaranteed dropout rather than an occasional one. It cost 100x the
    // reader's waits when measured.
    let scratch = Scratch::new("window-latency");
    let source = signal(Bits::S16, 2, 8_000);
    let bytes = fixtures::build(Kind::Wav, &source, Bits::S16, 44_100, 2);
    let path = fixtures::write(&scratch.dir, "latency", Kind::Wav, &bytes);

    let (window, reader, _) = Window::load(&path, SMALL_WINDOW_BYTES).expect("loads");
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    let handle = std::thread::spawn(move || window.run(rx, |_| {}));

    // Wait for the first frame to become readable. No command is ever sent,
    // so a thread that waits first cannot beat its own poll interval.
    let mut buf = vec![0i32; RING_CHANNELS];
    while reader.read_block(0, &mut buf).is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the window thread never filled anything"
        );
        std::thread::yield_now();
    }
    let latency = started.elapsed();

    tx.send(Command::Shutdown).expect("shutdown");
    handle.join().expect("window thread");

    // Generous against a loaded CI machine, but far below the 10 ms poll
    // interval the old ordering imposed.
    assert!(
        latency < Duration::from_millis(5),
        "first frame took {:?}; the fill must not wait on the channel",
        latency
    );
    println!("first frame readable after {:?}", latency);
}

#[test]
fn allocating_a_full_size_ring_is_quick_enough_to_do_per_track() {
    // The ring is per track because its capacity depends on the track's
    // sample rate, so this cost is paid on every load — and it includes the
    // pre-fault, which is the point of doing it here rather than discovering
    // the page faults inside the callback.
    //
    // A number, measured on this machine, not on the Pi: an aarch64 Mac is
    // several times quicker than a 1.2 GHz A53, so treat this as an upper
    // bound on plausibility rather than as the deck's figure.
    let frames = ring::capacity_frames(44_100, ring::WINDOW_BYTES_PLACEHOLDER);
    let started = Instant::now();
    let (_w, r) = ring::new(frames);
    let elapsed = started.elapsed();
    assert_eq!(r.capacity(), frames as u64);
    println!(
        "ring of {} frames ({} MiB) allocated and pre-faulted in {:?} on this host",
        frames,
        frames * ring::RING_FRAME_BYTES / 1024 / 1024,
        elapsed
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "allocating the ring took {:?}, which would stall a track load",
        elapsed
    );
}

#[test]
fn scrubbing_backwards_thrashes_the_window_a_known_v2_gap() {
    // `architecture.md` claims "the v1 read path, control-thread shape and
    // rate variable are all built to accept [v2] without rework". The ring
    // half of that is true — reads inside the window are free in either
    // direction, which is tested above. The **filling policy** is not.
    //
    // `fill_step` relocates to the playhead and then only ever appends
    // forward, so a playhead moving backwards past the window's start gets a
    // window laid out entirely *ahead* of where it is going. Every period
    // then relocates again and refills frames the callback will never ask
    // for.
    //
    // This test asserts the present behaviour rather than the wanted one, so
    // the gap is a recorded fact instead of a surprise when the jog is built.
    // The fix is direction-aware relocation in `window.rs` — relocate to
    // `playhead - ahead` when the playhead is descending — which is about
    // twenty lines and needs no change to the ring.
    let scratch = Scratch::new("window-backwards");
    let frames = 40_000u64;
    let source = signal(Bits::S24, 2, frames as usize);
    let bytes = fixtures::build(Kind::Wav, &source, Bits::S24, 44_100, 2);
    let path = fixtures::write(&scratch.dir, "backwards", Kind::Wav, &bytes);

    let (mut window, reader, _) =
        Window::load(&path, SMALL_WINDOW_BYTES).expect("loads");
    let block = 128u64;
    let jog_rate = 4u64; // |r| = 4, the documented seek rate

    // Settle forwards around the middle of the track.
    let mut playhead = 20_000u64;
    reader.publish_playhead(playhead);
    fill_until_quiet(&mut window);
    assert!(reader.resident().contains(&playhead));

    // Now scrub backwards, as a jog would.
    let mut relocations = 0usize;
    let mut served = 0usize;
    let mut missed = 0usize;
    let mut buf = vec![0i32; block as usize * RING_CHANNELS];

    for _ in 0..12 {
        playhead = playhead.saturating_sub(block * jog_rate);
        reader.publish_playhead(playhead);
        let f = window.fill_step().expect("fill");
        if f.relocated {
            relocations += 1;
        }
        fill_until_quiet(&mut window);
        // Backwards playback consumes input *below* the position, not above
        // it: at r = -4 an output period starting at p reads down to
        // p - 4*block. So this is the block the callback would actually need.
        // Reading forward from the playhead is what a first version of this
        // test did, and it passed — because relocating to the playhead and
        // filling forward serves exactly that, and nothing else.
        let need = playhead.saturating_sub(block);
        match reader.read_block(need, &mut buf) {
            Ok(()) => served += 1,
            Err(_) => missed += 1,
        }
    }

    println!(
        "backwards scrub: {} relocations, {} periods served, {} missed",
        relocations, served, missed
    );
    // The window does relocate repeatedly, which is the thrash.
    assert!(
        relocations >= 6,
        "expected the window to relocate on most periods, got {}",
        relocations
    );
    // And what it fills is ahead of the playhead, so the frames the callback
    // wants next are never resident.
    assert!(
        missed > 0,
        "expected backwards scrubbing to miss; if this now passes cleanly the \
         gap has been fixed and this test should be inverted"
    );
}
