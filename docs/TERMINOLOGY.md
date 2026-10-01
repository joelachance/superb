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
| Session (the document; musicians also say song or project) | `primitives::session` | Tracks, clips, tempo, routing, inserts: what gets saved |
| Undo history | `primitives::undo` | Undo / redo stack |
| Automation | `primitives::automation` | Envelopes that move knobs over time |
| Plugin host | `primitives::plugins` | Loading third-party effects and instruments (CLAP first) |

## Flows

| Prefer saying | Module | What it is in a DAW |
|---|---|---|
| The session | `session` | New, open, save; tracks, clips, markers, tempo; undo |
| Playback, the transport | `play` | Play, stop, locate, loop; hearing the session |
| The mix, the console | `mix` | Faders, pan, mute, solo, buses, sends |
| MIDI, the keyboard | `midi` | Monitoring a MIDI track, playing notes, connecting a controller |
| Recording, a take | `record` | Arm, record, stop; the take becomes a clip |
| Effects and instruments | `plugins` | Insert a plugin, turn its knobs, bypass it |
| Bounce, export, mixdown | `export` | Rendering the session to a file |
| Hardware, audio interface | `devices` | Which speakers, mics and MIDI ports exist |
| The session | `Session` | Everything a session in progress consists of: document, history, audio, engine |

## One word per thing

These pairs were once both in use. The left column is the word; the right
column is what not to say in code, docs or UI text.

| Say | Not |
|---|---|
| session | song, project, document (as a name) |
| rebuild (the console) | sync, refresh, update |
| drive (the engine, offline) | advance, tick, pump |
| take | recording (as a noun), capture (as a noun) |
| clip | item, region, event |
| strip | channel (a strip's channel count is something else) |
| insert | effect slot, plugin slot |
| switch | toggle (for mute, solo, arm, monitoring, bypass, send on/off) |
| edit | change, gesture (for anything the undo history records) |
| bounce | export (as a verb), render to file |
| gain (dB) | volume, level (for a strip's fader; sends do say `level_db`, see below) |

Open question kept on purpose: strips say `gain_db` and sends say
`level_db`, because a send level is relative to the strip it taps. If that
proves confusing in a UI, rename the send side, not the strip side.

## Words that confuse musicians

- **Buffer** — A small tray of samples (e.g. 128–1024 frames) the audio interface asks for on a deadline. *Not* mainly "we can't fit the session in RAM." Machines often hold the whole project in memory; buffering is about *timing* and *CPU batches*.
- **Real-time (RT)** — The audio callback must finish filling the next buffer before the interface needs it, or you hear clicks/dropouts. It is **not** a pub/sub event bus. Closer to: hardware demands audio on a schedule; UI *pushes* parameter changes into atomics/queues the callback *pulls*.
- **Control / UI thread** — Where you click Play, drag a fader, edit clips. That work is allowed to allocate and wait. It must not do heavy DSP inside the audio callback.
- **Signal path (we also say "graph")** — The wiring of processors. Same idea as inserts, sends, and buses on a console.
- **Rebuild** — In Superb, rebuilding the console from the session and swapping it into the running engine. Happens for you when you start playing; structural edits mark it as needed.
- **Take** — One pass of recording. It lands in the audio cache and becomes a clip on each armed track.
- **Insert** — An effect or instrument sitting in a track's signal path, in order. Stored in the session; instantiated at rebuild.
- **Switch vs. edit** — Mute, solo, arm, monitoring, bypass and send on/off are switches: saved with the session, heard at once, never undo steps. Everything else you change in the session is an edit and can be undone.

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
