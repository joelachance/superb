# primitives::plugins

Plugin hosting: a format-agnostic instance model, a graph node adapter, a
CLAP host, and a VST3 stub.

## Purpose

`primitives::plugins` is built on `primitives::graph`. It turns an
external plugin into a `superb::primitives::graph::Node`, so a hosted effect or
instrument takes part in scheduling, silence propagation, MIDI routing and
latency compensation like any built-in node. CLAP is the only backend;
VST3 is a stub (see the `vst3` module).

Third-party deps (all behind the default `clap` feature): `clap-sys` 0.5
(MIT/Apache-2.0, plain FFI bindings to the CLAP ABI) and `libloading` 0.7
(ISC).

## Key types

* `PluginInstance` — the contract: `descriptor`, `ports`, `params`,
  `param_value`, `set_param` (queues for the next block, or flushes
  immediately while inactive), `activate`/`deactivate`/`is_active`, `reset`,
  `process`, `latency_frames`, `save_state`/`load_state`.
* `PluginDescriptor` (`format`, `id`, `name`, `vendor`, `version`,
  `features`, `path`; `is_instrument`/`is_audio_effect`), `ParamInfo`
  (`id`, `name`, `module`, `min`/`max`/`default`, `automatable`, `stepped`,
  `hidden`, `readonly`, `bypass`), `AudioPortLayout` (`inputs`/`outputs`
  channel counts of the main ports), `ParamChange`.
* `PluginNode` — wraps `Box<dyn PluginInstance>` as a `Node`. `prepare`
  activates (a failure makes the node silent), `process` drains the
  wait-free parameter queue (`take_param_sender()` → `superb::primitives::realtime::spsc`
  producer for the UI thread) then renders; `latency_frames` forwards the
  plugin's latency so the graph compensates it.
* `clap::ClapBundle` — `load(path)` (`.clap` shared library, or a macOS
  bundle directory), `from_entry(&'static clap_plugin_entry, path)` for
  in-process plugins, `descriptors()`, `instantiate(id)`.
* `clap::ClapPlugin` — the `PluginInstance`; also `take_requests()` (the
  plugin's `request_restart` / `request_process` / `request_callback`
  flags), `on_main_thread()`, `last_process_status()`, `process_errors()`,
  `refresh_params()`.
* `scan::{default_search_paths, scan_directory, scan_default, ScanReport}`
  — platform paths plus `CLAP_PATH`/`VST3_PATH`; broken bundles are
  reported in `failures`, not fatal.
* `vst3::{AVAILABLE, unsupported, load}` — the stub.

## Usage

```rust
use superb::primitives::SampleRate;
use superb::primitives::graph::nodes::SineNode;
use superb::primitives::graph::AudioGraph;
use superb::primitives::plugins::scan::{scan_default, ScanReport};
use superb::primitives::plugins::{PluginFormat, PluginNode};

let report: ScanReport = scan_default(PluginFormat::Clap).unwrap();
if let Some(desc) = report.plugins.iter().find(|d| d.is_audio_effect()) {
    let bundle = superb::primitives::plugins::clap::ClapBundle::load(&desc.path).unwrap();
    let plugin = bundle.instantiate(&desc.id).unwrap();
    let mut node = PluginNode::new(Box::new(plugin));
    let mut params = node.take_param_sender().unwrap(); // keep for the UI thread

    let mut graph = AudioGraph::new();
    let osc = graph.add_node(Box::new(SineNode::new(440.0, 0.5, 2)));
    let fx = graph.add_node(Box::new(node));
    graph.connect(osc, fx).unwrap();
    graph.set_output(fx).unwrap();
    graph.prepare(SampleRate::HZ_48K, 256).unwrap(); // activates the plugin
    let _ = params.push(superb::primitives::plugins::ParamChange { id: 0, value: 0.5 });
}
```

`examples/clap_host_scan` (`cargo run --example clap_host_scan -- [dir] [--id id]`)
prints the scan, instantiates a plugin, lists its parameters and renders a
second of audio through it offline. It runs (and reports an empty scan)
on a machine with no plugins installed.

