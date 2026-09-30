//! # superb-session
//!
//! The document model of the Superb DAW SDK: what a project *is*, as
//! opposed to how it sounds (`superb-graph`/`superb-mixer`) or how it is
//! played (`superb-transport`/`superb-io`).
//!
//! * [`Session`] — name, sample rate, the [`TempoMap`](crate::primitives::transport::TempoMap),
//!   an optional loop range, ordered [`Track`]s holding [`Clip`]s,
//!   [`TrackSend`]s and [`Insert`]s (the effects on the track), track → bus
//!   output routing, and timeline [`Marker`]s. Every object has a stable ID
//!   from the primitives root; IDs are allocated by the session and never
//!   reused.
//! * [`SessionSnapshot`] — a versioned, `serde`-serialisable copy of the
//!   whole document. `Session::to_json` / `Session::from_json` and
//!   `save` / `load` go through it, so the file format is decoupled from
//!   the in-memory types.
//! * [`commands`] — undoable edits implementing
//!   [`Command<Session>`](crate::primitives::undo::Command), with coalescing for drags.
//!
//! Positions and lengths are in samples (`TimePosition` / `TimeDuration`);
//! MIDI note content inside clips is in beats relative to the clip start.
//! The session never touches audio threads: everything here allocates
//! freely and is meant for the UI/document thread.
//!
//! ```
//! use superb::primitives::{SampleRate, TimeDuration, TimePosition};
//! use superb::primitives::session::{Clip, ClipSource, Session, TrackKind};
//!
//! let mut session = Session::new("demo", SampleRate::HZ_48K);
//! let drums = session.add_track("drums", TrackKind::Audio);
//! let take = Clip::new(
//!     "kick loop",
//!     TimePosition::from_samples(48_000),
//!     TimeDuration::from_samples(96_000),
//!     ClipSource::Audio { path: "kick.wav".into(), offset: TimeDuration::ZERO },
//! ).unwrap();
//! let clip = session.add_clip(drums, take).unwrap();
//! session.add_marker("verse", TimePosition::from_samples(48_000));
//!
//! assert_eq!(session.clip(clip).unwrap().end().samples(), 144_000);
//! assert_eq!(session.end_position().samples(), 144_000);
//!
//! let json = session.to_json().unwrap();
//! let copy = Session::from_json(&json).unwrap();
//! assert_eq!(copy.snapshot(), session.snapshot());
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod commands;
mod document;
mod insert;
mod marker;
mod snapshot;
mod track;

pub use document::Session;
pub use insert::{Insert, PluginRef};
pub use marker::Marker;
pub use snapshot::{
    ClipSnapshot, InsertSnapshot, MarkerSnapshot, MeterSnapshot, SendSnapshot, SessionSnapshot,
    TempoPointSnapshot, TempoSnapshot, TrackSnapshot, FORMAT_VERSION,
};
pub use track::{Clip, ClipSource, MidiNote, Track, TrackKind, TrackSend};
