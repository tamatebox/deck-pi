//! What the deck says when the stick goes away mid-track.
//!
//! The audio behaviour is settled and correct: what is already resident in
//! the ring plays out, bounded by the forward half of the window, which is
//! `window_test.rs`'s `what_is_resident_keeps_playing_after_the_window_thread_stops`.
//! **This file is about the other half — what the deck reports afterwards.**
//! `decisions.md` puts telling "no stick" apart from "stick I cannot read" at
//! the centre of the media design, and the same distinction has to survive one
//! layer down, in the thread that hits the failure first.
//!
//! It did not. `sf_readf_int` returns the frames it managed and logs the
//! failure on the handle, so a `read(2)` error and a clean end of file are the
//! same return value. The file layer tested for a negative count — which
//! libsndfile never returns from a read — and a pulled stick arrived as
//! `EndOfTrack`.
//!
//! Linux only, because the fault is injected with `dup2` over the handle's own
//! descriptor and found through `/proc/self/fd`.

#![cfg(target_os = "linux")]

mod fixtures;

use std::os::fd::AsRawFd;

use deck_pi::file::{Track, RING_CHANNELS};
use deck_pi::window::{Command, Event, Window};
use fixtures::{Bits, Kind, Scratch};

/// Finds the descriptor libsndfile opened for `path`.
///
/// There is no API for this — `sf_open` takes a path and keeps the descriptor
/// to itself — so the test reads the process's own fd table. Returning
/// `Option` rather than panicking keeps the fault injection honest: if the
/// descriptor cannot be found the test says so instead of passing vacuously.
fn fd_for(path: &std::path::Path) -> Option<i32> {
    let want = std::fs::canonicalize(path).ok()?;
    for entry in std::fs::read_dir("/proc/self/fd").ok()?.flatten() {
        let n: i32 = entry.file_name().to_string_lossy().parse().ok()?;
        if std::fs::read_link(entry.path()).ok().as_deref() == Some(want.as_path()) {
            return Some(n);
        }
    }
    None
}

#[test]
fn a_read_that_fails_is_an_error_and_not_the_end_of_the_track() {
    let scratch = Scratch::new("medium-loss");
    let samples = fixtures::signal(Bits::S24, 2, 48_000);
    let bytes = fixtures::build(Kind::Wav, &samples, Bits::S24, 44_100, 2);
    let path = fixtures::write(&scratch.dir, "long", Kind::Wav, &bytes);

    let mut track = Track::open(&path).expect("open");
    let mut buf = vec![0i32; 2048];

    // Read once cleanly, so the failure below is unambiguously a failure
    // rather than a file that never worked.
    let got = track.read_into_ring(&mut buf).expect("the first read works");
    assert!(got > 0, "the fixture must actually contain audio");

    // Pull the medium. Replacing the descriptor with a directory is the
    // cheapest fault that makes `read(2)` fail on a handle that was healthy a
    // moment ago — EISDIR rather than EIO, but the file layer's job is to
    // notice that the read failed at all, not to classify errno.
    let fd = fd_for(&path).expect(
        "no descriptor in /proc/self/fd points at the fixture — the fault was \
         never injected, so this test would have passed without testing \
         anything. Check that libsndfile still opens the path directly rather \
         than through a layer; this is not a symptom of the fix being wrong.",
    );
    let dir = std::fs::File::open(&scratch.dir).expect("open the directory");
    // SAFETY: both descriptors are open and owned by this process; `dir`
    // outlives the call.
    let rc = unsafe { libc::dup2(dir.as_raw_fd(), fd) };
    assert_eq!(rc, fd, "dup2 must take, or the fault was never injected");

    let err = track
        .read_into_ring(&mut buf)
        .expect_err("a failed read must not read as a clean end of track");
    // The text comes from libsndfile, so assert only that it is attributed
    // rather than pinning a message the library may reword.
    assert!(
        !err.to_string().is_empty(),
        "the failure must carry libsndfile's reason: {err}"
    );
}

