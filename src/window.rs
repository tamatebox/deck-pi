//! The window thread.
//!
//! Reads through libsndfile into the ring and keeps it filled ahead of and
//! behind the playhead. Blocking, allocating and locking are all fine here;
//! this is the thread the deadline does not reach, and it is also where
//! failures surface — a pulled stick included
//! (docs/architecture.md, "Threading").
//!
//! # Why nothing prepends to the ring
//!
//! `architecture.md` asks for the window to be filled "ahead of **and
//! behind**" the playhead, which sounds like it needs a way to write behind
//! the oldest resident frame. It does not. The behind half accumulates on its
//! own: frames the playhead has passed simply are not discarded until they
//! fall more than the behind target back. So the steady state is the ± window
//! the table describes, reached by only ever appending.
//!
//! The one case it does not cover is the instant after a cold seek, when the
//! behind half is empty. That is not a gap in the design — `architecture.md`
//! already states the cost of scrubbing past the window's edge as "an
//! `sf_seek` and a refill", which is exactly what happens. Pre-locking cue
//! regions is the separate mechanism for making a *cold seek* not stall.
//!
//! # The window's bias follows the direction of travel
//!
//! An append-only ring has one asymmetry that matters: what accumulates for
//! free is whatever the playhead has *passed*. `ahead_target` and
//! `behind_target` are therefore relative to the **direction of travel**, not
//! to increasing frame number, and [`Window::above_target`] and
//! [`Window::below_target`] are what map them onto the ring. Ascending, they
//! map the obvious way; descending, they swap.
//!
//! Descending was measured as 12 relocations over 12 periods with **zero**
//! periods served, because relocating to the playhead and appending forward
//! lays the whole window out in the direction the playhead is leaving. So a
//! descending relocation restarts at `playhead - below_target()` instead,
//! which is what the `descending` flag below selects. Measured after the
//! change: 12 of 12 served, and 1 relocation instead of 12 once the window is
//! larger than the descent.
//!
//! **What this does not remove is one missed period per relocation.**
//! Relocating discards the ring, and a descending playhead's next input lies
//! at the *top* of the span about to be read — so it arrives last, and the
//! callback asks for it before it is there. Reading is roughly two orders of
//! magnitude faster than playback consumes, so the refill beats the next
//! period comfortably and the miss is one period, not a stall. Removing it
//! altogether would need the ring to accept writes *below* `start`, which is
//! a ring change and is out of scope here.

use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::time::Duration;

use crate::file::{OpenError, RING_CHANNELS, Track, TrackInfo};
use crate::ring::{self, RingReader, RingWriter};
use crate::sndfile::SndFileError;

/// Frames per libsndfile call. Large enough that per-call overhead is
/// irrelevant, small enough that one call cannot monopolise the thread while
/// a seek is waiting to be serviced. Not tuned against anything measured.
pub const CHUNK_FRAMES: usize = 8192;

/// How much input must stay resident *below* a descending playhead before the
/// window is rebuilt deeper.
///
/// It has to cover the most one output period can consume going backwards.
/// `architecture.md` fixes the period at 128-256 frames and `transport.rs`
/// fixes the seek rate at |r| = 4, so 1024 frames is the worst case; doubled
/// for v2's resampler, which holds input in a delay line beyond what it has
/// output. Small on purpose — it is subtracted from how far the playhead can
/// descend before the next relocation, so a large value would spend the
/// window on margin.
const REVERSE_MARGIN: u64 = 2048;

/// What one top-up pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Filled {
    /// Frames appended to the ring.
    pub frames: usize,
    /// The window was thrown away and restarted — a seek out of range, or the
    /// callback ran past the end of the window.
    pub relocated: bool,
    /// The track's last frame is resident; there is nothing left to read.
    pub at_end: bool,
}

/// Owns one track and the writing end of its ring.
pub struct Window {
    track: Track,
    writer: RingWriter,
    /// Reused across calls, so filling allocates nothing after construction —
    /// not because this thread may not allocate, but because a per-call
    /// allocation of this size would be pure waste.
    scratch: Vec<i32>,
    ahead_target: u64,
    behind_target: u64,
    /// The next track frame libsndfile will return. Kept in lockstep with the
    /// ring's `end`, and asserted to be.
    cursor: u64,
    at_end: bool,
    /// The playhead at the previous pass, for inferring the direction of
    /// travel. `None` until the first pass has seen one.
    last_playhead: Option<u64>,
    /// Latched, so it survives passes where the playhead has not moved —
    /// a pause mid-scrub must not silently re-bias the window forward.
    descending: bool,
}

