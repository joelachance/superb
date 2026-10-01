# plugins

Effects and instruments: find plugins, put one on a track, turn its knobs.

## Purpose

An insert is stored in the song (plugin reference, bypass, parameter
values, saved state) and turned into a running instance when the console
is built. Parameter changes go to the document through undo and to the
running instance through its wait-free queue.

## Verbs

| verb | does |
|---|---|
| `scan()` (feature `clap`) | every CLAP plugin in the standard folders and `CLAP_PATH`; failed bundles listed |
| `insert(rig, track, &PluginDescriptor) -> InsertId` | append to the track's chain; instantiated at the next sync |
| `remove(rig, insert)` | |
| `set_bypassed(rig, insert, on)` | switch (saved, not undoable); the node passes its input through from the next block, no sync |
| `set_param(rig, insert, param_id, value)` | document (coalescing per parameter) + running instance |
| `params(rig, insert) -> &[ParamInfo]` | the running instance's parameters; empty before the first sync |
| `loaded(rig, insert) -> Option<&PluginDescriptor>` | what the running instance says about itself |
| `failed(rig) -> &[(InsertId, String)]` | inserts that did not load at the last sync, with the reason |

## Usage

```rust
use superb::{play, plugins, project};

let report = plugins::scan()?;
if let Some(desc) = report.plugins.iter().find(|p| p.features.iter().any(|f| f == "audio-effect")) {
    let fx = plugins::insert(&mut rig, vox, desc)?;
    play::start(&mut rig)?;                       // the instance exists from here
    if let Some(p) = plugins::params(&rig, fx).first().cloned() {
        plugins::set_param(&mut rig, fx, p.id, p.max)?;
    }
    plugins::set_bypassed(&mut rig, fx, true)?;   // needs a sync to take effect
}
for (id, why) in plugins::failed(&rig) {
    eprintln!("{id}: {why}");
}
```

## Made of

`insert`: `Session::allocate_insert_id` → `Insert::with_id(PluginRef::from(desc))`
→ `AddInsert`, `needs_sync`. `set_param`: `set_insert_param` through undo →
`Producer<ParamChange>::push` on `rig.live.insert_mut(id).sender`. At sync,
`play::steps::attach_inserts` calls `PluginLoader::instantiate` (load the
bundle once, `instantiate`, `load_state`, re-apply stored parameters) and
`Mixer::add_insert`.

## Notes

* A song whose plugin is missing still opens and plays; the insert is
  reported by `failed` and the rest of the chain runs.
* Values are plain (in the parameter's `[min, max]`), not normalised.
* Bypass and remove change the console's shape and need a sync;
  parameter edits do not.
* VST3 descriptors are accepted by the document but reported as failed
  at sync until a host exists.
