# primitives::session

The session's memory: what a session *is* and remembers.

## Purpose

`primitives::session` depends on the `primitives` root,
`primitives::transport` (for `TempoMap` and `LoopRange`), `primitives::undo`,
and `serde` + `serde_json` for snapshots. It holds the editable description of
a session — tracks, clips, markers, tempo map, loop range — and nothing about
rendering. Turning a `Memory` into a mixer is the job of `session::steps::rebuild` (see
`docs/flows/session.md`); the session is deliberately free of `Arc<AtomicF32>`
handles and audio-thread concerns, so it can be cloned, diffed, serialised and
undone freely.

## Key types

State: each object keeps its settable fields under one `state`:
`Track.state: TrackState { volume_db, pan, mute, solo, arm, monitor, input }`,
`Clip.state: ClipState { gain_db, mute, fade_in, fade_out, archived }`,
`TrackSend.state: SendState { level_db, enabled }`, `Insert.state:
InsertState { bypass, params }` (the plugin's own saved bytes are
`Insert.preset`). Snapshots accept the older flat field names.

Inserts: a track's plugin chain is part of the memory. `Insert`
(`PluginRef` = format, id, path; `bypassed`; `params: Vec<(u32, f64)>`;
`state: Vec<u8>`) lives in `Track::inserts()`, is saved in the snapshot,
and is edited through the `AddInsert` / `RemoveInsert` / `MoveInsert`
commands and the coalescing `set_insert_bypass` / `set_insert_param`
property edits. `Memory::allocate_insert_id` mints IDs.


* `Memory` — name, `SampleRate`, `TempoMap`, `Option<LoopRange>`, ordered
  `Track`s, position-sorted `Marker`s, a per-session ID counter and a
  `revision()` bumped by every mutation. IDs are never reused; removed
  objects can be re-inserted under their original IDs (that is how undo
  works).
* `Track` — `TrackKind::{Audio, Midi, Bus}`, name, colour, gain/pan,
  mute/solo/arm, start-sorted `Clip`s (`Bus` tracks hold no clips), an
  `output()` (the bus track it feeds, `None` = master) and `sends()`.
* `TrackSend` — `SendId`, destination bus track (fixed at creation),
  `level_db`, `pre_fader`, `enabled`. Only audio/MIDI tracks send; only
  bus tracks receive. `Memory::set_track_output` / `add_send` validate
  the target kind and reject bus → bus cycles; removing a bus detaches
  everything routed into it (`tracks_routed_to` / `sends_to` let you
  capture that first — `commands::RemoveTrack` does).
* `Clip` — a `ClipId`, `start: TimePosition`, `length: TimeDuration` (> 0),
  an `ClipSource::{Empty, Audio { path, offset }, Midi { notes }}`, gain,
  mute, fades. `MidiNote` content is in beats relative to the clip start.
* `Marker` — `MarkerId`, name, position, colour.
* `MemorySnapshot` (+ `TrackSnapshot`, `ClipSnapshot`, `MarkerSnapshot`,
  `TempoSnapshot`) — primitive-only, `serde` types with a `format_version`.
  `Memory::snapshot` / `from_snapshot` / `to_json` / `from_json` /
  `save` / `load`.
* `commands` — `aura::primitives::undo::Command<Session>` implementations: `AddTrack`,
  `RemoveTrack`, `MoveTrack`, `SetTrackOutput`, `AddSend`, `RemoveSend`,
  `AddClip`, `RemoveClip`, `MoveClip`, `ResizeClip`, `AddMarker`,
  `RemoveMarker`, `MoveMarker`, `SetTempo`, `SetLoopRange`, plus
  `PropertyEdit` helpers (`set_track_volume_db`, `set_track_pan`,
  `set_clip_name`, `set_send_level_db`, `set_send_enabled`,
  `set_send_pre_fader`, ...). Drag-style commands coalesce.

## Usage

```rust
use aura::primitives::{SampleRate, TimeDuration, TimePosition};
use aura::primitives::session::commands::{AddClip, AddTrack, MoveClip};
use aura::primitives::session::{Clip, ClipSource, Session, Track, TrackKind};
use aura::primitives::undo::UndoStack;

let mut session = Memory::new("demo", SampleRate::HZ_48K);
let mut history = UndoStack::new();

// Allocate IDs first so you know them before the stack takes the command.
let track = session.allocate_track_id();
history.push(&mut session, AddTrack::new(Track::with_id(track, "vox", TrackKind::Audio))).unwrap();

let clip = session.allocate_clip_id();
let take = Clip::with_id(clip, "take 1", TimePosition::ZERO, TimeDuration::from_samples(1_000), ClipSource::Empty).unwrap();
history.push(&mut session, AddClip::new(track, take)).unwrap();

// A drag: many small moves, one undo step.
for x in [100, 200, 300] {
    history.push(&mut session, MoveClip::new(clip, TimePosition::from_samples(x))).unwrap();
}
history.seal();
assert_eq!(history.undo_len(), 3);

history.undo(&mut session).unwrap();                 // back to 0
history.undo(&mut session).unwrap();                 // clip gone
history.redo(&mut session).unwrap();                 // back under the same ClipId
assert!memory.clip(clip).is_ok());

let json = session.to_json().unwrap();
let copy = Memory::from_json(&json).unwrap();
assert_eq!(copy.snapshot(), session.snapshot());
```

