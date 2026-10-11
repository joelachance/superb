# Aura

Open-source Rust SDK for building a DAW: one crate, `aura`. Dual-licensed
Apache-2.0 OR MIT.

Read `CONTRIBUTING.md` for the checks and rules, `docs/ARCHITECTURE.md`
for the engine, `docs/flows.md` for the flow tier, and
`docs/TERMINOLOGY.md` for the words to use in code and docs.

## Layout

- `src/primitives/` is the engine; modules never import a flow or the `Session`.
- Flows (`session`, `track`, `clip`, `marker`, `transport`, `send`, `midi`,
  `record`, `metronome`, `export`, `devices`) are free functions over the `Session`
  struct in `src/session/mod.rs`; `session::steps` is its engine side.
  Verbs take `&mut Session`; steps take primitives; every verb doc ends
  with "Made of:".
- `examples/flows/`, `examples/primitives/`, registered in `Cargo.toml`.
- `docs/primitives/<module>.md`, `docs/flows/<flow>.md`; the docs site
  (Fumadocs) lives in `docs/` too and renders that Markdown: `cd docs && pnpm dev`.

## Commands

```sh
cargo test                                            # no hardware needed
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features hardware -- -D warnings
cargo fmt --all -- --check
cargo doc --no-deps
```

## Rules that must hold

- All code is original work for this repository. Never copy, port or
  paraphrase code from other projects, whatever their licence.
- Default-feature dependencies are permissive (Apache-2.0, MIT, ISC, BSD).
  MPL code stays behind the `files` feature and inside one file.
- `prepare` may allocate; `process` never does. New audio-path code gets a
  no-alloc test under `tests/`.
- No unsafe outside `primitives::realtime::{alloc, spsc}` and
  `primitives::plugins::clap`.
- Structural edits go through `session::structural` (rebuilds the mixer);
  state (mute, solo, arm, monitor, input, send on/off, looping) goes through
  `session::set_state`, never enters the undo history and is saved only
  where `session::state_file` says; level edits
  reach the memory and the live handle.
- Names follow CONTRIBUTING.md "Naming" and the one-word table in
  docs/TERMINOLOGY.md: session / memory / mixer / recording / clip,
  DAW words keep their DAW meaning (sync, advance, sample), setters are
  nouns without `set_`, state lives under `.state`, queries read as what
  they return, enums over bool parameters, no duplicate homes, no silent
  obligations, sugar only for controls a musician knows, plumbing
  `pub(crate)`.
- Files go where `session::Layout` says (`memory.json`, `state.json`,
  `audio/`, `bounces/`, `stems/<name>/`); sessions live under a
  `session::Root`, `~/aura` by default. No other code builds those paths.
- Every public item is documented; update the doc note and the example
  when an API changes.
