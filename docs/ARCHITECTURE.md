# Architecture

How Superb is put together, and the rules every module follows. Read
[TERMINOLOGY.md](TERMINOLOGY.md) first if the DAW words are new to you,
and [flows.md](flows.md) for the top tier in detail.

## The big picture

A DAW has two very different jobs. One is **memory**: what the session
remembers, its tracks, clips, tempo and routing, edited by a person on a
UI thread that may take its time and saved to a file. The other is an
**engine**: a callback the audio hardware fires every few milliseconds
that must fill the next buffer before the deadline, every time, without
ever waiting. Superb keeps the two apart
and connects them deliberately.

```text
 flows (free functions over a Session)
 session · track · clip · transport · send · midi · record · metronome · export · devices
 ─────────────────────────────────────────────────────────────────────
 primitives
   memory side (UI thread)             engine side (audio thread)

   session ── Memory: tracks, clips    io         EngineHandle ◄─queues─► AudioThread
   undo    ── history                  io         Engine, offline render, WAV, devices
                  session::steps::rebuild ───► mixer      strips, buses, sends, inserts
   automation ─► Arc<AtomicF32> ─────► graph      nodes, schedule, live swap
                                       plugins    PluginNode, CLAP host
                                       transport  playhead, tempo map
                                       midi       events per block
                                       buffers    planar f32 blocks
                                       realtime   queues, atomics, proofs
                                       (root)     time, IDs, errors
```

Primitives depend downward only and never import a flow. Flows import
primitives and each other, and all of them work on the `Session`.

## The Session

`Session` (`src/session/mod.rs`) is the one thing a program works on: a
plain record of the `Memory` (what gets saved), its `UndoStack`, the
`AudioCache`, the `EngineHandle` control handle, the `AudioThread` (until a
device takes it), the live controls of the last mixer that was built
(`MixerHandles`: one strip handle per track and bus, one per send, keyed
by the memory's ids) and the recording in progress. It has constructors and
nothing else. Flow verbs take `&mut Session` so a call does not need seven
arguments; steps take the fields they need, so a program can assemble its
own verb.

## The two-thread contract

Every type that runs on the audio thread follows the same shape:

1. **`prepare(sample_rate, max_frames)`** runs on the control thread. It may
   allocate: buffers, schedules, delay lines, plugin activation.
2. **`process(...)`** runs on the audio thread once per block. It must not
   allocate, block or lock, and it never processes more frames than
   `prepare` was told.

This is not a convention but a tested property. `primitives::realtime`
provides a counting global allocator, and the integration tests under
`tests/` run hundreds of blocks under it, moving parameters and swapping
graphs, and fail on a single heap operation.

Data crosses the thread boundary in exactly three ways:

* **Parameters** are `Arc<AtomicF32>` or `AtomicBool` handles. The UI
  stores, the audio thread loads once per block and ramps to the new value
  so nothing clicks.
* **Events** (MIDI, transport commands, plugin parameter changes, captured
  audio blocks) travel through the wait-free single-producer
  single-consumer queue in `primitives::realtime::spsc`. Queues are
  allocated once; a full queue refuses and counts, never blocks.
* **Structure** (nodes and edges) never changes under a running graph.
  The control side builds and prepares a whole new graph, publishes it,
  and the audio side swaps it in between two blocks with a pointer
  exchange (`primitives::graph::live`). The retired graph travels back
  over a second queue and is freed on the control thread, so nothing is
  ever dropped on the audio thread.

## From session to sound

1. **`primitives::session`** holds the `Memory` as plain data: tracks
   (audio, MIDI or bus) with their state (volume, pan, mute, solo, arm,
   monitor, input), clips on tracks, markers, a tempo map, loop range, routing
   (each track's output bus and its sends) and insert chains (plugin,
   state, preset). It knows nothing about audio
   threads, so it can be cloned, saved as JSON and undone freely. Edits go
   through `primitives::undo` commands, which restore removed objects under
   their original IDs so the UI's handles stay valid.
2. **`session::steps::rebuild`** turns the memory into a mixer and hands
   it to the engine. Structural flows call it; its steps are public:
   * `build_mixer` makes a strip per track and a bus per bus track with
     the track's volume, pan, mute and solo, routes each output where the
     memory says, adds a send per send in the memory, and attaches one source
     node per audible clip: `BufferPlayerNode` for audio held in memory,
     `ScheduledSynthNode` for MIDI notes converted through the tempo map,
     plus a live instrument for every monitoring MIDI track, a tap on the
     engine's live input for every monitoring audio track, and the
     metronome's click on the master. It returns
     the `MixerHandles` the control side keeps, keyed by the memory's ids (the
     mixer has its own id namespace, which the flows never see).
   * `publish` prepares the mixer's graph and sends it to the engine.
