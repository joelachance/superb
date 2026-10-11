//! Meters: sample peak, true peak, RMS, and loudness in LUFS, plus
//! normalising a buffer to a target loudness under a true-peak ceiling.
//!
//! Sample peak is the largest sample; true peak is the largest value the
//! waveform reaches between samples too, found by interpolating four
//! points between every pair (ITU-R BS.1770 Annex 2), which is what a
//! converter or a lossy encoder will see. Loudness follows BS.1770 and
//! EBU R128: a K-weighting filter (a shelf for the head, a high-pass for
//! the room), mean square over 400 ms windows every 100 ms, and gating
//! that ignores silence and quiet stretches when summing a whole
//! programme. Every channel counts with weight one, right for mono and
//! stereo; surround weights are not applied.

use super::filter::Biquad;
use crate::primitives::buffers::gain::{db_to_linear, linear_to_db};
use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::{AuraError, Result, SampleRate};

/// The loudness podcasts and streaming services ask for, in LUFS.
pub const PODCAST_LOUDNESS: f32 = -16.0;
/// The loudness broadcast asks for (EBU R128), in LUFS.
pub const BROADCAST_LOUDNESS: f32 = -23.0;
/// The true-peak ceiling both leave room under, in dBTP.
pub const TRUE_PEAK_CEILING: f32 = -1.0;

/// The largest absolute sample: 1.0 is full scale. 0.0 for no samples.
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

/// The root mean square of the samples: 0.707 for a full-scale sine. 0.0
/// for no samples.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    (sum / samples.len() as f64).sqrt() as f32
}

/// The true peak of a whole signal: the largest value between the samples
/// as well as on them, 1.0 being full scale. A few hundredths above
/// [`peak`] on ordinary material, up to 3 dB above on a hot sine.
pub fn true_peak(samples: &[f32]) -> f32 {
    let mut meter = TruePeakMeter::new();
    meter.push(samples);
    meter.finish()
}

/// Interpolation points between two samples, counting the sample itself.
const PHASES: usize = 4;
/// Samples on either side of the gap that shape each interpolated point.
const HALF: usize = 6;
const TAPS: usize = 2 * HALF;

/// A true-peak meter fed block by block, one channel at a time (make one
/// per channel). Keeps the last few samples so the points between a block
/// and the next are interpolated too; never allocates once built.
#[derive(Clone, Debug)]
pub struct TruePeakMeter {
    /// Windowed-sinc weights for the three points between two samples.
    kernel: [[f32; TAPS]; PHASES - 1],
    history: [f32; TAPS],
    max: f32,
}

impl Default for TruePeakMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl TruePeakMeter {
    /// A meter with nothing heard yet.
    pub fn new() -> Self {
        let mut kernel = [[0.0f32; TAPS]; PHASES - 1];
        for (p, taps) in kernel.iter_mut().enumerate() {
            // The point `fraction` of the way from sample HALF-1 to sample HALF.
            let fraction = (p + 1) as f64 / PHASES as f64;
            let mut sum = 0.0;
            for (j, tap) in taps.iter_mut().enumerate() {
                let x = fraction - (j as f64 - (HALF - 1) as f64);
                let sinc = if x == 0.0 {
                    1.0
                } else {
                    (core::f64::consts::PI * x).sin() / (core::f64::consts::PI * x)
                };
                // A Blackman window over the span the taps cover.
                let w = core::f64::consts::PI * x / HALF as f64;
                let window = 0.42 + 0.5 * w.cos() + 0.08 * (2.0 * w).cos();
                let weight = sinc * window;
                *tap = weight as f32;
                sum += weight;
            }
            for tap in taps.iter_mut() {
                *tap = (f64::from(*tap) / sum) as f32;
            }
        }
        TruePeakMeter {
            kernel,
            history: [0.0; TAPS],
            max: 0.0,
        }
    }

    /// How many samples the meter answers behind the one it hears: the
    /// points between two samples need the samples after them too.
    pub const fn lag() -> usize {
        HALF
    }

