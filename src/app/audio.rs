//! The audio thread: fill from the ring, hand the period to the device.
//!
//! This is the thread with the deadline. `architecture.md` calls it the audio
//! callback; there is no driver callback here because the ALSA sink is written
//! to rather than called from, so "the callback" is this loop.
//!
//! # It applies the realtime setup itself, and that is not a detail
//!
//! [`run`] calls `rt::apply` as its **first act, on this thread**, rather than
//! taking a process that was already set up. glibc's `pthread_create` defaults
//! to `PTHREAD_INHERIT_SCHED`, so a process configured in `main` and then
//! spawning threads hands `SCHED_FIFO` 75 to every one of them — including the
//! window thread, with libsndfile, a blocking read and an allocator on it. And
//! `rt::apply`'s read-backs cannot see it, because they report the calling
//! thread. `Window::run` asserts it is not realtime for the same reason.
//!
//! # Every allocation here is a bug, so the loop says so
//!
//! The per-period body runs inside [`deck_pi::no_alloc`](crate::no_alloc), so
//! a debug build aborts on an allocation and a release build counts one and
//! carries on. `tests/callback_rules.rs` already asserts this of the pieces;
//! wrapping the real loop is what extends it to the way they are assembled.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::engine::{Engine, Outcome};
use crate::ring::RingReader;
use crate::rt::{self, RtRequest};
use crate::sink::{AudioSink, SinkError};
use crate::transport::Transport;

/// Why the loop stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped {
    /// The track played out. The transport has been told.
    EndOfTrack,
    /// The window thread reported a failure — a pulled stick, dominantly.
    /// What was resident played out first; that is the design, not a
    /// consolation.
    MediumLost,
    /// Asked to stop from outside.
    Asked,
    /// An outcome v1 cannot serve. Should be unreachable — `NeedsResampler`
    /// is the only member and the transport's seqlock is what makes it so.
    Unexpected(String),
}

/// What the run did, for the display and for bring-up reporting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
    pub frames: u64,
    /// Periods the ring could not serve. Silence, not failures.
    pub misses: u64,
    pub underruns: u64,
    pub peak: i32,
    pub elapsed: Duration,
}

/// Everything the loop needs from outside itself.
///
/// A struct rather than eight arguments because the audio thread takes
/// ownership of all of it: the thread returns them, and **the control thread
/// is what drops them**. Dropping the ring here would free 64 MiB on the
/// deadline — the last `Arc<Shared>` goes with the reader — which is the
/// hazard `implementation.md` names under the language-specific note.
pub struct Deck<S: AudioSink> {
    pub transport: std::sync::Arc<Transport>,
    pub reader: RingReader,
    pub engine: Engine,
    pub sink: S,
}

/// Runs until the track ends, the medium goes, or `stop` is set.
///
/// Returns the deck it was given, so the caller drops it off the deadline.
pub fn run<S: AudioSink>(
    mut deck: Deck<S>,
    stop: &AtomicBool,
    medium_lost: &AtomicBool,
    rt: Option<&RtRequest>,
) -> (Deck<S>, Stopped, Report) {
    // First, on this thread. See the module doc.
    if let Some(req) = rt {
        if let Err(e) = rt::apply(req, deck.reader.capacity() * crate::ring::RING_FRAME_BYTES as u64)
        {
            // Not fatal: an unprivileged desk is the ordinary development
            // case, and refusing to play there would make the deck
            // untestable. `--rt-check` is where this is an error.
            eprintln!("deck: realtime setup refused, playing anyway — {e}");
        }
    }

    let period_frames = deck.sink.period_frames();
    let starves = deck.sink.starves_if_not_fed();
    let mut period = vec![0i32; period_frames * crate::file::RING_CHANNELS];
    let mut report = Report::default();
    let started = Instant::now();
    let mut verified = false;

    let why = loop {
        if stop.load(Ordering::Relaxed) {
            break Stopped::Asked;
        }
        let outcome = crate::no_alloc(|| deck.engine.fill(&deck.transport, &deck.reader, &mut period));
        match outcome {
            Outcome::Played { frames } | Outcome::PlayedTail { frames } => {
                let used = frames * crate::file::RING_CHANNELS;
                let write = crate::no_alloc(|| {
                    for &s in &period[..used] {
                        report.peak = report.peak.max(s.abs());
                    }
                    deck.sink.write_period(&period[..used])
                });
                match write {
                    Ok(()) => report.frames += frames as u64,
                    Err(SinkError::Underrun) => report.underruns += 1,
                    Err(e) => break Stopped::Unexpected(e.to_string()),
                }
                // Once, as soon as the stream is really running: `hw_params`
                // reads `closed` before the first write and after `drain`.
                if !verified {
                    verified = true;
                    if let Err(e) = deck.sink.verify_in_force() {
                        break Stopped::Unexpected(format!("ALSA substituted something: {e}"));
                    }
                }
            }
            Outcome::EndOfTrack => {
                deck.transport.reached_end();
                break Stopped::EndOfTrack;
            }
            // Any miss is a period of silence and none is a fault — see
            // `Miss`. Matching the whole enum is deliberate: naming one
            // variant is how `Relocated` once ended playback.
            Outcome::Missed(_) => {
                report.misses += 1;
                if medium_lost.load(Ordering::Relaxed) {
                    break Stopped::MediumLost;
                }
                if starves {
                    // The sink says it runs dry, so hand it the silence the
                    // engine already put in the buffer.
                    let write = crate::no_alloc(|| deck.sink.write_period(&period));
                    match write {
                        Ok(()) | Err(SinkError::Underrun) => {}
                        Err(e) => break Stopped::Unexpected(e.to_string()),
                    }
                } else {
                    std::thread::yield_now();
                }
            }
            other => break Stopped::Unexpected(format!("{other:?}")),
        }
    };

    let _ = deck.sink.drain();
    report.elapsed = started.elapsed();
    (deck, why, report)
}
