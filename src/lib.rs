//! # Superb
//!
//! An open-source, permissively licensed (Apache-2.0 OR MIT) Rust toolkit
//! for building a digital audio workstation.
//!
//! The crate has two tiers:
//!
//! * **Flows** (top-level modules such as [`play`]) gather the calls needed
//!   for one everyday task and document the primitive steps they are made of.
//! * **[`primitives`]** hold the engine: sample-accurate time, buffers,
//!   the real-time graph, the mixer, MIDI, automation, the project document
//!   with undo, plugin hosting and device / file I/O.
//!
//! Feature flags: `devices` (audio input and output through cpal),
//! `midi-io` (hardware MIDI input through midir), `hardware` (both) and
//! `files` (audio file decoding, resampling and bounce helpers). All are off
//! by default so the crate builds and tests without audio hardware.
//!
//! ```
//! use superb::prelude::*;
//!
//! // A stereo mixer with one track fed by a sine, rendered offline.
//! let mut mixer = Mixer::new(2);
//! let track = mixer.add_track("lead");
//! let osc = mixer.graph_mut().add_node(Box::new(SineNode::new(440.0, 0.5, 2)));
//! mixer.connect_source(osc, track).unwrap();
//! mixer.track_params(track).unwrap().set_gain_db(-6.0);
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

pub mod play;
pub mod primitives;

/// The most commonly used types and functions, for `use superb::prelude::*`.
pub mod prelude {
    pub use crate::play::{AudioCache, RenderOptions, SessionPlayer};
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
        Clip, ClipSource, Marker, Session, SessionSnapshot, Track, TrackKind, TrackSend,
    };
    pub use crate::primitives::transport::{LoopRange, TempoMap, Transport, TransportState};
    pub use crate::primitives::undo::{CoalesceKey, Command, PropertyEdit, UndoStack};
    pub use crate::primitives::{
        BeatPosition, BusId, ClipId, MarkerId, NodeId, Result, SampleRate, SendId, SuperbError,
        TimeDuration, TimePosition, TimeSignature, TrackId,
    };
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
        let mut session = Session::new("facade", SampleRate::HZ_48K);
        let mut history = UndoStack::new();
        let track = session.allocate_track_id();
        history
            .push(
                &mut session,
                crate::primitives::session::commands::AddTrack::new(Track::with_id(
                    track,
                    "t",
                    TrackKind::Audio,
                )),
            )
            .unwrap();
        assert!(session.track(track).is_ok());
        history.undo(&mut session).unwrap();
        assert!(session.track(track).is_err());
        let _ = crate::primitives::plugins::vst3::AVAILABLE;
        let _ = PluginDescriptor::new(crate::primitives::plugins::PluginFormat::Clap, "id", "name");
        let mut mixer = Mixer::new(2);
        let mut bridge = crate::play::SessionMixerBridge::new();
        assert_eq!(bridge.sync(&session, &mut mixer), 0);
        assert_eq!(bridge.apply_routing(&session, &mut mixer).unwrap(), 0);
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
