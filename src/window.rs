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
//! fall more than `behind_target` back. So the steady state is the ± window
//! the table describes, reached by only ever appending.
//!
//! The one case it does not cover is the instant after a cold seek, when the
//! behind half is empty. That is not a gap in the design — `architecture.md`
//! already states the cost of scrubbing past the window's edge as "an
//! `sf_seek` and a refill", which is exactly what happens. Pre-locking cue
//! regions is the separate mechanism for making a *cold seek* not stall.

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

    /// Restarts the window at `frame`, discarding what is resident.
    ///
    /// Called for an explicit seek. A seek that lands *inside* the window
    /// needs none of this — the callback simply reads a different frame — so
    /// the engine should only reach for it when [`fill_step`] reports it, or
    /// when it knows the target is out of range.
    pub fn relocate(&mut self, frame: u64) -> Result<(), SndFileError> {
        let at = self.track.seek(frame)?;
        self.writer.relocate(at);
        self.cursor = at;
        self.at_end = false;
        Ok(())
    }

    /// One top-up pass: trim what has fallen behind, then read forward until
    /// the window is full or the track ends.
    ///
    /// Returns without doing anything once the window is in its steady state,
    /// so the caller can poll this as often as it likes.
    pub fn fill_step(&mut self) -> Result<Filled, SndFileError> {
        let mut out = Filled::default();
        let playhead = self.writer.playhead();
        let resident = self.writer.resident();

        // Relocate if the playhead is not somewhere this window can reach.
        // `playhead == resident.end` is not a relocation: it means the
        // callback is right at the edge and filling forward will serve it.
        let contiguous = resident.start <= playhead && playhead <= resident.end;
        if !contiguous || resident.is_empty() {
            self.relocate(playhead)?;
            out.relocated = true;
        }

        // Discard what has fallen more than the behind window back. This is
        // published before the slots are reused, which is what lets the
        // callback notice if it is reading one of them.
        self.writer
            .drop_before(playhead.saturating_sub(self.behind_target));

        let fill_to = playhead + self.ahead_target;
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