#[test]
fn the_real_end_of_a_track_is_still_not_an_error() {
    // The other side of the same branch, and the way this fix could have
    // inverted the bug rather than removed it: if any benign path leaves an
    // error set on the handle, every track now ends in `Failed` instead of
    // `EndOfTrack`.
    //
    // Structurally that cannot happen — `sf_readf_int` passes 1 to
    // `VALIDATE_SNDFILE_AND_ASSIGN_PSF`, so it clears the error on entry and
    // whatever is set afterwards was set by that read (see `ffi.rs`). This
    // covers the remaining surface, which is a read *itself* setting one:
    // every container the deck accepts, at both depths, because AIFF, `sowt`
    // and RF64 are different code paths inside the library.
    let scratch = Scratch::new("medium-loss-eof");
    for kind in [Kind::Wav, Kind::Aiff, Kind::AiffcSowt, Kind::Rf64] {
        for bits in [Bits::S16, Bits::S24] {
            let samples = fixtures::signal(bits, 2, 512);
            let bytes = fixtures::build(kind, &samples, bits, 44_100, 2);
            let name = format!("{kind:?}-{bits:?}");
            let path = fixtures::write(&scratch.dir, &name, kind, &bytes);

            let mut track = Track::open(&path).unwrap_or_else(|e| panic!("open {name}: {e}"));
            let mut buf = vec![0i32; 4096];
            assert_eq!(
                track.read_into_ring(&mut buf).unwrap_or_else(|e| panic!("{name}: {e}")),
                512,
                "{name}"
            );
            assert_eq!(
                track
                    .read_into_ring(&mut buf)
                    .unwrap_or_else(|e| panic!("{name}: reading past the end must not error: {e}")),
                0,
                "{name}: a clean end of file is Ok(0) — this is what EndOfTrack is for"
            );
        }
    }
}

#[test]
fn an_interrupted_copy_ends_the_track_rather_than_reporting_a_lost_medium() {
    // A file whose data chunk stops before the header says it should — an
    // export or a copy that was interrupted, which is a plausible thing to
    // find on a stick prepared in a hurry. The file layer plays these
    // deliberately, so reading off the real end must be `Ok(0)` and not a
    // failure. This is the way the fix could have inverted the bug: turning
    // every short file into "the stick was pulled".
    //
    // **Note what this is not.** `declared_length_is_suspect` is a different
    // predicate — a 32-bit container declaring 2 GiB or more of audio, so a
    // wrapped size field is possible. That needs a 2 GiB fixture and is not
    // covered here. Truncation is the case that is cheap to build, and
    // libsndfile reports the *shortened* frame count for it rather than the
    // header's, which is why the assertion below is about the count agreeing
    // with the bytes that survive.
    let scratch = Scratch::new("medium-loss-truncated");
    let samples = fixtures::signal(Bits::S24, 2, 4_096);
    let mut bytes = fixtures::build(Kind::Wav, &samples, Bits::S24, 44_100, 2);
    let lost_frames = 2_048;
    bytes.truncate(bytes.len() - lost_frames * 2 * 3);
    let path = fixtures::write(&scratch.dir, "truncated", Kind::Wav, &bytes);

    let mut track = Track::open(&path).expect("a truncated file still opens");
    assert!(
        track.info().frames < 4_096,
        "libsndfile must see the real length, not the header's — got {}",
        track.info().frames
    );

    let mut buf = vec![0i32; 1_024 * 2];
    let mut total = 0u64;
    loop {
        let got = track
            .read_into_ring(&mut buf)
            .expect("running off a truncated file is an end of track, not a failed medium");
        if got == 0 {
            break;
        }
        total += got as u64;
    }
    assert_eq!(
        total,
        track.info().frames,
        "the read must deliver exactly what is there and then stop cleanly"
    );
}

