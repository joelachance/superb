# Architecture

How the Superb SDK is put together, and the rules every module follows.
Read [TERMINOLOGY.md](TERMINOLOGY.md) first if the DAW words are new to
you.

## The big picture

A DAW has two very different jobs. One is a **document**: the song, with
its tracks, clips, tempo and routing, edited by a person on a UI thread that
may take its time. The other is an **engine**: a callback the audio
hardware fires every few milliseconds that must fill the next buffer before
the deadline, every time, without ever waiting. Superb keeps the two apart
and connects them deliberately.

```text
 document side (UI thread)            engine side (audio thread)

 superb-session ─┐                     superb-io   Engine, device streams, files
 superb-undo    ─┤ superb (facade)         │
                 │  bridge, playback   superb-mixer   tracks, buses, sends
                 └──────────────────►      │
                                       superb-graph   nodes, schedule, live swap
 superb-automation ──► Arc<AtomicF32> ►    │
                                       superb-transport  playhead, tempo map
                                       superb-midi       events per block
                                       superb-buffers    planar f32 blocks
                                       superb-rt         queues, atomics, proofs
                                       superb-core       time, IDs, errors
```

Dependencies point downward only. `superb-core` depends on nothing in the
workspace; every other crate depends on it. The `superb` facade sits on top
and is the one crate an application imports.

## The two-thread contract

Every type that runs on the audio thread follows the same shape:

1. **`prepare(sample_rate, max_frames)`** runs on the control thread. It may
   allocate: buffers, schedules, delay lines, plugin activation.
2. **`process(...)`** runs on the audio thread once per block. It must not
   allocate, block or lock, and it never processes more frames than
   `prepare` was told.

This is not a convention but a tested property. `superb-rt` provides a
counting global allocator, and integration tests in `superb-graph` and
`superb-mixer` run hundreds of blocks under it, moving parameters and
swapping graphs, and fail on a single heap operation.

Data crosses the thread boundary in exactly three ways:

* **Parameters** are `Arc<AtomicF32>` or `AtomicBool` handles. The UI
  stores, the audio thread loads once per block and ramps to the new value
  so nothing clicks.
* **Events** (MIDI, commands, captured audio blocks) travel through the
  wait-free single-producer single-consumer queue in `superb-rt::spsc`.
  Queues are allocated once; a full queue drops and counts, never blocks.
* **Structure** (nodes and edges) never changes under a running graph.
  Either stop, edit and `prepare` again, or use `superb-graph::live`: edit
  a draft on the control thread, `publish` it, and the audio thread swaps
  in the whole prepared graph between two blocks with a pointer exchange.
  The retired graph travels back over a second queue and is dropped on the
  control thread, so nothing is ever freed on the audio thread.

## From project to sound