3. **`primitives::mixer`** builds the mixer out of graph nodes: every
   track is an input summing node, an insert chain, and a strip node
   (volume, pan, mute/solo, meter); buses are the same; sends are gain nodes tapped
   before or after the fader.
4. **`primitives::graph`** compiles the nodes into a topological schedule,
   propagates silence so idle branches cost nothing, delays shorter parallel
   paths so everything stays time-aligned, and routes MIDI to the nodes
   that asked for it.
5. **`Engine::live`** (`primitives::io`) is the running engine: an
   `EngineHandle` on the control side (commands, graph publishing, software
   MIDI, position and state atomics) and an `AudioThread` on the audio
   side, which applies commands, drains MIDI, renders a block through
   `Engine` and publishes the position. `session::steps::render_blocks`
   ticks the thread by hand; `devices::output` moves it into a device
   callback.
6. **`primitives::io::Engine`** owns a `Transport` and a processor. Each
   block it advances the playhead, splits the block at loop wraps, delivers
   queued MIDI at sample-accurate frames, runs the processor and copies the
   result out. `offline::render` drives a private engine for `export`.

Recording runs the other way: the audio input (`devices::input`, or any
producer) fills a capture queue, a collector thread (or `record::collect`,
for a queue you fill yourself) moves blocks into memory on the control
thread, and
`record::stop` writes each track's audio as a WAV file into the session
folder, keeps it in memory, and places one clip per armed track as a
single undo step.

## Rules of the house

These decisions are load-bearing. Change them only on purpose, and update
this page when you do.

**Time.** Audio time is an integer number of samples (`i64`, signed so
pre-roll exists). Musical time is `f64` beats, and a beat is a quarter
note whatever the meter (a 6/8 bar is three beats), as in Logic and
Cubase; meter changes start on bars and change numbering, grid and click
accents, never the audio's place in time. Only `TempoMap` converts
between them. Tempo ramps are linear in bpm over beats and integrated in
closed form so beats and seconds round-trip exactly.

**Buffers.** The graph works in planar `f32` (one slice per channel).
Buffers have a capacity and an active frame count; shrinking is free,
growing is an error. Interleaved buffers exist only at the device and file
boundary.

**Graph.** Every node has one input bus and one output bus. Several
producers feeding a node are summed. Channel-count mismatches follow one
rule: mono fans out, multi-channel folds to mono by averaging, otherwise
the common channels map one to one. Silence is a status a node returns,
not a scan of its buffer, and silent producers are skipped downstream.
Cycles are rejected when an edge is added. Latency compensation is computed
in `prepare` after nodes are prepared (a plugin may only know its latency
then), and it delays edges, never a node's own buffers.

**MIDI.** MIDI edges are a separate edge set from audio edges. With no
MIDI edges the block's events are broadcast to every node; once any edge
exists only its targets see them. External MIDI is placed by timestamp on
the engine's stream clock; a producer on a different clock is clamped into
the block rather than dropped. Hardware MIDI input (midir) and audio
callbacks (cpal) use different clocks today, so hardware MIDI lands at
block resolution until a bridge re-stamps it.

**Mixer.** Tracks default to a constant-power pan law (−3 dB at centre,
right for mono sources); buses and the master default to balance (unity at
centre, right for stereo material). Solo is a shared counter: a track is
heard if nothing is soloed or it is soloed itself; buses ignore solo.
Inserts sit between the input node and the strip node; pre-fader sends
tap after the last insert.

**Live engine.** Commands and graphs go one way, position and transport
state come back as atomics. The tempo map travels like a graph: a rebuild
sends the memory's whole map and the audio thread hands the old one back
to be freed, so a tempo or meter change never allocates there. Captured
audio enters the same way: a
`LiveInput` attached by command, pulled into a ring before each block and
handed to every node as `ProcessContext::input`, so monitoring is a node
on the strip and nothing locks. A published graph must be prepared for the engine's
sample rate and block size or it is rejected, never re-prepared on the
audio thread. The swap queue holds two graphs; `session::steps::rebuild` reclaims
retired graphs before building and, while the audio thread is still owned
by the `Session`, applies the swap itself so repeated rebuilds never back
up. When a device drives it, the audio thread publishes where and when
the transport began rolling on the device clock (`RollStart`); a recording
lines its first captured frame up with that, and the session's recording
offset then moves the take earlier by the measured round trip.

