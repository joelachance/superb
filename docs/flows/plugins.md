# plugins

Effects and instruments: find plugins, put one on a track, turn its knobs.

## Purpose

An insert is stored in the document (plugin reference, state: bypass and
parameter values, and the plugin's own preset) and turned into a running
instance when the mixer is built. Parameter changes go to the document
through undo and to the running instance through its wait-free queue;
bypass is state, heard at the next block.

## Verbs

| verb | does |
|---|---|
| `scan()` (feature `clap`) | every CLAP plugin in the standard folders and `CLAP_PATH`; failed bundles listed |
| `insert(session, track, &PluginDescriptor) -> InsertId` | append to the track's chain; instantiated at the next rebuild |
| `remove(session, insert)` | |
| `bypass(session, insert, on)` | state; the node passes its input through from the next block, no rebuild |
| `param(session, insert, param_id, value)` | document (coalescing per parameter) + running instance |
| `params(session, insert) -> &[ParamInfo]` | the running instance's parameters; empty until loaded |
| `info(session, insert) -> Option<&PluginDescriptor>` | what the running instance says about itself; `None` until loaded |
| `failures(session) -> &[(InsertId, String)]` | inserts that did not load at the last rebuild, with reasons |

## Usage

```rust
use superb::{plugins, session, transport};

let report = plugins::scan()?;
if let Some(desc) = report.plugins.iter().find(|p| p.features.iter().any(|f| f == "audio-effect")) {
    let fx = plugins::insert(&mut session, vox, desc)?;
    transport::start(&mut session)?;                   // the instance exists from here
    if let Some(p) = plugins::params(&session, fx).first().cloned() {
        plugins::param(&mut session, fx, p.id, p.max)?;
    }
    plugins::bypass(&mut session, fx, true)?;           // heard at the next block
}
for (id, why) in plugins::failures(&session) {
    eprintln!("{id}: {why}");
}
```

## Made of

`insert`: `Document::allocate_insert_id` → `Insert::with_id(PluginRef::from(desc))`
→ `AddInsert` through `session::structural`. `bypass`: `InsertState::bypass`
through `session::set_state` → the node's bypass flag in
`LiveInsert::bypass`. `param`: `set_insert_param` through `session::edit` →
`Producer<ParamChange>::push` on `LiveInsert::param_queue`. At rebuild,
`session::steps::attach_inserts` calls `PluginLoader::instantiate` (load
the bundle once, instantiate, restore the preset, re-apply stored
parameters) and `Mixer::add_insert`.

## Notes

* A session whose plugin is missing still opens and plays; the insert is
  reported by `failures` and the rest of the chain runs.
* Values are plain (in the parameter's `[min, max]`), not normalised.
* VST3 descriptors are accepted by the document but reported as failures
  at rebuild until a host exists.
