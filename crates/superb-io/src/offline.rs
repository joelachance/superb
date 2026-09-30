//! Offline (faster-than-real-time) rendering.

use superb_buffers::PlanarBuffer;
use superb_core::{Result, SampleRate, SuperbError};
use superb_graph::BlockProcessor;

use crate::Engine;

/// Renders `total_frames` from `engine` in blocks of `block_size`.
///
/// Prepares the engine at `sample_rate` / `block_size`, then renders and
/// returns a buffer with the processor's output channel count. The last
/// block is shortened to fit exactly. Allocates the result buffer; the
/// per-block work is the same allocation-free path devices use.
pub fn render<P: BlockProcessor>(
    engine: &mut Engine<P>,
    sample_rate: SampleRate,
    block_size: usize,
    total_frames: usize,
) -> Result<PlanarBuffer> {
    if block_size == 0 {
        return Err(SuperbError::InvalidArgument { what: "block_size" });
    }
    engine.prepare(sample_rate, block_size)?;
    let channels = engine.output_channels();
    render_prepared(engine, channels, block_size, total_frames)
}

/// Renders from an engine that has already been prepared, into `channels`
/// output channels (adapted from the processor's channel count).
pub fn render_prepared<P: BlockProcessor>(
    engine: &mut Engine<P>,
    channels: usize,
    block_size: usize,
    total_frames: usize,
) -> Result<PlanarBuffer> {
    let (_, max_frames) = engine.prepared_config().ok_or(SuperbError::NotPrepared)?;
    if block_size == 0 || block_size > max_frames {
        return Err(SuperbError::InvalidArgument { what: "block_size" });
    }
    let mut result = PlanarBuffer::new(channels, total_frames);
    let mut block = PlanarBuffer::new(channels, block_size);
    let mut written = 0;
    while written < total_frames {
        let n = block_size.min(total_frames - written);
        block.set_frames(n)?;
        engine.render(&mut block)?;
        result.copy_block_from(written, &block, n)?;
        written += n;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use superb_graph::nodes::{ConstantNode, SineNode};
    use superb_graph::AudioGraph;
    use superb_transport::Transport;

    const R: SampleRate = SampleRate::HZ_48K;

    #[test]
    fn renders_exact_length_with_partial_last_block() {
        let mut g = AudioGraph::new();
        let c = g.add_node(Box::new(ConstantNode::new(0.5, 2)));
        g.set_output(c).unwrap();
        let mut e = Engine::new(g, Transport::new(R, 120.0));
        let out = render(&mut e, R, 64, 1_000).unwrap();
        assert_eq!(out.frames(), 1_000);
        assert_eq!(out.channels(), 2);
        assert!(out.channel(1).iter().all(|&s| s == 0.5));
        assert_eq!(e.stats().blocks, 16); // 15 full + 1 partial
        assert!(render(&mut e, R, 0, 10).is_err());
    }

    #[test]
    fn sine_is_continuous_across_blocks_and_transport_moves() {
        let mut g = AudioGraph::new();
        let s = g.add_node(Box::new(SineNode::new(100.0, 1.0, 1)));
        g.set_output(s).unwrap();
        let mut e = Engine::new(g, Transport::new(R, 120.0));
        e.transport_mut().play();
        let out = render(&mut e, R, 37, 4_800).unwrap();
        let ch = out.channel(0);
        // 100 Hz at 48 kHz -> one cycle every 480 samples; sample 480 back at ~0
        assert!(ch[480].abs() < 1e-3);
        let max_step = 2.0 * core::f32::consts::PI * 100.0 / 48_000.0 * 1.01;
        for w in ch.windows(2) {
            assert!((w[1] - w[0]).abs() <= max_step);
        }
        assert_eq!(e.transport().position().samples(), 4_800);
    }

    #[test]
    fn render_prepared_validates() {
        let mut g = AudioGraph::new();
        let c = g.add_node(Box::new(ConstantNode::new(1.0, 1)));
        g.set_output(c).unwrap();
        let mut e = Engine::new(g, Transport::new(R, 120.0));
        assert_eq!(render_prepared(&mut e, 1, 8, 8).unwrap_err(), SuperbError::NotPrepared);
        e.prepare(R, 8).unwrap();
        assert!(render_prepared(&mut e, 1, 9, 8).is_err());
        let out = render_prepared(&mut e, 2, 8, 20).unwrap();
        assert_eq!(out.channels(), 2);
        assert_eq!(out.channel(1), &[1.0; 20]);
        let empty = render_prepared(&mut e, 1, 8, 0).unwrap();
        assert_eq!(empty.frames(), 0);
    }
}
