# Contributing to Superb

Thanks for helping. This page covers how to build, what the checks are,
and the few rules that keep the SDK safe to use on an audio thread.

## Build and test

You need Rust 1.86 or newer. Nothing else is required for the default
build; audio hardware and codec support are opt-in features.

```sh
cargo test                                   # the whole suite, no hardware
cargo test --features files                  # file decode / resample / bounce
cargo test --features devices                # device layer (hardware tests are #[ignore])
cargo test --features midi-io                # MIDI input bridge
cargo test --no-default-features             # without the CLAP host
cargo doc --no-deps --open                   # API docs
```

Tests that need a real audio or MIDI device are marked `#[ignore]` and run
with `cargo test --features hardware -- --ignored` on a machine that has one.

## Before you open a pull request

Run these; all of them must come back clean.

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features hardware -- -D warnings
cargo test
cargo doc --no-deps
```

Formatting uses rustfmt's defaults; there is no `rustfmt.toml`.

## Where code goes

The crate has two tiers, described in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
and [docs/flows.md](docs/flows.md).

**Primitives** (`src/primitives/`) are the engine. Put new code in the
lowest module that has everything it needs and nothing it does not:
shared types at the root, then `buffers` and `realtime`, then `transport`,
`midi` and `automation`, then `graph`, `mixer`, `plugins` and `io`, with
`undo` and `session` beside them. A primitive never imports a flow or the
`Session`.

**Flows** (`src/<flow>.rs`, one per thing you work with or task you do)
are free functions over the `Session`. A flow function earns its place by
replacing at least three primitive calls or by hiding an ordering hazard
(rebuild before publish, stop before draining a recording). Rules:

* A *verb* takes `&mut Session` and does the whole job. A *step* takes the
  individual primitives it needs and lives in the flow's `steps` module,
  so a program can build its own verb from the same parts.
* Every verb's doc comment ends with a **Made of:** list naming the
  primitive and step calls in order. Keep it accurate when you change the
  body; it is the contract that lets a reader bypass the verb.
* Edits that change the mixer's shape (tracks, clips, sends, routing,
  monitoring) go through `session::structural`, which rebuilds at once
  while a device drives the engine and otherwise marks the session for a
  rebuild at the next start. Edits to a level go to the memory *and* the
  live handle at once and do not rebuild.
* A continuous edit (fader, knob, clip drag) pushes through the undo stack
  and coalesces by time (`Session::COALESCE_WINDOW`), so a drag is one step
  without the UI having to say when it ended. State (mute, solo, arm,
  monitor, input, clip mute, send on/off, looping, click, count-in,
  playhead) is not an edit: it goes through `session::set_state`, which
  changes the memory directly and writes the state file if the
  application named one (`session::state_file`); it never enters the undo
  history or `memory.json`.
* No behaviour on the `Session` itself beyond constructors; it is a record.
* What is not an error but worth knowing goes to the `log` facade
  (`log::info!`, `log::warn!`), once, with the folder and the verb that
  fixes it; the application picks the logger. Errors stay `Result`s.

## Naming

Names follow [docs/TERMINOLOGY.md](docs/TERMINOLOGY.md): prefer the word a
musician would use (track, clip, bus, send, recording, transport) over engine
jargon, and when a jargon term is unavoidable, explain it once in the
module docs. Before adding a name, check it against these rules; each one
exists because its absence cost a rename.

* **One word per thing, across both tiers.** The thing you open is the
  *session*; what it remembers and saves is its *memory*; the buffers
  clips play are its *audio*; what the engine runs is the *mixer*;
  rebuilding it is *rebuild*; a recording in progress is a *recording*
  and its result a *clip*; clips and tracks are *archived*, never deleted.
  Never song,
  project, document, console, cache or take. If two words are in use for one thing, pick one and rename the
  other.
* **A word with a DAW meaning keeps it.** *Sync* is clock synchronisation,
  *advance* moves the playhead, *gain* is a multiplier, *bounce* renders
  to a file. Do not borrow one for something else; TERMINOLOGY lists them.
* **A flow is named for the thing or the task.** `session`, `track`,
  `clip`, `transport`, `send` and `midi` are things; `record`, `export`
  and `devices` are tasks. State
  lives with its thing: mute and arm are `track::mute` and `track::arm`,
  not `record::`. No invented areas.
* **Verbs are imperative and say what happens.** `start`, `stop`,
  `rebuild`, `bounce`. One verb per action: no `begin` next to `start`. A
  variant is an argument, not a second verb: `record::start(s,
  Source::Device(None))`, never `start_from`; a `bounce` that resamples
  takes the rate, it is not `bounce_at_rate`.
* **A setter is the noun of what it sets.** `track::volume(s, t, db)`,
  `track::input(s, t, ch)`, `send::level(s, send, db)`: no `set_` prefix.
  Units go in the doc, not the name, when only one unit exists at that
  tier (dB everywhere in the flows).
* **Queries read as what they return.** `is_x` for a bool, a noun for data
  (`playhead`, `state`, `peak`). Never a past participle for a getter:
  `loaded` reads as a question and returned a descriptor. A query that
  only reads a public field (`is_saved` for `history.is_clean()`,
  `is_recording` for `recording.is_some()`) is noise: leave it out.
* **Prefer the primitive's enum to a bool parameter.** `SendTap::PreFader`,
  not `pre_fader: true`.
* **Types and fields are named for their role, not their mechanism.**
  `EngineHandle` and `AudioThread` say which side you hold; `MixerHandles`
  are the live controls of the mixer the engine runs. A type called
  `Player` must play.
* **State is a field called `state`.** Every object in the memory keeps its
  settable fields under one `state` (`TrackState`, `ClipState`,
  `SendState`), with nouns for switches: `mute`, not `muted`.
* **One home per behaviour.** A device listing lives in `devices`, an
  offline render in `export`. When two functions do the same thing,
  delete one rather than alias it.
* **Sugar is welcome when it is a button.** A verb may be one line over
  a primitive or another verb (`rewind`, `play_note`) if a musician would
  recognise it as a control on a DAW and its "Made of:" line is the whole
  implementation.
  Sugar that exists for the SDK author's convenience, or that holds logic
  of its own, is not.
* **Plumbing stays private.** What a verb is made of is public only when
  someone building their own verb needs it (`session::steps`). Helpers
  under those are `pub(crate)`; making one public later is cheap, taking
  it back is not.
* **No silent obligations.** If a flow only works when the caller also
  remembers to do something later (end a drag, empty a queue), either the
  flow does it itself (a time window, a thread) or the obligation is a
  documented verb with a reason to call it. A forgotten `finish_gesture`
  merged a whole session of fader moves into one undo step.

## Real-time rules

The audio thread must never block, allocate or take a lock. The code
enforces this with a contract rather than a convention:

* `prepare` may allocate. `process` must not. If a type has both, say so
  in its docs and keep any growable container out of `process`.
* Prove it. Anything new on the audio path gets covered by a no-alloc test
  like `tests/graph_no_alloc.rs`, which runs `process` under the counting
  allocator from `primitives::realtime` and fails on a single heap call.
* Share parameters through `Arc<AtomicF32>` or `AtomicBool` handles.
  Structural changes (adding nodes, rewiring) happen off the audio thread
  and are followed by `prepare`, or go through `primitives::graph::live`,
  which swaps whole prepared graphs between blocks. The flows always use
  the second route.
* Nothing is dropped on the audio thread. Hand retired objects back to the
  control thread through a queue and free them there (`EngineHandle::reclaim`).
* Debug builds can catch mistakes: call
  `primitives::realtime::thread::debug_assert_not_rt` at the top of
  functions that must never run on the audio thread.

## Unsafe code

The crate is `#![deny(unsafe_code)]`. Exactly three modules opt out
because they cannot avoid it: the wait-free queue and the counting
allocator in `primitives::realtime`, and the CLAP foreign-function
interface in `primitives::plugins::clap`. All three use
`deny(unsafe_op_in_unsafe_fn)`, and every unsafe block carries a `SAFETY:`
comment stating the invariant it relies on. Keep it that way; a pull
request that adds unsafe elsewhere needs a very good reason.