1. **`superb-session`** holds the song as plain data: tracks (audio, MIDI or
   bus), clips on tracks, markers, a tempo map, loop range, and routing
   (each track's output bus and its sends). It knows nothing about audio
   threads, so it can be cloned, saved as JSON and undone freely. Edits go
   through `superb-undo` commands, which restore removed objects under
   their original IDs so the UI's handles stay valid.
2. **`superb::bridge`** maps session tracks onto mixer strips and buses and
   mirrors gain, pan, mute, solo, outputs and sends onto the mixer. The two
   sides keep separate ID spaces; the bridge is the side table between them.
3. **`superb-mixer`** builds a console out of graph nodes: every track is an
   input summing node plus a strip node (gain, pan, mute/solo, meter), buses
   are the same pair, sends are gain nodes tapped before or after the fader.
4. **`superb::playback`** attaches one source node per audible clip:
   `BufferPlayerNode` for audio held in memory, `ScheduledSynthNode` for
   MIDI notes converted through the tempo map.
5. **`superb-graph`** compiles the nodes into a topological schedule,
   propagates silence so idle branches cost nothing, delays shorter parallel
   paths so everything stays time-aligned, and routes MIDI to the nodes
   that asked for it.
6. **`superb-io::Engine`** owns a `Transport` and a processor. Each block it
   advances the playhead, splits the block at loop wraps, delivers queued
   MIDI at sample-accurate frames, runs the processor and copies the result
   out. `offline::render` drives it as fast as possible into a buffer;
   `device::run_engine` drives it from a real output callback.

## Rules of the house

These decisions are load-bearing. Change them only on purpose, and update
this page when you do.

**Time.** Audio time is an integer number of samples (`i64`, signed so
pre-roll exists). Musical time is `f64` beats. Only `TempoMap` converts
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

**Automation.** Envelope edits allocate and belong on the control thread.
The read path evaluates into atomics or a caller's scratch buffer without
touching the heap. Modes are Off, Read, Touch, Latch and Write; when the
transport is stopped every mode is a no-op in both directions.

**Undo.** History is a linear stack of steps; a step is one command, a
coalesced run of same-key commands, or a group. Commands capture the
previous state in `apply`, not at construction, so redo after other edits
is still correct. A failed revert clears the whole history because the
document state is unknown. The clean (saved) state is a depth index that
becomes unreachable if the saved step is coalesced onto or discarded.

**Session.** The document is plain data with no `Arc` handles and no audio
types. IDs come from one per-session counter and are never reused. The
file format is a separate primitive-only snapshot with a format version,
not the in-memory types, and old fields load with defaults. Routing is
validated in the document (targets must be buses, bus chains may not form
a cycle), and removing a bus detaches what fed it instead of failing.

**Files.** The MPL-2.0 decoder is imported from exactly one source file and
returns Superb types only; a default-feature test scans the workspace for
stray mentions. Decoding is whole-file and lenient: a truncated file yields
the frames it has, a corrupt packet is skipped, an unrecognised container
is an error. Writing is WAV only.

**Plugins.** Hosted plugins enter the graph only as `PluginNode`, so
scheduling, silence, MIDI routing and latency compensation treat them like
built-ins. The CLAP host is the only FFI in the SDK; every unsafe block
states its invariant, and the host is tested against an in-process plugin
so no shared library is needed. `start_processing` is deferred to the
first `process` and `stop_processing` to `deactivate`.

**Bridge.** `sync` creates missing mixer strips and buses but never removes
them, so live audio is not cut behind an application's back. Sends are the
one thing `apply_routing` tears down, because a lingering send would keep
feeding a bus the document no longer routes to.

**Features.** Hardware (`devices`, `midi-io`) and the copyleft decoder
(`files`) are opt-in so the default build and `cargo test --workspace`
need nothing but Rust.

## Where does new code go?

| you want to add | it belongs in |
|---|---|
| a new kind of processor (effect, generator) | a `Node` impl, in `superb-graph::nodes` if general, otherwise in your app |
| a new console feature (VCA groups, inserts) | `superb-mixer`, built from graph nodes |
| something the song remembers | `superb-session` (plain data) plus a command in `superb-session::commands` |
| a new audio file format | `superb-io::files`, keeping any copyleft crate inside `decode.rs` |
| a new audio or MIDI backend | `superb-io::device` or `superb-midi::input`, behind a feature |
| a new plugin format | a `PluginInstance` impl in `superb-plugins` |
| glue between document and engine | `superb::bridge` or `superb::playback` |
| a shared primitive (time type, queue) | `superb-core` or `superb-rt` |

## Not there yet

Recording into clips (capture streams exist; wiring them to an armed track
does not). Duplex device streams. Re-stamping hardware MIDI onto the audio
clock. Incremental re-wiring of a running mixer when the document changes
(today a change means a fresh `SessionPlayer::wire`). VST3 hosting. SysEx,
MIDI Time Code and Standard MIDI Files. Encoders other than WAV. Per-clip
automation lanes owned by the session. Multi-bus or sidechain nodes.
