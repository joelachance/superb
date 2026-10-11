//! Interleaved audio block.

use crate::primitives::{AuraError, Result};

/// A block of audio stored frame-major: `[ch0, ch1, ..., ch0, ch1, ...]`.
///
/// This is the layout most device APIs expose. The process graph uses
/// [`PlanarBuffer`](crate::primitives::buffers::PlanarBuffer); convert at the device boundary with
/// [`interleave`](crate::primitives::buffers::interleave) / [`deinterleave`](crate::primitives::buffers::deinterleave).
#[derive(Clone, Debug)]
pub struct InterleavedBuffer {
    data: Vec<f32>,
    channels: usize,
    capacity: usize,
    frames: usize,
}

impl InterleavedBuffer {
    /// Allocates a silent interleaved buffer for `capacity` frames.
    pub fn new(channels: usize, capacity: usize) -> Self {
        Self {
            data: vec![0.0; channels * capacity],
            channels,
            capacity,
            frames: capacity,
        }
    }

    /// Wraps an existing interleaved vector. `data.len()` must be divisible by `channels`.
    pub fn from_vec(channels: usize, data: Vec<f32>) -> Result<Self> {
        if channels == 0 || data.len() % channels != 0 {
            return Err(AuraError::InvalidArgument {
                what: "interleaved length not divisible by channels",
            });
        }
        let frames = data.len() / channels;
        Ok(Self {
            data,
            channels,
            capacity: frames,
            frames,
        })
    }

    /// Number of channels.
    #[inline]
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Active frame count.
    #[inline]
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Maximum frame count without reallocation.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Sets the active frame count without reallocating.
    pub fn set_frames(&mut self, frames: usize) -> Result<()> {
        if frames > self.capacity {
            return Err(AuraError::CapacityExceeded {
                what: "InterleavedBuffer frames",
                capacity: self.capacity,
            });
        }
        self.frames = frames;
        Ok(())
    }

    /// All active samples, frame-major.
    #[inline]
    pub fn samples(&self) -> &[f32] {
        &self.data[..self.frames * self.channels]
    }

    /// Mutable access to all active samples, frame-major.
    #[inline]
    pub fn samples_mut(&mut self) -> &mut [f32] {
        let n = self.frames * self.channels;
        &mut self.data[..n]
    }

    /// One frame (all channels at a single time index).
    #[inline]
    pub fn frame(&self, index: usize) -> &[f32] {
        assert!(index < self.frames, "frame out of range");
        &self.data[index * self.channels..(index + 1) * self.channels]
    }

    /// Mutable access to one frame.
    #[inline]
    pub fn frame_mut(&mut self, index: usize) -> &mut [f32] {
        assert!(index < self.frames, "frame out of range");
        let c = self.channels;
        &mut self.data[index * c..(index + 1) * c]
    }

    /// Iterates over frames.
    pub fn frames_iter(&self) -> impl Iterator<Item = &[f32]> + '_ {
        self.samples().chunks_exact(self.channels.max(1))
    }

    /// Reads a single sample.
    #[inline]
    pub fn sample(&self, frame: usize, channel: usize) -> f32 {
        self.frame(frame)[channel]
    }

    /// Fills every active sample with zero.
    pub fn clear(&mut self) {
        self.samples_mut().fill(0.0);
    }

    /// `true` if all active samples are exactly zero.
    pub fn is_silent(&self) -> bool {
        self.samples().iter().all(|&s| s == 0.0)
    }

    /// Multiplies all samples by `gain`.
    pub fn apply_gain(&mut self, gain: f32) {
        for s in self.samples_mut() {
            *s *= gain;
        }
    }

    /// Consumes the buffer and returns the underlying vector (full capacity).
    pub fn into_vec(self) -> Vec<f32> {
        self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_indexing() {
        let b = InterleavedBuffer::from_vec(2, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]).unwrap();
        assert_eq!(b.frames(), 3);
        assert_eq!(b.frame(1), &[3.0, 4.0]);
        assert_eq!(b.sample(2, 1), 6.0);
        assert_eq!(b.frames_iter().count(), 3);
        assert!(InterleavedBuffer::from_vec(2, vec![1.0, 2.0, 3.0]).is_err());
        assert!(InterleavedBuffer::from_vec(0, vec![]).is_err());
    }

    #[test]
    fn set_frames_and_mutation() {
        let mut b = InterleavedBuffer::new(2, 4);
        b.frame_mut(0).copy_from_slice(&[0.5, -0.5]);
        assert!(!b.is_silent());
        b.set_frames(2).unwrap();
        assert_eq!(b.samples().len(), 4);
        assert!(b.set_frames(5).is_err());
        b.apply_gain(2.0);
        assert_eq!(b.frame(0), &[1.0, -1.0]);
        b.clear();
        assert!(b.is_silent());
        assert_eq!(b.into_vec().len(), 8);
    }

    #[test]
    #[should_panic]
    fn frame_out_of_range_panics() {
        let b = InterleavedBuffer::new(2, 2);
        let _ = b.frame(2);
    }
}