Direct (non-undoable) editing uses the same verbs on `Memory`:
`add_track`, `remove_track`, `move_track`, `track_mut`, `add_clip`,
`move_clip`, `move_clip_to_track`, `resize_clip`, `clip_mut`,
`add_marker`, `move_marker`, `tempo_map_mut`, `set_loop_range`,
`set_track_output`, `add_send`, `remove_send`, `send_mut`.

Routing lives in the memory too:

```rust
use aura::primitives::SampleRate;
use aura::primitives::session::commands::{AddSend, SetTrackOutput, set_send_level_db};
use aura::primitives::session::{Session, TrackKind, TrackSend};
use aura::primitives::undo::UndoStack;

let mut session = Memory::new("routing", SampleRate::HZ_48K);
let mut history = UndoStack::new();
let vox = session.add_track("vox", TrackKind::Audio);
let drums = session.add_track("drums", TrackKind::Bus);
let verb = session.add_track("verb", TrackKind::Bus);

history.push(&mut session, SetTrackOutput::new(vox, Some(drums))).unwrap();
assert!memory.set_track_output(drums, Some(drums)).is_err());        // cycle
assert!memory.set_track_output(vox, Some(vox)).is_err());            // not a bus

let send = session.allocate_send_id();
history.push(&mut session, AddSend::new(vox, TrackSend::with_id(send, verb).pre_fader())).unwrap();
history.push(&mut session, set_send_level_db(send, -12.0)).unwrap();
assert_eq!(memory.send(send).unwrap().state.level_db, -12.0);

// Removing the verb bus drops the send; undo puts both back.
history.push(&mut session, aura::primitives::session::commands::RemoveTrack::new(verb)).unwrap();
assert_eq!(session.send_count(), 0);
history.undo(&mut session).unwrap();
assert_eq!(session.send_track(send).unwrap(), vox);
```

## File format

`to_json` writes a `MemorySnapshot`: `format_version` (currently 1), name,
`sample_rate_hz`, tempo points (`beat`, `bpm`, `ramp` — the ramp flag is
optional on read and defaults to `false`) and meters (`bar`,
`numerator`, `denominator`), loop range as `[start, end)` samples, tracks
with their clips, `output` (raw bus track ID or `null`) and `sends`
(`id`, `bus`, `level_db`, `pre_fader`, `enabled`), markers, and `next_id`.
`output` and `sends` are optional on read (files written before routing
existed load with master routing and no sends). Loading validates the
sample rate, tempo/meter values, clip lengths, ID uniqueness (IDs of 0
are rejected), that bus tracks carry no clips or sends, that routing
targets are bus tracks with no bus → bus cycle (routing is applied after
every track exists, so a track may feed a bus listed later), and refuses
files with a newer `format_version`. A stale `next_id` is corrected from
the IDs present.

## Real-time notes

None — nothing in this module is meant for the audio thread. Every mutating
method may allocate. Keep the session on the UI/memory thread and push
derived parameter changes to the engine through `Arc<AtomicF32>` handles
or `aura::primitives::graph::live`.

## Limitations / next

* Clips are positioned in samples; there is no "beat-locked" clip mode
  that follows tempo changes (convert through `Memory::tempo_map()`).
* No per-clip automation lanes yet (`primitives::automation` lanes are not
  owned by the session).
* Routing is one output per track plus sends to buses; there are no
  sidechain inputs, no per-send pan, and a send's destination cannot be
  changed in place (remove + add).
* No project-relative path resolution for `ClipSource::Audio`.

See `examples/session_undo` for the stack driving a session memory,
saving it as JSON and reloading it.

## From memory to mixer

The `Memory` and the live [`Mixer`](mixer.md) use separate `TrackId` /
`SendId` namespaces. `aura::session::steps::build_mixer` builds a fresh
mixer from a `Memory` (a strip per track, a node per clip) and returns the
live controls keyed by the memory's ids; `aura::export` runs such a mixer
offline. See `docs/flows/session.md` and the `session_steps` example.

