# superb-undo

Coalescing undo/redo command stack for document edits.

## Purpose

`superb-undo` sits in front of any document type `T` (typically
`superb::primitives::session::Session`) on the UI thread. It knows nothing about audio:
mutations happen through `Command::apply` / `revert`, and nothing here is
real-time safe.

Depends only on `superb-core` (for `Result` / `SuperbError`). No third-party
deps.

## Key types

* `Command<T>` — reversible edit: `apply`, `revert`, optional
  `CoalesceKey`, optional `merge` so a run of small edits collapses into
  one step. Downcasting helpers (`is` / `downcast_ref` / `downcast_mut` /
  `downcast`) live on `dyn Command<T>`.
* `CoalesceKey { kind, target }` — identifies a mergeable run (e.g. kind =
  "clip move", target = clip raw ID).
* `UndoStack<T>` — history: `push` / `push_boxed` / `push_at`, `undo` /
  `redo`, `seal` (end a coalesce run, e.g. on mouse-up),
  `begin_group` / `end_group` / `cancel_group`, `mark_clean` / `is_clean`,
  depth `limit`, optional coalesce time `window`.
* `PropertyEdit<T, V>` — ready-made "set this value" command from
  getter/setter closures; merges by keeping the first previous value and
  the latest new value.

## Usage

```rust
use superb::primitives::undo::{CoalesceKey, PropertyEdit, UndoStack};

struct Doc { gain_db: f32 }

let mut doc = Doc { gain_db: 0.0 };
let mut history = UndoStack::new();
let key = CoalesceKey::new(1, 0);
let edit = |db: f32| PropertyEdit::new(
    "Set gain",
    Some(key),
    db,
    |d: &Doc| d.gain_db,
    |d: &mut Doc, v| { d.gain_db = v; Ok(()) },
);

history.push(&mut doc, edit(-1.0)).unwrap();
history.push(&mut doc, edit(-2.0)).unwrap();
history.push(&mut doc, edit(-3.0)).unwrap();
assert_eq!(history.undo_len(), 1); // coalesced
assert_eq!(doc.gain_db, -3.0);

assert!(history.undo(&mut doc).unwrap());
assert_eq!(doc.gain_db, 0.0);
assert!(history.redo(&mut doc).unwrap());
assert_eq!(doc.gain_db, -3.0);
```

Contract notes:

* Capture "previous" state during `apply`, not at construction, so redo
  after intervening edits stays correct.
* A failed `apply` in `push` records nothing. A failed `revert`/`apply`
  during undo/redo clears the whole history (document state is unknown).
* Groups nest; only the outermost label becomes the undo step. Undo/redo
  while a group is open is refused.

## Real-time notes

Not RT-safe. Pair with RT-safe dirty flags later if the audio thread needs
to know the document changed; the stack itself stays off-thread.

## Limitations / next

* No persistent undo history across process restarts (session snapshots
  are the document; the stack is ephemeral).
* The crate is `#![forbid(unsafe_code)]`: downcasting goes through
  `dyn Any` (trait upcasting, Rust 1.86+).
* Session-facing commands live in `superb::primitives::session::commands`, not here.
