//! Playing a song: the session-to-mixer bridge and offline clip playback.
//!
//! The verb-level API (start, stop, seek, loop, live re-wire) is built on
//! the two modules below.

pub mod bridge;
pub mod playback;

pub use bridge::{MixerBinding, SessionMixerBridge};
pub use playback::{
    render_region, schedule_midi_notes, AudioCache, PlaybackSource, RenderOptions, SessionPlayer,
};
