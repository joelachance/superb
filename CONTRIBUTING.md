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
`Rig`.

**Flows** (`src/<flow>.rs`, one file per everyday task) are free functions
over the `Rig`. A flow function earns its place by replacing at least three
primitive calls or by hiding an ordering hazard (sync before publish, seal
after a switch, stop before draining a take). Rules:

* A *verb* takes `&mut Rig` and does the whole job. A *step* takes the
  individual primitives it needs and lives in the flow's `steps` module,
  so a program can build its own verb from the same parts.
* Every verb's doc comment ends with a **Made of:** list naming the
  primitive and step calls in order. Keep it accurate when you change the
  body; it is the contract that lets a reader bypass the verb.
* Edits that change the console's shape (tracks, clips, sends, routing,
  inserts, monitoring) set `rig.needs_sync`. Edits to a level go to the
  document *and* the live handle at once and do not need a sync.
* A continuous edit (fader, knob, clip drag) pushes through the undo stack
  and coalesces until `finish_gesture`. A switch (mute, arm, bypass) goes
  through `project::switch`, which seals it as its own step.
* No behaviour on the `Rig` itself beyond constructors; it is a record.

Names follow [docs/TERMINOLOGY.md](docs/TERMINOLOGY.md): prefer the word a
musician would use (track, clip, bus, send, take, transport) over engine
jargon, and when a jargon term is unavoidable, explain it once in the
module docs.

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
  control thread through a queue and free them there (`LiveEngine::reclaim`).
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

* The crate has `#![warn(missing_docs)]`. Document every public item, and
  write module docs that explain what the module is for and why it is
  shaped the way it is, not just what each function does.
* Each primitive module has a usage note in `docs/primitives/<module>.md`
  and each flow in `docs/flows/<flow>.md`. Update them when the public API
  changes.
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
