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
| Meters, loudness | `primitives::analysis` | Peak, true peak, RMS, LUFS; normalising; lining two takes up; finding silence |
| Dynamics | `primitives::graph::dynamics` | Compressor, limiter, gate, de-esser, leveler: a level turned into a gain |
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
| Recording | `record` | Start, stop; a clip on every armed track; punch in and out |
| The metronome, the click | `metronome` | A click while rolling; count-in before a recording |
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
| state (mute, solo, arm, monitor, input, clip mute, send on/off, looping, metronome on, count-in) | switch, toggle, flag |
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
- **Rebuild** — In Aura, rebuilding the mixer from the memory and swapping it into the running engine. Happens for you: at once while a device is playing, otherwise when you next start.
- **Monitor** — Hearing a track's input live through its strip: the audio input for an audio track, played MIDI through an instrument for a MIDI track. `track::monitor`; not the speakers.
- **Meter, time signature** — Changes start on a bar (`session::meter_change`) and change how bars and beats are counted, where the grid falls and which beats the click accents; they never move audio. A beat stays a quarter note whatever the signature, so a 6/8 bar is three beats long and the click ticks eighths.
- **Latency** — Monitoring latency is hearing yourself late, the buffers in and out; cured by small buffers or the interface's direct monitoring, never by software. Where a take lands is a separate matter and is lined up for you: a device recording compares the input's capture instants with the output's playback instants, so the buffers never shift it. Nothing to set by hand.
- **Master** — The bus every track ends up in. A track in the memory (`session.memory.master()`), always there and never among `tracks()`, so volume, pan, mute and the meter work on it with the track verbs; it cannot be archived, soloed, routed or sent to.
- **Note** — One `MidiNote` in a MIDI clip: beat and length in beats from the clip start, pitch, velocity, channel. Edited with `clip::add_note`, `clip::remove_note` and `clip::change_note`; identified by its content, not an id.
- **Import** — Bringing an audio file into the session folder as 32-bit float at the session's rate, resampled on the way in if it was at another rate (`session::import_audio`); what the file was (rate, bit depth, channels) comes back as `Imported`.
- **Marker** — A name at a position on the timeline (`marker::add`, `move_to`, `rename`, `remove`); it plays nothing and moves nothing.
- **Metronome, click, count-in** — The click on every beat while the transport rolls (`metronome::enable`), and the bars it plays before a recording begins (`metronome::count_in`), during which the transport rolls early and the click sounds even when the metronome is off.
- **Punch in / punch out** — Dropping into record at one timeline position and out at another while the transport rolls. `record::punch_in` and `record::punch_out` set the points; `record::start` honours them.
- **Recording** — A recording in progress. Every armed audio track's audio streams into its own file in the session folder from the first block (`audio/vox 1.wav`, with a `.recording` tag beside it until it is finished); when it stops, each gets a clip named after its file (`vox 1`), and every armed MIDI track gets a clip of the notes that arrived.
- **Sealed** — Frames of a recording's files that are on disk for good: written, synced and covered by a header a reader accepts (`Recording::sealed_frames`). Everything before them survives a crash and never changes; an uploader may ship their bytes while the recording runs.
- **True peak** — The highest point the waveform reaches between the samples as well as on them (`analysis::meter::true_peak`), in dBTP; what a converter or an encoder will see. Sample peak can under-read it by up to 3 dB.
- **LUFS** — Loudness units relative to full scale (`analysis::meter::Loudness`): what the ear hears over time rather than what the meter's needle touches. Momentary (400 ms), short-term (3 s), integrated (the whole programme, gated so silence does not count). Podcasts deliver at −16, broadcast at −23.
- **Offset** — How many samples one recording lags another (`analysis::offset::estimate`), found from the audio itself, with a confidence; what lines a second microphone or a remote guest up.
- **Recover** — Placing the recordings a crash left unfinished: their files are still there with their tags; `record::unfinished` lists them and `record::recover` finishes the headers and places the clips. Nothing is deleted.
- **Insert** — An effect or instrument sitting in a track's signal path, in order. A primitive today (the memory stores it); the flow for it comes back with plugins.
- **State vs. edit** — A track's mute, solo, arm, monitor and input, a clip's mute, a send's on/off, whether playback loops, the click and its count-in, and where the playhead is are state: heard at the next block, never in the undo history, never in `memory.json`, and saved only where the application says with `session::state_file` (a `state.json` beside the memory, say). Everything else you change is an edit: undoable, and saved with `session::save`.

## Plugin formats (short map)

These are industry names, not Aura terms; use them as-is in docs. All
backends live under `primitives::plugins`.

| Say | Notes |
|---|---|
| CLAP | Open plugin format; the one Aura hosts today |
| VST3 | The most widely used commercial format; not hosted yet |
| Audio Units (AU) | Apple platforms only |
| LV2 | Open format, most common on Linux |
| AAX | Pro Tools only |
