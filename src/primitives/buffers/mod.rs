//! # primitives::buffers
//!
//! Audio block buffers for the Aura DAW SDK.
//!
//! * [`PlanarBuffer`] — one contiguous `f32` slice per channel. This is the
//!   native layout of the process graph: DSP code operates on plain slices.
//! * [`InterleavedBuffer`] — frames of `[ch0, ch1, ...]` as used by most
//!   device APIs (cpal, ALSA, CoreAudio).
//! * [`interleave`] / [`deinterleave`] — conversions between the two.
//! * [`convert`] — integer/float sample format conversion via `dasp_sample`.
//! * [`gain`] — decibel helpers.
//!
//! Every buffer is allocated once with a *capacity* in frames and can then be
//! resized *down* for smaller blocks without allocating, which is what a
//! real-time callback needs.
//!
//! ```
//! use aura::primitives::buffers::{PlanarBuffer, InterleavedBuffer, interleave};
//!
//! let mut block = PlanarBuffer::new(2, 4);
//! block.channel_mut(0).copy_from_slice(&[1.0, 2.0, 3.0, 4.0]);
//! block.channel_mut(1).copy_from_slice(&[-1.0, -2.0, -3.0, -4.0]);
//!
//! let mut out = InterleavedBuffer::new(2, 4);
//! interleave(&block, &mut out);
//! assert_eq!(out.samples(), &[1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 4.0, -4.0]);
//!
//! block.apply_gain(0.5);
//! assert_eq!(block.channel(0)[3], 2.0);
//! assert!(!block.is_silent());
//! block.clear();
//! assert!(block.is_silent());
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod convert;
pub mod gain;
mod interleaved;
mod layout;
mod planar;

pub use interleaved::InterleavedBuffer;
pub use layout::ChannelLayout;
pub use planar::PlanarBuffer;

use crate::primitives::{AuraError, Result};

/// Copies a planar block into an interleaved one.
///
/// Both buffers must have the same channel count and frame count.
pub fn interleave(src: &PlanarBuffer, dst: &mut InterleavedBuffer) {
    assert_eq!(src.channels(), dst.channels(), "channel count mismatch");
    assert_eq!(src.frames(), dst.frames(), "frame count mismatch");
    let channels = src.channels();
    let out = dst.samples_mut();
    for (ch, plane) in src.planes().enumerate() {
        for (frame, &sample) in plane.iter().enumerate() {
            out[frame * channels + ch] = sample;
        }
    }
}

/// Copies an interleaved block into a planar one.
///
/// Both buffers must have the same channel count and frame count.
pub fn deinterleave(src: &InterleavedBuffer, dst: &mut PlanarBuffer) {
    assert_eq!(src.channels(), dst.channels(), "channel count mismatch");
    assert_eq!(src.frames(), dst.frames(), "frame count mismatch");
    let channels = src.channels();
    let input = src.samples();
    for (ch, plane) in dst.planes_mut().enumerate() {
        for (frame, out) in plane.iter_mut().enumerate() {
            *out = input[frame * channels + ch];
        }
    }
}

/// Interleaves raw channel slices into `dst`.
///
/// `dst.len()` must equal `planes.len() * frames`, where `frames` is the
/// length of every plane. Returns an error rather than panicking so device
/// callbacks can fail soft.
pub fn interleave_slices(planes: &[&[f32]], dst: &mut [f32]) -> Result<()> {
    let channels = planes.len();
    if channels == 0 {
        return Ok(());
    }
    let frames = planes[0].len();
    for p in planes {
        if p.len() != frames {
            return Err(AuraError::FrameMismatch {
                expected: frames,
                actual: p.len(),
            });
        }
    }
    if dst.len() != channels * frames {
        return Err(AuraError::FrameMismatch {
            expected: channels * frames,
            actual: dst.len(),
        });
    }
    for (ch, plane) in planes.iter().enumerate() {
        for (frame, &sample) in plane.iter().enumerate() {
            dst[frame * channels + ch] = sample;
        }
    }
    Ok(())
}

/// De-interleaves `src` (with `channels` interleaved channels) into a planar buffer.
///
/// `dst` is resized (within its capacity) to `src.len() / channels` frames.
pub fn deinterleave_slice(src: &[f32], channels: usize, dst: &mut PlanarBuffer) -> Result<()> {
    if channels == 0 || src.len() % channels != 0 {
        return Err(AuraError::InvalidArgument {
            what: "interleaved length not divisible by channels",
        });
    }
    if dst.channels() != channels {
        return Err(AuraError::ChannelMismatch {
            expected: dst.channels(),
            actual: channels,
        });
    }
    let frames = src.len() / channels;
    dst.set_frames(frames)?;
    for (ch, plane) in dst.planes_mut().enumerate() {
        for (frame, out) in plane.iter_mut().enumerate() {
            *out = src[frame * channels + ch];
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleave_round_trip_preserves_every_sample() {
        let mut planar = PlanarBuffer::new(3, 5);
        for ch in 0..3 {
            for (i, s) in planar.channel_mut(ch).iter_mut().enumerate() {
                *s = (ch * 100 + i) as f32;
            }
        }
        let mut inter = InterleavedBuffer::new(3, 5);
        interleave(&planar, &mut inter);
        // Golden vector: frame-major ordering.
        assert_eq!(
            &inter.samples()[..6],
            &[0.0, 100.0, 200.0, 1.0, 101.0, 201.0]
        );

        let mut back = PlanarBuffer::new(3, 5);
        deinterleave(&inter, &mut back);
        for ch in 0..3 {
            assert_eq!(planar.channel(ch), back.channel(ch));
        }
    }

    #[test]
    fn slice_variants_validate_lengths() {
        let a = [1.0, 2.0];
        let b = [3.0, 4.0];
        let mut out = [0.0; 4];
        interleave_slices(&[&a, &b], &mut out).unwrap();
        assert_eq!(out, [1.0, 3.0, 2.0, 4.0]);

        let mut short = [0.0; 3];
        assert!(interleave_slices(&[&a, &b], &mut short).is_err());
        let c = [9.0];
        assert!(interleave_slices(&[&a, &c], &mut out).is_err());
        assert!(interleave_slices(&[], &mut []).is_ok());

        let mut planar = PlanarBuffer::new(2, 8);
        deinterleave_slice(&out, 2, &mut planar).unwrap();
        assert_eq!(planar.frames(), 2);
        assert_eq!(planar.channel(0), &[1.0, 2.0]);
        assert_eq!(planar.channel(1), &[3.0, 4.0]);
        assert!(deinterleave_slice(&out[..3], 2, &mut planar).is_err());
        assert!(deinterleave_slice(&out, 3, &mut planar).is_err());
        // more frames than capacity
        let big = [0.0; 32];
        assert!(deinterleave_slice(&big, 2, &mut planar).is_err());
    }
}
