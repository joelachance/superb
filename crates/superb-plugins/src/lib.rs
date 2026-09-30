//! # superb-plugins
//!
//! Plugin hosting for the Superb DAW SDK.
//!
//! * [`PluginInstance`] — the format-agnostic contract a hosted plugin
//!   fulfils: descriptor, audio port layout, parameters, activate/process,
//!   latency, state. [`PluginDescriptor`] / [`ParamInfo`] describe it.
//! * [`PluginNode`] — adapts any `Box<dyn PluginInstance>` to
//!   [`superb_graph::Node`], with a wait-free parameter queue so a UI
//!   thread can move plugin parameters while the graph runs.
//! * [`clap`] (feature `clap`, on by default) — a CLAP host built on the
//!   `clap-sys` bindings: load a `.clap` bundle (or an in-process entry),
//!   list descriptors, instantiate, and drive the plugin through the
//!   standard extensions (`params`, `state`, `latency`, `audio-ports`).
//! * [`scan`] — default search paths and directory scanning.
//! * [`vst3`] — a stub. VST3 hosting stays disabled until the Steinberg
//!   SDK licence is accepted for this project.
//!
//! Only [`clap`] contains `unsafe` code (FFI); the crate is
//! `deny(unsafe_op_in_unsafe_fn)` and every unsafe block states its
//! invariant. Nothing here copies code from any plugin SDK's host examples.
//!
//! ```
//! use superb_core::SampleRate;
//! use superb_graph::nodes::SineNode;
//! use superb_graph::{AudioGraph, BlockTime};
//! use superb_midi::MidiEventBuffer;
//! use superb_plugins::{ParamChange, PluginNode};
//!
//! # struct Gain { gain: f64, active: bool, desc: superb_plugins::PluginDescriptor }
//! # impl superb_plugins::PluginInstance for Gain {
//! #     fn descriptor(&self) -> &superb_plugins::PluginDescriptor { &self.desc }
//! #     fn ports(&self) -> superb_plugins::AudioPortLayout { superb_plugins::AudioPortLayout { inputs: 1, outputs: 1 } }
//! #     fn params(&self) -> &[superb_plugins::ParamInfo] { &[] }
//! #     fn param_value(&self, _id: u32) -> superb_core::Result<f64> { Ok(self.gain) }
//! #     fn set_param(&mut self, _id: u32, v: f64) -> superb_core::Result<()> { self.gain = v; Ok(()) }
//! #     fn activate(&mut self, _r: SampleRate, _m: usize) -> superb_core::Result<()> { self.active = true; Ok(()) }
//! #     fn deactivate(&mut self) { self.active = false; }
//! #     fn is_active(&self) -> bool { self.active }
//! #     fn reset(&mut self) {}
//! #     fn process(&mut self, _c: &superb_graph::ProcessContext<'_>, i: &superb_buffers::PlanarBuffer, o: &mut superb_buffers::PlanarBuffer) -> superb_graph::ProcessStatus {
//! #         o.copy_from(i).unwrap(); o.apply_gain(self.gain as f32); superb_graph::ProcessStatus::Normal }
//! #     fn latency_frames(&self) -> usize { 0 }
//! #     fn save_state(&mut self) -> superb_core::Result<Vec<u8>> { Ok(Vec::new()) }
//! #     fn load_state(&mut self, _b: &[u8]) -> superb_core::Result<()> { Ok(()) }
//! # }
//! # let instance = Box::new(Gain { gain: 1.0, active: false, desc: superb_plugins::PluginDescriptor::new(superb_plugins::PluginFormat::Clap, "x", "gain") });
//! // `instance` comes from `superb_plugins::clap::ClapBundle::instantiate` (or any
//! // other `PluginInstance`); wrap it in a node and place it in the graph.
//! let mut node = PluginNode::new(instance);
//! let mut params = node.take_param_sender().unwrap();
//!
//! let mut graph = AudioGraph::new();
//! let osc = graph.add_node(Box::new(SineNode::new(440.0, 1.0, 1)));
//! let fx = graph.add_node(Box::new(node));
//! graph.connect(osc, fx).unwrap();
//! graph.set_output(fx).unwrap();
//! graph.prepare(SampleRate::HZ_48K, 256).unwrap();
//!
//! params.push(ParamChange { id: 0, value: 0.5 }).unwrap(); // from the UI thread
//! graph.process(BlockTime::free_running(), 256, &MidiEventBuffer::with_capacity(0)).unwrap();
//! assert!(graph.output().peak() <= 0.5 + 1e-6);
//! ```

#![warn(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(feature = "clap")]
pub mod clap;
mod model;
mod node;
pub mod scan;
pub mod vst3;

pub use model::{
    AudioPortLayout, ParamChange, ParamInfo, PluginDescriptor, PluginFormat, PluginInstance,
};
pub use node::PluginNode;
