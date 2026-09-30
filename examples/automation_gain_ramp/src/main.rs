//! Offline demo: a linear gain ramp applied sample-accurately into an
//! `Arc<AtomicF32>`, then printed as a short ASCII meter.
//!
//! What you'll learn:
//! - How an `AutomationLane` drives a shared atomic from envelope points
//! - Why `Arc` + atomics let the UI/control thread and audio thread share a float
//! - `mut`, block processing, and a tiny closure for formatting
//!
//! ```text
//! cargo run -p automation_gain_ramp
//! ```

use std::sync::Arc;

use superb::automation::{advance_block, AutomationLane, AutomationMode, EnvelopePoint};
use superb::core::{IdGenerator, TimePosition};
use superb::rt::AtomicF32;

fn main() {
    // IdGenerator hands out unique IDs for lanes, nodes, tracks, etc.
    let ids = IdGenerator::new();

    // Arc = atomically reference-counted shared ownership.
    // AtomicF32 is a lock-free float the audio code can read without mutexes.
    // Both the lane and our print loop hold a clone of the same Arc.
    let gain = Arc::new(AtomicF32::new(0.0));

    // `mut` because we will edit the lane's envelope and mode below.
    // Arc::clone(&gain) bumps the refcount (cheap) — it does not deep-copy the float.
    let mut lane = AutomationLane::new(ids.next(), Arc::clone(&gain));

    // Envelope: 0 → 1 over 200 samples, then hold (Read mode follows the curve).
    // envelope_mut() borrows the envelope uniquely so we can upsert points.
    lane.envelope_mut()
        .upsert(EnvelopePoint::linear(TimePosition::from_samples(0), 0.0));
    lane.envelope_mut()
        .upsert(EnvelopePoint::linear(TimePosition::from_samples(200), 1.0));
    // Read = playback follows the envelope into the atomic (no writing back).
    lane.set_mode(AutomationMode::Read);

    // Process in fixed-size "blocks" — the same idea as a real-time audio callback.
    let block = 32usize;
    let mut pos = TimePosition::ZERO;
    // Scratch holds one sample of ramp value per frame in this block.
    // vec![value; n] allocates a Vec of length n filled with `value`.
    let mut scratch = vec![0.0f32; block];

    println!("sample-accurate gain ramp (Read mode), block={block}");
    for _ in 0..8 {
        // apply_block writes per-sample values into scratch and updates the atomic
        // to the last sample (what a continuous control would "settle" at).
        lane.apply_block(pos, block, true, Some(&mut scratch));
        let first = scratch[0];
        let last = scratch[block - 1];

        // Closure: |v| means "take one f32 named v". It builds a 20-char meter.
        let bar = |v: f32| {
            let n = (v * 20.0).round().clamp(0.0, 20.0) as usize;
            format!("{}{}", "#".repeat(n), ".".repeat(20 - n))
        };
        println!(
            "t={:>4}  atomic={:.3}  block=[{:.3} .. {:.3}]  {}",
            pos.samples(),
            gain.get(),
            first,
            last,
            bar(last)
        );
        // advance_block moves the timeline forward by `block` samples.
        pos = advance_block(pos, block);
    }
}
