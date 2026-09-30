//! A fixed delay line used for latency compensation (and by the
//! [`DelayNode`](crate::primitives::graph::nodes::DelayNode) fixture).

use crate::primitives::buffers::PlanarBuffer;

/// A multi-channel ring buffer that delays audio by a fixed number of
/// frames. Allocated once (`new`); `process` never allocates.
pub(crate) struct DelayLine {
    delay: usize,
    len: usize,
    planes: Vec<Vec<f32>>,
    write: usize,
    /// Number of most recently written frames that are zero (saturates at `len`).
    silent_run: usize,
}

impl DelayLine {
    /// Creates a line delaying `channels` channels by `delay` frames, able
    /// to process blocks of up to `max_frames`.
    pub(crate) fn new(channels: usize, delay: usize, max_frames: usize) -> Self {
        let len = (delay + max_frames).max(1);
        Self {
            delay,
            len,
            planes: vec![vec![0.0; len]; channels],
            write: 0,
            silent_run: len,
        }
    }

    /// Delay in frames.
    pub(crate) fn delay(&self) -> usize {
        self.delay
    }

    /// Clears the history without reallocating.
    pub(crate) fn reset(&mut self) {
        for p in &mut self.planes {
            p.iter_mut().for_each(|s| *s = 0.0);
        }
        self.write = 0;
        self.silent_run = self.len;
    }

    /// Pushes `frames` frames of `src` (`None` = silence) and writes the
    /// delayed `frames` frames into `out`, which must already have `frames`
    /// active frames. Channels are matched by index (extra channels on
    /// either side are ignored / left untouched).
    ///
    /// Returns `true` if the delayed output is known to be silent, i.e. only
    /// silence has been pushed for at least `delay + frames` frames. The
    /// flag is conservative (a live source that happens to output zeros is
    /// not detected); `out` is fully written either way.
    pub(crate) fn process(
        &mut self,
        src: Option<&PlanarBuffer>,
        frames: usize,
        out: &mut PlanarBuffer,
    ) -> bool {
        let frames = frames.min(self.len.saturating_sub(self.delay));
        let len = self.len;
        let write = self.write;
        match src {
            Some(src) => {
                for (ch, plane) in self.planes.iter_mut().enumerate() {
                    if ch < src.channels() {
                        let s = &src.channel(ch)[..frames];
                        for (f, &v) in s.iter().enumerate() {
                            plane[(write + f) % len] = v;
                        }
                    } else {
                        for f in 0..frames {
                            plane[(write + f) % len] = 0.0;
                        }
                    }
                }
                self.silent_run = 0;
            }
            None => {
                for plane in &mut self.planes {
                    for f in 0..frames {
                        plane[(write + f) % len] = 0.0;
                    }
                }
                self.silent_run = (self.silent_run + frames).min(len);
            }
        }
        let read_start = (write + len - self.delay) % len;
        for (ch, plane) in self.planes.iter().enumerate() {
            if ch >= out.channels() {
                break;
            }
            let dst = &mut out.channel_mut(ch)[..frames];
            for (f, d) in dst.iter_mut().enumerate() {
                *d = plane[(read_start + f) % len];
            }
        }
        self.write = (write + frames) % len;
        self.silent_run >= self.delay + frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(channels: usize, start: f32, frames: usize) -> PlanarBuffer {
        let mut b = PlanarBuffer::new(channels, frames);
        for ch in 0..channels {
            for (f, s) in b.channel_mut(ch).iter_mut().enumerate() {
                *s = start + f as f32 + ch as f32 * 1000.0;
            }
        }
        b
    }

    #[test]
    fn delays_by_exactly_n_frames_across_block_sizes() {
        let mut line = DelayLine::new(2, 5, 8);
        assert_eq!(line.delay(), 5);
        let mut out = PlanarBuffer::new(2, 8);
        let mut expected_in = Vec::new();
        let mut got = Vec::new();
        let mut t = 0.0;
        for &n in &[8usize, 3, 1, 7, 8, 2] {
            let src = ramp(2, t, n);
            expected_in.extend_from_slice(src.channel(1));
            out.set_frames(n).unwrap();
            let silent = line.process(Some(&src), n, &mut out);
            assert!(!silent);
            got.extend_from_slice(out.channel(1));
            t += n as f32;
        }
        // Output is 5 zeros followed by the input.
        assert_eq!(&got[..5], &[0.0; 5]);
        assert_eq!(&got[5..], &expected_in[..expected_in.len() - 5]);
    }

    #[test]
    fn silence_is_reported_only_when_the_tail_has_drained() {
        let mut line = DelayLine::new(1, 4, 4);
        let mut out = PlanarBuffer::new(1, 4);
        assert!(line.process(None, 4, &mut out), "fresh line is silent");
        assert!(
            !line.process(Some(&ramp(1, 1.0, 4)), 4, &mut out),
            "output is still the old zeros but input is live"
        );
        assert_eq!(out.channel(0), &[0.0; 4]);
        assert!(!line.process(None, 4, &mut out), "the ramp comes out now");
        assert_eq!(out.channel(0), &[1.0, 2.0, 3.0, 4.0]);
        assert!(line.process(None, 4, &mut out), "drained");
        assert_eq!(out.channel(0), &[0.0; 4]);
        line.process(Some(&ramp(1, 9.0, 2)), 2, &mut out);
        line.reset();
        assert!(line.process(None, 4, &mut out));
        assert_eq!(out.channel(0), &[0.0; 4]);
    }

    #[test]
    fn zero_delay_and_channel_mismatch_are_tolerated() {
        let mut line = DelayLine::new(2, 0, 4);
        let mut out = PlanarBuffer::new(3, 4);
        out.channel_mut(2).fill(7.0);
        assert!(!line.process(Some(&ramp(1, 1.0, 4)), 4, &mut out));
        assert_eq!(out.channel(0), &[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(
            out.channel(1),
            &[0.0; 4],
            "missing source channel reads as silence"
        );
        assert_eq!(
            out.channel(2),
            &[7.0; 4],
            "extra destination channel untouched"
        );
        // Blocks larger than max_frames are truncated rather than corrupting the ring.
        let mut line = DelayLine::new(1, 2, 2);
        let mut out = PlanarBuffer::new(1, 4);
        line.process(Some(&ramp(1, 1.0, 4)), 4, &mut out);
        assert_eq!(&out.channel(0)[..2], &[0.0, 0.0]);
    }
}
