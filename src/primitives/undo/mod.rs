//! # primitives::undo
//!
//! A coalescing undo/redo command stack for the Superb DAW SDK.
//!
//! * [`Command`] — a reversible edit of some document type `T`: `apply`,
//!   `revert`, an optional [`CoalesceKey`] and an optional `merge` so a run
//!   of small edits (a fader drag, typing a name) collapses into one step.
//! * [`UndoStack`] — the history: `push` applies and records, `undo`/`redo`
//!   walk it, `begin_group`/`end_group` bundle several commands into one
//!   step, `mark_clean`/`is_clean` track the saved state, and a depth limit
//!   bounds memory.
//! * [`PropertyEdit`] — a ready-made command for "set this value" edits
//!   expressed with getter/setter closures.
//!
//! The stack knows nothing about audio; it is meant to sit in front of the
//! session document (`primitives::session`) on the UI thread. Nothing here is
//! real-time safe and nothing needs to be.
//!
//! ```
//! use superb::primitives::undo::{CoalesceKey, PropertyEdit, UndoStack};
//!
//! struct Doc { gain_db: f32 }
//!
//! let mut doc = Doc { gain_db: 0.0 };
//! let mut history = UndoStack::new();
//! let key = CoalesceKey::new(1, 0);
//! let edit = |db: f32| PropertyEdit::new("Set gain", Some(key), db, |d: &Doc| d.gain_db, |d: &mut Doc, v| { d.gain_db = v; Ok(()) });
//!
//! // A fader drag: three pushes with the same key become a single undo step.
//! history.push(&mut doc, edit(-1.0)).unwrap();
//! history.push(&mut doc, edit(-2.0)).unwrap();
//! history.push(&mut doc, edit(-3.0)).unwrap();
//! assert_eq!(history.undo_len(), 1);
//! assert_eq!(doc.gain_db, -3.0);
//!
//! assert!(history.undo(&mut doc).unwrap());
//! assert_eq!(doc.gain_db, 0.0);
//! assert!(history.redo(&mut doc).unwrap());
//! assert_eq!(doc.gain_db, -3.0);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod command;
mod stack;

pub use command::{CoalesceKey, Command, PropertyEdit};
pub use stack::UndoStack;