    /// Hears one sample; returns the true peak around the sample heard
    /// [`lag`](Self::lag) samples ago, the latest the meter can answer for.
    #[inline]
    pub fn hear(&mut self, sample: f32) -> f32 {
        self.history.copy_within(1.., 0);
        self.history[TAPS - 1] = sample;
        // The sample at HALF-1 is complete, and so are the points after it.
        let mut here = self.history[HALF - 1].abs();
        for taps in &self.kernel {
            let value: f32 = taps.iter().zip(&self.history).map(|(w, h)| w * h).sum();
            here = here.max(value.abs());
        }
        self.max = self.max.max(here);
        here
    }

    /// Hears a block; returns the true peak within it (the points it
    /// completes, a few samples behind its end).
    pub fn push(&mut self, samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |m, &s| m.max(self.hear(s)))
    }

    /// The true peak heard so far.
    pub fn max(&self) -> f32 {
        self.max
    }

    /// Hears silence for the few samples the interpolation lags, so the
    /// end of the signal counts, and returns the true peak of it all.
    pub fn finish(&mut self) -> f32 {
        self.push(&[0.0; HALF]);
        self.max
    }
}

/// The two K-weighting filters of BS.1770 as the standard gives them, at
/// 48 kHz: a +4 dB high shelf from about 1.5 kHz (the head's effect on
/// what reaches the ears), then a high-pass at 38 Hz (the room). Each is
/// `[b0, b1, b2, a1, a2]` with `a0 = 1`.
const K_SHELF_48K: [f64; 5] = [
    1.535_124_859_586_97,
    -2.691_696_189_406_38,
    1.198_392_810_852_85,
    -1.690_659_293_182_41,
    0.732_480_774_215_85,
];
const K_HIGH_PASS_48K: [f64; 5] = [1.0, -2.0, 1.0, -1.990_047_454_833_98, 0.990_072_250_366_21];
/// The frequencies the two filters are anchored at: moving a filter to
/// another rate keeps its response exact there.
const K_SHELF_HZ: f64 = 1_681.974_450_955_533;
const K_HIGH_PASS_HZ: f64 = 38.135_470_876_024_44;

/// The K-weighting at `rate`: the standard's 48 kHz filters, each taken
/// back to the analog filter it discretises and discretised again at
/// `rate` (the bilinear transform both ways, pre-warped at the filter's
/// anchor frequency, so that frequency keeps its exact response). At 48 kHz
/// this reproduces the standard's coefficients.
fn k_weighting(rate: f64) -> [Biquad; 2] {
    [
        Biquad::moved(K_SHELF_48K, K_SHELF_HZ, 48_000.0, rate),
        Biquad::moved(K_HIGH_PASS_48K, K_HIGH_PASS_HZ, 48_000.0, rate),
    ]
}

/// Loudness in LUFS from a K-weighted mean square summed over channels.
fn lufs(mean_square: f64) -> f32 {
    (-0.691 + 10.0 * mean_square.log10()) as f32
}

/// 100 ms sub-blocks per 400 ms momentary window.
const MOMENTARY: usize = 4;
/// 100 ms sub-blocks per 3 s short-term window.
const SHORT_TERM: usize = 30;
/// Below this, a window is silence and never counts (absolute gate).
const ABSOLUTE_GATE: f32 = -70.0;
/// Windows this far under the ungated average are quiet and do not count
/// either (relative gate).
const RELATIVE_GATE: f32 = -10.0;

/// A loudness meter fed block by block: K-weighted, in 100 ms sub-blocks,
/// answering momentary (400 ms), short-term (3 s) and integrated (gated,
/// the whole programme) loudness in LUFS. Never allocates while fed
/// except to remember one number per 100 ms.
#[derive(Clone, Debug)]
pub struct Loudness {
    filters: Vec<[Biquad; 2]>,
    sub_len: usize,
    sub_fill: usize,
    sub_sum: Vec<f64>,
    /// Per 100 ms: the K-weighted mean square summed over channels.
    blocks: Vec<f64>,
}

