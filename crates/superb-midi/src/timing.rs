//! Mapping between microsecond timestamps and sample-accurate frame offsets.
//!
//! Hardware MIDI input arrives on its own thread stamped with a free-running
//! microsecond clock ([`crate::TimestampedMessage`]). The audio callback
//! instead needs a sample offset *within the block it is currently
//! processing* ([`crate::MidiEvent::frame`]). [`FrameMapper`] bridges the
//! two: call [`FrameMapper::set_block`] once per process callback with the
//! block's start time and length, then [`FrameMapper::frame_for`] for every
//! pending input message.

use superb_core::SampleRate;

/// Converts a duration in microseconds to a (floored) number of samples at
/// `rate`.
pub fn us_to_frames(us: u64, rate: SampleRate) -> u64 {
    let hz = rate.hz() as u128;
    ((us as u128) * hz / 1_000_000) as u64
}

/// Maps microsecond timestamps to sample offsets within the process block
/// currently being rendered.
///
/// The mapper holds no heap data and is cheap to keep as a field on an audio
/// callback's state; call [`FrameMapper::set_block`] at the top of every
/// `process()` call.
#[derive(Clone, Copy, Debug)]
pub struct FrameMapper {
    sample_rate: SampleRate,
    block_start_us: u64,
    frames: u32,
}

impl FrameMapper {
    /// Creates a mapper for the given sample rate. The block is initially
    /// empty (`frames == 0`); call [`FrameMapper::set_block`] before use.
    pub fn new(sample_rate: SampleRate) -> Self {
        Self { sample_rate, block_start_us: 0, frames: 0 }
    }

    /// Configures the current process block: `block_start_us` is the
    /// timestamp (on the same clock as incoming [`crate::TimestampedMessage`]s)
    /// of the block's first frame, and `frames` is the block length.
    pub fn set_block(&mut self, block_start_us: u64, frames: u32) {
        self.block_start_us = block_start_us;
        self.frames = frames;
    }

    /// Maps `timestamp_us` to a frame offset within the current block,
    /// clamped to `0..frames`.
    ///
    /// Timestamps at or before the block start map to `0`; timestamps at or
    /// after the block end map to `frames - 1`. An empty block (`frames == 0`)
    /// always maps to `0`.
    pub fn frame_for(&self, timestamp_us: u64) -> u32 {
        if self.frames == 0 {
            return 0;
        }
        if timestamp_us <= self.block_start_us {
            return 0;
        }
        let elapsed_us = timestamp_us - self.block_start_us;
        let frame = us_to_frames(elapsed_us, self.sample_rate);
        let max_frame = (self.frames - 1) as u64;
        frame.min(max_frame) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn us_to_frames_is_proportional() {
        let rate = SampleRate::new(48_000).unwrap();
        assert_eq!(us_to_frames(0, rate), 0);
        assert_eq!(us_to_frames(1_000_000, rate), 48_000);
        assert_eq!(us_to_frames(500_000, rate), 24_000);
        // 1 sample period at 48k is ~20.83us; floors within a sample.
        assert_eq!(us_to_frames(20, rate), 0);
        assert_eq!(us_to_frames(21, rate), 1);
    }

    #[test]
    fn clamps_before_block_start_to_zero() {
        let mut mapper = FrameMapper::new(SampleRate::new(48_000).unwrap());
        mapper.set_block(1_000_000, 512);
        assert_eq!(mapper.frame_for(0), 0);
        assert_eq!(mapper.frame_for(999_999), 0);
        assert_eq!(mapper.frame_for(1_000_000), 0);
    }

    #[test]
    fn clamps_after_block_end_to_last_frame() {
        let rate = SampleRate::new(48_000).unwrap();
        let mut mapper = FrameMapper::new(rate);
        let frames = 512u32;
        mapper.set_block(1_000_000, frames);
        // Block duration in us for 512 frames at 48k = 512/48000 s ~= 10666.67us
        let far_future = 1_000_000 + 1_000_000; // 1 second later, way past block end
        assert_eq!(mapper.frame_for(far_future), frames - 1);
    }

    #[test]
    fn maps_proportionally_within_block() {
        let rate = SampleRate::new(48_000).unwrap();
        let mut mapper = FrameMapper::new(rate);
        mapper.set_block(0, 480); // 480 frames = 10ms at 48k
        // Halfway through the block (5ms = 5000us) should land near frame 240.
        assert_eq!(mapper.frame_for(5_000), 240);
        // Exactly 1 sample period in.
        assert_eq!(mapper.frame_for(21), 1); // 1_000_000/48_000 ~= 20.83us per sample
    }

    #[test]
    fn empty_block_always_maps_to_zero() {
        let mapper = FrameMapper::new(SampleRate::new(48_000).unwrap());
        assert_eq!(mapper.frame_for(0), 0);
        assert_eq!(mapper.frame_for(1_000_000), 0);
    }
}
