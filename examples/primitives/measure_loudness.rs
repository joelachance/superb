//! `analysis`: meters, loudness, lining two recordings up, silence.
//!
//! What you'll learn:
//! - Sample peak, RMS and true peak are three different numbers for one
//!   signal, and why a hot sine reads higher between its samples
//! - `Loudness` measures LUFS the way a broadcast meter does: momentary,
//!   short-term and gated integrated, fed block by block
//! - `normalize` brings a buffer to a target loudness under a true-peak
//!   ceiling, and says how far it got
//! - `offset::estimate` finds how far one recording lags another, with a
//!   confidence, so a second microphone or a guest's track lines up
//! - `silence::stretches` finds the dead air
//!
//! ```text
//! cargo run --example measure_loudness
//! ```

use aura::prelude::*;
use aura::primitives::analysis::{meter, offset, silence};
use aura::primitives::buffers::gain::{db_to_linear, linear_to_db};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let hz = rate.hz() as f32;

    // Two seconds of a 1 kHz tone at -12 dBFS with a fade at each end.
    let mut tone: Vec<f32> = (0..2 * rate.hz() as usize)
        .map(|i| db_to_linear(-12.0) * (core::f32::consts::TAU * 1_000.0 * i as f32 / hz).sin())
        .collect();
    let n = tone.len();
    for i in 0..480 {
        let g = i as f32 / 480.0;
        tone[i] *= g;
        tone[n - 1 - i] *= g;
    }

    // Three numbers for one signal.
    println!(
        "sample peak {:.2} dBFS, rms {:.2} dBFS, true peak {:.2} dBTP",
        linear_to_db(meter::peak(&tone)),
        linear_to_db(meter::rms(&tone)),
        linear_to_db(meter::true_peak(&tone))
    );

    // Loudness, fed in blocks as an engine or a recording would deliver them.
    let mut loudness = meter::Loudness::new(rate, 1)?;
    for block in tone.chunks(256) {
        loudness.push(&[block])?;
    }
    println!(
        "after {:.1} s: momentary {:.2} LUFS, short-term {:.2} LUFS, integrated {:.2} LUFS",
        loudness.seconds(),
        loudness.momentary().unwrap_or(f32::NEG_INFINITY),
        loudness.short_term().unwrap_or(f32::NEG_INFINITY),
        loudness.integrated().unwrap_or(f32::NEG_INFINITY)
    );
    // A -12 dBFS sine has a mean square 15 dB under full scale, and
    // K-weighting leaves 1 kHz alone, so it reads about -15 LUFS.

    // Normalise to what podcasts ask for: -16 LUFS, peaks under -1 dBTP.
    let mut audio = PlanarBuffer::from_planes(&[&tone])?;
    if let Some(done) = meter::normalize(
        &mut audio,
        rate,
        meter::PODCAST_LOUDNESS,
        meter::TRUE_PEAK_CEILING,
    )? {
        println!(
            "normalised by {:+.2} dB: {:.2} -> {:.2} LUFS, true peak {:.2} -> {:.2} dBTP",
            done.gain_db,
            done.loudness_before,
            done.loudness_after,
            done.true_peak_before,
            done.true_peak_after
        );
    }

    // Lining up: a second "microphone" hears the same tone 20 ms later,
    // quieter, with noise. The estimate finds the lag in samples.
    let lag = (0.020 * hz) as usize;
    let mut seed = 7u64;
    let mut noise = move || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((seed >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    };
    // Noise in both, so the correlation has something to grab besides a
    // periodic tone (which would match at every cycle).
    let hiss: Vec<f32> = (0..n).map(|_| 0.2 * noise()).collect();
    let reference: Vec<f32> = tone.iter().zip(&hiss).map(|(t, h)| t + h).collect();
    let other: Vec<f32> = (0..n)
        .map(|i| {
            let late = if i >= lag { reference[i - lag] } else { 0.0 };
            0.5 * late + 0.05 * noise()
        })
        .collect();
    let found = offset::estimate(&reference, &other, rate.hz() as usize / 10)?;
    println!(
        "the second microphone lags by {} samples ({:.1} ms), confidence {:.2}",
        found.shift,
        found.shift as f32 / hz * 1_000.0,
        found.confidence
    );

    // Dead air: a second of near silence in the middle.
    let mut with_gap = tone.clone();
    for s in &mut with_gap[rate.hz() as usize / 2..rate.hz() as usize * 3 / 2] {
        *s *= 0.000_1;
    }
    for range in silence::stretches(&with_gap, db_to_linear(-60.0), rate.hz() as usize / 4) {
        println!(
            "silence from {:.3} s to {:.3} s",
            range.start as f32 / hz,
            range.end as f32 / hz
        );
    }
    let kept = silence::trimmed(&with_gap, db_to_linear(-60.0));
    println!(
        "trimmed of silence at the ends: {:.3} s .. {:.3} s",
        kept.start as f32 / hz,
        kept.end as f32 / hz
    );
    Ok(())
}