impl Loudness {
    /// A meter for `channels` channels at `rate`.
    ///
    /// # Errors
    /// Zero channels.
    pub fn new(rate: SampleRate, channels: usize) -> Result<Self> {
        if channels == 0 {
            return Err(AuraError::InvalidArgument {
                what: "loudness channels",
            });
        }
        Ok(Loudness {
            filters: vec![k_weighting(f64::from(rate.hz())); channels],
            sub_len: (rate.hz() as usize / 10).max(1),
            sub_fill: 0,
            sub_sum: vec![0.0; channels],
            blocks: Vec::new(),
        })
    }

    /// Hears a block: one slice per channel, all the same length.
    ///
    /// # Errors
    /// The wrong number of channels.
    pub fn push(&mut self, planes: &[&[f32]]) -> Result<()> {
        if planes.len() != self.filters.len() {
            return Err(AuraError::ChannelMismatch {
                expected: self.filters.len(),
                actual: planes.len(),
            });
        }
        let frames = planes.first().map_or(0, |p| p.len());
        for n in 0..frames {
            for (ch, plane) in planes.iter().enumerate() {
                let Some(&s) = plane.get(n) else { continue };
                let [shelf, high_pass] = &mut self.filters[ch];
                let y = high_pass.process(shelf.process(f64::from(s)));
                self.sub_sum[ch] += y * y;
            }
            self.sub_fill += 1;
            if self.sub_fill == self.sub_len {
                let mean_square: f64 = self.sub_sum.iter().map(|s| s / self.sub_len as f64).sum();
                self.blocks.push(mean_square);
                self.sub_sum.iter_mut().for_each(|s| *s = 0.0);
                self.sub_fill = 0;
            }
        }
        Ok(())
    }

    /// Hears a planar block.
    ///
    /// # Errors
    /// The wrong number of channels.
    pub fn push_block(&mut self, block: &PlanarBuffer) -> Result<()> {
        let planes: Vec<&[f32]> = block.planes().collect();
        self.push(&planes)
    }

    /// Loudness of the last 400 ms, once that much was heard.
    pub fn momentary(&self) -> Option<f32> {
        self.last(MOMENTARY)
    }

    /// Loudness of the last 3 s, once that much was heard.
    pub fn short_term(&self) -> Option<f32> {
        self.last(SHORT_TERM)
    }

    fn last(&self, count: usize) -> Option<f32> {
        if self.blocks.len() < count {
            return None;
        }
        let tail = &self.blocks[self.blocks.len() - count..];
        Some(lufs(tail.iter().sum::<f64>() / count as f64))
    }

    /// Loudness of everything heard, gated: 400 ms windows every 100 ms,
    /// those under −70 LUFS dropped, then those more than 10 LU under the
    /// average of the rest dropped too. `None` before 400 ms, or when
    /// every window was silence.
    pub fn integrated(&self) -> Option<f32> {
        if self.blocks.len() < MOMENTARY {
            return None;
        }
        let windows: Vec<f64> = self
            .blocks
            .windows(MOMENTARY)
            .map(|w| w.iter().sum::<f64>() / MOMENTARY as f64)
            .filter(|&z| lufs(z) > ABSOLUTE_GATE)
            .collect();
        if windows.is_empty() {
            return None;
        }
        let relative = lufs(windows.iter().sum::<f64>() / windows.len() as f64) + RELATIVE_GATE;
        let kept: Vec<f64> = windows
            .into_iter()
            .filter(|&z| lufs(z) > relative)
            .collect();
        if kept.is_empty() {
            return None;
        }
        Some(lufs(kept.iter().sum::<f64>() / kept.len() as f64))
    }

    /// Seconds heard so far, to the 100 ms.
    pub fn seconds(&self) -> f32 {
        self.blocks.len() as f32 / 10.0
    }
}