#[test]
fn a_stick_pulled_mid_track_is_reported_as_a_failure_and_not_as_the_end() {
    // **The end-to-end claim, and the one that was wrong.**
    // `Event::Failed`'s own doc comment says "the dominant cause is the stick
    // being removed" — and until the `sf_error` fix, the pulled stick could
    // not reach either of its two construction sites. `window.rs:369` is
    // behind `fill_step`'s `?`, which for the read path required a negative
    // return that libsndfile never produces; `window.rs:394` sits inside the
    // `Command::Relocate` arm, which nothing in `src/` sent until
    // `app::deck::Deck` did — long after this test was written.
    //
    // So the variant was declared, constructed in two places, documented, and
    // unreachable by the single event it was written for. That greps clean,
    // which is why nothing found it — and why this test asserts the *absence*
    // of `EndOfTrack` as well as the presence of `Failed`. Reporting both
    // would still be wrong.
    //
    // **Measured with the `sf_error` check reverted**, this test reports:
    //
    // ```text
    // events were [EndOfTrack, Failed("Internal psf_fseek() failed.")]
    // ```
    //
    // Read that closely, because it is the whole finding in one line.
    // `EndOfTrack` comes **first** — that is what the deck would have said.
    // `Failed` arrives afterwards and by way of a failed *seek*, which is the
    // only route to it that ever worked, and it names an internal function
    // rather than the medium. Neither the order nor the reason was right.
    let scratch = Scratch::new("medium-loss-window");
    let frames = 400_000usize;
    let source = fixtures::signal(Bits::S24, 2, frames);
    let bytes = fixtures::build(Kind::Wav, &source, Bits::S24, 44_100, 2);
    let path = fixtures::write(&scratch.dir, "pulled", Kind::Wav, &bytes);

    // Small enough that the window cannot hold the whole track, so the filler
    // must keep reading as the playhead advances — which is what gives the
    // pull something to interrupt.
    const WINDOW_BYTES: usize = 256 * 1024;
    let (window, reader, _) = Window::load(&path, WINDOW_BYTES).expect("loads");
    let fd = fd_for(&path).expect(
        "no descriptor in /proc/self/fd points at the fixture — the fault was \
         never injected. Check that libsndfile still opens the path directly.",
    );

    let (tx, rx) = std::sync::mpsc::channel();
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let ev = std::sync::Arc::clone(&events);
    let handle = std::thread::spawn(move || window.run(rx, move |e| ev.lock().unwrap().push(e)));

    // Let it fill, and play a little, so the failure below interrupts a
    // healthy stream rather than a cold start.
    let block = 256usize;
    let mut buf = vec![0i32; block * RING_CHANNELS];
    let mut playhead = 0u64;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while playhead < 8_192 {
        reader.publish_playhead(playhead);
        if reader.read_block(playhead, &mut buf).is_ok() {
            playhead += block as u64;
        } else {
            std::thread::yield_now();
        }
        assert!(std::time::Instant::now() < deadline, "the window never filled");
    }

    // Pull the stick.
    let dir = std::fs::File::open(&scratch.dir).expect("open the directory");
    // SAFETY: both descriptors are open and owned by this process.
    let rc = unsafe { libc::dup2(dir.as_raw_fd(), fd) };
    assert_eq!(rc, fd, "dup2 must take, or the fault was never injected");

    // Keep asking for audio ahead of what is resident, so the filler has to
    // go back to the medium that is no longer there.
    let mut failed = false;
    while std::time::Instant::now() < deadline {
        reader.publish_playhead(playhead);
        let _ = reader.read_block(playhead, &mut buf);
        playhead += block as u64;
        if events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, Event::Failed(_)))
        {
            failed = true;
            break;
        }
        std::thread::yield_now();
    }

    let _ = tx.send(Command::Shutdown);
    let _ = handle.join();

    let seen = events.lock().unwrap().clone();
    assert!(
        failed,
        "a pulled stick must reach Event::Failed; events were {seen:?}"
    );
    assert!(
        !seen.contains(&Event::EndOfTrack),
        "a pulled stick must not be reported as the track ending; events were {seen:?}"
    );
}
