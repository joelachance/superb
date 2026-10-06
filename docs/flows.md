# Flows

The top tier of the `superb` crate: one module per thing you work with or
task you do, each a set of free functions over a [`Session`](#the-session).
This page covers what they share; each flow has its own note in
`docs/flows/`.

| flow | doc |
|---|---|
| `session` — new, open, save; markers, tempo, loop range; undo; the engine side in `session::steps` | [session.md](flows/session.md) |
| `track` — add, remove, rename; a track's state: mute, solo, arm, monitor, input, volume, pan, route; meters | [track.md](flows/track.md) |
| `clip` — audio or MIDI on a track: add, move, archive, restore | [clip.md](flows/clip.md) |
| `transport` — start, stop, seek, rewind, loop; where the playhead is | [transport.md](flows/transport.md) |
| `send` — a copy of a track into a bus: add, remove, level, on/off | [send.md](flows/send.md) |
| `midi` — send notes, connect a keyboard | [midi.md](flows/midi.md) |
| `record` — start a recording, stop it, get clips | [record.md](flows/record.md) |
| `export` — bounce the session, or a range of it, to WAV | [export.md](flows/export.md) |
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
  `session::steps::build_mixer(&memory, &audio, channels)`. Verbs are
  built from steps; so can your own verbs be.

## The Session

```rust
pub struct Session {
    pub memory: Memory,              // tracks, clips, routing, tempo: what gets saved
    pub history: UndoStack<Memory>,  // every edit goes through here
    pub audio: AudioCache,           // the audio clips play, keyed by path
    pub folder: PathBuf,             // memory.json and audio/ live here
    pub engine: EngineHandle,        // control side of the running engine
    pub audio_thread: Option<AudioThread>, // the audio side, until a device takes it
    pub mixer: MixerHandles,         // live strip and send controls from the last rebuild
    pub recording: Option<Recording>,
    pub channels: usize,             // 2 = stereo
    pub output: Option<OutputStream>,          // feature devices
    pub input: Option<AudioInput>,             // feature devices: the audio input, once opened
    pub midi_inputs: Vec<MidiInputBridge>,      // feature midi-io
}
```

`session::create(folder, ..)` or `session::open(folder, ..)` makes one;
`Session::new` wraps a `Memory` you already have. The `Memory` is what the session remembers and
saves: plain data, undoable, written to JSON. The audio clips play sits
beside it in `session.audio`, because the memory holds no buffers, only
the paths clips refer to. A session is a folder from the moment it is
created, `memory.json` beside `audio/`, and recordings are written into it
as they stop. The
mixer the engine runs is built from the memory and is not something you
name.

## Edits and state

Every *edit* to the memory is a command on `session.history`, so it can
be undone. Structural edits (add, move, split, archive, restore) are one
step each; a continuous edit (volume, pan, send level, clip gain and fades, a clip
drag or trim) coalesces:
moves of the same control within half a second
(`Session::COALESCE_WINDOW`) merge into one step, so a drag is one step and
not a hundred, and a pause starts the next. A UI that wants mouse-up
precision calls the primitive `session.history.seal()`. `record::stop`
groups the clips it places into one step called "Record".

*State* (a track's mute, solo, arm, monitor and input; a clip's mute; a
send's on/off; whether playback loops) is not an edit. It is saved with the session and heard at the next block,
but it never enters the undo history: nobody expects Undo to un-mute a
track. Flows set it through `session::set_state`, which also
tells the history its saved point is gone, so `session.history.is_clean()`
turns false.

Volume and pan are state too (they live in `TrackState`), but they are
continuous, so they are edits in the history sense: they coalesce and
undo.

## The mixer follows the memory

The engine runs a mixer built from the memory. Edits that change its
*shape* (a new track, a clip added, moved or archived, a send, a route,
monitoring on a track) rebuild it: at once while a device is playing
(so the change is heard within a block), otherwise at the next `transport::start`
or `session::steps::render_blocks`, which is cheaper when many edits
arrive together (opening a file). You never call `rebuild` yourself unless
you want to choose the moment; it is a step, `session::steps::rebuild`.

Edits to a level (volume, pan, send level) and state changes (mute, solo,
send on/off) do not rebuild: the flow writes them to the memory and to
the live control in `session.mixer` at once.

After a rebuild, `session.mixer` holds the new mixer's controls. Handles
from before are stale; look them up again rather than keeping them across
a rebuild. A rebuild resets the meters.

## Commands reach the engine at the next block

`transport::start`, `stop`, `seek` and friends send a command over a
queue. The engine applies it at the start of its next block, so reading
`transport::state` immediately after `transport::start` still shows
`Stopped`. Offline the next block is the one you render; on a device it is
a few milliseconds away.

## Without hardware

Everything runs without an audio device.
`session::steps::render_blocks(&mut session, blocks)` ticks the audio
thread by hand and hands you the audio, standing in for the sound card:
that is how the tests and most examples hear the session. `export` renders through
its own offline engine. `record::start` with a `Source::Queue` records
from any capture producer, so a recording can come from a file or a
generator. With the `devices` feature, `devices::output` moves the audio
thread into a real output stream and `devices::input` opens the input that
monitoring tracks hear and `Source::Device` records from.

## Writing a new flow

A verb earns its place by replacing at least three primitive calls or by
hiding an ordering hazard. Put the shared parts in `steps`, end every verb
doc with **Made of:**, go through `session::structural` for edits that
change the mixer's shape and `session::set_state` for state, add tests
that run without hardware and an example under `examples/flows/`. See
[CONTRIBUTING.md](../CONTRIBUTING.md).
