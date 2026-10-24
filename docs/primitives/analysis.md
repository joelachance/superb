# primitives::analysis

Measuring audio rather than making it: meters, loudness, lining two
recordings up, silence.

## Purpose

A DAW shows levels and loudness, a podcast recorder normalises what it
delivers and lines a guest's track up with the host's, an editor cuts dead
air. All of that is math over `f32` slices, so this module has no threads,
no files, no devices and no feature flags: it runs wherever the crate does,
in an application that is not a DAW too. The one-shot functions allocate;
the running meters allocate when built and never while fed.

## Key types

* `meter::peak`, `meter::rms` — the largest sample and the root mean
  square, linear (1.0 is full scale; `buffers::gain::linear_to_db` for dB).
* `meter::true_peak`, `meter::TruePeakMeter` — the largest value the
  waveform reaches *between* the samples too, by interpolating four points
  per sample (ITU-R BS.1770 Annex 2); what a converter or a lossy encoder
  will see. The meter is fed block by block, one per channel, and
  `finish` flushes the few samples it lags.
* `meter::Loudness` — LUFS as a broadcast meter measures them (BS.1770,
  EBU R128): K-weighted, 400 ms windows every 100 ms, `momentary` (400
  ms), `short_term` (3 s) and gated `integrated` (silence under −70 LUFS
  dropped, then anything 10 LU under the average of the rest). Every
  channel weighs one: right for mono and stereo.
* `meter::normalize`, `meter::Normalized` — one gain that brings a buffer
  to a target loudness, stopping at a true-peak ceiling; `PODCAST_LOUDNESS`
  (−16), `BROADCAST_LOUDNESS` (−23), `TRUE_PEAK_CEILING` (−1).
* `offset::estimate`, `offset::Offset` — how many samples one recording
  lags another, by cross-correlation (an FFT the size of both), with a
  confidence from 0 to 1.
* `silence::stretches`, `silence::trimmed` — runs under a threshold for at
  least so long, and the span that is not.

## Usage

```rust
use superb::primitives::analysis::{meter, offset, silence};
use superb::primitives::buffers::gain::db_to_linear;

// Live meters, fed as blocks arrive (one true-peak meter per channel).
let mut loudness = meter::Loudness::new(rate, 2)?;
let mut tp = [meter::TruePeakMeter::new(), meter::TruePeakMeter::new()];
for block in blocks {
    loudness.push_block(&block)?;
    for (ch, m) in tp.iter_mut().enumerate() { m.push(block.channel(ch)); }
}
println!("{:?} LUFS momentary, {:.1} dBTP", loudness.momentary(), linear_to_db(tp[0].max()));

// Deliver at -16 LUFS with peaks under -1 dBTP.
let done = meter::normalize(&mut mix, rate, meter::PODCAST_LOUDNESS, meter::TRUE_PEAK_CEILING)?;

// Line a guest's track up with the host's: a minute of each is plenty.
let found = offset::estimate(&host[..rate.hz() as usize * 60], &guest[..rate.hz() as usize * 60], rate.hz() as usize)?;
if found.confidence > 0.5 {
    // guest[n] sounds like host[n - shift]: start the guest `shift` samples earlier.
    println!("guest is {} samples late", found.shift);
}

// Dead air of a quarter second or more under -60 dBFS.
for range in silence::stretches(&take, db_to_linear(-60.0), rate.hz() as usize / 4) { /* cut */ }
```

`examples/primitives/measure_loudness.rs` runs all of this.

## Notes

* A −12 dBFS 1 kHz sine reads −15 LUFS: 3 dB for a sine's mean square,
  and K-weighting leaves 1 kHz alone. A 0 dBFS one reads −3.01, the
  standard's check value.
* The K-weighting filters are the standard's 48 kHz filters, taken back to
  the analog filters they discretise and discretised again at the
  session's rate, so a 44.1 or 96 kHz session measures the same.
* `offset::estimate` works on mono signals; sum a stereo pair or pick a
  channel. It removes the means first (a DC offset is not similarity) and
  does not match inverted polarity. Its cost is two FFTs the size of both
  signals together, so pass excerpts: a minute of each at 48 kHz is a few
  hundred megabytes for under a second; an hour is neither needed nor
  wise. A periodic signal matches at every period: speech and noise do
  not, and a confidence under 0.5 means do not trust the shift.
* A tone that starts or stops with a step overshoots between its samples
  as any bandlimited step does, and the true-peak meter reports that; a
  recording never does this, a test signal might.
* Nothing here runs on the audio thread yet. `TruePeakMeter::push` and
  `Loudness::push` do not allocate, so a graph node could carry them; the
  gating in `Loudness::integrated` allocates and belongs on the control
  side.