## How the CLAP host drives a plugin

1. **Load**: `libloading` maps the library, `clap_entry` is looked up,
   `clap_version_is_compatible` checked, `init(path)` called, and the
   `clap.plugin-factory` fetched. `deinit` runs when the last `Arc` to the
   bundle drops — instances hold one, so a bundle can never unload under a
   live plugin.
2. **Instantiate**: a `clap_host` (name/vendor/url/version, no host
   extensions yet, request flags recorded in atomics) is boxed so its
   address is stable; `create_plugin` + `init`; the `params`, `state`,
   `latency` and `audio-ports` extensions are queried once; parameter infos
   and the main port layout are read (stereo in/out is assumed when the
   ports extension is absent).
3. **Activate**: `activate(sample_rate, 1, max_frames)`; latency is read
   *after* activation as the spec requires. `start_processing` is deferred
   to the first `process` call and `stop_processing` to `deactivate`.
4. **Process** (allocation-free): parameter changes queued by `set_param`
   become `clap_event_param_value` events at time 0, the block's
   `MidiEventBuffer` becomes `clap_event_note` (note on/off) or
   `clap_event_midi` (everything else) events at their frame; channel
   pointer tables (pre-sized, ≤ 64 channels) and a `clap_event_transport`
   built from `BlockTime` (tempo, beat/second position, playing flag) are
   passed in one `clap_process`. Silent input sets `constant_mask`.
   `CLAP_PROCESS_ERROR` and `CLAP_PROCESS_SLEEP` map to
   `ProcessStatus::Silent`, everything else to `Normal`. Output events from
   the plugin are accepted and discarded.
5. **State**: `clap.state` `save`/`load` over `Vec<u8>`-backed
   `clap_ostream`/`clap_istream`.

Everything is verified against an **in-process test plugin**
(`src/clap/test_plugin.rs`, `cfg(test)`): a stereo gain written directly
against `clap-sys` with a `params` extension (one automatable + one hidden
diagnostic parameter), `state`, `latency` (16 frames) and `audio-ports`. The
tests cover descriptor parsing, params/flush/process, MIDI conversion, host
request flags, state round trips, drop/destroy accounting, and a
`PluginNode` inside an `AudioGraph` with latency compensation. Loading a
real `.clap` from disk is exercised only for the failure paths (missing
file, non-library file, empty bundle directory) because CI has no plugins.

## Real-time notes

`ClapPlugin::process` does not allocate: the event `Vec` (capacity
`EVENT_CAPACITY` = 1024) and the channel pointer `Vec`s are allocated at
instantiation and never grow (excess events are dropped, `set_param`
returns `CapacityExceeded`). `PluginNode::process` pops from an SPSC queue
and forwards. What the *plugin* does inside its `process` is its own
business — CLAP requires it to be RT-safe too.

Threading follows CLAP: `load`/`instantiate`/`activate`/`deactivate`/
`param_value`/`save_state`/`load_state`/`refresh_params` on the main
thread; `process`/`reset`/`set_param` on the audio thread once active.
`ClapPlugin: Send` (moved whole into the graph) — it is not `Sync`.

## Limitations / next

* **VST3 is a stub** (`vst3::AVAILABLE == false`); every call returns
  `vst3::unsupported()`.
* **No host extensions** yet: the plugin cannot ask for `params.rescan`
  notifications, thread checks, timers, GUI, or logging — `get_extension`
  returns null. Request flags (`restart`/`process`/`callback`) are recorded
  and readable but the application must act on them (re-`prepare`, call
  `on_main_thread`).
* **Main ports only**: one input and one output port (index 0), 32-bit
  float; no sidechains, no 64-bit processing, no note ports for MIDI 2.
* **Output events are discarded**, so parameter changes the plugin makes
  itself (gestures, automation writes) are not reported back.
* `default_search_paths` follows the CLAP spec's conventions but does not
  read platform registries; `scan_directory` loads every bundle to read
  descriptors (no cache).
