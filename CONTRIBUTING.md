# Contributing to Superb

Thanks for helping. This page covers how to build, what the checks are,
and the few rules that keep the SDK safe to use on an audio thread.

## Build and test

You need Rust 1.86 or newer. Nothing else is required for the default
build; audio hardware and codec support are opt-in features.

```sh
cargo test --workspace                          # the whole suite, no hardware
cargo test -p superb-io --features files        # file decode / resample / bounce
cargo test -p superb-io --features devices      # device layer (hardware tests are #[ignore])
cargo test -p superb-midi --features midir      # MIDI input bridge
cargo doc --workspace --no-deps --open          # API docs
```

Tests that need a real audio or MIDI device are marked `#[ignore]` and run
with `cargo test -- --ignored` on a machine that has one.

## Before you open a pull request

Run these four commands; all of them must come back clean.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

Formatting uses rustfmt's defaults; there is no `rustfmt.toml`.

## Where code goes

[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) describes the crates and the
dependency order between them. Put new code in the lowest crate that has
everything it needs and nothing it does not. The `superb` facade is for
re-exports and for glue that needs both the document and the engine;
anything else belongs in a specific crate.

Names follow [docs/TERMINOLOGY.md](docs/TERMINOLOGY.md): prefer the word a
musician would use (track, clip, bus, send, transport) over engine jargon,
and when a jargon term is unavoidable, explain it once in the module docs.

## Real-time rules

The audio thread must never block, allocate or take a lock. The code
enforces this with a contract rather than a convention:

* `prepare` may allocate. `process` must not. If a type has both, say so
  in its docs and keep any growable container out of `process`.
* Prove it. Anything new on the audio path gets covered by a no-alloc test
  like `crates/superb-graph/tests/no_alloc.rs`, which runs `process` under
  the counting allocator from `superb-rt` and fails on a single heap call.
* Share parameters through `Arc<AtomicF32>` or `AtomicBool` handles.
  Structural changes (adding nodes, rewiring) happen off the audio thread
  and are followed by `prepare`, or go through `superb-graph::live`, which
  swaps whole prepared graphs between blocks.
* Nothing is dropped on the audio thread. Hand retired objects back to the
  control thread through a queue.
* Debug builds can catch mistakes: call
  `superb_rt::thread::debug_assert_not_rt` at the top of functions that
  must never run on the audio thread.

## Unsafe code

Every crate is `#![forbid(unsafe_code)]` except two places where it cannot
be avoided: the wait-free queue and counting allocator in `superb-rt`, and
the CLAP foreign-function interface in `superb-plugins`. Both use
`deny(unsafe_op_in_unsafe_fn)`, and every unsafe block carries a `SAFETY:`
comment stating the invariant it relies on. Keep it that way; a pull
request that adds unsafe elsewhere needs a very good reason.

## Documentation

* Every crate has `#![warn(missing_docs)]`. Document every public item, and
  write module docs that explain what the module is for and why it is
  shaped the way it is, not just what each function does.
* Each crate has a usage note in `docs/superb-<crate>.md`. Update it when
  the public API changes.
* Examples live in `examples/` and import the `superb` facade. Keep them
  runnable without hardware unless hardware is the point of the example,
  and keep the line-by-line comments: they double as a tutorial.

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
  fails if it is mentioned anywhere else. `superb-io`'s `files` feature is
  the model.
* Plugin formats whose SDKs carry their own licence terms (VST3, for
  example) are not hosted until those terms have been reviewed.

## Commits

Make small commits that each do one thing, and write the message for the
person reading `git log` in a year: say what changed and why, not how.
