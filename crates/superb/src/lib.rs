//! # Superb SDK
//!
//! An open-source, permissively licensed (Apache-2.0 OR MIT) Rust toolkit
//! for building a digital audio workstation. This facade re-exports the
//! Phase 1 crates plus Phase 2 automation so an application can depend on `superb` alone:
//!
//! | module | crate | purpose |
//! |---|---|---|
//! | [`core`] | `superb-core` | sample-accurate time, beats, IDs, errors |
//! | [`buffers`] | `superb-buffers` | planar / interleaved `f32` blocks |
//! | [`rt`] | `superb-rt` | SPSC queues, atomics, no-alloc proofs, load meter |
//! | [`transport`] | `superb-transport` | playhead, tempo map, loop segmentation |
//! | [`midi`] | `superb-midi` | MIDI messages, block event buffer, midir bridge |
//! | [`graph`] | `superb-graph` | process graph, `Node` trait, fixture nodes |
//! | [`mixer`] | `superb-mixer` | tracks, buses, sends, pan, mute/solo, meters |
//! | [`automation`] | `superb-automation` | envelopes, read/touch/latch/write, sample-accurate apply |
//! | [`undo`] | `superb-undo` | coalescing undo/redo command stack, groups, clean-state tracking |
//! | [`session`] | `superb-session` | document model: tracks, items, markers, tempo map, JSON snapshots, undoable commands |
//! | [`plugins`] | `superb-plugins` | plugin instance model, `PluginNode`, CLAP host (`clap-sys`), VST3 stub |
//! | [`io`] | `superb-io` | engine loop, offline render, WAV, file decode/resample/bounce (`files`), cpal input/output |
//! | [`bridge`] | (facade) | session → mixer strip/bus map, strip-param mirror, output/send routing mirror |
//! | [`playback`] | (facade) | session item → strip wiring + offline region render |
//!
//! Feature flags: `devices` (cpal input/output), `midi-io` (midir input),
//! `hardware` (both), `files` (symphonia decode + rubato resampling +
//! bounce helpers in `io::files`; pure Rust, no hardware). All are off by
//! default so the workspace builds and tests without audio hardware or
//! ALSA headers. symphonia is MPL-2.0 and is imported by exactly one file
//! in the workspace (`superb-io/src/files/decode.rs`); see
//! `docs/superb-io.md`.
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
//! let audio = superb::io::offline::render(&mut engine, SampleRate::HZ_48K, 256, 48_000).unwrap();
//! assert_eq!(audio.frames(), 48_000);
//! assert!(audio.peak() > 0.1 && audio.peak() < 0.5);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod bridge;
pub mod playback;

pub use superb_buffers as buffers;
pub use superb_core as core;
pub use superb_graph as graph;
pub use superb_io as io;
pub use superb_midi as midi;
pub use superb_mixer as mixer;
pub use superb_automation as automation;
pub use superb_plugins as plugins;
pub use superb_rt as rt;
pub use superb_session as session;
pub use superb_transport as transport;
pub use superb_undo as undo;

/// The most commonly used items, for `use superb::prelude::*`.
pub mod prelude {
    pub use superb_buffers::{InterleavedBuffer, PlanarBuffer};
    pub use superb_core::{
        BeatPosition, BusId, ClipId, MarkerId, NodeId, Result, SampleRate, SendId, SuperbError, TimeDuration,
        TimePosition, TimeSignature, TrackId,
    };
    pub use superb_graph::nodes::{BufferPlayerNode, GainNode, MonoSynthNode, PassThroughNode, ScheduledNoteEvent, ScheduledSynthNode, SineNode};
    pub use superb_graph::{
        live_pair, AudioGraph, BlockProcessor, BlockTime, GraphCommand, GraphEditor, LiveGraph, MidiEdge, MidiPort,
        Node, NodeChannels, ProcessContext, ProcessStatus,
    };
    pub use superb_io::{Engine, EngineStats, MidiTiming};
    #[cfg(feature = "files")]
    pub use superb_io::files::AudioFile;
    pub use superb_midi::{MidiChannel, MidiEvent, MidiEventBuffer, MidiMessage, TimestampedMessage};
    pub use superb_mixer::{Mixer, PanLaw, SendParams, SendTap, StripParams};
    pub use superb_automation::{AutomationLane, AutomationMode, Envelope, EnvelopePoint};
    pub use superb_plugins::{PluginDescriptor, PluginInstance, PluginNode};
    pub use superb_rt::{AtomicF32, LoadMeter};
    pub use superb_session::{Item, ItemSource, Marker, Session, SessionSnapshot, Track, TrackKind, TrackSend};
    pub use superb_transport::{LoopRange, TempoMap, Transport, TransportState};
    pub use superb_undo::{CoalesceKey, Command, PropertyEdit, UndoStack};
    pub use crate::playback::{AudioCache, RenderOptions, SessionPlayer};
}

#[cfg(test)]
mod tests {
    use super::prelude::*;

    #[test]
    fn facade_exposes_every_crate() {
        let _ = SampleRate::HZ_48K;
        let _ = PlanarBuffer::new(1, 1);
        let _ = AtomicF32::new(0.0);
        let _ = Transport::new(SampleRate::CD, 100.0);
        let _ = MidiEventBuffer::with_capacity(1);
        let _ = AudioGraph::new();
        let _ = Mixer::new(2);
        let _ = AutomationMode::Read;
        let _ = crate::io::wav::WavFormat::Float32;
        let mut session = Session::new("facade", SampleRate::HZ_48K);
        let mut history = UndoStack::new();
        let track = session.allocate_track_id();
        history.push(&mut session, crate::session::commands::AddTrack::new(Track::with_id(track, "t", TrackKind::Audio))).unwrap();
        assert!(session.track(track).is_ok());
        history.undo(&mut session).unwrap();
        assert!(session.track(track).is_err());
        assert!(!crate::plugins::vst3::AVAILABLE);
        let _ = PluginDescriptor::new(crate::plugins::PluginFormat::Clap, "id", "name");
        let mut mixer = Mixer::new(2);
        let mut bridge = crate::bridge::SessionMixerBridge::new();
        assert_eq!(bridge.sync(&session, &mut mixer), 0);
        assert_eq!(bridge.apply_routing(&session, &mut mixer).unwrap(), 0);
        let _ = TrackSend::new(track);
    }

    #[cfg(feature = "files")]
    #[test]
    fn facade_exposes_files() {
        assert!(crate::io::FILES_FEATURE);
        let file = AudioFile { buffer: PlanarBuffer::new(1, 48), sample_rate: SampleRate::HZ_48K };
        let cd = file.resampled_to(SampleRate::CD).unwrap();
        assert_eq!(cd.frames(), crate::io::files::resampled_frames(48, SampleRate::HZ_48K, SampleRate::CD));
        assert!(crate::io::files::decode_bytes(vec![0u8; 64], None).is_err());
    }
}
