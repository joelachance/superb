//! Live audio input for a running engine: captured blocks become the
//! `input` a node sees in its `ProcessContext`.

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::io::capture::CaptureConsumer;
use crate::primitives::{AuraError, Result};

/// Captured audio on its way into the engine.
///
/// The input side of a capture queue (a device callback, or anything else)
/// pushes blocks; before every block the engine [`pull`](Self::pull)s them
/// into a ring four blocks deep and [`read_block`](Self::read_block)s the
/// frames it is about to render, so nodes such as
/// [`LiveInputNode`](crate::primitives::graph::nodes::LiveInputNode) can
/// play them. When the ring runs dry the missing frames are silence and
/// count as underruns; when it overflows the oldest frames go and count as
/// overruns, so the input never drifts behind the engine. Made on the
/// control thread (the ring is allocated here); everything after is
/// allocation-free.
pub struct LiveInput {
    blocks: CaptureConsumer,
    ring: Vec<Vec<f32>>,
    depth: usize,
    head: usize,
    len: usize,
    block: PlanarBuffer,
    segment: PlanarBuffer,
    overruns: u64,
    underruns: u64,
    started: bool,
}

impl LiveInput {
    /// Wraps the consumer end of a capture queue whose blocks carry
    /// `channels` channels, for an engine that renders at most `max_frames`
    /// frames per block.
    pub fn new(blocks: CaptureConsumer, channels: usize, max_frames: usize) -> Result<Self> {
        if channels == 0 || max_frames == 0 {
            return Err(AuraError::InvalidArgument {
                what: "live input channels and max_frames",
            });
        }
        let depth = max_frames * 4;
        Ok(Self {
            blocks,
            ring: vec![vec![0.0; depth]; channels],
            depth,
            head: 0,
            len: 0,
            block: PlanarBuffer::new(channels, max_frames),
            segment: PlanarBuffer::new(channels, max_frames),
            overruns: 0,
            underruns: 0,
            started: false,
        })
    }

    /// Channels in every block.
    pub fn channels(&self) -> usize {
        self.ring.len()
    }

    /// Frames waiting to be read.
    pub fn buffered(&self) -> usize {
        self.len
    }

    /// Frames thrown away because the ring was full: the engine fell
    /// behind the input.
    pub fn overruns(&self) -> u64 {
        self.overruns
    }

    /// Frames filled with silence because the ring was empty after the
    /// first block arrived: the input fell behind the engine.
    pub fn underruns(&self) -> u64 {
        self.underruns
    }

    /// Moves every captured block waiting on the queue into the ring.
    /// Allocation-free.
    pub fn pull(&mut self) {
        while let Some(block) = self.blocks.pop() {
            self.push(block.buffer());
            self.blocks.recycle(block);
        }
    }

    /// Appends one block to the ring, dropping the oldest frames when it
    /// is full. A block with fewer channels than the ring pads with silence.
    pub fn push(&mut self, audio: &PlanarBuffer) {
        let frames = audio.frames().min(self.depth);
        let free = self.depth - self.len;
        if frames > free {
            let drop = frames - free;
            self.head = (self.head + drop) % self.depth;
            self.len -= drop;
            self.overruns += drop as u64;
        }
        let start = (self.head + self.len) % self.depth;
        let first = frames.min(self.depth - start);
        for (ch, plane) in self.ring.iter_mut().enumerate() {
            if ch < audio.channels() {
                let src = &audio.channel(ch)[..frames];
                plane[start..start + first].copy_from_slice(&src[..first]);
                plane[..frames - first].copy_from_slice(&src[first..]);
            } else {
                plane[start..start + first].fill(0.0);
                plane[..frames - first].fill(0.0);
            }
        }
        self.len += frames;
        self.started = true;
    }

    /// Takes the next `frames` frames out of the ring as this block's input,
    /// silence where fewer are waiting. Allocation-free; the engine calls it
    /// once per block, then [`segment`](Self::segment) per transport segment.
    pub fn read_block(&mut self, frames: usize) -> &PlanarBuffer {
        let frames = frames.min(self.block.capacity());
        let _ = self.block.set_frames(frames);
        let have = frames.min(self.len);
        let first = have.min(self.depth - self.head);
        let (head, ring, block) = (self.head, &self.ring, &mut self.block);
        for (ch, plane) in ring.iter().enumerate() {
            let out = &mut block.channel_mut(ch)[..frames];
            out[..first].copy_from_slice(&plane[head..head + first]);
            out[first..have].copy_from_slice(&plane[..have - first]);
            out[have..].fill(0.0);
        }
        if have < frames && self.started {
            self.underruns += (frames - have) as u64;
        }
        self.head = (self.head + have) % self.depth;
        self.len -= have;
        &self.block
    }

