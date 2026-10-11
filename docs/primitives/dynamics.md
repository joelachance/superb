# primitives::graph::dynamics

The processors that turn a level into a gain: compressor, limiter, gate,
de-esser, leveler, as nodes for the insert chain.

## Purpose

A voice recording wants its loud words held back (compressor), its peaks
kept under a ceiling (limiter), the room between sentences quietened
(gate), its sharp "s" sounds softened (de-esser) and its overall level
ridden evenly through a conversation (leveler). Every DAW has these, and
a recorder that delivers finished audio needs them before any plugin.
`primitives::graph::dynamics` has the clean textbook version of each: a
peak or RMS detector, a static curve in decibels, attack and release on
the gain. Character, colour and noise reduction stay with CLAP plugins.

Every node is stereo-linked: one gain for all its channels, from the
loudest of them, so the image never shifts. Each is driven by a `Params`
struct of atomics shared with the control thread: setters there, the
node reads them every sample, and a `reduction_db` (or `gain_db`) goes
the other way for a meter. `prepare` allocates whatever a node needs;
`process` never does.

## Key types

* `Compressor` / `CompressorParams` — threshold, ratio, attack, release,
  soft knee, makeup. Feed-forward, peak detector, log-domain curve.
* `Limiter` / `LimiterParams` — a true-peak ceiling (dBTP) and a release.
  A true-peak detector per channel, a 5 ms lookahead
  (`LIMITER_LOOKAHEAD_MS`) over which the largest excess is averaged into
  a ramp that reaches a peak's full reduction exactly as the peak passes;
  reports its delay as latency so the graph lines other paths up; a final
  clip at the ceiling catches what rounding leaves.
* `Gate` / `GateParams` — threshold, attack, hold, release, range; an
  infinite ratio gates, a finite one expands.
* `DeEsser` / `DeEsserParams` — a band-pass sidechain around a frequency
  (6.5 kHz suits most voices) driving a fast compressor on the whole
  signal.
* `Leveler` / `LevelerParams` — a slow gain rider: RMS over about 300 ms
  toward a target, a few dB per second at most, within a range, holding
  still under a floor so pauses are never turned up.

## Usage

```rust
use std::sync::Arc;
use aura::primitives::graph::dynamics::*;
use aura::primitives::graph::AudioGraph;

let comp = Arc::new(CompressorParams::new(-18.0, 3.0));   // threshold dBFS, ratio
comp.set_attack_ms(5.0);
comp.set_makeup_db(4.0);
let lim = Arc::new(LimiterParams::new(-1.0));             // ceiling dBTP
let gate = Arc::new(GateParams::new(-45.0));
let ess = Arc::new(DeEsserParams::new(6_500.0, -24.0));
let ride = Arc::new(LevelerParams::new(-18.0));           // target RMS dBFS

let mut graph = AudioGraph::new();
let input = graph.add_node(Box::new(source));
let g = graph.add_node(Box::new(Gate::new(2, Arc::clone(&gate))));
let d = graph.add_node(Box::new(DeEsser::new(2, Arc::clone(&ess))));
let c = graph.add_node(Box::new(Compressor::new(2, Arc::clone(&comp))));
let r = graph.add_node(Box::new(Leveler::new(2, Arc::clone(&ride))));
let l = graph.add_node(Box::new(Limiter::new(2, Arc::clone(&lim))));
for (from, to) in [(input, g), (g, d), (d, c), (c, r), (r, l)] { graph.connect(from, to)?; }
graph.set_output(l)?;
graph.prepare(rate, 256)?;
// ... while it runs, from the control thread:
println!("compressor {:.1} dB, limiter {:.1} dB", comp.reduction_db(), lim.reduction_db());
```

## Notes

* The order above is the usual one for speech: gate first (so the
  compressor does not lift the room), de-esser before the compressor (so
  it does not push the sibilance back up), the leveler riding slowly over
  the result, the limiter last as a safety net.
* The limiter's lookahead is fixed at 5 ms plus the true-peak detector's
  six samples; it reports both as latency. With latency compensation on
  (the graph's default), a parallel dry path is delayed to match.
* Detectors are peak (compressor, gate, de-esser) or RMS (leveler); none
  is RMS-and-peak switchable, and none has a sidechain input from another
  node. Both are reasonable next steps.
* Nothing here is wired to the memory's inserts yet: the nodes exist for
  a graph you build by hand or for the plugins flow to come. The mixer
  does not know them.
* The limiter clips at the ceiling after limiting. On ordinary material
  the clip never engages; on a test signal that steps from silence to full
  scale in one sample it can, since no finite lookahead sees a step before
  the step.
