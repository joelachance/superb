//! # Superb
//!
//! An open-source, permissively licensed (Apache-2.0 OR MIT) Rust toolkit
//! for building a digital audio workstation.
//!
//! The crate has two tiers, so you always know what a call will do:
//!
//! * **Flows** are top-level modules named after everyday tasks:
//!   [`session`], [`track`], [`transport`], [`mix`], [`midi`], [`record`], [`plugins`],
//!   [`export`] and [`devices`]. Each is a set of free functions over a
//!   [`Session`] (the state of a document in progress), and every function's docs
//!   end with a "Made of:" list of the primitive calls it performs, so you
//!   can call those yourself when the flow is not quite what you want.
//! * **[`primitives`]** are the engine: sample-accurate time, buffers, the
//!   real-time graph, the mixer, MIDI, automation, the document document
//!   with undo, plugin hosting and device / file I/O. They know nothing
//!   about flows.
//!
//! Feature flags: `clap` (CLAP plugin hosting, on by default), `devices`
//! (audio input and output through cpal), `midi-io` (hardware MIDI input
//! through midir), `hardware` (both) and `files` (audio file decoding,
//! resampling and bounce helpers). Hardware and codecs are off by default
//! so the crate builds and tests without audio hardware.
//!
//! A document from nothing to sound, without an audio device:
//!
//! ```
//! use superb::prelude::*;
//! use superb::{session, track, transport};
//!
//! let mut session = session::create("demo", SampleRate::HZ_48K, 256)?;
//! let lead = track::add(&mut session, "lead", TrackKind::Audio)?;
//!
//! // Clips point at audio in the cache: a decoded file, or a buffer you made.
//! let mut tone = PlanarBuffer::new(2, 4_800);
//! tone.fill(0.25);
//! session.cache.insert("tone.wav", tone);
//! session::add_audio_clip(&mut session, lead, "tone.wav", TimePosition::ZERO, TimeDuration::from_samples(4_800))?;
//! track::volume(&mut session, lead, -6.0)?;
//!
//! transport::start(&mut session)?;                    // builds the mixer, starts playing
//! let audio = session::steps::render_blocks(&mut session, 4)?;   // renders 4 blocks of 256 frames
//! assert_eq!(audio.frames(), 1_024);
//! assert!(audio.peak() > 0.05);
//! # Ok::<(), SuperbError>(())
//! ```
//!
//! The same thing from the primitives, bypassing the flows entirely:
//!
//! ```
//! use superb::prelude::*;
//!
//! let mut mixer = Mixer::new(2);
//! let track = mixer.add_track("lead");
//! let osc = mixer.graph_mut().add_node(Box::new(SineNode::new(440.0, 0.5, 2)));
//! mixer.connect_source(osc, track).unwrap();
//! mixer.track_params(track).unwrap().set_volume_db(-6.0);
//!
//! let mut engine = Engine::new(mixer, Transport::new(SampleRate::HZ_48K, 120.0));
//! engine.transport_mut().play();
//! let audio = superb::primitives::io::offline::render(&mut engine, SampleRate::HZ_48K, 256, 48_000).unwrap();
//! assert_eq!(audio.frames(), 48_000);
//! assert!(audio.peak() > 0.1 && audio.peak() < 0.5);
//! ```

#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod devices;
pub mod export;
pub mod midi;
pub mod mix;
pub mod plugins;
pub mod primitives;
pub mod record;
pub mod session;
pub mod track;
pub mod transport;

pub use session::Session;

