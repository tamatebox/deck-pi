//! A safe handle over one open libsndfile file.
//!
//! This runs in the window thread when filling the ring and in the browser
//! when reading a header (docs/architecture.md, "Shape of the program"). It
//! may block, allocate and lock; it must never be reached from the audio
//! callback.

pub mod ffi;

use std::ffi::{CStr, CString};
use std::fmt;
use std::path::Path;

pub use ffi::SF_INFO;

/// An open file. Closed on drop.
pub struct SndFile {
    handle: *mut ffi::SNDFILE,
    info: SF_INFO,
}

// One handle is used by one thread at a time — the browser's or the window
// thread's. `SNDFILE` carries its own unsynchronised read cursor, so this is
// deliberately `Send` and deliberately **not** `Sync`.
unsafe impl Send for SndFile {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SndFileError {
    pub message: String,
}

impl fmt::Display for SndFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SndFileError {}

fn last_error(handle: *mut ffi::SNDFILE) -> SndFileError {
    // SAFETY: sf_strerror accepts null (meaning "the last error with no
    // handle"), and returns a static, NUL-terminated string owned by the
    // library.
    let msg = unsafe {
        let p = ffi::sf_strerror(handle);
        if p.is_null() {
            "libsndfile reported no error text".to_string()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    };
    SndFileError { message: msg }
}

impl SndFile {
    /// Opens for reading and returns the header libsndfile parsed. Reads no
    /// audio: this is the call the browser makes on the highlighted row.
    pub fn open(path: &Path) -> Result<Self, SndFileError> {
        // A path with an interior NUL cannot exist on either filesystem, so
        // this is a rejection rather than an I/O error.
        let c_path = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| SndFileError {
            message: "path contains an interior NUL byte".to_string(),
        })?;

        let mut info = SF_INFO::default();
        // SAFETY: `c_path` is NUL-terminated and outlives the call; `info` is
        // a valid, correctly-laid-out SF_INFO for the library to write.
        let handle = unsafe { ffi::sf_open(c_path.as_ptr(), ffi::SFM_READ, &mut info) };
        if handle.is_null() {
            return Err(last_error(std::ptr::null_mut()));
        }
        Ok(SndFile { handle, info })
    }

    pub fn info(&self) -> &SF_INFO {
        &self.info
    }

    /// Reads up to `frames` frames as left-justified int32, interleaved.
    /// `buf` must hold `frames * channels` samples.
    ///
    /// A short read is not an error — it is the end of the track. **A short
    /// read with an error set on the handle is the medium going away**, and
    /// telling those two apart is the whole reason `sf_error` is called
    /// here. `sf_readf_int` returns the frames it managed in both cases, so
    /// the return value cannot distinguish them; an earlier version tested
    /// `got < 0`, which libsndfile never returns from a read, and a pulled
    /// stick therefore arrived at the caller as a clean end of file.
    pub fn read_frames(&mut self, buf: &mut [i32], frames: i64) -> Result<i64, SndFileError> {
        let wanted = frames
            .checked_mul(self.info.channels as i64)
            .expect("frame count times channel count overflows i64");
        assert!(
            buf.len() as i64 >= wanted,
            "buffer holds {} samples, {} frames of {}ch needs {}",
            buf.len(),
            frames,
            self.info.channels,
            wanted
        );
        // SAFETY: the handle is non-null and open; `buf` has room for
        // `frames * channels` ints, asserted above.
        let got = unsafe { ffi::sf_readf_int(self.handle, buf.as_mut_ptr(), frames) };
        // Immediately, and before anything else touches the handle: an
        // `sf_seek` clears the error on its way through, so a check deferred
        // past one reports a healthy file for a stick that has been pulled.
        // SAFETY: the handle is non-null and open.
        let err = unsafe { ffi::sf_error(self.handle) };
        if err != ffi::SF_ERR_NO_ERROR {
            return Err(last_error(self.handle));
        }
        Ok(got)
    }

    /// Absolute seek, in frames. Returns the resulting frame offset.
    pub fn seek(&mut self, frame: i64) -> Result<i64, SndFileError> {
        // SAFETY: handle is non-null and open.
        let at = unsafe { ffi::sf_seek(self.handle, frame, ffi::SF_SEEK_SET) };
        if at < 0 {
            return Err(last_error(self.handle));
        }
        Ok(at)
    }
}

impl Drop for SndFile {
    fn drop(&mut self) {
        // The return is **deliberately** discarded, and this line says so
        // because a silently ignored libsndfile return is exactly the shape
        // of the read bug above. `sf_close` reports a failure to flush, and
        // handles here are opened `SFM_READ` only, so there is nothing to
        // flush and nothing it can tell us that we could act on inside a
        // `Drop`.
        //
        // SAFETY: handle is non-null, open, and never closed twice — `Drop`
        // runs once and nothing else calls `sf_close`.
        let _ = unsafe { ffi::sf_close(self.handle) };
    }
}
