//! Graph nodes used by the mixer.

use std::sync::Arc;

use superb_buffers::PlanarBuffer;
use superb_graph::{Node, NodeChannels, ProcessContext, ProcessStatus};

use crate::{SendParams, StripParams};

/// Maximum channels a strip ramps individually; extra channels get the raw gain.
const MAX_RAMP_CHANNELS: usize = 8;

/// Gain → pan → mute/solo → meter, with per-block linear ramps so that
/// parameter changes never click.
pub struct StripNode {
    params: Arc<StripParams>,
    channels: usize,
    last_gains: [f32; MAX_RAMP_CHANNELS],
    primed: bool,
}

impl StripNode {
    /// Creates a strip with `channels` channels driven by `params`.
    pub fn new(params: Arc<StripParams>, channels: usize) -> Self {
        Self {
            params,
            channels,
            last_gains: [0.0; MAX_RAMP_CHANNELS],
            primed: false,
        }
    }

    /// The parameter block.
    pub fn params(&self) -> Arc<StripParams> {
        Arc::clone(&self.params)
    }

    /// Per-channel target gains for the current parameter values.
    fn target_gains(&self, out: &mut [f32; MAX_RAMP_CHANNELS]) {
        let g = self.params.effective_gain();
        if self.channels == 2 {
            let (l, r) = self.params.pan_law().gains(self.params.pan());
            out[0] = g * l;
            out[1] = g * r;
        } else {
            for o in out.iter_mut().take(self.channels.min(MAX_RAMP_CHANNELS)) {
                *o = g;
            }
        }
    }
}

impl Node for StripNode {
    fn name(&self) -> &str {
        "strip"
    }

    fn channels(&self) -> NodeChannels {
        NodeChannels::symmetric(self.channels)
    }

    fn reset(&mut self) {
        self.primed = false;
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus {
        let mut targets = [0.0f32; MAX_RAMP_CHANNELS];
        self.target_gains(&mut targets);
        if !self.primed {
            self.last_gains = targets;
            self.primed = true;
        }
        let from = self.last_gains;
        self.last_gains = targets;

        let all_zero =
            (0..self.channels.min(MAX_RAMP_CHANNELS)).all(|c| from[c] == 0.0 && targets[c] == 0.0);
        if ctx.input_is_silent || all_zero {
            for c in 0..self.channels.min(crate::params::METER_CHANNELS) {
                self.params.raise_peak(c, 0.0);
            }
            return ProcessStatus::Silent;
        }
        if output.copy_from(input).is_err() {
            return ProcessStatus::Silent;
        }
        let frames = ctx.frames;
        for (c, plane) in output.planes_mut().enumerate() {
            let (a, b) = if c < MAX_RAMP_CHANNELS {
                (from[c], targets[c])
            } else {
                (targets[0], targets[0])
            };
            if a == b {
                if b != 1.0 {
                    for s in plane.iter_mut() {
                        *s *= b;
                    }
                }
            } else if frames == 1 {
                plane[0] *= b;
            } else {
                let denom = (frames - 1) as f32;
                for (i, s) in plane.iter_mut().enumerate() {
                    *s *= a + (b - a) * (i as f32 / denom);
                }
            }
            if c < crate::params::METER_CHANNELS {
                let peak = plane.iter().fold(0.0f32, |m, s| m.max(s.abs()));
                self.params.raise_peak(c, peak);
            }
        }
        ProcessStatus::Normal
    }
}

/// A send: input × level, ramped per block.
pub struct SendNode {
    params: Arc<SendParams>,
    channels: usize,
    last: f32,
    primed: bool,
}

impl SendNode {
    /// Creates a send with `channels` channels driven by `params`.
    pub fn new(params: Arc<SendParams>, channels: usize) -> Self {
        Self {
            params,
            channels,
            last: 0.0,
            primed: false,
        }
    }

    /// The parameter block.
    pub fn params(&self) -> Arc<SendParams> {
        Arc::clone(&self.params)
    }
}

impl Node for SendNode {
    fn name(&self) -> &str {
        "send"
    }

    fn channels(&self) -> NodeChannels {
        NodeChannels::symmetric(self.channels)
    }

    fn reset(&mut self) {
        self.primed = false;
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus {
        let target = self.params.effective_level();
        if !self.primed {
            self.last = target;
            self.primed = true;
        }
        let from = self.last;
        self.last = target;
        if ctx.input_is_silent || (from == 0.0 && target == 0.0) {
            return ProcessStatus::Silent;
        }
        if output.copy_from(input).is_err() {
            return ProcessStatus::Silent;
        }
        if from == target {
            output.apply_gain(target);
        } else {
            output.apply_gain_ramp(from, target);
        }
        ProcessStatus::Normal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PanLaw;
    use superb_core::SampleRate;
    use superb_graph::BlockTime;
    use superb_midi::MidiEventBuffer;

    fn ctx<'a>(frames: usize, midi: &'a MidiEventBuffer, silent: bool) -> ProcessContext<'a> {
        ProcessContext {
            sample_rate: SampleRate::HZ_48K,
            frames,
            time: BlockTime::free_running(),
            midi,
            input_is_silent: silent,
        }
    }

    fn assert_ramp(actual: &[f32], expected: &[f32]) {
        assert_eq!(actual.len(), expected.len());
        for (a, e) in actual.iter().zip(expected) {
            assert!((a - e).abs() < 1e-6, "{actual:?} vs {expected:?}");
        }
    }