/// The most commonly used types and functions, for `use superb::prelude::*`.
pub mod prelude {
    pub use crate::primitives::automation::{
        AutomationLane, AutomationMode, Envelope, EnvelopePoint,
    };
    pub use crate::primitives::buffers::{InterleavedBuffer, PlanarBuffer};
    pub use crate::primitives::graph::nodes::{
        BufferPlayerNode, GainNode, MonoSynthNode, PassThroughNode, ScheduledNoteEvent,
        ScheduledSynthNode, SineNode,
    };
    pub use crate::primitives::graph::{
        live_pair, AudioGraph, BlockProcessor, BlockTime, GraphCommand, GraphEditor, LiveGraph,
        MidiEdge, MidiPort, Node, NodeChannels, ProcessContext, ProcessStatus,
    };
    pub use crate::primitives::io::cache::AudioCache;
    #[cfg(feature = "files")]
    pub use crate::primitives::io::files::AudioFile;
    pub use crate::primitives::io::{Engine, EngineStats, MidiTiming};
    pub use crate::primitives::midi::{
        MidiChannel, MidiEvent, MidiEventBuffer, MidiMessage, TimestampedMessage,
    };
    pub use crate::primitives::mixer::{Mixer, PanLaw, SendParams, SendTap, StripParams};
    pub use crate::primitives::plugins::{PluginDescriptor, PluginInstance, PluginNode};
    pub use crate::primitives::realtime::{AtomicF32, LoadMeter};
    pub use crate::primitives::session::{
        Clip, ClipSource, Document, DocumentSnapshot, Marker, Track, TrackKind, TrackSend,
    };
    pub use crate::primitives::transport::{LoopRange, TempoMap, Transport, TransportState};
    pub use crate::primitives::undo::{CoalesceKey, Command, PropertyEdit, UndoStack};
    pub use crate::primitives::{
        BeatPosition, BusId, ClipId, MarkerId, NodeId, Result, SampleRate, SendId, SuperbError,
        TimeDuration, TimePosition, TimeSignature, TrackId,
    };
    pub use crate::session::Session;
    pub use crate::session::Wiring;
}

#[cfg(test)]
mod tests {
    use super::prelude::*;

    #[test]
    fn prelude_reaches_every_primitive() {
        let _ = SampleRate::HZ_48K;
        let _ = PlanarBuffer::new(1, 1);
        let _ = AtomicF32::new(0.0);
        let _ = Transport::new(SampleRate::CD, 100.0);
        let _ = MidiEventBuffer::with_capacity(1);
        let _ = AudioGraph::new();
        let _ = Mixer::new(2);
        let _ = AutomationMode::Read;
        let _ = crate::primitives::io::wav::WavFormat::Float32;
        let mut document = Document::new("facade", SampleRate::HZ_48K);
        let mut history = UndoStack::new();
        let track = document.allocate_track_id();
        history
            .push(
                &mut document,
                crate::primitives::session::commands::AddTrack::new(Track::with_id(
                    track,
                    "t",
                    TrackKind::Audio,
                )),
            )
            .unwrap();
        assert!(document.track(track).is_ok());
        history.undo(&mut document).unwrap();
        assert!(document.track(track).is_err());
        let _ = crate::primitives::plugins::vst3::AVAILABLE;
        let _ = PluginDescriptor::new(crate::primitives::plugins::PluginFormat::Clap, "id", "name");
        let mut mixer = Mixer::new(2);
        let mut bridge = crate::session::bridge::MixerBridge::new();
        assert_eq!(bridge.bind(&document, &mut mixer), 0);
        assert_eq!(bridge.apply_routing(&document, &mut mixer).unwrap(), 0);
        let _ = TrackSend::new(track);
    }

    #[cfg(feature = "files")]
    #[test]
    fn files_feature_is_wired_through() {
        let _ = crate::primitives::io::FILES_FEATURE;
        let file = AudioFile {
            buffer: PlanarBuffer::new(1, 48),
            sample_rate: SampleRate::HZ_48K,
        };
        let cd = file.resampled_to(SampleRate::CD).unwrap();
        assert_eq!(
            cd.frames(),
            crate::primitives::io::files::resampled_frames(48, SampleRate::HZ_48K, SampleRate::CD)
        );
        assert!(crate::primitives::io::files::decode_bytes(vec![0u8; 64], None).is_err());
    }
}
