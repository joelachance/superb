# Superb

Open-source Rust SDK for building a DAW: one crate, `superb`. Dual-licensed
Apache-2.0 OR MIT.

Read `CONTRIBUTING.md` for the checks and rules, `docs/ARCHITECTURE.md`
for the engine, `docs/flows.md` for the flow tier, and
`docs/TERMINOLOGY.md` for the words to use in code and docs.

## Layout

- `src/primitives/` is the engine; modules never import a flow or the `Rig`.
- `src/<flow>.rs` (`session`, `play`, `mix`, `midi`, `record`, `plugins`,
  `export`, `devices`) are free functions over `src/rig.rs`. Verbs take
  `&mut Rig`; steps take primitives; every verb doc ends with "Made of:".
- `examples/flows/`, `examples/primitives/`, registered in `Cargo.toml`.
- `docs/primitives/<module>.md`, `docs/flows/<flow>.md`.

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
- Structural edits set `rig.needs_rebuild`; switches (mute, solo, arm,
  bypass) go through `session::switch` and never enter the undo history;
  level edits reach the document and the live handle.
- Names follow the rules in CONTRIBUTING.md "Naming" and the one-word
  table in docs/TERMINOLOGY.md: one word per thing (session, rebuild,
  take), DAW words keep their DAW meaning (sync, advance), verbs are
  imperative with suffixed variants, queries read as what they return,
  enums over bool parameters, no duplicate homes, no silent obligations.
- Every public item is documented; update the doc note and the example
  when an API changes.