    fn ones(channels: usize, frames: usize) -> PlanarBuffer {
        let mut b = PlanarBuffer::new(channels, frames);
        b.fill(1.0);
        b
    }

    #[test]
    fn first_block_does_not_ramp_from_zero() {
        let midi = MidiEventBuffer::with_capacity(0);
        let params = Arc::new(StripParams::new(None));
        params.set_pan_law(PanLaw::Balance);
        let mut strip = StripNode::new(Arc::clone(&params), 2);
        let input = ones(2, 8);
        let mut out = PlanarBuffer::new(2, 8);
        assert_eq!(
            strip.process(&ctx(8, &midi, false), &input, &mut out),
            ProcessStatus::Normal
        );
        assert_eq!(out.channel(0), &[1.0; 8]);
        assert_eq!(out.channel(1), &[1.0; 8]);
        assert_eq!(params.peak(0), 1.0);
    }

    #[test]
    fn mute_ramps_to_silence_then_reports_silent() {
        let midi = MidiEventBuffer::with_capacity(0);
        let params = Arc::new(StripParams::new(None));
        params.set_pan_law(PanLaw::Balance);
        let mut strip = StripNode::new(Arc::clone(&params), 2);
        let input = ones(2, 4);
        let mut out = PlanarBuffer::new(2, 4);
        strip.process(&ctx(4, &midi, false), &input, &mut out);
        params.set_mute(true);
        assert_eq!(
            strip.process(&ctx(4, &midi, false), &input, &mut out),
            ProcessStatus::Normal
        );
        assert_ramp(out.channel(0), &[1.0, 2.0 / 3.0, 1.0 / 3.0, 0.0]);
        assert_eq!(
            strip.process(&ctx(4, &midi, false), &input, &mut out),
            ProcessStatus::Silent
        );
        assert_eq!(params.take_peak(0), 1.0);
        assert_eq!(params.peak(0), 0.0);
        params.set_mute(false);
        assert_eq!(
            strip.process(&ctx(4, &midi, false), &input, &mut out),
            ProcessStatus::Normal
        );
        assert_ramp(out.channel(1), &[0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0]);
        assert_eq!(
            strip.process(&ctx(4, &midi, true), &input, &mut out),
            ProcessStatus::Silent
        );
    }

    #[test]
    fn pan_applies_only_to_stereo() {
        let midi = MidiEventBuffer::with_capacity(0);
        let params = Arc::new(StripParams::new(None));
        params.set_pan(-1.0);
        let mut stereo = StripNode::new(Arc::clone(&params), 2);
        let input = ones(2, 4);
        let mut out = PlanarBuffer::new(2, 4);
        stereo.process(&ctx(4, &midi, false), &input, &mut out);
        assert!((out.channel(0)[3] - 1.0).abs() < 1e-6);
        assert!(out.channel(1)[3].abs() < 1e-6);

        let mut mono = StripNode::new(Arc::clone(&params), 1);
        let input = ones(1, 4);
        let mut out = PlanarBuffer::new(1, 4);
        mono.process(&ctx(4, &midi, false), &input, &mut out);
        assert_eq!(out.channel(0), &[1.0; 4]);
        assert_eq!(mono.channels(), NodeChannels::symmetric(1));
        assert_eq!(mono.name(), "strip");
        assert!(Arc::ptr_eq(&mono.params(), &params));
    }

    #[test]
    fn single_frame_blocks_take_the_target() {
        let midi = MidiEventBuffer::with_capacity(0);
        let params = Arc::new(StripParams::new(None));
        let mut strip = StripNode::new(Arc::clone(&params), 1);
        let input = ones(1, 1);
        let mut out = PlanarBuffer::new(1, 1);
        strip.process(&ctx(1, &midi, false), &input, &mut out);
        params.set_gain(0.25);
        strip.process(&ctx(1, &midi, false), &input, &mut out);
        assert_eq!(out.channel(0), &[0.25]);
        strip.reset();
        params.set_gain(0.5);
        strip.process(&ctx(1, &midi, false), &input, &mut out);
        assert_eq!(out.channel(0), &[0.5]);
    }

    #[test]
    fn send_scales_and_disables() {
        let midi = MidiEventBuffer::with_capacity(0);
        let params = Arc::new(SendParams::new(0.5));
        let mut send = SendNode::new(Arc::clone(&params), 2);
        let input = ones(2, 4);
        let mut out = PlanarBuffer::new(2, 4);
        assert_eq!(
            send.process(&ctx(4, &midi, false), &input, &mut out),
            ProcessStatus::Normal
        );
        assert_eq!(out.channel(1), &[0.5; 4]);
        params.set_enabled(false);
        send.process(&ctx(4, &midi, false), &input, &mut out); // ramp down
        assert_eq!(out.channel(0)[0], 0.5);
        assert_eq!(out.channel(0)[3], 0.0);
        assert_eq!(
            send.process(&ctx(4, &midi, false), &input, &mut out),
            ProcessStatus::Silent
        );
        assert_eq!(
            send.process(&ctx(4, &midi, true), &input, &mut out),
            ProcessStatus::Silent
        );
        assert_eq!(send.name(), "send");
        assert!(Arc::ptr_eq(&send.params(), &params));
        send.reset();
    }
}
