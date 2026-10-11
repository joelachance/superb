//! Planar (non-interleaved) audio block.

use crate::primitives::{AuraError, Result};

use crate::primitives::buffers::ChannelLayout;

/// A block of audio with one contiguous `f32` slice per channel.
///
/// The buffer is allocated once for `capacity` frames; [`PlanarBuffer::set_frames`]
/// shrinks the *active* frame count without reallocating so a variable-size
/// device callback can reuse one buffer.
#[derive(Clone, Debug)]
pub struct PlanarBuffer {
    data: Vec<f32>,
    channels: usize,
    capacity: usize,
    frames: usize,
}

impl PlanarBuffer {
    /// Allocates a silent buffer with `channels` planes of `capacity` frames.
    ///
    /// The active frame count starts equal to `capacity`.
    pub fn new(channels: usize, capacity: usize) -> Self {
        Self {
            data: vec![0.0; channels * capacity],
            channels,
            capacity,
            frames: capacity,
        }
    }

    /// Allocates a buffer from a channel layout.
    pub fn with_layout(layout: ChannelLayout, capacity: usize) -> Self {
        Self::new(layout.channels(), capacity)
    }

    /// Builds a buffer from per-channel slices (all must be the same length).
    pub fn from_planes(planes: &[&[f32]]) -> Result<Self> {
        let channels = planes.len();
        let frames = planes.first().map_or(0, |p| p.len());
        let mut buf = Self::new(channels, frames);
        for (ch, plane) in planes.iter().enumerate() {
            if plane.len() != frames {
                return Err(AuraError::FrameMismatch {
                    expected: frames,
                    actual: plane.len(),
                });
            }
            buf.channel_mut(ch).copy_from_slice(plane);
        }
        Ok(buf)
    }

    /// Number of channels.
    #[inline]
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Active number of frames.
    #[inline]
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Maximum number of frames without reallocating.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Channel layout for this buffer.
    pub fn layout(&self) -> ChannelLayout {
        ChannelLayout::from_count(self.channels)
    }

    /// Sets the active frame count. Fails if `frames > capacity`.
    ///
    /// This never allocates and is safe to call on the audio thread.
    pub fn set_frames(&mut self, frames: usize) -> Result<()> {
        if frames > self.capacity {
            return Err(AuraError::CapacityExceeded {
                what: "PlanarBuffer frames",
                capacity: self.capacity,
            });
        }
        self.frames = frames;
        Ok(())
    }

    /// Reallocates so that `capacity` frames fit. **Not** real-time safe.
    pub fn resize_capacity(&mut self, channels: usize, capacity: usize) {
        *self = Self::new(channels, capacity);
    }

    /// Immutable view of one channel's active frames.
    #[inline]
    pub fn channel(&self, ch: usize) -> &[f32] {
        assert!(
            ch < self.channels,
            "channel {ch} out of range ({})",
            self.channels
        );
        let start = ch * self.capacity;
        &self.data[start..start + self.frames]
    }

    /// Mutable view of one channel's active frames.
    #[inline]
    pub fn channel_mut(&mut self, ch: usize) -> &mut [f32] {
        assert!(
            ch < self.channels,
            "channel {ch} out of range ({})",
            self.channels
        );
        let start = ch * self.capacity;
        &mut self.data[start..start + self.frames]
    }

