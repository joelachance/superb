//! Pre-allocated planar-block SPSC for capture → engine / control thread.
//!
//! Independent of any audio backend: `aura::primitives::io::device`
//! fills it from a real input, and anything else (a test, a file reader, a
//! network stream) can fill it the same way.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::realtime::spsc::{self, Consumer, Producer};
use crate::primitives::{AuraError, Result};

/// One captured planar block travelling through a recycle SPSC pair.
///
/// Allocated up front by [`capture_queue`]; the audio callback copies into a
/// free slot and the consumer returns it with [`CaptureConsumer::recycle`].
#[derive(Debug)]
pub struct CaptureBlock {
    buffer: PlanarBuffer,
    block_start_us: u64,
}

impl CaptureBlock {
    /// Planar samples for this block (active frame count is exact).
    pub fn buffer(&self) -> &PlanarBuffer {
        &self.buffer
    }

    /// Mutable planar view (e.g. to apply gain before recycling).
    pub fn buffer_mut(&mut self) -> &mut PlanarBuffer {
        &mut self.buffer
    }

    /// Capture timestamp in microseconds on the device stream clock.
    pub fn block_start_us(&self) -> u64 {
        self.block_start_us
    }

    /// Active frame count.
    pub fn frames(&self) -> usize {
        self.buffer.frames()
    }

    /// Channel count.
    pub fn channels(&self) -> usize {
        self.buffer.channels()
    }
}

/// Audio-thread end: copies planar blocks into recycled slots and pushes them.
pub struct CaptureProducer {
    free: Consumer<CaptureBlock>,
    filled: Producer<CaptureBlock>,
    dropped: Arc<AtomicU64>,
    channels: usize,
    max_frames: usize,
}

impl CaptureProducer {
    /// Copies `block` into a free slot and enqueues it.
    ///
    /// Returns `false` on overrun (no free slot) — the block is dropped and
    /// [`dropped`](CaptureProducer::dropped) increments. Never allocates.
    /// Panics in debug if `block` has more frames than the pre-allocated
    /// capacity or a different channel count.
    pub fn push(&mut self, block: &PlanarBuffer, block_start_us: u64) -> bool {
        debug_assert_eq!(
            block.channels(),
            self.channels(),
            "capture channel mismatch"
        );
        debug_assert!(
            block.frames() <= self.max_frames(),
            "capture block exceeds capacity"
        );
        let Some(mut slot) = self.free.pop() else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        let frames = block.frames().min(slot.buffer.capacity());
        let _ = slot.buffer.set_frames(frames);
        // Same channel count by construction; copy_block_from adapts if not.
        let _ = slot.buffer.copy_block_from(0, block, frames);
        slot.block_start_us = block_start_us;
        match self.filled.push(slot) {
            Ok(()) => true,
            Err(_slot) => {
                // Only reachable if the consumer abandoned the filled ring
                // while we held a free slot; drop on this thread and count.
                self.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Number of blocks dropped because the free queue was empty.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Shared dropped counter (also readable from the consumer side).
    pub fn dropped_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.dropped)
    }

    /// Channel count of every pre-allocated block.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Frame capacity of every pre-allocated block.
    pub fn max_frames(&self) -> usize {
        self.max_frames
    }
}

/// Control / engine-thread end: pops filled blocks and recycles them.
pub struct CaptureConsumer {
    free: Producer<CaptureBlock>,
    filled: Consumer<CaptureBlock>,
    dropped: Arc<AtomicU64>,
}

impl CaptureConsumer {
    /// Takes the next captured block, if any.
    pub fn pop(&mut self) -> Option<CaptureBlock> {
        self.filled.pop()
    }

    /// Returns a block to the free pool for the producer to reuse.
    ///
    /// If the free queue is full (producer abandoned), the block is dropped
    /// and its allocation is reclaimed on this thread — never on the audio
    /// callback.
    pub fn recycle(&mut self, block: CaptureBlock) {
        if let Err(_block) = self.free.push(block) {
            // Drop on the control thread intentionally.
        }
    }

    /// Drains up to `max` blocks into `f`, recycling each after `f` returns.
    pub fn drain(&mut self, max: usize, mut f: impl FnMut(&CaptureBlock)) -> usize {
        let mut n = 0;
        while n < max {
            let Some(block) = self.pop() else { break };
            f(&block);
            self.recycle(block);
            n += 1;
        }
        n
    }

