# plugins

Effects and instruments: find plugins, put one on a track, turn its knobs.

## Purpose

An insert is stored in the session (plugin reference, bypass, parameter
values, saved state) and turned into a running instance when the console
is built. Parameter changes go to the document through undo and to the
running instance through its wait-free queue.

## Verbs

| verb | does |
|---|---|
| `scan()` (feature `clap`) | every CLAP plugin in the standard folders and `CLAP_PATH`; failed bundles listed |
| `insert(rig, track, &PluginDescriptor) -> InsertId` | append to the track's chain; instantiated at the next rebuild |
| `remove(rig, insert)` | |
| `set_bypassed(rig, insert, on)` | switch (saved, not undoable); the node passes its input through from the next block, no rebuild |
| `set_param(rig, insert, param_id, value)` | document (coalescing per parameter) + running instance |
| `params(rig, insert) -> &[ParamInfo]` | the running instance's parameters; empty before the first rebuild |
| `info(rig, insert) -> Option<&PluginDescriptor>` | what the running instance says about itself; `None` until it is loaded |
| `failures(rig) -> &[(InsertId, String)]` | inserts that did not load at the last rebuild, with the reason |

## Usage

```rust
use superb::{play, plugins, session};

let report = plugins::scan()?;
if let Some(desc) = report.plugins.iter().find(|p| p.features.iter().any(|f| f == "audio-effect")) {
    let fx = plugins::insert(&mut rig, vox, desc)?;
    play::start(&mut rig)?;                       // the instance exists from here
    if let Some(p) = plugins::params(&rig, fx).first().cloned() {
        plugins::set_param(&mut rig, fx, p.id, p.max)?;
    }
    plugins::set_bypassed(&mut rig, fx, true)?;   // needs a rebuild to take effect
}
for (id, why) in plugins::failures(&rig) {
    eprintln!("{id}: {why}");
}
```

## Made of

`insert`: `Session::allocate_insert_id` → `Insert::with_id(PluginRef::from(desc))`
→ `AddInsert`, `needs_rebuild`. `set_param`: `set_insert_param` through undo →
`Producer<ParamChange>::push` on `rig.live.insert_mut(id).param_queue`. At rebuild,
`play::steps::attach_inserts` calls `PluginLoader::instantiate` (load the
bundle once, `instantiate`, `load_state`, re-apply stored parameters) and
`Mixer::add_insert`.

## Notes

* A session whose plugin is missing still opens and plays; the insert is
  reported by `failures` and the rest of the chain runs.
* Values are plain (in the parameter's `[min, max]`), not normalised.
* Bypass and remove change the console's shape and need a rebuild;
  parameter edits do not.
* VST3 descriptors are accepted by the document but reported as failed
  at rebuild until a host exists.
