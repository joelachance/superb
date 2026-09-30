//! # superb-mixer
//!
//! A mixing console built on [`AudioGraph`](crate::primitives::graph::AudioGraph).
//!
//! * [`Mixer`] — owns a graph and wires **tracks**, **buses**, a **master**
//!   bus and **sends** into it. Implements
//!   [`BlockProcessor`](crate::primitives::graph::BlockProcessor) so it drops straight
//!   into `superb::primitives::io::Engine`.
//! * [`StripParams`] — the lock-free control surface of a track or bus:
//!   gain, pan, mute, solo and peak meters. Cloned `Arc`s go to the UI
//!   thread; the audio thread only reads atomics.
//! * [`SendParams`] — level and enable switch of a send.
//! * [`PanLaw`] — constant-power (−3 dB centre) or balance (0 dB centre).
//!
//! Every track is two graph nodes: an *input* summing point and a *strip*
//! (gain → pan → mute/solo → meter). Sources connect to the input node, so
//! pre-fader sends can tap it while post-fader sends tap the strip output.
//!
//! ```
//! use superb::primitives::SampleRate;
//! use superb::primitives::graph::nodes::ConstantNode;
//! use superb::primitives::graph::{BlockProcessor, BlockTime};
//! use superb::primitives::midi::MidiEventBuffer;
//! use superb::primitives::mixer::Mixer;
//!
//! let mut mixer = Mixer::new(2);
//! let drums = mixer.add_track("drums");
//! let src = mixer.graph_mut().add_node(Box::new(ConstantNode::new(0.5, 2)));
//! mixer.connect_source(src, drums).unwrap();
//! mixer.track_params(drums).unwrap().set_gain_db(-6.0206);
//!
//! mixer.prepare(SampleRate::HZ_48K, 64).unwrap();
//! mixer.process(BlockTime::free_running(), 64, &MidiEventBuffer::with_capacity(0)).unwrap();
//! let out = mixer.output();
//! // 0.5 * 0.5 gain * constant-power centre (0.7071)
//! assert!((out.channel(0)[63] - 0.1768).abs() < 1e-3);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod console;
mod params;
mod strip;

pub use console::{BusInfo, Mixer, SendInfo, SendTap, TrackInfo};
pub use params::{PanLaw, SendParams, StripParams};
pub use strip::{SendNode, StripNode};