/// What [`normalize`] did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Normalized {
    /// The gain applied, in dB.
    pub gain_db: f32,
    /// Integrated loudness before, in LUFS.
    pub loudness_before: f32,
    /// True peak before, in dBTP.
    pub true_peak_before: f32,
    /// Integrated loudness after, in LUFS: the target, unless the ceiling
    /// held the gain back.
    pub loudness_after: f32,
    /// True peak after, in dBTP: at or under the ceiling.
    pub true_peak_after: f32,
}

/// Brings `audio` to `target_lufs` integrated loudness with one gain,
/// unless that would push its true peak over `ceiling_dbtp`, in which case
/// the gain stops at the ceiling and the result says how far it got
/// ([`PODCAST_LOUDNESS`] and [`TRUE_PEAK_CEILING`] are the usual asks).
/// Returns `None`, touching nothing, when the audio is too quiet or short
/// to measure.
///
/// # Errors
/// No channels.
pub fn normalize(
    audio: &mut PlanarBuffer,
    rate: SampleRate,
    target_lufs: f32,
    ceiling_dbtp: f32,
) -> Result<Option<Normalized>> {
    let mut meter = Loudness::new(rate, audio.channels())?;
    meter.push_block(audio)?;
    let Some(loudness_before) = meter.integrated() else {
        return Ok(None);
    };
    let true_peak_before = linear_to_db(audio.planes().map(true_peak).fold(0.0f32, f32::max));
    let wanted = target_lufs - loudness_before;
    let gain_db = wanted.min(ceiling_dbtp - true_peak_before);
    audio.apply_gain(db_to_linear(gain_db));
    Ok(Some(Normalized {
        gain_db,
        loudness_before,
        true_peak_before,
        loudness_after: loudness_before + gain_db,
        true_peak_after: true_peak_before + gain_db,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: SampleRate = SampleRate::HZ_48K;

    /// `seconds` of a sine at `hz` and `amplitude`, starting at `phase`.
    fn sine(hz: f32, amplitude: f32, seconds: f32, phase: f32) -> Vec<f32> {
        let n = (seconds * R.hz() as f32) as usize;
        (0..n)
            .map(|i| {
                amplitude * (phase + core::f32::consts::TAU * hz * i as f32 / R.hz() as f32).sin()
            })
            .collect()
    }

    /// The first and last 200 samples ramped, so the signal does not start
    /// or stop with a step: a bandlimited step overshoots, and a true-peak
    /// meter is right to say so.
    fn faded(mut s: Vec<f32>) -> Vec<f32> {
        let n = s.len();
        for i in 0..200.min(n) {
            let g = i as f32 / 200.0;
            s[i] *= g;
            s[n - 1 - i] *= g;
        }
        s
    }

    #[test]
    fn peak_and_rms() {
        assert_eq!(peak(&[]), 0.0);
        assert_eq!(rms(&[]), 0.0);
        assert_eq!(peak(&[0.1, -0.7, 0.3]), 0.7);
        let s = sine(1_000.0, 1.0, 1.0, 0.0);
        assert!((peak(&s) - 1.0).abs() < 1e-3);
        assert!((rms(&s) - core::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3);
    }

    #[test]
    fn true_peak_sees_between_the_samples() {
        // A sine at an eighth of the rate, sampled off its crests: the
        // samples reach cos(22.5°) = 0.924, the waveform reaches 1.0.
        let s = faded(sine(
            R.hz() as f32 / 8.0,
            1.0,
            0.1,
            core::f32::consts::PI / 8.0,
        ));
        let sample_peak = peak(&s);
        assert!((sample_peak - 0.924).abs() < 1e-3, "{sample_peak}");
        let tp = true_peak(&s);
        assert!(tp > 0.99 && tp < 1.01, "true peak {tp}");
        // On the crests the two agree.
        let on = faded(sine(
            R.hz() as f32 / 8.0,
            0.5,
            0.1,
            core::f32::consts::PI / 2.0,
        ));
        let tp_on = true_peak(&on);
        assert!((tp_on - 0.5).abs() < 0.005, "{tp_on}");
        // A tone that starts with a bang overshoots between the samples,
        // and the meter says so.
        let bang = sine(R.hz() as f32 / 8.0, 0.5, 0.01, core::f32::consts::PI / 2.0);
        assert!(true_peak(&bang) > 0.52);
        // Block by block, the meter sees the same, with the lag flushed out.
        let mut meter = TruePeakMeter::new();
        for block in s.chunks(100) {
            meter.push(block);
        }
        assert!((meter.finish() - tp).abs() < 1e-6);
        assert_eq!(TruePeakMeter::default().max(), 0.0);
        assert_eq!(true_peak(&[]), 0.0);
    }

    #[test]
    fn the_k_weighting_at_48k_is_the_standards() {
        let [shelf, high_pass] = k_weighting(48_000.0);
        let [b0, b1, b2, a1, a2] = K_SHELF_48K;
        for (got, want) in [shelf.b0, shelf.b1, shelf.b2, shelf.a1, shelf.a2]
            .iter()
            .zip([b0, b1, b2, a1, a2])
        {
            assert!((got - want).abs() < 1e-9, "{got} vs {want}");
        }
        assert!((high_pass.a1 - K_HIGH_PASS_48K[3]).abs() < 1e-9);
        // At another rate the anchor frequency keeps its response: the
        // high-pass is 3 dB down near its corner either way.
        let gain = |filter: &Biquad, hz: f64, rate: f64| {
            let mut f = *filter;
            let n = (rate as usize) / 2;
            let mut peak = 0.0f64;
            for i in 0..n {
                let y = f.process((2.0 * core::f64::consts::PI * hz * i as f64 / rate).sin());
                if i > n / 2 {
                    peak = peak.max(y.abs());
                }
            }
            20.0 * peak.log10()
        };
        let [_, hp44] = k_weighting(44_100.0);
        assert!(
            (gain(&high_pass, 1_000.0, 48_000.0) - gain(&hp44, 1_000.0, 44_100.0)).abs() < 0.05
        );
        assert!((gain(&high_pass, 38.0, 48_000.0) - gain(&hp44, 38.0, 44_100.0)).abs() < 0.05);
    }

    #[test]
    fn a_full_scale_tone_reads_minus_three_lufs() {
        let s = sine(1_000.0, 1.0, 2.0, 0.0);
        let mut meter = Loudness::new(R, 1).unwrap();
        assert_eq!(meter.momentary(), None);
        assert_eq!(meter.integrated(), None);
        meter.push(&[&s]).unwrap();
        assert!((meter.seconds() - 2.0).abs() < 1e-6);
        let integrated = meter.integrated().unwrap();
        assert!((integrated + 3.01).abs() < 0.1, "{integrated} LUFS");
        assert!((meter.momentary().unwrap() + 3.01).abs() < 0.1);
        let short = meter.short_term();
        assert!(short.is_none(), "under three seconds: {short:?}");
        meter.push(&[&s]).unwrap();
        assert!((meter.short_term().unwrap() + 3.01).abs() < 0.1);

        // 20 dB down reads 20 LU down; stereo of the same doubles the power.
        let quiet = sine(1_000.0, 0.1, 1.0, 0.0);
        let mut meter = Loudness::new(R, 1).unwrap();
        meter.push(&[&quiet]).unwrap();
        assert!((meter.integrated().unwrap() + 23.01).abs() < 0.1);
        let mut stereo = Loudness::new(R, 2).unwrap();
        stereo.push(&[&s[..48_000], &s[..48_000]]).unwrap();
        assert!((stereo.integrated().unwrap() + 0.0).abs() < 0.1);
        // Another rate gives the same answer.
        let mut cd = Loudness::new(SampleRate::CD, 1).unwrap();
        let s44: Vec<f32> = (0..44_100)
            .map(|i| (core::f32::consts::TAU * 1_000.0 * i as f32 / 44_100.0).sin())
            .collect();
        cd.push(&[&s44]).unwrap();
        assert!((cd.integrated().unwrap() + 3.01).abs() < 0.1);
        assert!(Loudness::new(R, 0).is_err());
        assert!(meter.push(&[&quiet, &quiet]).is_err(), "channel mismatch");
    }

    #[test]
    fn gating_leaves_silence_out() {
        let mut meter = Loudness::new(R, 1).unwrap();
        meter.push(&[&vec![0.0; 48_000]]).unwrap();
        assert_eq!(meter.integrated(), None, "silence has no loudness");
        assert!(meter.momentary().unwrap() < -100.0);
        // One second of tone, then ten seconds of near silence: the
        // programme is as loud as the tone.
        // (Within a dB: the windows straddling the edge of the tone are
        // partly tone, loud enough to count, and quieter than it.)
        let mut meter = Loudness::new(R, 1).unwrap();
        meter.push(&[&sine(1_000.0, 1.0, 1.0, 0.0)]).unwrap();
        meter.push(&[&sine(1_000.0, 0.000_1, 10.0, 0.0)]).unwrap();
        let integrated = meter.integrated().unwrap();
        assert!((integrated + 3.01).abs() < 1.0, "{integrated}");
        assert!(meter.short_term().unwrap() < -70.0, "the tail is quiet");
        // A quiet stretch above the absolute gate but far under the loud
        // part is dropped by the relative gate.
        let mut meter = Loudness::new(R, 1).unwrap();
        meter.push(&[&sine(1_000.0, 1.0, 2.0, 0.0)]).unwrap();
        meter.push(&[&sine(1_000.0, 0.01, 2.0, 0.0)]).unwrap(); // −43 LUFS
        let integrated = meter.integrated().unwrap();
        assert!((integrated + 3.01).abs() < 1.0, "{integrated}");
        assert!(
            integrated < -3.01,
            "the edge windows count, the quiet ones do not"
        );
    }

    #[test]
    fn normalizing_hits_the_target_unless_the_ceiling_stops_it() {
        let mut audio = PlanarBuffer::from_planes(&[&sine(1_000.0, 1.0, 2.0, 0.0)]).unwrap();
        let done = normalize(&mut audio, R, PODCAST_LOUDNESS, TRUE_PEAK_CEILING)
            .unwrap()
            .unwrap();
        assert!((done.loudness_before + 3.01).abs() < 0.1);
        assert!(
            done.true_peak_before.abs() < 0.1,
            "{} dBTP",
            done.true_peak_before
        );
        assert!((done.gain_db + 12.99).abs() < 0.1, "{}", done.gain_db);
        assert!((done.loudness_after - PODCAST_LOUDNESS).abs() < 0.1);
        let mut check = Loudness::new(R, 1).unwrap();
        check.push_block(&audio).unwrap();
        assert!((check.integrated().unwrap() - PODCAST_LOUDNESS).abs() < 0.1);

        // Asking for louder than the ceiling allows: the ceiling wins.
        let mut audio = PlanarBuffer::from_planes(&[&sine(1_000.0, 1.0, 2.0, 0.0)]).unwrap();
        let done = normalize(&mut audio, R, -3.0, TRUE_PEAK_CEILING)
            .unwrap()
            .unwrap();
        assert!((done.gain_db + 1.0).abs() < 0.1, "{}", done.gain_db);
        assert!((done.true_peak_after + 1.0).abs() < 0.1);
        assert!(done.loudness_after < -3.5);

        // Silence: nothing to measure, nothing done.
        let mut silent = PlanarBuffer::new(2, 48_000);
        assert_eq!(normalize(&mut silent, R, -16.0, -1.0).unwrap(), None);
        assert!(normalize(&mut PlanarBuffer::new(0, 10), R, -16.0, -1.0).is_err());
    }
}
