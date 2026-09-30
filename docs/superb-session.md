# superb-session

The document model: what a project *is*.

## Purpose

`superb-session` depends on `superb-core`, `superb-transport` (for
`TempoMap` and `LoopRange`), `superb-undo`, and `serde` + `serde_json` for
snapshots. It holds the editable description of a
project — tracks, items (clips), markers, tempo map, loop range — and
nothing about rendering. Turning a `Session` into a `superb-mixer` /
`superb-graph` topology is the application's (or a later crate's) job; the
session is deliberately free of `Arc<AtomicF32>` handles and audio-thread
concerns, so it can be cloned, diffed, serialised and undone freely.

## Key types

* `Session` — name, `SampleRate`, `TempoMap`, `Option<LoopRange>`, ordered
  `Track`s, position-sorted `Marker`s, a per-session ID counter and a
  `revision()` bumped by every mutation. IDs are never reused; removed
  objects can be re-inserted under their original IDs (that is how undo
  works).
* `Track` — `TrackKind::{Audio, Midi, Bus}`, name, colour, gain/pan,
  mute/solo/arm, start-sorted `Item`s (`Bus` tracks hold no items), an
  `output()` (the bus track it feeds, `None` = master) and `sends()`.
* `TrackSend` — `SendId`, destination bus track (fixed at creation),
  `level_db`, `pre_fader`, `enabled`. Only audio/MIDI tracks send; only
  bus tracks receive. `Session::set_track_output` / `add_send` validate
  the target kind and reject bus → bus cycles; removing a bus detaches
  everything routed into it (`tracks_routed_to` / `sends_to` let you
  capture that first — `commands::RemoveTrack` does).
* `Item` — a `ClipId`, `start: TimePosition`, `length: TimeDuration` (> 0),
  an `ItemSource::{Empty, Audio { path, offset }, Midi { notes }}`, gain,
  mute, fades. `MidiNote` content is in beats relative to the item start.
* `Marker` — `MarkerId`, name, position, colour.
* `SessionSnapshot` (+ `TrackSnapshot`, `ItemSnapshot`, `MarkerSnapshot`,
  `TempoSnapshot`) — primitive-only, `serde` types with a `format_version`.
  `Session::snapshot` / `from_snapshot` / `to_json` / `from_json` /
  `save` / `load`.
* `commands` — `superb_undo::Command<Session>` implementations: `AddTrack`,
  `RemoveTrack`, `MoveTrack`, `SetTrackOutput`, `AddSend`, `RemoveSend`,
  `AddItem`, `RemoveItem`, `MoveItem`, `ResizeItem`, `AddMarker`,
  `RemoveMarker`, `MoveMarker`, `SetTempo`, `SetLoopRange`, plus
  `PropertyEdit` helpers (`set_track_gain_db`, `set_track_pan`,
  `set_item_name`, `set_send_level_db`, `set_send_enabled`,
  `set_send_pre_fader`, ...). Drag-style commands coalesce.

## Usage

```rust
use superb_core::{SampleRate, TimeDuration, TimePosition};
use superb_session::commands::{AddItem, AddTrack, MoveItem};
use superb_session::{Item, ItemSource, Session, Track, TrackKind};
use superb_undo::UndoStack;

let mut session = Session::new("demo", SampleRate::HZ_48K);
let mut history = UndoStack::new();

// Allocate IDs first so you know them before the stack takes the command.
let track = session.allocate_track_id();
history.push(&mut session, AddTrack::new(Track::with_id(track, "vox", TrackKind::Audio))).unwrap();

let clip = session.allocate_clip_id();
let item = Item::with_id(clip, "take 1", TimePosition::ZERO, TimeDuration::from_samples(1_000), ItemSource::Empty).unwrap();
history.push(&mut session, AddItem::new(track, item)).unwrap();

// A drag: many small moves, one undo step.
for x in [100, 200, 300] {
    history.push(&mut session, MoveItem::new(clip, TimePosition::from_samples(x))).unwrap();
}
history.seal();
assert_eq!(history.undo_len(), 3);

history.undo(&mut session).unwrap();                 // back to 0
history.undo(&mut session).unwrap();                 // item gone
history.redo(&mut session).unwrap();                 // back under the same ClipId
assert!(session.item(clip).is_ok());

let json = session.to_json().unwrap();
let copy = Session::from_json(&json).unwrap();
assert_eq!(copy.snapshot(), session.snapshot());
```

