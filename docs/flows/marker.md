# marker

Named places on the timeline.

## Purpose

A marker is a name at a position, nothing more: it plays nothing and
moves nothing. Add one, move it, rename it, remove it. Every change is an
undo step; nothing here rebuilds the mixer. The markers live in the
memory (`session.memory.markers()`, in position order) and are saved with
the session.

## Verbs

| verb | does | undo |
|---|---|---|
| `add(session, name, at) -> MarkerId` | a marker at `at` | one |
| `move_to(session, marker, at)` | to another position | drag (moves within `Session::COALESCE_WINDOW` merge) |
| `rename(session, marker, name)` | | quick renames merge |
| `remove(session, marker)` | gone; undo puts it back under its id | one |

## Reference

Every function of the `marker` flow: what it takes, what it returns, what it refuses, and how it undoes. A `Result<T>` is `Ok(T)` or a `SuperbError`; the errors listed are the ones you can cause.

### `add`

```rust
pub fn add(session: &mut Session, name: &str, at: TimePosition) -> Result<MarkerId>
```

Drops a named marker at `at`. One undo step.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `name: &str` — the new name
- `at: TimePosition` — a position on the timeline, in samples (`TimePosition::from_seconds` converts)

**Returns** `Result<MarkerId>` — the new marker's id

**Errors** none in practice.

**Undo** one undo step.

**Made of** `Memory::allocate_marker_id` → `Marker::with_id` → `AddMarker` through `session::edit`.

**Example**

```rust
let verse = marker::add(&mut session, "verse", TimePosition::from_seconds(8.0, rate))?;
```

### `move_to`

```rust
pub fn move_to(session: &mut Session, marker: MarkerId, at: TimePosition) -> Result<()>
```

Moves a marker to `at`. Moves within `Session::COALESCE_WINDOW` are one undo
step, so a drag undoes in one.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `marker: MarkerId` — the marker's id, from `marker::add` or `session.memory.markers()`
- `at: TimePosition` — a position on the timeline, in samples (`TimePosition::from_seconds` converts)

**Returns** `Result<()>` — nothing

**Errors** an unknown marker.

**Undo** one undo step per drag: calls within `Session::COALESCE_WINDOW` (half a second) merge.

**Made of** `MoveMarker` through `session::edit`.

**Example**

```rust
marker::move_to(&mut session, verse, TimePosition::from_seconds(8.5, rate))?;
```

### `rename`

```rust
pub fn rename(session: &mut Session, marker: MarkerId, name: &str) -> Result<()>
```

Renames a marker. Quick successive renames of one marker are one undo step.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `marker: MarkerId` — the marker's id, from `marker::add` or `session.memory.markers()`
- `name: &str` — the new name

**Returns** `Result<()>` — nothing

**Errors** an unknown marker.

**Undo** one undo step per drag: calls within `Session::COALESCE_WINDOW` (half a second) merge.

**Made of** `set_marker_name` through `session::edit`.

**Example**

```rust
marker::rename(&mut session, verse, "verse 1")?;
```

### `remove`

```rust
pub fn remove(session: &mut Session, marker: MarkerId) -> Result<()>
```

Removes a marker; undo puts it back under its id.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `marker: MarkerId` — the marker's id, from `marker::add` or `session.memory.markers()`

**Returns** `Result<()>` — nothing

**Errors** an unknown marker.

**Undo** one undo step.

**Made of** `RemoveMarker` through `session::edit`.

**Example**

```rust
marker::remove(&mut session, verse)?;
```

## Usage

```rust
use superb::prelude::*;
use superb::{marker, session};

let verse = marker::add(&mut session, "verse", TimePosition::from_seconds(8.0, rate))?;
marker::move_to(&mut session, verse, TimePosition::from_seconds(8.5, rate))?;
marker::rename(&mut session, verse, "verse 1")?;
for m in session.memory.markers() {
    println!("{} at {}", m.name, m.position().to_seconds(rate));
}
marker::remove(&mut session, verse)?;
```

## Made of

`AddMarker`, `MoveMarker`, `RemoveMarker` and `set_marker_name` from
`primitives::session::commands`, each through `session::edit`;
`Memory::allocate_marker_id` for the id, `Memory::markers`,
`next_marker` and `previous_marker` to read them.

## Notes

* Markers have a colour (`Marker.color`) the memory keeps but no verb
  sets yet.
* `Memory::next_marker(position)` and `previous_marker(position)` are the
  primitives a "jump to marker" button is made of.