    /// The part of this block's input starting `offset` frames in, `frames`
    /// long, for one transport segment.
    pub fn segment(&mut self, offset: usize, frames: usize) -> &PlanarBuffer {
        let available = self.block.frames();
        let offset = offset.min(available);
        let frames = frames.min(available - offset).min(self.segment.capacity());
        let _ = self.segment.set_frames(frames);
        let (block, segment) = (&self.block, &mut self.segment);
        for ch in 0..block.channels() {
            segment.channel_mut(ch)[..frames]
                .copy_from_slice(&block.channel(ch)[offset..offset + frames]);
        }
        &self.segment
    }
}

impl core::fmt::Debug for LiveInput {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LiveInput")
            .field("channels", &self.channels())
            .field("buffered", &self.len)
            .field("overruns", &self.overruns)
            .field("underruns", &self.underruns)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::io::capture::capture_queue;

    fn block(value: f32, frames: usize) -> PlanarBuffer {
        let mut b = PlanarBuffer::new(2, frames);
        b.fill(value);
        b
    }

    fn mono(value: f32, frames: usize) -> PlanarBuffer {
        let mut b = PlanarBuffer::new(1, frames);
        b.fill(value);
        b
    }

    #[test]
    fn frames_come_out_in_order_across_the_wrap() {
        let (mut mic, blocks) = capture_queue(2, 16, 8).unwrap();
        let mut input = LiveInput::new(blocks, 2, 16).unwrap();
        assert_eq!(input.channels(), 2);
        // 48 frames in, 40 out, 24 in: the ring (64 deep) wraps.
        for v in [1.0, 2.0, 3.0] {
            assert!(mic.push(&block(v, 16), 0));
        }
        input.pull();
        assert_eq!(input.buffered(), 48);
        let out = input.read_block(16);
        assert!(out.channel(0).iter().all(|&s| s == 1.0));
        let out = input.read_block(16);
        assert!(out.channel(1).iter().all(|&s| s == 2.0));
        let out = input.read_block(8);
        assert!(out.channel(0).iter().all(|&s| s == 3.0));
        assert!(mic.push(&block(4.0, 16), 0));
        assert!(mic.push(&block(5.0, 8), 0));
        input.pull();
        let out = input.read_block(16);
        assert!(out.channel(0)[..8].iter().all(|&s| s == 3.0));
        assert!(out.channel(0)[8..].iter().all(|&s| s == 4.0));
        let out = input.read_block(16);
        assert!(out.channel(0)[..8].iter().all(|&s| s == 4.0));
        assert!(out.channel(0)[8..].iter().all(|&s| s == 5.0));
        assert_eq!(input.buffered(), 0);
        assert_eq!(input.overruns(), 0);
        assert_eq!(input.underruns(), 0);
    }

    #[test]
    fn running_dry_is_silence_and_overflowing_keeps_the_newest() {
        let (mut mic, blocks) = capture_queue(1, 16, 8).unwrap();
        let mut input = LiveInput::new(blocks, 2, 16).unwrap();
        // Nothing has arrived yet: silence, not an underrun.
        let out = input.read_block(16);
        assert!(out.is_silent());
        assert_eq!(out.channels(), 2, "the ring's channels, padded");
        assert_eq!(input.underruns(), 0);
        assert!(mic.push(&PlanarBuffer::new(1, 8), 0));
        input.pull();
        let out = input.read_block(16);
        assert_eq!(out.frames(), 16);
        assert_eq!(input.underruns(), 8, "half the block was missing");
        // Six blocks into a ring four deep: the two oldest go.
        for v in 1..=6 {
            assert!(mic.push(&mono(v as f32, 16), 0));
        }
        input.pull();
        assert_eq!(input.buffered(), 64);
        assert_eq!(input.overruns(), 32);
        assert!(input.read_block(16).channel(0).iter().all(|&s| s == 3.0));
        assert_eq!(input.segment(4, 8).frames(), 8);
        assert!(LiveInput::new(capture_queue(1, 1, 1).unwrap().1, 0, 16).is_err());
    }
}
