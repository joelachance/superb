//! [`PluginNode`]: a plugin instance as a graph node.

use superb_buffers::PlanarBuffer;
use superb_core::SampleRate;
use superb_graph::{Node, NodeChannels, ProcessContext, ProcessStatus};
use superb_rt::spsc::{self, Consumer, Producer};

use crate::{ParamChange, PluginInstance};

/// Wraps a [`PluginInstance`] so it can be placed in an
/// [`AudioGraph`](superb_graph::AudioGraph).
///
/// `prepare` activates the plugin (a failure makes the node silent rather
/// than panicking), `process` first drains the parameter queue into the
/// instance and then renders. Take the queue's sending half with
/// [`take_param_sender`](PluginNode::take_param_sender) before handing the
/// node to the graph.
pub struct PluginNode {
    instance: Box<dyn PluginInstance>,
    name: String,
    channels: NodeChannels,
    params_rx: Consumer<ParamChange>,
    params_tx: Option<Producer<ParamChange>>,
    prepared: bool,
}

impl PluginNode {
    /// Capacity of the parameter queue (changes per block before drops).
    pub const PARAM_QUEUE_CAPACITY: usize = 256;

    /// Wraps an instance. The node's channel layout is the plugin's main
    /// input/output port layout.
    pub fn new(instance: Box<dyn PluginInstance>) -> Self {
        let ports = instance.ports();
        let name = instance.descriptor().name.clone();
        let (tx, rx) = spsc::channel(Self::PARAM_QUEUE_CAPACITY);
        Self {
            instance,
            name,
            channels: NodeChannels {
                inputs: ports.inputs,
                outputs: ports.outputs,
            },
            params_rx: rx,
            params_tx: Some(tx),
            prepared: false,
        }
    }

    /// The sending half of the parameter queue (once). Pushing a
    /// [`ParamChange`] is wait-free; the node applies it at the start of the
    /// next block.
    pub fn take_param_sender(&mut self) -> Option<Producer<ParamChange>> {
        self.params_tx.take()
    }

    /// The wrapped instance.
    pub fn instance(&self) -> &dyn PluginInstance {
        self.instance.as_ref()
    }

    /// Mutable access to the wrapped instance (main thread, e.g. state I/O).
    pub fn instance_mut(&mut self) -> &mut dyn PluginInstance {
        self.instance.as_mut()
    }

    /// Unwraps the instance.
    pub fn into_instance(self) -> Box<dyn PluginInstance> {
        self.instance
    }

    /// `true` if the last `prepare` activated the plugin successfully.
    pub fn is_prepared(&self) -> bool {
        self.prepared
    }
}

impl Node for PluginNode {
    fn name(&self) -> &str {
        &self.name
    }

    fn channels(&self) -> NodeChannels {
        self.channels
    }

    fn prepare(&mut self, sample_rate: SampleRate, max_frames: usize) {
        if self.instance.is_active() {
            self.instance.deactivate();
        }
        self.prepared = self.instance.activate(sample_rate, max_frames).is_ok();
    }

