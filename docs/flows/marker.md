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
