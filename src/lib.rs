//! # superb
//!
//! An open-source, permissively licensed (Apache-2.0 OR MIT) Rust toolkit
//! for building a digital audio workstation.
//!
//! The crate has two tiers, so you always know what a call will do:
//!
//! * **Flows** are top-level modules named after everyday tasks:
//!   [`session`], [`track`], [`clip`], [`transport`], [`send`], [`midi`],
//!   [`record`], [`metronome`], [`export`] and [`devices`]. Each is a set of free functions over a
//!   [`session::Session`] (its memory, undo history, audio and running engine), and every function's docs
//!   end with a "Made of:" list of the primitive calls it performs, so you
//!   can call those yourself when the flow is not quite what you want.
//! * **[`primitives`]** are the engine: sample-accurate time, buffers, the
//!   real-time graph, the mixer, MIDI, automation, the memory with undo,
//!   plugin hosting and device / file I/O. They know nothing
//!   about flows.
//!
//! Feature flags: `clap` (CLAP plugin hosting, on by default), `devices`
//! (audio input and output through cpal), `midi-io` (hardware MIDI input
//! through midir), `hardware` (both) and `files` (audio file decoding,
//! resampling and bounce helpers). Hardware and codecs are off by default
//! so the crate builds and tests without audio hardware.
//!
//! A memory from nothing to sound, without an audio device:
//!
//! ```
//! use superb::primitives::buffers::PlanarBuffer;
//! use superb::primitives::session::TrackKind;
//! use superb::primitives::{SuperbError, SampleRate, TimePosition};
//! use superb::{clip, session, track, transport};
//!
//! let folder = std::env::temp_dir().join("demo.superb");   // a session is a folder
//! let mut session = session::create(&folder, SampleRate::HZ_48K, 256)?;
//! let lead = track::add(&mut session, "lead", TrackKind::Audio)?;
//!
//! // Clips name audio held in memory: a decoded file, or a buffer you made.
//! let mut tone = PlanarBuffer::new(2, 4_800);
//! tone.fill(0.25);
//! session.audio.insert("tone.wav", tone);
//! clip::add(&mut session, lead, "tone.wav", TimePosition::ZERO)?;   // as long as the audio
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
//! use superb::primitives::graph::nodes::SineNode;
//! use superb::primitives::io::Engine;
//! use superb::primitives::mixer::Mixer;
//! use superb::primitives::transport::Transport;
//! use superb::primitives::SampleRate;
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

pub mod clip;
pub mod devices;
pub mod export;
pub mod marker;
pub mod metronome;
pub mod midi;
pub mod primitives;
pub mod record;
pub mod send;
pub mod session;
pub mod track;
pub mod transport;

#[cfg(test)]
mod tests {
    #[cfg(feature = "files")]
    #[test]
    fn files_feature_is_wired_through() {
        use crate::primitives::buffers::PlanarBuffer;
        use crate::primitives::io::files::AudioFile;
        use crate::primitives::SampleRate;

        let _ = crate::primitives::io::FILES_FEATURE;
        let file = AudioFile {
            buffer: PlanarBuffer::new(1, 48),
            sample_rate: SampleRate::HZ_48K,
            bits_per_sample: Some(32),
        };
        let cd = file.resampled_to(SampleRate::CD).unwrap();
        assert_eq!(
            cd.frames(),
            crate::primitives::io::files::resampled_frames(48, SampleRate::HZ_48K, SampleRate::CD)
        );
        assert!(crate::primitives::io::files::decode_bytes(vec![0u8; 64], None).is_err());
    }
}
