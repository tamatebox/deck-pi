//! Raw libsndfile declarations.
//!
//! Hand-written rather than taken from a binding crate: the surface needed is
//! small enough to own and audit, and the published crates stopped moving in
//! 2021 (docs/implementation.md, docs/decisions.md).
//!
//! Every declaration and constant here was read out of `sndfile.h` 1.2.2.
//! `sf_count_t` is `int64_t` unconditionally, so the 64-bit offsets that let
//! multi-GB RF64 files work do not depend on the userspace bit width.

#![allow(non_camel_case_types)]

use std::os::raw::{c_char, c_int};

pub type sf_count_t = i64;

/// `struct SF_INFO`. Field order and types are load-bearing — this is
/// written by `sf_open` through a pointer.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SF_INFO {
    pub frames: sf_count_t,
    pub samplerate: c_int,
    pub channels: c_int,
    pub format: c_int,
    pub sections: c_int,
    pub seekable: c_int,
}

/// Opaque `SNDFILE`. Never constructed on this side.
#[repr(C)]
pub struct SNDFILE {
    _opaque: [u8; 0],
}

// Open modes.
pub const SFM_READ: c_int = 0x10;

// Masks.
pub const SF_FORMAT_TYPEMASK: c_int = 0x0FFF_0000;
pub const SF_FORMAT_SUBMASK: c_int = 0x0000_FFFF;

// Major (container) types this deck accepts.
pub const SF_FORMAT_WAV: c_int = 0x01_0000;
pub const SF_FORMAT_AIFF: c_int = 0x02_0000;
pub const SF_FORMAT_W64: c_int = 0x0B_0000;
pub const SF_FORMAT_WAVEX: c_int = 0x13_0000;
pub const SF_FORMAT_RF64: c_int = 0x22_0000;

// Minor (sample format) types. Only the first two are in scope; the rest are
// declared so a rejection can name what it actually found.
pub const SF_FORMAT_PCM_16: c_int = 0x0002;
pub const SF_FORMAT_PCM_24: c_int = 0x0003;
pub const SF_FORMAT_PCM_S8: c_int = 0x0001;
pub const SF_FORMAT_PCM_32: c_int = 0x0004;
pub const SF_FORMAT_PCM_U8: c_int = 0x0005;
pub const SF_FORMAT_FLOAT: c_int = 0x0006;
pub const SF_FORMAT_DOUBLE: c_int = 0x0007;

// `whence` for sf_seek. Same values as the C library's SEEK_*.
pub const SF_SEEK_SET: c_int = 0;
pub const SF_SEEK_CUR: c_int = 1;
pub const SF_SEEK_END: c_int = 2;

extern "C" {
    pub fn sf_open(path: *const c_char, mode: c_int, sfinfo: *mut SF_INFO) -> *mut SNDFILE;
    pub fn sf_close(sndfile: *mut SNDFILE) -> c_int;
    pub fn sf_seek(sndfile: *mut SNDFILE, frames: sf_count_t, whence: c_int) -> sf_count_t;

    /// Reads frames as int32, **left-justified**: the source's most
    /// significant bit lands on the destination's most significant bit. So
    /// int16 arrives as `v << 16` and int24 as `v << 8`, and the big-endian
    /// AIFF byte swap and the 24-bit unpack both happen inside this call.
    pub fn sf_readf_int(sndfile: *mut SNDFILE, ptr: *mut c_int, frames: sf_count_t) -> sf_count_t;

    /// Passing null asks for the last error not attached to a handle — which
    /// is what a failed `sf_open` leaves behind.
    pub fn sf_strerror(sndfile: *mut SNDFILE) -> *const c_char;

    /// The error number on a handle, `SF_ERR_NO_ERROR` when clean.
    ///
    /// **This is the only way to see a read failure.** `sf_readf_int` does
    /// not report one: a failed `read(2)` is logged into the handle and the
    /// call returns the frames it managed, which is usually zero — the same
    /// value a clean end of file returns. Checking the return value for a
    /// negative count, as this FFI once did, tests for something the library
    /// never produces.
    ///
    /// **Two facts, from `VALIDATE_SNDFILE_AND_ASSIGN_PSF`'s third argument
    /// in `src/sndfile.c`**, which decides whether an entry point clears
    /// `psf->error` on the way in. Read the macro rather than re-deriving
    /// either of these from an experiment:
    ///
    /// - The read and seek families both pass **1**, so they clear. A check
    ///   deferred past a later `sf_seek` therefore reports "No Error" for a
    ///   medium that has gone — read it *immediately*.
    /// - `sf_readf_int` clears on entry too, which is the more useful half:
    ///   an error seen after a read was necessarily set **by that read**. It
    ///   cannot be inherited from `sf_open`, from an earlier seek, or from
    ///   any prior history — so checking it here cannot turn a healthy track
    ///   into a failure, which is the way this fix could have inverted the
    ///   bug rather than removed it.
    ///
    /// `sf_error` and `sf_strerror` both pass **0**, so asking does not
    /// destroy the answer and the message still matches the code. Had
    /// `sf_error` cleared, `last_error` would report "No Error" for every
    /// failure.
    ///
    /// Sourced from libsndfile `master`; the behaviour was confirmed
    /// empirically against the installed **1.2.2**. Two sources for one
    /// claim, not one source twice — the macro is longstanding, but if this
    /// ever surprises you, read the tag you are linking against.
    pub fn sf_error(sndfile: *mut SNDFILE) -> c_int;
}

/// `sf_error`'s clean value.
pub const SF_ERR_NO_ERROR: c_int = 0;

#[cfg(test)]
mod tests {
    use super::*;

    /// If this ever fails, `sf_open` is writing outside the struct.
    #[test]
    fn sf_info_matches_c_layout() {
        assert_eq!(std::mem::size_of::<SF_INFO>(), 32);
        assert_eq!(std::mem::align_of::<SF_INFO>(), 8);
        let i = SF_INFO::default();
        let base = &i as *const _ as usize;
        assert_eq!(&i.frames as *const _ as usize - base, 0);
        assert_eq!(&i.samplerate as *const _ as usize - base, 8);
        assert_eq!(&i.channels as *const _ as usize - base, 12);
        assert_eq!(&i.format as *const _ as usize - base, 16);
        assert_eq!(&i.sections as *const _ as usize - base, 20);
        assert_eq!(&i.seekable as *const _ as usize - base, 24);
    }
}
