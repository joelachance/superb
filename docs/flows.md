# Flows

The top tier of the `superb` crate: one module per thing you work with or
task you do, each a set of free functions over a [`Session`](#the-session).
This page covers what they share; each flow has its own note in
`docs/flows/`.

| flow | doc |
|---|---|
| `session` — new, open, save; clips, markers, tempo, loop range; undo; the engine side in `session::steps` | [session.md](flows/session.md) |
| `track` — add, remove, rename; a track's state: mute, solo, arm, monitor, volume, pan, route | [track.md](flows/track.md) |
| `transport` — start, stop, seek, rewind, loop; where the playhead is | [transport.md](flows/transport.md) |
| `mix` — buses, sends, meters | [mix.md](flows/mix.md) |
| `midi` — send notes, connect a keyboard | [midi.md](flows/midi.md) |
| `record` — start a recording, stop it, get clips | [record.md](flows/record.md) |
| `export` — bounce a range or the session to WAV | [export.md](flows/export.md) |
| `devices` — audio and MIDI hardware; play through an output | [devices.md](flows/devices.md) |

## Why free functions

You should know what you are going to get when you call something. A flow
function is sugar: it does one job by calling primitives in a fixed order,
and its doc comment ends with **Made of:** naming those calls. When the
function is not quite what you want, you call the same primitives yourself
in your own order. There is no object that owns the session and decides
what you may do; the `Session` is a struct with public fields, and every
flow just borrows it.

Two kinds of function appear in a flow:

* **Verbs** take `&mut Session` and do the whole job: `transport::start(&mut session)`.
* **Steps** take the primitives they need and live in a `steps` module:
  `session::steps::build_mixer(&document, &cache, channels)`. Verbs are
  built from steps; so can your own verbs be.

## The Session

```rust
pub struct Session {
    pub document: Document,          // tracks, clips, routing, tempo: what gets saved
    pub history: UndoStack<Document>,// every edit goes through here
    pub cache: AudioCache,           // audio in memory, keyed by the path clips use
    pub engine: EngineHandle,        // control side of the running engine
    pub engine_loop: Option<EngineLoop>, // the audio side, until a device takes it
    pub live: LiveHandles,           // strip, send and plugin handles from the last rebuild
    pub recording: Option<Recording>,
    pub channels: usize,             // 2 = stereo
    pub needs_rebuild: bool,
    pub output: Option<OutputStream>,          // feature devices
    pub midi_inputs: Vec<MidiInputBridge>,      // feature midi-io
}
```

`session::create` or `session::open` makes one; `Session::new` wraps a
`Document` you already have. The `Document` is the session's saved shape:
plain data, undoable, written to JSON. The mixer the engine runs is built
from it and is not something you name.

## Edits and state

Every *edit* to the document is a command on `session.history`, so it can
be undone. Structural edits (add, remove, move) are one step each; a
continuous edit (volume, pan, send level, plugin parameter, clip drag)
coalesces: moves of the same control within half a second
(`Session::COALESCE_WINDOW`) merge into one step, so a drag is one step and
not a hundred, and a pause starts the next. A UI that wants mouse-up
precision calls the primitive `session.history.seal()`. `record::stop`
groups the clips it places into one step called "Record".

*State* (a track's mute, solo, arm and monitor; a send's on/off; an
insert's bypass) is not an edit. It is saved with the session and heard at
the next block, but it never enters the undo history: nobody expects Undo
to un-mute a track. Flows set it through `session::set_state`, which also
tells the history its saved point is gone, so `is_saved` turns false.

Volume and pan are state too (they live in `TrackState`), but they are
continuous, so they are edits in the history sense: they coalesce and
undo.

## The mixer follows the document

The engine runs a mixer built from the document. Edits that change its
*shape* (a new track, a clip moved, a send, a route, an insert, monitoring
on a MIDI track) rebuild it: at once while a device is playing (so the
change is heard within a block), otherwise at the next `transport::start`
or `session::steps::render_blocks`, which is cheaper when many edits
arrive together (opening a file). `session.needs_rebuild` is the flag in
between. You never call `rebuild` yourself unless you want to choose the
moment; it is a step, `session::steps::rebuild`.

Edits to a level (volume, pan, send level, plugin parameter) and state
changes (mute, solo, bypass, send on/off) do not rebuild: the flow writes
them to the document and to the live handle in `session.live` at once.

After a rebuild, `session.live` points at the new mixer. Handles from
before are stale; do not keep them across a rebuild. A rebuild recreates
plugin instances with their stored parameters and preset, so reverb tails
and meters reset.

## Commands reach the engine at the next block

`transport::start`, `stop`, `seek` and friends send a command over a
queue. The engine applies it at the start of its next block, so reading
`transport::state` immediately after `transport::start` still shows
`Stopped`. Offline the next block is the one you render; on a device it is
a few milliseconds away.

## Without hardware

Everything runs without an audio device.
`session::steps::render_blocks(&mut session, blocks)` runs the engine loop
by hand and hands you the audio, standing in for the sound card: that is
how the tests and most examples hear the session. `export` renders through
its own offline engine. `record::start_from` accepts any capture producer,
so a recording can come from a file or a generator. With the `devices`
feature, `devices::output` moves the engine loop into a real output stream
and `record::start` opens a real input.

## Writing a new flow

A verb earns its place by replacing at least three primitive calls or by
hiding an ordering hazard. Put the shared parts in `steps`, end every verb
doc with **Made of:**, go through `session::structural` for edits that
change the mixer's shape and `session::set_state` for state, add tests
that run without hardware and an example under `examples/flows/`. See
[CONTRIBUTING.md](../CONTRIBUTING.md).
