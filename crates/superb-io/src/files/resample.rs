//! Offline sample-rate conversion with rubato (MIT OR Apache-2.0).
//!
//! MPL-2.0 boundary: symphonia is confined to decode.rs — this file never
//! imports it.

use rubato::audioadapter_buffers::direct::{SequentialSliceOfSlices, SequentialSliceOfVecs};
use rubato::{Fft, FixedSync, Indexing, Resampler};

use superb_buffers::PlanarBuffer;
use superb_core::{Result, SampleRate, SuperbError};

/// Input frames handed to rubato's FFT resampler per call.
const CHUNK: usize = 1024;

/// Number of frames [`resample`] produces for `frames` input frames:
/// `ceil(frames * to / from)`, computed in integer arithmetic.
pub fn resampled_frames(frames: usize, from: SampleRate, to: SampleRate) -> usize {
    if from == to {
        return frames;
    }
    let num = frames as u128 * to.hz() as u128;
    let den = from.hz() as u128;
    num.div_ceil(den) as usize
}

fn rerr(context: &str, e: impl std::fmt::Display) -> SuperbError {
    SuperbError::Message(format!("{context}: {e}"))
}

/// Converts `audio` from `from` to `to` with a band-limited FFT resampler.
///
/// * Equal rates return a plain copy without touching rubato.
/// * The result has [`resampled_frames`] frames — `ceil(frames * to / from)`
///   — and the same channel count. The resampler's start-up delay is
///   trimmed and the tail is flushed with silence, so sample 0 of the output
///   lines up with sample 0 of the input (to within the filter's sub-sample
///   phase) and the final input sample is fully represented.
/// * A zero-frame input yields a zero-frame output.
/// * Allocates: this is a worker-thread operation, not for the audio callback.
///
/// The chunk loop is driven here rather than through rubato's whole-clip
/// helper so the delay trim is exact for every chunk geometry.
pub fn resample(audio: &PlanarBuffer, from: SampleRate, to: SampleRate) -> Result<PlanarBuffer> {
    let channels = audio.channels();
    let frames = audio.frames();
    if channels == 0 {
        return Err(SuperbError::InvalidArgument { what: "resample channels" });
    }
    if from == to {
        let mut out = PlanarBuffer::new(channels, frames);
        out.copy_from(audio)?;
        return Ok(out);
    }
    if frames == 0 {
        return Ok(PlanarBuffer::new(channels, 0));
    }

    let mut resampler = Fft::<f32>::new(from.hz() as usize, to.hz() as usize, CHUNK, channels, FixedSync::Input)
        .map_err(|e| rerr("resampler construction", e))?;
    let delay = resampler.output_delay();
    let wanted = resampled_frames(frames, from, to);

    let planes: Vec<&[f32]> = audio.planes().collect();
    let input = SequentialSliceOfSlices::new(&planes, channels, frames).map_err(|e| rerr("resampler input", e))?;

    // Everything the resampler emits, including the start-up delay we trim
    // off afterwards and the tail we flush with silence.
    let mut collected: Vec<Vec<f32>> = (0..channels).map(|_| Vec::with_capacity(wanted + delay + CHUNK)).collect();
    let mut scratch: Vec<Vec<f32>> = vec![Vec::new(); channels];
    let mut consumed = 0usize;
    let mut indexing = Indexing { input_offset: 0, output_offset: 0, partial_len: None, active_channels_mask: None };

    while collected[0].len() < wanted + delay {
        let need = resampler.input_frames_next();
        let avail = frames - consumed;
        indexing.input_offset = consumed;
        indexing.partial_len = if avail < need { Some(avail) } else { None };
        let out_max = resampler.output_frames_next();
        for plane in &mut scratch {
            plane.resize(out_max, 0.0);
        }
        let mut output = SequentialSliceOfVecs::new_mut(&mut scratch, channels, out_max)
            .map_err(|e| rerr("resampler output", e))?;
        let (_, produced) = resampler
            .process_into_buffer(&input, &mut output, Some(&indexing))
            .map_err(|e| rerr("resampling", e))?;
        consumed += need.min(avail);
        for (dst, src) in collected.iter_mut().zip(&scratch) {
            dst.extend_from_slice(&src[..produced]);
        }
        if produced == 0 && avail == 0 {
            return Err(SuperbError::Message(
                "resampler produced no more output before reaching the expected length".into(),
            ));
        }
    }

    let mut out = PlanarBuffer::new(channels, wanted);
    for (ch, plane) in collected.iter().enumerate() {
        out.channel_mut(ch).copy_from_slice(&plane[delay..delay + wanted]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::f32::consts::TAU;

    fn sine(hz: f32, rate: SampleRate, frames: usize, channels: usize) -> PlanarBuffer {
        let mut b = PlanarBuffer::new(channels, frames);
        for ch in 0..channels {
            let amp = 0.5 / (ch + 1) as f32;
            for (i, s) in b.channel_mut(ch).iter_mut().enumerate() {
                *s = amp * (TAU * hz * i as f32 / rate.hz() as f32).sin();
            }
        }
        b
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn frame_count_rule() {
        assert_eq!(resampled_frames(48_000, SampleRate::HZ_48K, SampleRate::CD), 44_100);
        assert_eq!(resampled_frames(1_000, SampleRate::HZ_48K, SampleRate::CD), 919); // 918.75 rounds up
        assert_eq!(resampled_frames(7, SampleRate::CD, SampleRate::CD), 7);
        assert_eq!(resampled_frames(0, SampleRate::CD, SampleRate::HZ_96K), 0);
    }

    #[test]
    fn same_rate_is_a_copy() {
        let src = sine(440.0, SampleRate::CD, 300, 2);
        let out = resample(&src, SampleRate::CD, SampleRate::CD).unwrap();
        assert_eq!(out.channel(0), src.channel(0));
        assert_eq!(out.channel(1), src.channel(1));
    }

    #[test]
    fn down_48k_to_44k1_keeps_length_energy_and_phase() {
        let src = sine(1_000.0, SampleRate::HZ_48K, 48_000, 2);
        let out = resample(&src, SampleRate::HZ_48K, SampleRate::CD).unwrap();
        assert_eq!(out.channels(), 2);
        assert_eq!(out.frames(), 44_100);
        for ch in 0..2 {
            let steady_in = &src.channel(ch)[4_800..43_200];
            let steady_out = &out.channel(ch)[4_410..39_690];
            let ratio = rms(steady_out) / rms(steady_in);
            assert!((ratio - 1.0).abs() < 0.02, "ch{ch} energy ratio {ratio}");
        }
        // Fit a 1 kHz sine/cosine pair to the steady-state output: amplitude
        // must survive and the residual phase (sub-sample delay left after
        // rubato trims its integer start-up delay) must stay under ~0.1 rad.
        let seg = &out.channel(0)[2_000..40_000];
        let (mut a, mut b) = (0.0f64, 0.0f64);
        for (k, &y) in seg.iter().enumerate() {
            let t = TAU as f64 * 1_000.0 * (2_000 + k) as f64 / 44_100.0;
            a += y as f64 * t.sin();
            b += y as f64 * t.cos();
        }
        let n = seg.len() as f64;
        let (a, b) = (2.0 * a / n, 2.0 * b / n);
        let amplitude = (a * a + b * b).sqrt();
        let phase = b.atan2(a);
        assert!((amplitude - 0.5).abs() < 0.01, "fitted amplitude {amplitude}");
        assert!(phase.abs() < 0.1, "residual phase {phase} rad");
        // Continuity: consecutive samples never jump more than the slope allows.
        let max_step = 0.5 * TAU * 1_000.0 / 44_100.0 * 1.05;
        let violations: Vec<_> = out.channel(0).windows(2).enumerate().skip(100).filter(|(_, w)| (w[1] - w[0]).abs() > max_step).take(5).collect();
        assert!(violations.is_empty(), "steps too large at {violations:?}");
    }

    #[test]
    fn up_44k1_to_96k_short_clip() {
        let src = sine(220.0, SampleRate::CD, 1_000, 1);
        let out = resample(&src, SampleRate::CD, SampleRate::HZ_96K).unwrap();
        assert_eq!(out.frames(), resampled_frames(1_000, SampleRate::CD, SampleRate::HZ_96K));
        assert_eq!(out.frames(), 2_177); // 2176.87 rounds up
        assert!(out.peak() > 0.4 && out.peak() < 0.6);
    }

    #[test]
    fn dc_survives_resampling() {
        let mut src = PlanarBuffer::new(1, 10_000);
        src.fill(0.25);
        let out = resample(&src, SampleRate::HZ_48K, SampleRate::CD).unwrap();
        let middle = &out.channel(0)[1_000..8_000];
        assert!(middle.iter().all(|s| (s - 0.25).abs() < 0.01));
    }

    #[test]
    fn edge_cases() {
        assert!(resample(&PlanarBuffer::new(0, 10), SampleRate::CD, SampleRate::HZ_48K).is_err());
        let empty = resample(&PlanarBuffer::new(2, 0), SampleRate::CD, SampleRate::HZ_48K).unwrap();
        assert_eq!((empty.channels(), empty.frames()), (2, 0));
    }
}