impl Window {
    /// Opens a track and allocates its ring.
    ///
    /// The ring is per track because its capacity is a function of the
    /// track's sample rate, which is only known once the header is read. That
    /// is affordable for the same reason reopening the ALSA device per track
    /// is: one Pi is one deck, so nothing audible is interrupted.
    pub fn load(
        path: &Path,
        window_bytes: usize,
    ) -> Result<(Window, RingReader, TrackInfo), OpenError> {
        let track = Track::open(path)?;
        let info = track.info().clone();
        let capacity = ring::capacity_frames(info.rate, window_bytes) as u64;
        let (writer, reader) = ring::new(capacity as usize);

        // Split the window evenly: the "±" in the table is half each way.
        let behind_target = capacity / 2;
        let ahead_target = capacity - behind_target;

        Ok((
            Window {
                track,
                writer,
                scratch: vec![0i32; CHUNK_FRAMES * RING_CHANNELS],
                ahead_target,
                behind_target,
                cursor: 0,
                at_end: false,
                last_playhead: None,
                descending: false,
            },
            reader,
            info,
        ))
    }

    pub fn ahead_target(&self) -> u64 {
        self.ahead_target
    }

    pub fn behind_target(&self) -> u64 {
        self.behind_target
    }

    pub fn resident(&self) -> std::ops::Range<u64> {
        self.writer.resident()
    }

    /// True while the playhead is moving towards lower frame numbers — a
    /// reverse jog in v2, or a held REW in v1.
    pub fn descending(&self) -> bool {
        self.descending
    }

    /// Frames wanted above the playhead: the ahead half when ascending, the
    /// behind half when descending. See [`Window::below_target`].
    fn above_target(&self) -> u64 {
        if self.descending {
            self.behind_target
        } else {
            self.ahead_target
        }
    }

    /// [`REVERSE_MARGIN`], clamped to what the window can actually hold.
    ///
    /// Without the clamp a ring smaller than the margin could never satisfy
    /// the room-below test and would relocate on every pass forever.
    fn reverse_margin(&self) -> u64 {
        REVERSE_MARGIN.min(self.below_target())
    }

    /// Frames wanted below the playhead — the mirror of [`Window::above_target`].
    fn below_target(&self) -> u64 {
        if self.descending {
            self.ahead_target
        } else {
            self.behind_target
        }
    }

    /// Restarts the window at `frame`, discarding what is resident.
    ///
    /// Called for an explicit seek. A seek that lands *inside* the window
    /// needs none of this — the callback simply reads a different frame — so
    /// the engine should only reach for it when [`Window::fill_step`] reports
    /// it, or when it knows the target is out of range.
    ///
    /// **Clears the inferred direction of travel**, because an explicit seek
    /// is a discontinuity rather than motion: a cue jump backwards must not
    /// leave the window biased as though the playhead were scrubbing down.
    /// The automatic relocation inside [`Window::fill_step`] keeps the
    /// direction, which is what makes a held REW or a reverse jog work.
    pub fn relocate(&mut self, frame: u64) -> Result<(), SndFileError> {
        self.descending = false;
        self.last_playhead = None;
        self.restart_at(frame)
    }

    fn restart_at(&mut self, frame: u64) -> Result<(), SndFileError> {
        let at = self.track.seek(frame)?;
        self.writer.relocate(at);
        self.cursor = at;
        self.at_end = false;
        Ok(())
    }

    /// Updates the latched direction of travel from a new playhead value.
    ///
    /// An unchanged playhead leaves it alone: `fill_step` is polled far more
    /// often than the playhead moves, so treating "not descending this pass"
    /// as ascending would flip the bias back on the very next poll.
    fn observe(&mut self, playhead: u64) {
        if let Some(prev) = self.last_playhead {
            match playhead.cmp(&prev) {
                std::cmp::Ordering::Less => self.descending = true,
                std::cmp::Ordering::Greater => self.descending = false,
                std::cmp::Ordering::Equal => {}
            }
        }
        self.last_playhead = Some(playhead);
    }

