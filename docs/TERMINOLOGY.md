# Terminology (musician-friendly)

Prefer these **common DAW words** in docs, UI text and teaching comments.
Module names are short and technical; this table maps each one to the word
a musician would use.

## Primitives

| Prefer saying | Module | What it is in a DAW |
|---|---|---|
| Shared types / basics | `primitives` (root) | Sample rate, time, IDs, errors — the shared vocabulary |
| Audio buffers | `primitives::buffers` | Short chunks of samples the engine processes each callback |
| Real-time helpers | `primitives::realtime` | Safe ways for the UI and the audio engine to share knobs and queues without glitches |
| Transport | `primitives::transport` | Play / stop / locate, tempo, loop — the transport bar |
| Signal path / routing graph | `primitives::graph` | How audio is wired: track → inserts → buses → master (not a chart "graph") |
| Mixer / channel strips | `primitives::mixer` | Faders, pan, mute/solo, meters, sends, insert chains |
| MIDI | `primitives::midi` | Notes and controllers (not audio) |
| Engine, devices and files | `primitives::io` | The block loop, speakers, mics, bounce, open/save audio files |
| Memory (what the session remembers and saves; a musician just says the session) | `primitives::session` | Tracks and their state, clips (archived ones included), markers, tempo, routing |
| Undo history | `primitives::undo` | Undo / redo stack |
| Automation | `primitives::automation` | Envelopes that move knobs over time |
| Plugin host | `primitives::plugins` | Loading third-party effects and instruments (CLAP first) |

## Flows

| Prefer saying | Module | What it is in a DAW |
|---|---|---|
| The session | `session` | New, open, save; markers, tempo; undo; the engine side in `session::steps` |
| A track and its state | `track` | Add, remove, rename; mute, solo, arm, monitor, input, volume, pan, route; meters |
| A clip | `clip` | Audio or MIDI on a track: add, move, archive, restore |
| The transport | `transport` | Start, stop, locate, loop; where the playhead is |
| A send | `send` | A copy of a track into a bus: add, remove, level, on/off |
| MIDI, the keyboard | `midi` | Playing notes, connecting a controller |
| Recording | `record` | Start, stop; a clip on every armed track |
| Bounce, export, mixdown | `export` | Rendering the session, or a range of it, to a file |
| Hardware, audio interface | `devices` | Which speakers, mics and MIDI ports exist |
| The session (the thing you open) | `Session` | Its memory, the undo history, the audio clips play and the running engine |

## One word per thing

These pairs were once both in use. The left column is the word; the right
column is what not to say in code, docs or UI text.

| Say | Not |
|---|---|
| session (the thing you open) | song, project, rig, workspace |
| memory (what the session remembers and saves: tracks, clips, routing, tempo) | document, model, session file |
| session folder (`memory.json` beside `audio/`, made by `create`) | project file, bundle, package |
| mixer (what the engine runs, built for you) | console |
| rebuild (the mixer) | sync, refresh, update |
| render_blocks (run the engine with no sound card) | drive, advance, tick, sample |
| sync (reserved: aligning to a clock) | anything else |
| recording (in progress) and clip (the result) | take |
| clip | item, region, event |
| archive (a clip or a track; it stays, silent) | delete, remove (nothing in the memory is deleted) |
| strip | channel (a strip's channel count is something else) |
| insert | effect slot, plugin slot |
| state (mute, solo, arm, monitor, input, clip mute, send on/off, looping) | switch, toggle, flag |
| edit | change, gesture (for anything the undo history records) |
| volume (a strip's fader, dB) | gain (that is the engine's linear multiplier) |
| level (a send, dB) | gain, amount |
| playhead | position, cursor |
| bounce | export (as a verb), render to file |

## Words that confuse musicians

- **Buffer** — A small tray of samples (e.g. 128–1024 frames) the audio interface asks for on a deadline. *Not* mainly "we can't fit the session in RAM." Machines often hold the whole project in memory; buffering is about *timing* and *CPU batches*.
- **Real-time (RT)** — The audio callback must finish filling the next buffer before the interface needs it, or you hear clicks/dropouts. It is **not** a pub/sub event bus. Closer to: hardware demands audio on a schedule; UI *pushes* parameter changes into atomics/queues the callback *pulls*.
- **Control / UI thread** — Where you click Play, drag a fader, edit clips. That work is allowed to allocate and wait. It must not do heavy DSP inside the audio callback.
- **Signal path (we also say "graph")** — The wiring of processors. Same idea as inserts, sends, and buses on a mixing desk.
- **Rebuild** — In Superb, rebuilding the mixer from the memory and swapping it into the running engine. Happens for you: at once while a device is playing, otherwise when you next start.
- **Monitor** — Hearing a track's input live through its strip: the audio input for an audio track, played MIDI through an instrument for a MIDI track. `track::monitor`; not the speakers.
- **Punch in / punch out** — Dropping into record at one timeline position and out at another while the transport rolls. `record::punch_in` and `record::punch_out` set the points; `record::start` honours them.
- **Recording** — A recording in progress. When it stops, every armed track gets a clip named after it (`vox 1`); the clips share the audio.
- **Insert** — An effect or instrument sitting in a track's signal path, in order. A primitive today (the memory stores it); the flow for it comes back with plugins.
- **State vs. edit** — A track's mute, solo, arm, monitor and input, a send's on/off and whether playback loops are state: saved with the session, heard at once, never undo steps. Volume and pan are state too but continuous, so they are undoable edits that coalesce. Everything else you change is an edit.

## Plugin formats (short map)

These are industry names, not Superb terms; use them as-is in docs. All
backends live under `primitives::plugins`.

| Say | Notes |
|---|---|
| CLAP | Open plugin format; the one Superb hosts today |
| VST3 | The most widely used commercial format; not hosted yet |
| Audio Units (AU) | Apple platforms only |
| LV2 | Open format, most common on Linux |
| AAX | Pro Tools only |