**Automation.** Envelope edits allocate and belong on the control thread.
The read path evaluates into atomics or a caller's scratch buffer without
touching the heap. Modes are Off, Read, Touch, Latch and Write; when the
transport is stopped every mode is a no-op in both directions.

**Undo.** History is a linear stack of steps; a step is one command, a
coalesced run of same-key commands, or a group. Commands capture the
previous state in `apply`, not at construction, so redo after other edits
is still correct. A failed revert clears the whole history because the
memory state is unknown. The clean (saved) state is a depth index that
becomes unreachable if the saved step is coalesced onto or discarded.
Flows add one convention on top: state (mute, solo, arm, monitor, input,
send on/off, looping) is not an edit. It changes the memory directly and tells
the history its saved point is gone (`UndoStack::mark_dirty`), so it is
saved but never undone. Volume and pan are state in the memory but
continuous, so they go through the history and coalesce.

**Memory.** The memory is plain data with no `Arc` handles and no audio
types. Each object's settable fields sit under one `state` field
(`TrackState`, `ClipState`, `SendState`, `InsertState`). IDs come from one per-session counter and are never reused. The
file format is a separate primitive-only snapshot with a format version,
not the in-memory types, and old fields load with defaults. Routing is
validated in the memory (targets must be buses, bus chains may not form
a cycle), and removing a bus detaches what fed it instead of failing.

**Files.** A session is a folder from its creation: `memory.json` beside `audio/`,
where recordings land as 32-bit float WAV at the session's rate, named
`<track> <n>.wav`; clips refer to them by relative path. Undoing a
recording deletes the files it wrote; nothing else does. The MPL-2.0 decoder is imported from exactly one source file and
returns Superb types only; a default-feature test scans the crate for stray
mentions. Decoding is whole-file and lenient: a truncated file yields the
frames it has, a corrupt packet is skipped, an unrecognised container is
an error. Writing is WAV only.

**Plugins.** Hosted plugins enter the graph only as `PluginNode`, so
scheduling, silence, MIDI routing and latency compensation treat them like
built-ins. The CLAP host is the only FFI in the SDK; every unsafe block
states its invariant, and the host is tested against an in-process plugin
so no shared library is needed. `start_processing` is deferred to the
first `process` and `stop_processing` to `deactivate`. The session stores a
plugin reference, parameter values and saved state; the instance is
recreated at every rebuild and the stored values re-applied.

**Rebuild.** A rebuild builds a whole new mixer and swaps it in; it never
edits the running one. That keeps the audio thread's contract simple at
the cost of resetting plugin tails and meters when the mixer's shape
changes. Structural edits trigger it themselves: at once while a device
drives the engine, otherwise at the next start or render. Level edits and
state changes do not rebuild because they go to the live handles directly.

**Features.** Hardware (`devices`, `midi-io`) and the copyleft decoder
(`files`) are opt-in so the default build and `cargo test` need nothing
but Rust. The CLAP host (`clap`) is on by default and can be turned off.

## Where does new code go?

| you want to add | it belongs in |
|---|---|
| a new kind of processor (effect, generator) | a `Node` impl, in `primitives::graph::nodes` if general, otherwise in your app |
| a new mixer feature (VCA groups) | `primitives::mixer`, built from graph nodes |
| something the session remembers | `primitives::session` (plain data) plus a command in `primitives::session::commands` |
| a new audio file format | `primitives::io::files`, keeping any copyleft crate inside `decode.rs` |
| a new audio or MIDI backend | `primitives::io::device` or `primitives::midi::input`, behind a feature |
| a new plugin format | a `PluginInstance` impl in `primitives::plugins` |
| an everyday task made of several primitive calls | a verb in the matching flow, with its steps in `steps` and a "Made of:" list |
| a new thing or task (say, comping) | a new top-level flow module, plus a doc in `docs/flows/` and an example |
| a shared primitive (time type, queue) | the `primitives` root or `primitives::realtime` |

## Not there yet

Duplex device streams. Re-stamping hardware MIDI onto the audio clock.
A plugins flow (the host is a primitive). Incremental edits to a running mixer (a
rebuild replaces it). Loop recording. VST3 hosting. SysEx, MIDI Time Code and
Standard MIDI Files. Encoders other than WAV. Per-clip automation lanes
owned by the session. Multi-bus or sidechain nodes.
