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
    ///
    /// **Only reachable under [`AtEnd::Stop`]**, which is the bring-up CLI's
    /// setting. On the deck the thread stays up and the end of a track is not
    /// the end of the run — see that type.
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

/// What the end of the track does to the run.
///
/// **The audio thread is per *track*, not per *play*, and this is where that
/// distinction is spent.** `decisions.md` puts the thread, the window thread
/// and the sink on a track's lifetime — "a track change always contains a
/// pause, which is what lets the audio thread, window thread and sink be
/// per-track and every drop happen off the deadline" — so PAUSE, the end of a
/// track, and a Back Cue back into it are all things that happen *inside* one
/// run. Tearing down at the end would mean the next PLAY had to reopen the
/// ALSA device and refill 64 MiB, and it would mean a deck sitting at the end
/// of a track could not be cued back into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtEnd {
    /// End the run. `src/main.rs`'s answer: one file, pulled through, then a
    /// report.
    Stop,
    /// Keep the device fed and the thread alive. **The deck's answer.** The
    /// engine has already written silence into the period, so this costs one
    /// write per period and nothing else.
    Idle,
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
    pub at_end: AtEnd,
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
            // Everything from here down is a period of silence, which the
            // engine has already written into `period`. **None of them is a
            // fault**, and three of them used to be: the arm below was a
            // catch-all, so `Paused` and `Seeking` ended the run with
            // `Unexpected`. Nothing noticed because the only caller was a
            // bring-up CLI that plays one file and never touches a control —
            // the shape `implementation.md` calls an unstated premise, here
            // "the deck is always playing".
            Outcome::EndOfTrack => {
                // **This loop does not tell the transport.** It reports —
                // `Engine::fill` has already published the position — and the
                // control thread derives the end from it and calls
                // `reached_end` itself. Writing control state from here makes
                // a second writer of a type whose other methods are
                // read-modify-write across several atomics, and it cost a
                // lost PLAY after a Back Cue before it was moved. The
                // reasoning is on `Transport::reached_end`; `AtEnd::Stop`'s
                // caller does it after `run` returns, on its own thread.
                if deck.at_end == AtEnd::Stop {
                    break Stopped::EndOfTrack;
                }
                if let Err(e) = idle(&mut deck.sink, &period, starves, &mut report) {
                    break e;
                }
            }
            // Any miss is a period of silence and none is a fault — see
            // `Miss`. Matching the whole enum is deliberate: naming one
            // variant is how `Relocated` once ended playback.
            Outcome::Missed(_) => {
                report.misses += 1;
                if medium_lost.load(Ordering::Relaxed) {
                    break Stopped::MediumLost;
                }
                if let Err(e) = idle(&mut deck.sink, &period, starves, &mut report) {
                    break e;
                }
            }
            // PAUSE, and FF/REW held. Ordinary operation: the deck is not
            // producing audio and is not going anywhere.
            Outcome::Paused | Outcome::Seeking => {
                if let Err(e) = idle(&mut deck.sink, &period, starves, &mut report) {
                    break e;
                }
            }
            // **Deliberately still fatal.** v1 has no resampler, so there is
            // nothing to serve this with, and the transport's seqlock is what
            // makes it unreachable — see `Transport`'s publishing order,
            // which measured 10.7M of 35.6M fills reporting it before the
            // pair was published as one. A deck that stops loudly beats one
            // that plays something it cannot audit.
            Outcome::NeedsResampler { .. } => {
                break Stopped::Unexpected(format!("{outcome:?}"))
            }
        }
    };

    let _ = deck.sink.drain();
    report.elapsed = started.elapsed();
    (deck, why, report)
}

/// One period in which no audio was produced.
///
/// **A sink that does not pace must not be given `AtEnd::Idle` on a realtime
/// thread.** The yield below is a spin, and the resting state of a loaded,
/// paused deck is exactly this branch — so a non-pacing sink there would spin
/// a `SCHED_FIFO` 75 thread on a pinned core for as long as nothing is
/// playing. Every sink that reaches the deck blocks in `write_period`, which
/// is what makes the spin a test-only cost today; that is a constraint on
/// future sinks rather than an observation about this one.
///
/// Two sinks with opposite obligations, which is why
/// [`AudioSink::starves_if_not_fed`] is on the trait rather than assumed here.
/// A real device runs dry if it is not written to, so it gets the silence and
/// the write is what paces the loop. A collector does not, and writing to it
/// would record silence the deck never emitted — `null_test.rs` compares what
/// the sink received against the source, so a padded capture is a failed null
/// test rather than a slow one.
fn idle<S: AudioSink>(
    sink: &mut S,
    silence: &[i32],
    starves: bool,
    report: &mut Report,
) -> Result<(), Stopped> {
    if !starves {
        // Nothing to feed and nothing to wait on. Yielding rather than
        // sleeping keeps the wake-up latency off the next real period.
        std::thread::yield_now();
        return Ok(());
    }
    match crate::no_alloc(|| sink.write_period(silence)) {
        Ok(()) => Ok(()),
        Err(SinkError::Underrun) => {
            report.underruns += 1;
            Ok(())
        }
        Err(e) => Err(Stopped::Unexpected(e.to_string())),
    }
}
