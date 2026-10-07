# primitives::mixer

A mixing console built on `superb::primitives::graph::AudioGraph`.

## Purpose

`primitives::mixer` is built on `primitives::graph`. It wires **tracks**,
**buses**, a **master** bus and **sends** into an `AudioGraph` and implements
`superb::primitives::graph::BlockProcessor`, so a `Mixer` drops straight into
`superb::primitives::io::Engine` (or `offline::render`) exactly like a bare
`AudioGraph` does.

## Key types

* `Mixer` — `new`, `add_track`/`remove_track`, `add_bus`/`remove_bus`,
  `master`, `add_insert`/`remove_insert`/`inserts` (per-strip effect chain,
  keyed by `StripId`), `add_send`/`remove_send`, `set_track_output`/`set_bus_output`,
  `connect_source` (wires a graph node into a track's input),
  `graph_mut` (for adding source/effect nodes), `track_params`/
  `bus_params`/`send_params` (control handles), `solo_count`/`clear_solo`.
* `StripParams` — the lock-free control surface of a track or bus:
  `gain`/`set_gain` (linear), `volume_db`/`set_volume_db` (the fader), `pan`/`set_pan`, `pan_law`/
  `set_pan_law`, `mute`/`set_mute` (`is_muted`), `solo`/`set_solo`
  (`is_solo`), `peak`/`take_peak`.
* `SendParams` — `level`/`set_level`, `level_db`/`set_level_db`,
  `is_enabled`/`set_enabled`.
* `SendTap::{PreFader, PostFader}` — where a send taps its track.
* `PanLaw::{ConstantPower, Balance}` — tracks default to `ConstantPower`
  (−3 dB centre); buses and the master default to `Balance` (unity centre).
* `StripNode` / `SendNode` — the graph nodes a strip and a send compile to.

## Usage

```rust
use superb::primitives::SampleRate;
use superb::primitives::graph::nodes::ConstantNode;
use superb::primitives::graph::{BlockProcessor, BlockTime};
use superb::primitives::midi::MidiEventBuffer;
use superb::primitives::mixer::Mixer;

let mut mixer = Mixer::new(2);
let drums = mixer.add_track("drums");
let src = mixer.graph_mut().add_node(Box::new(ConstantNode::new(0.5, 2)));
mixer.connect_source(src, drums).unwrap();
mixer.track_params(drums).unwrap().set_volume_db(-6.0206);

mixer.prepare(SampleRate::HZ_48K, 64).unwrap();
mixer.process(BlockTime::free_running(), 64, &MidiEventBuffer::with_capacity(0), None).unwrap();
let out = mixer.output();
// 0.5 * 0.5 gain * constant-power centre (0.7071)
assert!((out.channel(0)[63] - 0.1768).abs() < 1e-3);
```

Every track is **two** graph nodes: an input summing node (`PassThroughNode`,
where `connect_source` and pre-fader sends attach) followed by a `StripNode`
(gain → pan → mute/solo → meter). Post-fader sends tap the strip node
instead. This split is why `SendTap::PreFader` sends survive a track's mute
and gain changes while `SendTap::PostFader` sends do not — see
`sends_pre_and_post_fader` in `src/primitives/mixer/mixer.rs`.

## Real-time notes

`Mute`/`solo`/`gain`/`pan` changes go straight through `StripParams`'s
atomics from any thread and need no re-`prepare`; `StripNode::process` and
`SendNode::process` ramp linearly from the last block's gain to the new
target across the block (`assert_ramp` in `src/primitives/mixer/strip.rs`)
so parameter moves are click-free rather than stepped. Solo uses a single
`Arc<AtomicUsize>` shared by every track's `StripParams`
(`mute_and_solo_semantics`): soloing increments it, un-soloing decrements
it, and `is_effectively_muted` just checks whether the count is nonzero and
the strip itself isn't one of the soloed ones — this is why solo state is
consistent no matter which thread flips it. Structural edits
(`add_track`, `add_send`, `set_track_output`, `set_bus_output`, ...) call
`superb::primitives::realtime::thread::debug_assert_not_rt` and mutate the graph, so they need
a `prepare` afterward (`routing_to_buses_and_removal_reroutes_to_master`,
`pan_laws_on_master_output`); parameter changes do not.
`tests/mixer_no_alloc.rs::mixer_process_is_allocation_free`
proves `Mixer::process` performs no heap activity even while gain, pan,
mute, solo and send-enable move every block.

## Limitations / next

* **Inserts are a flat chain** — `add_insert(strip, node)` appends to a
  track's or bus's chain between its input node and its strip node, and
  pre-fader sends tap after the last insert (`StripId` names either kind
  of strip). There is no reordering in place yet: remove and re-add.
* **No per-track MIDI API yet** — MIDI routing lives on the graph: call
  `mixer.graph_mut().connect_midi(MidiPort::External, synth_node)` for each
  instrument node that should hear the block's MIDI (see
  `docs/primitives/graph.md`, "MIDI edges"). With no MIDI edges the mixer keeps
  the broadcast behaviour. Re-`prepare` after adding edges, as for audio
  edges.
* **No automation** — parameter changes are immediate (ramped, not
  scheduled); `primitives::automation` is a separate module.
* **Metering is peak-hold on the first two channels only**
  (`params::METER_CHANNELS = 2`); higher channel counts on a strip aren't
  metered beyond L/R, and there's no RMS/LUFS option on the strip yet;
  `primitives::analysis::meter` measures RMS, true peak and LUFS off the
  audio thread.
* **No VCA/groups** — gain and mute/solo apply per strip; there is no
  linked-fader group concept.
