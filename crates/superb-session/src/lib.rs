//! # superb-session
//!
//! The document model of the Superb DAW SDK: what a project *is*, as
//! opposed to how it sounds (`superb-graph`/`superb-mixer`) or how it is
//! played (`superb-transport`/`superb-io`).
//!
//! * [`Session`] — name, sample rate, the [`TempoMap`](superb_transport::TempoMap),
//!   an optional loop range, ordered [`Track`]s holding [`Item`]s (clips)
//!   and [`TrackSend`]s, track → bus output routing, and timeline
//!   [`Marker`]s. Every object has a stable ID from `superb-core`; IDs are
//!   allocated by the session and never reused.
//! * [`SessionSnapshot`] — a versioned, `serde`-serialisable copy of the
//!   whole document. `Session::to_json` / `Session::from_json` and
//!   `save` / `load` go through it, so the file format is decoupled from
//!   the in-memory types.
//! * [`commands`] — undoable edits implementing
//!   [`superb_undo::Command<Session>`], with coalescing for drags.
//!
//! Positions and lengths are in samples (`TimePosition` / `TimeDuration`);
//! MIDI note content inside items is in beats relative to the item start.
//! The session never touches audio threads: everything here allocates
//! freely and is meant for the UI/document thread.
//!
//! ```
//! use superb_core::{SampleRate, TimeDuration, TimePosition};
//! use superb_session::{Item, ItemSource, Session, TrackKind};
//!
//! let mut session = Session::new("demo", SampleRate::HZ_48K);
//! let drums = session.add_track("drums", TrackKind::Audio);
//! let item = Item::new(
//!     "kick loop",
//!     TimePosition::from_samples(48_000),
//!     TimeDuration::from_samples(96_000),
//!     ItemSource::Audio { path: "kick.wav".into(), offset: TimeDuration::ZERO },
//! ).unwrap();
//! let clip = session.add_item(drums, item).unwrap();
//! session.add_marker("verse", TimePosition::from_samples(48_000));
//!
//! assert_eq!(session.item(clip).unwrap().end().samples(), 144_000);
//! assert_eq!(session.end_position().samples(), 144_000);
//!
//! let json = session.to_json().unwrap();
//! let copy = Session::from_json(&json).unwrap();
//! assert_eq!(copy.snapshot(), session.snapshot());
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod commands;
mod marker;
mod session;
mod snapshot;
mod track;

pub use marker::Marker;
pub use session::Session;
pub use snapshot::{
    ItemSnapshot, MarkerSnapshot, MeterSnapshot, SendSnapshot, SessionSnapshot, TempoPointSnapshot, TempoSnapshot,
    TrackSnapshot, FORMAT_VERSION,
};
pub use track::{Item, ItemSource, MidiNote, Track, TrackKind, TrackSend};
