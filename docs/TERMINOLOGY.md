# Superb SDK terminology (musician-friendly)

Crate folder names stay as-is for now (`superb-*`). Prefer these **common DAW words** in docs, UI, and teaching comments. This is a naming map, not a rename of the crates.

| Prefer saying | Instead of (crate / jargon) | What it is in a DAW |
|---|---|---|
| Shared types / basics | `superb-core` | Sample rate, time, IDs, errors — the shared vocabulary |
| Audio buffers | `superb-buffers` | Short chunks of samples the engine processes each callback |
| Real-time helpers | `superb-rt` | Safe ways for the UI and the audio engine to share knobs/queues without glitches |
| Transport | `superb-transport` | Play / stop / locate, tempo, loop — the transport bar |
| Signal path / routing graph | `superb-graph` | How audio is wired: track → inserts → buses → master (not a chart “graph”) |
| Mixer / channel strips | `superb-mixer` | Faders, pan, mute/solo, meters, sends |
| MIDI | `superb-midi` | Notes and controllers (not audio) |
| Devices & files (I/O) | `superb-io` | Speakers, mics, bounce, open/save audio files |
| Project / session | `superb-session` | The song document: tracks, clips, tempo, routing |
| Undo history | `superb-undo` | Undo / redo stack |
| Automation | `superb-automation` | Envelopes that move knobs over time |
| Plugin host | `superb-plugins` | Loading third-party effects/instruments (CLAP first) |
| Facade / app entry | `superb` | One crate an app imports; glue between project and mixer |

## Words that confuse musicians

- **Buffer** — A small tray of samples (e.g. 128–1024 frames) the audio interface asks for on a deadline. *Not* mainly “we can’t fit the song in RAM.” Machines often hold the whole project in memory; buffering is about *timing* and *CPU batches*.
- **Real-time (RT)** — The audio callback must finish filling the next buffer before the interface needs it, or you hear clicks/dropouts. It is **not** a pub/sub event bus. Closer to: hardware demands audio on a schedule; UI *pushes* parameter changes into atomics/queues the callback *pulls*.
- **Control / UI thread** — Where you click Play, drag a fader, edit clips. That work is allowed to allocate and wait. It must not do heavy DSP inside the audio callback.
- **Signal path (we also say “graph”)** — The wiring of processors. Same idea as inserts, sends, and buses on a console.

## Plugin formats (short map)

These are industry names, not Superb terms; use them as-is in docs. All
backends live under the plugin host crate.

| Say | Notes |
|---|---|
| CLAP | Open plugin format; the one Superb hosts today |
| VST3 | The most widely used commercial format; not hosted yet |
| Audio Units (AU) | Apple platforms only |
| LV2 | Open format, most common on Linux |
| AAX | Pro Tools only |
