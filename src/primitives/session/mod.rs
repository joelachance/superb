//! # primitives::session
//!
//! The document model of the Superb DAW SDK: what a session *is*, as
//! opposed to how it sounds (`primitives::graph` / `primitives::mixer`) or how it is
//! played (`primitives::transport` / `primitives::io`).
//!
//! * [`Document`] — name, sample rate, the [`TempoMap`](crate::primitives::transport::TempoMap),
//!   an optional loop range, ordered [`Track`]s holding [`Clip`]s,
//!   [`TrackSend`]s and [`Insert`]s (the effects on the track), track → bus
//!   output routing, and timeline [`Marker`]s. Every object has a stable ID
//!   from the primitives root; IDs are allocated by the document and never
//!   reused.
//! * [`DocumentSnapshot`] — a versioned, `serde`-serialisable copy of the
//!   whole document. `Document::to_json` / `Document::from_json` and
//!   `save` / `load` go through it, so the file format is decoupled from
//!   the in-memory types.
//! * [`commands`] — undoable edits implementing
//!   [`Command<Document>`](crate::primitives::undo::Command), with coalescing for drags.
//!
//! Positions and lengths are in samples (`TimePosition` / `TimeDuration`);
//! MIDI note content inside clips is in beats relative to the clip start.
//! The document never touches audio threads: everything here allocates
//! freely and is meant for the UI/document thread.
//!
//! ```
//! use superb::primitives::{SampleRate, TimeDuration, TimePosition};
//! use superb::primitives::session::{Clip, ClipSource, Document, TrackKind};
//!
//! let mut document = Document::new("demo", SampleRate::HZ_48K);
//! let drums = document.add_track("drums", TrackKind::Audio);
//! let take = Clip::new(
//!     "kick loop",
//!     TimePosition::from_samples(48_000),
//!     TimeDuration::from_samples(96_000),
//!     ClipSource::Audio { path: "kick.wav".into(), offset: TimeDuration::ZERO },
//! ).unwrap();
//! let clip = document.add_clip(drums, take).unwrap();
//! document.add_marker("verse", TimePosition::from_samples(48_000));
//!
//! assert_eq!(document.clip(clip).unwrap().end().samples(), 144_000);
//! assert_eq!(document.end_position().samples(), 144_000);
//!
//! let json = document.to_json().unwrap();
//! let copy = Document::from_json(&json).unwrap();
//! assert_eq!(copy.snapshot(), document.snapshot());
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod commands;
mod document;
mod insert;
mod marker;
mod snapshot;
mod track;

pub use document::Document;
pub use insert::{Insert, InsertState, PluginRef};
pub use marker::Marker;
pub use snapshot::{
    ClipSnapshot, DocumentSnapshot, InsertSnapshot, MarkerSnapshot, MeterSnapshot, SendSnapshot,
    TempoPointSnapshot, TempoSnapshot, TrackSnapshot, FORMAT_VERSION,
};
pub use track::{Clip, ClipSource, MidiNote, SendState, Track, TrackKind, TrackSend, TrackState};