Direct (non-undoable) editing uses the same verbs on `Session`:
`add_track`, `remove_track`, `move_track`, `track_mut`, `add_item`,
`move_item`, `move_item_to_track`, `resize_item`, `item_mut`,
`add_marker`, `move_marker`, `tempo_map_mut`, `set_loop_range`,
`set_track_output`, `add_send`, `remove_send`, `send_mut`.

Routing lives in the document too:

```rust
use superb_core::SampleRate;
use superb_session::commands::{AddSend, SetTrackOutput, set_send_level_db};
use superb_session::{Session, TrackKind, TrackSend};
use superb_undo::UndoStack;

let mut session = Session::new("routing", SampleRate::HZ_48K);
let mut history = UndoStack::new();
let vox = session.add_track("vox", TrackKind::Audio);
let drums = session.add_track("drums", TrackKind::Bus);
let verb = session.add_track("verb", TrackKind::Bus);

history.push(&mut session, SetTrackOutput::new(vox, Some(drums))).unwrap();
assert!(session.set_track_output(drums, Some(drums)).is_err());        // cycle
assert!(session.set_track_output(vox, Some(vox)).is_err());            // not a bus

let send = session.allocate_send_id();
history.push(&mut session, AddSend::new(vox, TrackSend::with_id(send, verb).pre_fader())).unwrap();
history.push(&mut session, set_send_level_db(send, -12.0)).unwrap();
assert_eq!(session.send(send).unwrap().level_db, -12.0);

// Removing the verb bus drops the send; undo puts both back.
history.push(&mut session, superb_session::commands::RemoveTrack::new(verb)).unwrap();
assert_eq!(session.send_count(), 0);
history.undo(&mut session).unwrap();
assert_eq!(session.send_track(send).unwrap(), vox);
```

## File format

`to_json` writes a `SessionSnapshot`: `format_version` (currently 1), name,
`sample_rate_hz`, tempo points (`beat`, `bpm`, `ramp` — the ramp flag is
optional on read and defaults to `false`) and meters (`bar`,
`numerator`, `denominator`), loop range as `[start, end)` samples, tracks
with their items, `output` (raw bus track ID or `null`) and `sends`
(`id`, `bus`, `level_db`, `pre_fader`, `enabled`), markers, and `next_id`.
`output` and `sends` are optional on read (files written before routing
existed load with master routing and no sends). Loading validates the
sample rate, tempo/meter values, item lengths, ID uniqueness (IDs of 0
are rejected), that bus tracks carry no items or sends, that routing
targets are bus tracks with no bus → bus cycle (routing is applied after
every track exists, so a track may feed a bus listed later), and refuses
files with a newer `format_version`. A stale `next_id` is corrected from
the IDs present.

## Real-time notes

None — nothing in this crate is meant for the audio thread. Every mutating
method may allocate. Keep the session on the UI/document thread and push
derived parameter changes to the engine through `Arc<AtomicF32>` handles
or `superb_graph::live`.

## Limitations / next

* Items are positioned in samples; there is no "beat-locked" item mode
  that follows tempo changes (convert through `Session::tempo_map()`).
* No per-item automation lanes yet (`superb-automation` lanes are not
  owned by the session).
* Routing is one output per track plus sends to buses; there are no
  sidechain inputs, no per-send pan, and a send's destination cannot be
  changed in place (remove + add).
* No project-relative path resolution for `ItemSource::Audio`.

See `examples/session_undo` for the stack driving a session document,
saving it as JSON and reloading it.

## Mixer bridge

Mirroring a session into a live [`Mixer`](superb-mixer.md) is handled by
`superb::bridge::SessionMixerBridge` (separate `TrackId` namespaces,
`sync` + `apply_routing` + `apply_strip_params`). Playing items onto those strips is
`superb::playback::{SessionPlayer, render_region, AudioCache}` — offline
region render first; see `docs/superb.md` and `examples/session_playback`.