    /// One top-up pass: trim what has fallen behind, then read forward until
    /// the window is full or the track ends.
    ///
    /// Returns without doing anything once the window is in its steady state,
    /// so the caller can poll this as often as it likes.
    pub fn fill_step(&mut self) -> Result<Filled, SndFileError> {
        let mut out = Filled::default();
        let playhead = self.writer.playhead();
        self.observe(playhead);
        let resident = self.writer.resident();

        // Relocate if the playhead is not somewhere this window can serve.
        //
        // `playhead == resident.end` is not a relocation while ascending: it
        // means the callback is right at the edge and appending forward will
        // serve it. Descending, that same position serves nothing, because
        // the next input lies *below* the playhead — so there has to be a
        // second test, for room underneath.
        let in_span = resident.start <= playhead && playhead <= resident.end;
        let below = self.reverse_margin();
        let room_below =
            !self.descending || resident.start == 0 || resident.start + below <= playhead;
        if !in_span || !room_below || resident.is_empty() {
            // Ascending, restart at the playhead: appending forward serves
            // the immediate need first and the behind half accumulates for
            // free. Descending, neither is true — restart below the playhead
            // so that what gets read is what the callback will ask for.
            let target = if self.descending {
                playhead.saturating_sub(self.below_target())
            } else {
                playhead
            };
            // **A relocation to where the window already is, on a track with
            // nothing left to read, is not a relocation** — and treating it as
            // one was a loop. `resident.is_empty()` is true at the end of
            // every track, because everything has been consumed; the restart
            // then seeks to the same place, reads zero, leaves the span empty,
            // and `run` sees `relocated` and clears the `EndOfTrack` latch, so
            // the event fires again on the very next pass. **Measured at 15
            // `EndOfTrack` events in 200 ms**, with a seek plus a read syscall
            // behind each one.
            //
            // Two ways in, and the second needs no seek at all: a playhead
            // sitting exactly at `frames`, and **loading a file with no audio
            // in it** — a header-only WAV, which is what an interrupted export
            // leaves behind.
            let pointless = self.at_end && target == self.cursor;
            if !pointless {
                self.restart_at(target)?;
                out.relocated = true;
            }
        }

        // Discard what has fallen out of the window on the side the playhead
        // came from. This is published before the slots are reused, which is
        // what lets the callback notice if it is reading one of them.
        self.writer
            .drop_before(playhead.saturating_sub(self.below_target()));

        let fill_to = playhead + self.above_target();
        while !self.at_end {
            let end = self.writer.resident().end;
            debug_assert_eq!(
                end, self.cursor,
                "the libsndfile cursor and the ring's end must advance together"
            );
            if end >= fill_to {
                break;
            }
            let room = self.writer.writable().min(fill_to - end).min(CHUNK_FRAMES as u64);
            if room == 0 {
                break;
            }
            let want = room as usize * RING_CHANNELS;
            let got = self.track.read_into_ring(&mut self.scratch[..want])?;
            if got == 0 {
                self.at_end = true;
                break;
            }
            self.writer.append(&self.scratch[..got * RING_CHANNELS]);
            self.cursor += got as u64;
            out.frames += got;
        }

        out.at_end = self.at_end;
        Ok(out)
    }

    /// True once the whole remaining track is resident.
    pub fn at_end_of_track(&self) -> bool {
        self.at_end
    }
}

/// What the engine can ask the window thread to do.
///
/// **Provisional.** The transport is not built yet, so this is the smallest
/// set that makes the thread real rather than a guess at the engine's control
/// protocol. Track loading is not here: a new track means a new ring, whose
/// reading end has to reach the callback, and that wiring belongs to the
/// engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Restart the window at this frame. Only needed for a seek that lands
    /// outside it.
    Relocate(u64),
    Shutdown,
}

/// What the window thread reports back. Nothing here is on a deadline, so an
/// ordinary channel is the right shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The whole remaining track is resident.
    EndOfTrack,
    /// Reading failed. The dominant cause is the stick being removed, and the
    /// audio side survives it by design: what is already resident keeps
    /// playing, for as long as the forward half of the window lasts.
    Failed(String),
    Stopped,
}

/// How long the thread waits for a command before topping up again.
///
/// A bound, not a period: the loop returns immediately from `fill_step` once
/// the window is full, so this is only how stale the trim can be.
const POLL: Duration = Duration::from_millis(10);

impl Window {
    /// Runs until told to shut down or until a read fails. Intended to be the
    /// body of a dedicated thread.
    ///
    /// Fills **before** waiting on the channel, and only blocks once a pass
    /// has made no progress. Waiting first would put [`POLL`] of latency in
    /// front of every track load and every relocation — a guaranteed dropout
    /// at exactly the two moments the window is empty.
    pub fn run(mut self, commands: Receiver<Command>, events: impl Fn(Event)) {
        // Latched, so reaching the end of the track is reported once rather
        // than on every poll for as long as the track sits there.
        let mut end_reported = false;
        loop {
            let idle = match self.fill_step() {
                Ok(filled) => {
                    if filled.relocated {
                        end_reported = false;
                    }
                    if filled.at_end && !end_reported {
                        end_reported = true;
                        events(Event::EndOfTrack);
                    }
                    filled.frames == 0
                }
                Err(e) => {
                    events(Event::Failed(e.to_string()));
                    return;
                }
            };

            // Only sleep when there is nothing to do. While the window is
            // still filling, commands are checked without blocking so a seek
            // is not held up behind a full refill either.
            let command = if idle {
                match commands.recv_timeout(POLL) {
                    Ok(c) => Some(c),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => Some(Command::Shutdown),
                }
            } else {
                match commands.try_recv() {
                    Ok(c) => Some(c),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => Some(Command::Shutdown),
                }
            };

            match command {
                Some(Command::Relocate(frame)) => {
                    if let Err(e) = self.relocate(frame) {
                        events(Event::Failed(e.to_string()));
                        return;
                    }
                    end_reported = false;
                }
                Some(Command::Shutdown) => {
                    events(Event::Stopped);
                    return;
                }
                None => {}
            }
        }
    }
}
