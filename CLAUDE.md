# Superb SDK

Open-source Rust SDK for building a DAW. Dual-licensed Apache-2.0 OR MIT.

Read `CONTRIBUTING.md` for the checks and rules, `docs/ARCHITECTURE.md`
for how the crates fit together, and `docs/TERMINOLOGY.md` for the words
to use in code and docs.

## Commands

```sh
cargo test --workspace                          # no hardware needed
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo doc --workspace --no-deps
```

## Rules that must hold

- All code is original work for this repository. Never copy, port or
  paraphrase code from other projects, whatever their licence.
- Default-feature dependencies are permissive (Apache-2.0, MIT, ISC, BSD).
  MPL code stays behind the `files` feature and inside one file.
- `prepare` may allocate; `process` never does. New audio-path code gets a
  no-alloc test.
- No unsafe outside `superb-rt`'s primitives and the CLAP FFI.
- Every public item is documented; update `docs/superb-<crate>.md` and the
  examples when an API changes.
