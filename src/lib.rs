//! deck-pi — bit-perfect single-deck DJ transport.
//!
//! One process, one binary: engine, browser and display. The only boundary
//! that constrains structure is the deadline, and exactly one thing has one —
//! the audio callback (docs/architecture.md).
//!
//! Built so far: the file layer and the FFI beneath it. Both sit on the side
//! of the line that is allowed to block.

pub mod file;
pub mod sndfile;
