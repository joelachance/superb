//! Real audio devices via `cpal` (feature `devices`).
//!
//! * [`output_devices`] / [`OutputStream`] / [`run_engine`] — playback.
//! * [`input_devices`] / [`InputStream`] / [`run_capture`] — capture.
//! * [`capture_queue`] — pre-allocated planar-block SPSC (free + filled)
//!   so a capture callback can hand blocks to another thread without
//!   allocating.
//!
//! The callback thread is marked real-time with `superb_rt::thread::enter_rt`.
//! Sample format conversion uses `cpal`'s `FromSample` (no allocation). Scratch
//! [`PlanarBuffer`]s are allocated when the stream opens and reused every
//! callback; active frames are adjusted with `set_frames`.
//!
//! # MIDI / capture clocks
//!
//! Output callbacks stamp MIDI via cpal's `OutputCallbackInfo::timestamp().callback`
//! (see [`OutputStream::open_timed`]). Input callbacks stamp blocks with
//! `InputCallbackInfo::timestamp().capture` — the ADC capture instant — via
//! [`InputStream::open_timed`]. cpal stream clocks and midir's connection clock
//! are different epochs; see the crate docs for the MIDI caveat.

use cpal::{FromSample, Sample};
use superb_buffers::PlanarBuffer;
use superb_core::{Result, SuperbError};

mod capture;
mod input;
mod output;

pub use capture::{capture_queue, CaptureBlock, CaptureConsumer, CaptureProducer};
pub use input::{input_devices, run_capture, InputConfig, InputStream};
pub use output::{output_devices, run_engine, OutputConfig, OutputStream};

/// Description of an input or output device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Human readable device name (pass to [`OutputConfig::device_name`] /
    /// [`InputConfig::device_name`]).
    pub name: String,
    /// `true` for the host's default device of that direction.
    pub is_default: bool,
    /// Highest channel count any supported config advertises (0 if unknown).
    pub max_channels: usize,
}

/// Microseconds since the stream's epoch for a cpal `StreamInstant`
/// (saturating at `u64::MAX`).
pub(crate) fn stream_instant_us(instant: cpal::StreamInstant) -> u64 {
    (instant.as_nanos() / 1_000).min(u64::MAX as u128) as u64
}

/// Shared validation for stream configs.
pub(crate) fn validate_io_config(
    channels: usize,
    max_block_frames: usize,
    what: &'static str,
) -> Result<()> {
    if channels == 0 || max_block_frames == 0 {
        return Err(SuperbError::InvalidArgument { what });
    }
    Ok(())
}

/// Copies interleaved device samples into a planar scratch (no allocation).
pub(crate) fn deinterleave_from_device<T>(data: &[T], channels: usize, planar: &mut PlanarBuffer)
where
    T: Sample,
    f32: FromSample<T>,
{
    let frames = planar.frames();
    debug_assert_eq!(data.len(), frames * channels);
    for ch in 0..channels {
        let plane = planar.channel_mut(ch);
        for (frame, sample) in plane.iter_mut().enumerate() {
            *sample = data[frame * channels + ch].to_sample::<f32>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_instant_converts_to_microseconds() {
        assert_eq!(
            stream_instant_us(cpal::StreamInstant::from_micros(1_234_567)),
            1_234_567
        );
        assert_eq!(stream_instant_us(cpal::StreamInstant::from_nanos(2_500)), 2);
        assert_eq!(stream_instant_us(cpal::StreamInstant::new(0, 0)), 0);
    }

    #[test]
    fn deinterleave_from_device_matches_planar_layout() {
        let interleaved = [0.0f32, 1.0, 2.0, 3.0, 4.0, 5.0];
        let mut planar = PlanarBuffer::new(2, 3);
        deinterleave_from_device(&interleaved, 2, &mut planar);
        assert_eq!(planar.channel(0), &[0.0, 2.0, 4.0]);
        assert_eq!(planar.channel(1), &[1.0, 3.0, 5.0]);
    }
}