## Documentation

* The crate has `#![warn(missing_docs)]`. Write docs for every public item, and
  write module docs that explain what the module is for and why it is
  shaped the way it is, not just what each function does.
* Each primitive module has a usage note in `docs/primitives/<module>.md`
  and each flow in `docs/flows/<flow>.md`. Update them when the public API
  changes.
* A flow page's `## Reference` and `## Types` sections are generated:
  `python3 scripts/reference.py` from the repository root reads every
  public function's signature, doc comment and "Made of:" line and the
  notes in the script (what each argument means, what comes back, what
  fails, how it undoes). When you add or change a verb, add or change its
  note there and rerun the script; do not edit the generated text.
* The documentation site under `website/` (Fumadocs on Next.js) is built
  from the same Markdown: `cd website && pnpm install && pnpm dev` turns
  `README.md`, `docs/`, `examples/README.md` and `CONTRIBUTING.md` into
  pages and serves them. Nothing under `website/content/` is committed;
  edit the Markdown.
* Examples live in `examples/flows/` and `examples/primitives/` and are
  registered in `Cargo.toml`. Keep them runnable without hardware unless
  hardware is the point of the example, and keep the line-by-line
  comments: they double as a tutorial.

## Licensing and originality

Superb is dual-licensed Apache-2.0 OR MIT, and its value depends on that
staying true.

* All code and tests must be original work written for this repository.
  Do not paste, port or closely paraphrase code from other projects,
  whatever their licence. Reading documentation and public specifications
  is fine; copying is not.
* Dependencies in the default feature set must be permissively licensed
  (Apache-2.0, MIT, BSD, ISC or similar). No GPL, LGPL or AGPL.
* A file-level copyleft dependency (MPL-2.0) is acceptable only behind an
  opt-in feature, imported from exactly one source file, with a test that
  fails if it is mentioned anywhere else. The `files` feature is the model.
* Plugin formats whose SDKs carry their own licence terms (VST3, for
  example) are not hosted until those terms have been reviewed.

## Commits

Make small commits that each do one thing, and write the message for the
person reading `git log` in a year: say what changed and why, not how.
