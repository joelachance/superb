# Flows

The top tier of the `superb` crate: one module per everyday task, each a
set of free functions over a `Rig`. This page covers what they share; each
flow has its own note in `docs/flows/`.

| flow | doc |
|---|---|
| `session` — new, open, save; tracks, clips, markers, tempo, loop; undo | [session.md](flows/session.md) |
| `play` — rebuild the console, start, stop, seek, loop, render offline or on a device | [play.md](flows/play.md) |
| `mix` — faders, pan, mute, solo, buses, sends, routing, meters | [mix.md](flows/mix.md) |
| `midi` — monitor a MIDI track, send notes, connect a keyboard | [midi.md](flows/midi.md) |
| `record` — arm, capture a take, place clips | [record.md](flows/record.md) |
| `plugins` — scan, insert, parameters, bypass | [plugins.md](flows/plugins.md) |
| `export` — bounce a range or the session to WAV | [export.md](flows/export.md) |
| `devices` — audio and MIDI hardware, rig-matched configurations | [devices.md](flows/devices.md) |

## Why free functions

You should know what you are going to get when you call something. A flow
verb is sugar: it does one everyday job by calling primitives in a fixed
order, and its doc comment ends with **Made of:** naming those calls. When
the verb is not quite what you want, you call the same primitives yourself
in your own order. There is no `Studio` object that owns the session and
decides what you may do; the `Rig` is a struct with public fields, and every
flow just borrows it.

Two kinds of function appear in a flow:

* **Verbs** take `&mut Rig` and do the whole job: `play::start(&mut rig)`.
* **Steps** take the primitives they need and live in the flow's `steps`
  module: `play::steps::build_console(&session, &cache, channels)`.
  Verbs are built from steps; so can your own verbs be.

## The Rig

```rust
pub struct Rig {
    pub session: Session,            // the session
    pub history: UndoStack<Document>, // every edit goes through here
    pub cache: AudioCache,           // audio in memory, keyed by the path clips use
    pub engine: EngineHandle,          // control handle: commands, graph swaps, position
    pub engine_loop: Option<EngineLoop>, // the audio side, until a device takes it
    pub live: LiveHandles,           // strip, send and plugin handles from the last rebuild
    pub recording: Option<Recording>,
    pub channels: usize,             // 2 = stereo
    pub needs_rebuild: bool,
    pub output: Option<OutputStream>,          // feature devices
    pub midi_inputs: Vec<MidiInputBridge>,      // feature midi-io
    pub plugins: PluginLoader,
}
```

`session::create` or `session::open` makes one; `Rig::new` wraps a
`Document` you already have.

## Rebuilding the console

The console the engine runs is built from the session. Edits that change its
*shape* (a new track, a clip moved, a send, a route, an insert, monitoring
on a MIDI track) set `rig.needs_rebuild`. `play::rebuild` rebuilds the console
and publishes it as a whole prepared graph; the engine swaps it in between
two blocks. `play::start`, `play::drive` and `record::start_from` rebuild
for you when the flag is set, so most programs never call `rebuild` directly.
Call it yourself when you want to choose the moment the swap happens.

Edits to a *level* (gain, pan, mute, solo, send level, plugin parameter) do
not need a rebuild: the flow writes them to the document and to the live
handle in `rig.live` at once, and they are heard on the next block.

After a rebuild, `rig.live` points at the new console. Handles from before are
stale; do not keep them across a rebuild.

## Edits and switches

Every *edit* to the session is a command on `rig.history`, so it can be
undone. Structural edits (add, remove, move) are one step each; a
continuous edit (fader, pan, send level, plugin parameter, clip drag)
coalesces: moves of the same control within half a second
(`Rig::COALESCE_WINDOW`) merge into one step, so a drag is one step and
not a hundred, and a pause starts the next. A UI that wants mouse-up
precision calls the primitive `rig.history.seal()`. `record::stop` groups
the clips it places into one step called "Record take".

*Switches* (mute, solo, arm, monitoring, bypass, send on/off) are not
edits. They are saved with the session, and they are heard at the next
block, but they never enter the undo history: nobody expects Undo to
un-mute a track. Flows flip them through `session::switch`, which also
tells the history that the saved state is gone, so `is_saved` turns false.

## Commands reach the engine at the next block

`play::start`, `stop`, `seek` and friends send a command over a queue.
The engine applies it at the start of its next block, so reading
`play::state` immediately after `play::start` still shows `Stopped`. With
`play::drive` the next block is the one you render; on a device it is a
few milliseconds away.

## Without hardware

Everything runs without an audio device. `play::drive(&mut rig, blocks)`
ticks the engine loop by hand and hands you the audio, which is how the
tests and most examples hear the rig. `export` renders through its own
offline engine. `record::start_from` accepts any capture producer, so a
take can come from a file or a generator. With the `devices` feature,
`play::start_output` moves the engine loop into a real output stream and
`record::start` opens a real input.

## Writing a new flow

A verb earns its place by replacing at least three primitive calls or by
hiding an ordering hazard. Put the shared parts in `steps`, end every verb
doc with **Made of:**, set `needs_rebuild` for structural edits, flip switches
through `session::switch`, add tests that run without hardware and an
example under `examples/flows/`. See [CONTRIBUTING.md](../CONTRIBUTING.md).