    fn reset(&mut self) {
        self.instance.reset();
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus {
        if !self.prepared {
            return ProcessStatus::Silent;
        }
        while let Some(change) = self.params_rx.pop() {
            // Unknown IDs / full plugin queues are dropped: nothing can be
            // reported from the audio thread.
            let _ = self.instance.set_param(change.id, change.value);
        }
        self.instance.process(ctx, input, output)
    }

    fn latency_frames(&self) -> usize {
        self.instance.latency_frames()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{AudioPortLayout, ParamInfo, PluginDescriptor, PluginFormat};
    use superb_core::{Result, SuperbError};
    use superb_graph::nodes::{ConstantNode, PassThroughNode};
    use superb_graph::{AudioGraph, BlockTime};
    use superb_midi::MidiEventBuffer;

    /// A pure-Rust gain plugin used to test the adapter without FFI.
    pub(crate) struct MockGain {
        pub desc: PluginDescriptor,
        pub params: Vec<ParamInfo>,
        pub gain: f64,
        pub active: bool,
        pub fail_activate: bool,
        pub latency: usize,
        pub resets: usize,
        pub queue: Vec<(u32, f64)>,
    }

    impl MockGain {
        pub fn new(latency: usize) -> Self {
            Self {
                desc: PluginDescriptor::new(PluginFormat::Clap, "superb.mock.gain", "Mock Gain"),
                params: vec![ParamInfo {
                    id: 1,
                    name: "Gain".into(),
                    module: String::new(),
                    min: 0.0,
                    max: 2.0,
                    default: 1.0,
                    automatable: true,
                    stepped: false,
                    hidden: false,
                    readonly: false,
                    bypass: false,
                }],
                gain: 1.0,
                active: false,
                fail_activate: false,
                latency,
                resets: 0,
                queue: Vec::with_capacity(8),
            }
        }
    }

    impl PluginInstance for MockGain {
        fn descriptor(&self) -> &PluginDescriptor {
            &self.desc
        }
        fn ports(&self) -> AudioPortLayout {
            AudioPortLayout {
                inputs: 2,
                outputs: 2,
            }
        }
        fn params(&self) -> &[ParamInfo] {
            &self.params
        }
        fn param_value(&self, id: u32) -> Result<f64> {
            if id == 1 {
                Ok(self.gain)
            } else {
                Err(SuperbError::NotFound {
                    kind: "param",
                    id: id as u64,
                })
            }
        }
        fn set_param(&mut self, id: u32, value: f64) -> Result<()> {
            if id != 1 {
                return Err(SuperbError::NotFound {
                    kind: "param",
                    id: id as u64,
                });
            }
            if self.active {
                if self.queue.len() == self.queue.capacity() {
                    return Err(SuperbError::CapacityExceeded {
                        what: "mock queue",
                        capacity: 8,
                    });
                }
                self.queue.push((id, value));
            } else {
                self.gain = value;
            }
            Ok(())
        }
        fn activate(&mut self, _sample_rate: SampleRate, _max_frames: usize) -> Result<()> {
            if self.fail_activate {
                return Err(SuperbError::Message("mock refuses to activate".into()));
            }
            self.active = true;
            Ok(())
        }
        fn deactivate(&mut self) {
            self.active = false;
        }
        fn is_active(&self) -> bool {
            self.active
        }
        fn reset(&mut self) {
            self.resets += 1;
        }
        fn process(
            &mut self,
            _ctx: &ProcessContext<'_>,
            input: &PlanarBuffer,
            output: &mut PlanarBuffer,
        ) -> ProcessStatus {
            for (_, v) in self.queue.drain(..) {
                self.gain = v;
            }
            output.copy_from(input).unwrap();
            output.apply_gain(self.gain as f32);
            ProcessStatus::Normal
        }
        fn latency_frames(&self) -> usize {
            self.latency
        }
        fn save_state(&mut self) -> Result<Vec<u8>> {
            Ok(self.gain.to_le_bytes().to_vec())
        }
        fn load_state(&mut self, bytes: &[u8]) -> Result<()> {
            let arr: [u8; 8] = bytes
                .try_into()
                .map_err(|_| SuperbError::InvalidArgument { what: "state" })?;
            self.gain = f64::from_le_bytes(arr);
            Ok(())
        }
    }

    fn no_midi() -> MidiEventBuffer {
        MidiEventBuffer::with_capacity(0)
    }

    #[test]
    fn node_reports_plugin_layout_name_and_latency() {
        let mut node = PluginNode::new(Box::new(MockGain::new(12)));
        assert_eq!(node.name(), "Mock Gain");
        assert_eq!(node.channels(), NodeChannels::symmetric(2));
        assert_eq!(node.latency_frames(), 12);
        assert!(!node.is_prepared());
        assert!(node.take_param_sender().is_some());
        assert!(
            node.take_param_sender().is_none(),
            "sender is handed out once"
        );
        assert_eq!(node.instance().descriptor().id, "superb.mock.gain");
        node.instance_mut().set_param(1, 0.25).unwrap();
        assert_eq!(node.into_instance().param_value(1).unwrap(), 0.25);
    }

    #[test]
    fn queued_params_reach_the_plugin_inside_the_graph() {
        let mut node = PluginNode::new(Box::new(MockGain::new(4)));
        let mut tx = node.take_param_sender().unwrap();
        let mut g = AudioGraph::new();
        let src = g.add_node(Box::new(ConstantNode::new(1.0, 2)));
        let fx = g.add_node(Box::new(node));
        let sink = g.add_node(Box::new(PassThroughNode::new(2)));
        g.connect(src, fx).unwrap();
        g.connect(fx, sink).unwrap();
        g.connect(src, sink).unwrap(); // parallel dry path, compensated by 4 frames
        g.set_output(sink).unwrap();
        g.prepare(SampleRate::HZ_48K, 8).unwrap();
        assert_eq!(g.latency_frames(), 4);
        assert_eq!(g.edge_delay(src, sink).unwrap(), 4);

        g.process(BlockTime::free_running(), 8, &no_midi()).unwrap();
        assert_eq!(g.node_output(fx).unwrap().channel(1), &[1.0; 8]);

        tx.push(ParamChange { id: 1, value: 0.5 }).unwrap();
        tx.push(ParamChange { id: 99, value: 7.0 }).unwrap(); // unknown: dropped silently
        g.process(BlockTime::free_running(), 8, &no_midi()).unwrap();
        assert_eq!(g.node_output(fx).unwrap().channel(0), &[0.5; 8]);
        g.reset();
        let plugin = g.remove_node(fx).unwrap();
        assert_eq!(plugin.name(), "Mock Gain");
    }

    #[test]
    fn failed_activation_makes_the_node_silent() {
        let mut mock = MockGain::new(0);
        mock.fail_activate = true;
        let mut node = PluginNode::new(Box::new(mock));
        node.prepare(SampleRate::HZ_48K, 16);
        assert!(!node.is_prepared());
        let input = PlanarBuffer::new(2, 16);
        let mut output = PlanarBuffer::new(2, 16);
        let midi = no_midi();
        let ctx = ProcessContext {
            sample_rate: SampleRate::HZ_48K,
            frames: 16,
            time: BlockTime::free_running(),
            midi: &midi,
            input_is_silent: false,
        };
        assert_eq!(
            node.process(&ctx, &input, &mut output),
            ProcessStatus::Silent
        );

        // Re-preparing an active plugin deactivates first.
        let mut node = PluginNode::new(Box::new(MockGain::new(0)));
        node.prepare(SampleRate::HZ_48K, 16);
        node.prepare(SampleRate::CD, 32);
        assert!(node.is_prepared() && node.instance().is_active());
        node.reset();
        assert_eq!(
            node.process(&ctx, &input, &mut output),
            ProcessStatus::Normal
        );
    }
}