    /// Iterates over every channel's active frames.
    pub fn planes(&self) -> impl Iterator<Item = &[f32]> + '_ {
        let frames = self.frames;
        self.data
            .chunks_exact(self.capacity.max(1))
            .take(self.channels)
            .map(move |c| &c[..frames])
    }

    /// Iterates mutably over every channel's active frames.
    pub fn planes_mut(&mut self) -> impl Iterator<Item = &mut [f32]> + '_ {
        let frames = self.frames;
        let channels = self.channels;
        self.data
            .chunks_exact_mut(self.capacity.max(1))
            .take(channels)
            .map(move |c| &mut c[..frames])
    }

    /// Returns mutable views of two *different* channels at once.
    pub fn channel_pair_mut(&mut self, a: usize, b: usize) -> (&mut [f32], &mut [f32]) {
        assert!(a != b, "channels must differ");
        assert!(
            a < self.channels && b < self.channels,
            "channel out of range"
        );
        let (lo, hi, swapped) = if a < b { (a, b, false) } else { (b, a, true) };
        let (head, tail) = self.data.split_at_mut(hi * self.capacity);
        let lo_slice = &mut head[lo * self.capacity..lo * self.capacity + self.frames];
        let hi_slice = &mut tail[..self.frames];
        if swapped {
            (hi_slice, lo_slice)
        } else {
            (lo_slice, hi_slice)
        }
    }

    /// Fills every active frame with zero.
    pub fn clear(&mut self) {
        for plane in self.planes_mut() {
            plane.fill(0.0);
        }
    }

    /// Fills every active frame of every channel with `value`.
    pub fn fill(&mut self, value: f32) {
        for plane in self.planes_mut() {
            plane.fill(value);
        }
    }

    /// `true` if every active sample is exactly `0.0` (or `-0.0`).
    pub fn is_silent(&self) -> bool {
        self.planes().all(|p| p.iter().all(|&s| s == 0.0))
    }

    /// `true` if every active sample's magnitude is below `threshold`.
    pub fn is_below(&self, threshold: f32) -> bool {
        self.planes().all(|p| p.iter().all(|s| s.abs() < threshold))
    }

    /// Multiplies every active sample by `gain`.
    pub fn apply_gain(&mut self, gain: f32) {
        if gain == 1.0 {
            return;
        }
        for plane in self.planes_mut() {
            for s in plane {
                *s *= gain;
            }
        }
    }

    /// Applies a linear ramp from `from` to `to` across the block (per channel).
    ///
    /// The last frame receives exactly `to` so consecutive blocks can chain
    /// without clicks.
    pub fn apply_gain_ramp(&mut self, from: f32, to: f32) {
        let n = self.frames;
        if n == 0 {
            return;
        }
        if n == 1 {
            for plane in self.planes_mut() {
                plane[0] *= to;
            }
            return;
        }
        let span = to - from;
        let denom = (n - 1) as f32;
        for plane in self.planes_mut() {
            for (i, s) in plane.iter_mut().enumerate() {
                // Computed per index (not accumulated) so the last frame is exactly `to`.
                *s *= from + span * (i as f32 / denom);
            }
        }
    }

    /// Copies the active frames of `src` into `self` (channel counts and frame counts must match).
    pub fn copy_from(&mut self, src: &PlanarBuffer) -> Result<()> {
        self.check_compatible(src)?;
        for (dst, s) in self.planes_mut().zip(src.planes()) {
            dst.copy_from_slice(s);
        }
        Ok(())
    }

    /// Adds the active frames of `src` into `self` (mixing).
    pub fn add_from(&mut self, src: &PlanarBuffer) -> Result<()> {
        self.add_from_with_gain(src, 1.0)
    }

    /// Adds `src * gain` into `self`.
    pub fn add_from_with_gain(&mut self, src: &PlanarBuffer, gain: f32) -> Result<()> {
        self.check_compatible(src)?;
        for (dst, s) in self.planes_mut().zip(src.planes()) {
            for (d, x) in dst.iter_mut().zip(s) {
                *d += x * gain;
            }
        }
        Ok(())
    }

    /// Adds a single channel of `src` into a single channel of `self` with gain.
    pub fn add_channel_from(
        &mut self,
        dst_ch: usize,
        src: &PlanarBuffer,
        src_ch: usize,
        gain: f32,
    ) -> Result<()> {
        if src.frames != self.frames {
            return Err(AuraError::FrameMismatch {
                expected: self.frames,
                actual: src.frames,
            });
        }
        let s = src.channel(src_ch);
        for (d, x) in self.channel_mut(dst_ch).iter_mut().zip(s) {
            *d += x * gain;
        }
        Ok(())
    }

    /// Copies `len` frames from the start of `src` into `self` at `dst_offset`,
    /// adapting channel counts: equal counts map 1:1, a mono source fans out
    /// to every destination channel, otherwise the first `min` channels are
    /// copied and any extra destination channels are zeroed.
    ///
    /// A source with no channels counts as silence whatever its frame count,
    /// so an unwired graph output can be copied without a special case.
    /// Fails if either buffer is too short. Never allocates.
    pub fn copy_block_from(
        &mut self,
        dst_offset: usize,
        src: &PlanarBuffer,
        len: usize,
    ) -> Result<()> {
        if src.channels == 0 {
            if self.frames < dst_offset + len {
                return Err(AuraError::FrameMismatch {
                    expected: dst_offset + len,
                    actual: self.frames,
                });
            }
            for ch in 0..self.channels {
                self.channel_mut(ch)[dst_offset..dst_offset + len].fill(0.0);
            }
            return Ok(());
        }
        if src.frames < len {
            return Err(AuraError::FrameMismatch {
                expected: len,
                actual: src.frames,
            });
        }
        if self.frames < dst_offset + len {
            return Err(AuraError::FrameMismatch {
                expected: dst_offset + len,
                actual: self.frames,
            });
        }
        let (sc, dc) = (src.channels, self.channels);
        for ch in 0..dc {
            let dst = &mut self.channel_mut(ch)[dst_offset..dst_offset + len];
            if sc == 0 {
                dst.fill(0.0);
            } else if sc == 1 {
                dst.copy_from_slice(&src.channel(0)[..len]);
            } else if ch < sc {
                dst.copy_from_slice(&src.channel(ch)[..len]);
            } else {
                dst.fill(0.0);
            }
        }
        Ok(())
    }

    /// Largest absolute sample value across all channels (0.0 for an empty block).
    pub fn peak(&self) -> f32 {
        self.planes()
            .flat_map(|p| p.iter())
            .fold(0.0f32, |m, s| m.max(s.abs()))
    }

    /// Per-channel peak absolute values written into `out` (one entry per channel).
    pub fn channel_peaks(&self, out: &mut [f32]) {
        for (o, plane) in out.iter_mut().zip(self.planes()) {
            *o = plane.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        }
    }

    /// Root-mean-square across all active samples (0.0 for an empty block).
    pub fn rms(&self) -> f32 {
        let n = self.channels * self.frames;
        if n == 0 {
            return 0.0;
        }
        let sum: f32 = self.planes().flat_map(|p| p.iter()).map(|s| s * s).sum();
        (sum / n as f32).sqrt()
    }

    fn check_compatible(&self, other: &PlanarBuffer) -> Result<()> {
        if other.channels != self.channels {
            return Err(AuraError::ChannelMismatch {
                expected: self.channels,
                actual: other.channels,
            });
        }
        if other.frames != self.frames {
            return Err(AuraError::FrameMismatch {
                expected: self.frames,
                actual: other.frames,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(channels: usize, frames: usize) -> PlanarBuffer {
        let mut b = PlanarBuffer::new(channels, frames);
        for ch in 0..channels {
            for (i, s) in b.channel_mut(ch).iter_mut().enumerate() {
                *s = (ch * 10 + i) as f32;
            }
        }
        b
    }

    #[test]
    fn new_buffer_is_silent_with_expected_shape() {
        let b = PlanarBuffer::new(2, 64);
        assert_eq!(b.channels(), 2);
        assert_eq!(b.frames(), 64);
        assert_eq!(b.capacity(), 64);
        assert!(b.is_silent());
        assert_eq!(b.peak(), 0.0);
        assert_eq!(b.rms(), 0.0);
        assert_eq!(b.layout(), ChannelLayout::Stereo);
    }

    #[test]
    fn channels_are_independent_planes() {
        let b = ramp(2, 4);
        assert_eq!(b.channel(0), &[0.0, 1.0, 2.0, 3.0]);
        assert_eq!(b.channel(1), &[10.0, 11.0, 12.0, 13.0]);
        let planes: Vec<&[f32]> = b.planes().collect();
        assert_eq!(planes.len(), 2);
        assert_eq!(planes[1][0], 10.0);
    }

    #[test]
    fn shrinking_frames_keeps_capacity_and_plane_stride() {
        let mut b = ramp(2, 8);
        b.set_frames(3).unwrap();
        assert_eq!(b.frames(), 3);
        assert_eq!(b.capacity(), 8);
        assert_eq!(b.channel(1), &[10.0, 11.0, 12.0]);
        assert!(b.set_frames(9).is_err());
        b.set_frames(8).unwrap();
        assert_eq!(b.channel(1)[7], 17.0);
        b.set_frames(0).unwrap();
        assert!(b.is_silent()); // vacuous
        assert_eq!(b.peak(), 0.0);
    }

    #[test]
    fn gain_and_ramp() {
        let mut b = PlanarBuffer::new(1, 5);
        b.fill(1.0);
        b.apply_gain(0.5);
        assert_eq!(b.channel(0), &[0.5; 5]);
        b.fill(1.0);
        b.apply_gain_ramp(0.0, 1.0);
        assert_eq!(b.channel(0), &[0.0, 0.25, 0.5, 0.75, 1.0]);
        let mut one = PlanarBuffer::new(1, 1);
        one.fill(2.0);
        one.apply_gain_ramp(0.0, 0.5);
        assert_eq!(one.channel(0), &[1.0]);
    }

    #[test]
    fn mixing_and_copying() {
        let a = ramp(2, 4);
        let mut b = PlanarBuffer::new(2, 4);
        b.fill(1.0);
        b.add_from(&a).unwrap();
        assert_eq!(b.channel(0), &[1.0, 2.0, 3.0, 4.0]);
        b.add_from_with_gain(&a, -1.0).unwrap();
        assert_eq!(b.channel(1), &[1.0; 4]);
        b.copy_from(&a).unwrap();
        assert_eq!(b.channel(1), a.channel(1));
        b.clear();
        b.add_channel_from(1, &a, 0, 2.0).unwrap();
        assert_eq!(b.channel(1), &[0.0, 2.0, 4.0, 6.0]);
        assert!(b.channel(0).iter().all(|&s| s == 0.0));

        let wrong_ch = PlanarBuffer::new(1, 4);
        assert!(matches!(
            b.add_from(&wrong_ch),
            Err(AuraError::ChannelMismatch { .. })
        ));
        let wrong_frames = PlanarBuffer::new(2, 3);
        assert!(matches!(
            b.copy_from(&wrong_frames),
            Err(AuraError::FrameMismatch { .. })
        ));
    }

    #[test]
    fn copy_block_adapts_channels_and_checks_bounds() {
        let src = ramp(2, 4);
        let mut dst = PlanarBuffer::new(2, 10);
        dst.fill(9.0);
        dst.copy_block_from(3, &src, 4).unwrap();
        assert_eq!(
            dst.channel(1),
            &[9.0, 9.0, 9.0, 10.0, 11.0, 12.0, 13.0, 9.0, 9.0, 9.0]
        );
        assert!(dst.copy_block_from(7, &src, 4).is_err()); // dst too short
        assert!(dst.copy_block_from(0, &src, 5).is_err()); // src too short

        let mono = ramp(1, 4);
        let mut stereo = PlanarBuffer::new(2, 4);
        stereo.copy_block_from(0, &mono, 4).unwrap();
        assert_eq!(stereo.channel(0), stereo.channel(1));
        assert_eq!(stereo.channel(1), &[0.0, 1.0, 2.0, 3.0]);

        let mut quad = PlanarBuffer::new(4, 4);
        quad.fill(5.0);
        quad.copy_block_from(0, &src, 4).unwrap();
        assert_eq!(quad.channel(1), &[10.0, 11.0, 12.0, 13.0]);
        assert_eq!(quad.channel(3), &[0.0; 4]);

        let empty = PlanarBuffer::new(0, 4);
        quad.copy_block_from(0, &empty, 4).unwrap();
        assert!(quad.is_silent());
        // No channels means silence even when the source reports no frames.
        quad.fill(3.0);
        quad.copy_block_from(1, &PlanarBuffer::new(0, 0), 2)
            .unwrap();
        assert_eq!(quad.channel(2), &[3.0, 0.0, 0.0, 3.0]);
        assert!(quad
            .copy_block_from(3, &PlanarBuffer::new(0, 0), 2)
            .is_err());
    }

    #[test]
    fn metering() {
        let mut b = PlanarBuffer::new(2, 4);
        b.channel_mut(0).copy_from_slice(&[0.5, -0.25, 0.0, 0.0]);
        b.channel_mut(1).copy_from_slice(&[0.0, 0.0, -0.75, 0.0]);
        assert_eq!(b.peak(), 0.75);
        let mut peaks = [0.0; 2];
        b.channel_peaks(&mut peaks);
        assert_eq!(peaks, [0.5, 0.75]);
        let expected = ((0.25 + 0.0625 + 0.5625) / 8.0f32).sqrt();
        assert!((b.rms() - expected).abs() < 1e-6);
        assert!(!b.is_silent());
        assert!(b.is_below(0.76));
        assert!(!b.is_below(0.75));
    }

    #[test]
    fn channel_pair_mut_gives_disjoint_slices_in_either_order() {
        let mut b = ramp(3, 2);
        {
            let (l, r) = b.channel_pair_mut(2, 0);
            assert_eq!(l, &[20.0, 21.0]);
            assert_eq!(r, &[0.0, 1.0]);
            l[0] = -1.0;
            r[1] = -2.0;
        }
        assert_eq!(b.channel(2)[0], -1.0);
        assert_eq!(b.channel(0)[1], -2.0);
    }

    #[test]
    fn from_planes_validates() {
        let ok = PlanarBuffer::from_planes(&[&[1.0, 2.0], &[3.0, 4.0]]).unwrap();
        assert_eq!(ok.channel(1), &[3.0, 4.0]);
        assert!(PlanarBuffer::from_planes(&[&[1.0, 2.0], &[3.0]]).is_err());
        let empty = PlanarBuffer::from_planes(&[]).unwrap();
        assert_eq!(empty.channels(), 0);
        assert_eq!(empty.planes().count(), 0);
    }

    #[test]
    #[should_panic]
    fn out_of_range_channel_panics() {
        let b = PlanarBuffer::new(1, 4);
        let _ = b.channel(1);
    }
}
