//! The null test's collector, as a sink.
//!
//! `implementation.md` defines the software half of the null test as "play a
//! file, collect the buffers handed to ALSA, and check them against the
//! source". This is that collector, standing where ALSA stands, so the test
//! exercises the real path rather than a parallel one — and it works on a
//! machine with no sound card at all.

use super::{AudioSink, SampleFormat, SinkError, SinkParams, SINK_CHANNELS};

pub struct CaptureSink {
    params: SinkParams,
    /// Pre-allocated to its full size. Writes never grow it, so
    /// [`write_period`](CaptureSink::write_period) allocates nothing and the
    /// whole chain — ring, callback and sink — can be proven allocation-free
    /// with `assert_no_alloc`. A write past the end is an error rather than a
    /// reallocation, for the same reason.
    captured: Vec<i32>,
    filled: usize,
    periods_written: u64,
    drained: bool,
}

impl CaptureSink {
    /// `capacity_frames` is the most it will ever accept.
    pub fn new(rate: u32, period_frames: usize, capacity_frames: usize) -> Self {
        CaptureSink {
            params: SinkParams {
                rate,
                channels: SINK_CHANNELS as u32,
                format: SampleFormat::S24Le,
                period_frames,
                // Two, matching the low end of architecture.md's 2-3. Nothing
                // here is buffered in hardware, so this is only what the
                // params report.
                periods: 2,
            },
            captured: vec![0i32; capacity_frames * SINK_CHANNELS],
            filled: 0,
            periods_written: 0,
            drained: false,
        }
    }

    /// Everything handed to the sink, in order.
    pub fn captured(&self) -> &[i32] {
        &self.captured[..self.filled]
    }

    pub fn frames_written(&self) -> usize {
        self.filled / SINK_CHANNELS
    }

    pub fn periods_written(&self) -> u64 {
        self.periods_written
    }

    pub fn was_drained(&self) -> bool {
        self.drained
    }

    /// Forgets what was captured without releasing the allocation, so a long
    /// run can be checked in chunks and still allocate nothing.
    pub fn reset(&mut self) {
        self.filled = 0;
        self.periods_written = 0;
        self.drained = false;
    }
}

impl AudioSink for CaptureSink {
    fn params(&self) -> SinkParams {
        self.params
    }

    fn write_period(&mut self, period: &[i32]) -> Result<(), SinkError> {
        if period.len() % SINK_CHANNELS != 0 {
            return Err(SinkError::WrongPeriodLength {
                given: period.len(),
                expected: self.params.period_frames * SINK_CHANNELS,
            });
        }
        let end = self.filled + period.len();
        if end > self.captured.len() {
            // Deliberately not a realloc: growing here would allocate on the
            // audio thread, which is the one thing this sink exists to keep
            // provable.
            return Err(SinkError::Device(
                "capture buffer full — construct it with more capacity".into(),
            ));
        }
        self.captured[self.filled..end].copy_from_slice(period);
        self.filled = end;
        self.periods_written += 1;
        Ok(())
    }

    fn drain(&mut self) -> Result<(), SinkError> {
        self.drained = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_keeps_every_period_in_order() {
        let mut s = CaptureSink::new(44_100, 4, 16);
        s.write_period(&[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        s.write_period(&[9, 10]).unwrap();
        assert_eq!(s.captured(), &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(s.frames_written(), 5);
        assert_eq!(s.periods_written(), 2);
    }

    #[test]
    fn a_half_frame_write_is_refused() {
        let mut s = CaptureSink::new(44_100, 4, 16);
        assert!(matches!(
            s.write_period(&[1, 2, 3]),
            Err(SinkError::WrongPeriodLength { .. })
        ));
    }

    #[test]
    fn overflow_is_an_error_rather_than_a_reallocation() {
        // If this ever grew the Vec instead, every allocation-freedom test
        // that runs a sink would start passing for the wrong reason.
        let mut s = CaptureSink::new(44_100, 2, 2);
        s.write_period(&[1, 2, 3, 4]).unwrap();
        assert!(s.write_period(&[5, 6]).is_err());
        assert_eq!(s.frames_written(), 2, "the failed write must not be kept");
    }

    #[test]
    fn reset_keeps_the_allocation() {
        let mut s = CaptureSink::new(44_100, 2, 4);
        let before = s.captured.as_ptr();
        s.write_period(&[1, 2, 3, 4]).unwrap();
        s.reset();
        assert_eq!(s.frames_written(), 0);
        assert_eq!(s.captured.as_ptr(), before, "reset must not reallocate");
        s.write_period(&[9, 9, 9, 9]).unwrap();
        assert_eq!(s.captured(), &[9, 9, 9, 9]);
    }
}
