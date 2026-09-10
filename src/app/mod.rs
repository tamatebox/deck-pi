//! The deck, as opposed to the bring-up CLI.
//!
//! `CLAUDE.md` says `src/main.rs` is not the deck: it reports what the file
//! layer makes of a path and can pull one file through the machinery. This is
//! where the modules are joined to each other instead.
//!
//! # Why any of it is here rather than in a binary
//!
//! Every defect this project has found in the last review was a **caller**
//! failing to hold up an obligation stated somewhere else: a miss treated as a
//! fault, a release that undid a Back Cue, a realtime setup applied on the
//! wrong thread, a cue keyed on the browser's selection. The app loop is that
//! caller. Putting it in a binary makes it the one piece nothing can test and
//! nothing else can reuse, so the obligations stay in prose and the prose is
//! not a barrier — which is the conclusion `docs/implementation.md` reaches
//! under *What reads as handled and is not*.
//!
//! So the loop is library code, and the two binaries under `src/bin` are thin.

pub mod audio;