    /// Blocks dropped by the producer due to overrun.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Shared dropped counter.
    pub fn dropped_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.dropped)
    }

    /// Number of filled blocks waiting.
    pub fn len(&self) -> usize {
        self.filled.len()
    }

    /// `true` if no filled blocks are waiting.
    pub fn is_empty(&self) -> bool {
        self.filled.is_empty()
    }
}

/// Creates a recycle SPSC pair with `capacity` pre-allocated planar blocks.
///
/// Each block holds `channels` planes of `max_block_frames` frames. The
/// producer end is meant for the capture callback; the consumer end for the
/// control or engine thread.
///
/// # Errors
/// Returns [`AuraError::InvalidArgument`] if any argument is zero.
pub fn capture_queue(
    channels: usize,
    max_block_frames: usize,
    capacity: usize,
) -> Result<(CaptureProducer, CaptureConsumer)> {
    if channels == 0 || max_block_frames == 0 || capacity == 0 {
        return Err(AuraError::InvalidArgument {
            what: "capture_queue",
        });
    }
    let (mut free_tx, free_rx) = spsc::channel::<CaptureBlock>(capacity);
    let (filled_tx, filled_rx) = spsc::channel::<CaptureBlock>(capacity);
    for _ in 0..capacity {
        let block = CaptureBlock {
            buffer: PlanarBuffer::new(channels, max_block_frames),
            block_start_us: 0,
        };
        free_tx
            .push(block)
            .map_err(|_| AuraError::Message("capture_queue: free ring rejected block".into()))?;
    }
    let dropped = Arc::new(AtomicU64::new(0));
    let producer = CaptureProducer {
        free: free_rx,
        filled: filled_tx,
        dropped: Arc::clone(&dropped),
        channels,
        max_frames: max_block_frames,
    };
    let consumer = CaptureConsumer {
        free: free_tx,
        filled: filled_rx,
        dropped,
    };
    Ok((producer, consumer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_args() {
        assert!(capture_queue(0, 64, 4).is_err());
        assert!(capture_queue(2, 0, 4).is_err());
        assert!(capture_queue(2, 64, 0).is_err());
    }

    #[test]
    fn round_trip_preserves_samples_and_timestamp() {
        let (mut tx, mut rx) = capture_queue(1, 16, 2).unwrap();
        let mut src = PlanarBuffer::new(1, 16);
        let _ = src.set_frames(8);
        for (i, s) in src.channel_mut(0).iter_mut().enumerate() {
            *s = i as f32 * 0.1;
        }
        assert!(tx.push(&src, 99));
        let block = rx.pop().unwrap();
        assert_eq!(block.block_start_us(), 99);
        assert_eq!(block.frames(), 8);
        assert!((block.buffer().channel(0)[7] - 0.7).abs() < 1e-6);
        rx.recycle(block);
        assert!(rx.is_empty());
    }

    #[test]
    fn overrun_increments_dropped_and_does_not_allocate() {
        let (mut tx, mut rx) = capture_queue(1, 4, 2).unwrap();
        let src = PlanarBuffer::new(1, 4);
        assert!(tx.push(&src, 0));
        assert!(tx.push(&src, 1));
        // Free pool empty → overrun.
        assert!(!tx.push(&src, 2));
        assert_eq!(tx.dropped(), 1);
        assert_eq!(rx.dropped(), 1);
        // Drain one and push again.
        let b = rx.pop().unwrap();
        rx.recycle(b);
        assert!(tx.push(&src, 3));
        assert_eq!(tx.dropped(), 1);
    }

    #[test]
    fn drain_recycles_every_block() {
        let (mut tx, mut rx) = capture_queue(2, 8, 3).unwrap();
        let src = PlanarBuffer::new(2, 8);
        assert!(tx.push(&src, 0));
        assert!(tx.push(&src, 1));
        let mut seen = 0u64;
        assert_eq!(rx.drain(8, |b| seen += b.block_start_us()), 2);
        assert_eq!(seen, 1);
        // Both recycled → two free slots again.
        assert!(tx.push(&src, 10));
        assert!(tx.push(&src, 11));
    }
}
