//! Dynamics: the processors that turn a level into a gain. A compressor, a
//! true-peak limiter with lookahead, a gate (and expander), a de-esser and
//! a slow leveler, each a [`Node`] for the insert chain, each driven by a
//! shared `Params` struct of atomics a control thread sets while the audio
//! thread reads. Clean textbook designs: a peak or RMS detector, a static
//! curve in decibels, attack and release on the gain. Character and noise
//! reduction are a plugin's job.
//!
//! Every node here is stereo-linked: one gain for all its channels, from
//! the loudest of them, so the image never shifts. `prepare` allocates
//! the lookahead and the detector state; `process` never does.

use std::sync::Arc;

use crate::primitives::analysis::filter::Biquad;
use crate::primitives::analysis::meter::TruePeakMeter;
use crate::primitives::buffers::gain::{db_to_linear, linear_to_db};
use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::graph::{Node, NodeChannels, ProcessContext, ProcessStatus};
use crate::primitives::realtime::AtomicF32;
use crate::primitives::SampleRate;

/// The one-pole coefficient that gets 63% of the way in `ms`
/// milliseconds; 0 for an instant move.
fn coefficient(ms: f32, rate: f32) -> f32 {
    if ms <= 0.0 {
        0.0
    } else {
        (-1.0 / (ms * 0.001 * rate)).exp()
    }
}

/// Moves `current` toward `target` by one sample: `attack` when the
/// reduction grows, `release` when it shrinks (both one-pole
/// coefficients).
#[inline]
fn follow(current: f32, target: f32, attack: f32, release: f32) -> f32 {
    let c = if target > current { attack } else { release };
    target + c * (current - target)
}

/// The loudest sample across channels at frame `n`.
#[inline]
fn peak_at(input: &PlanarBuffer, n: usize) -> f32 {
    input.planes().fold(0.0f32, |m, p| m.max(p[n].abs()))
}

/// The static curve of a compressor: how many dB to take off a signal at
/// `level_db`, given a threshold, a ratio and a soft knee of `knee_db`
/// around the threshold. Zero or more.
fn compression_db(level_db: f32, threshold_db: f32, ratio: f32, knee_db: f32) -> f32 {
    let over = level_db - threshold_db;
    let slope = 1.0 - 1.0 / ratio.max(1.0);
    if knee_db > 0.0 && over.abs() <= knee_db / 2.0 {
        let x = over + knee_db / 2.0;
        slope * x * x / (2.0 * knee_db)
    } else if over > 0.0 {
        slope * over
    } else {
        0.0
    }
}

// ---------------------------------------------------------------------------
// Compressor

/// A compressor's controls, set from the control thread, read every
/// sample by the audio thread. `reduction_db` is the other way round: what
/// the compressor is doing right now, for a meter.
#[derive(Debug)]
pub struct CompressorParams {
    threshold_db: AtomicF32,
    ratio: AtomicF32,
    attack_ms: AtomicF32,
    release_ms: AtomicF32,
    knee_db: AtomicF32,
    makeup_db: AtomicF32,
    reduction_db: AtomicF32,
}

impl CompressorParams {
    /// A compressor at `threshold_db` with `ratio` to one; 10 ms attack,
    /// 100 ms release, a 6 dB knee, no makeup.
    pub fn new(threshold_db: f32, ratio: f32) -> Self {
        CompressorParams {
            threshold_db: AtomicF32::new(threshold_db),
            ratio: AtomicF32::new(ratio),
            attack_ms: AtomicF32::new(10.0),
            release_ms: AtomicF32::new(100.0),
            knee_db: AtomicF32::new(6.0),
            makeup_db: AtomicF32::new(0.0),
            reduction_db: AtomicF32::new(0.0),
        }
    }

    /// Where compression begins, in dBFS of the loudest channel's peak.
    pub fn threshold_db(&self) -> f32 {
        self.threshold_db.get()
    }

    /// Sets the threshold.
    pub fn set_threshold_db(&self, db: f32) {
        self.threshold_db.set(db);
    }

    /// How many dB in make one dB out above the threshold.
    pub fn ratio(&self) -> f32 {
        self.ratio.get()
    }

    /// Sets the ratio (1 does nothing, 20 is nearly a limiter).
    pub fn set_ratio(&self, ratio: f32) {
        self.ratio.set(ratio.max(1.0));
    }

    /// How fast the gain comes down when the signal rises, in ms.
    pub fn attack_ms(&self) -> f32 {
        self.attack_ms.get()
    }

    /// Sets the attack.
    pub fn set_attack_ms(&self, ms: f32) {
        self.attack_ms.set(ms.max(0.0));
    }

    /// How fast the gain comes back when the signal falls, in ms.
    pub fn release_ms(&self) -> f32 {
        self.release_ms.get()
    }

    /// Sets the release.
    pub fn set_release_ms(&self, ms: f32) {
        self.release_ms.set(ms.max(0.0));
    }

    /// The width of the soft knee around the threshold, in dB; 0 is hard.
    pub fn knee_db(&self) -> f32 {
        self.knee_db.get()
    }

    /// Sets the knee.
    pub fn set_knee_db(&self, db: f32) {
        self.knee_db.set(db.max(0.0));
    }

    /// Gain added after compression, in dB.
    pub fn makeup_db(&self) -> f32 {
        self.makeup_db.get()
    }

    /// Sets the makeup gain.
    pub fn set_makeup_db(&self, db: f32) {
        self.makeup_db.set(db);
    }

    /// How much the compressor took off at the end of the last block, in
    /// dB (zero or more): for a gain-reduction meter.
    pub fn reduction_db(&self) -> f32 {
        self.reduction_db.get()
    }
}

/// A feed-forward compressor: the loudest channel's peak, in dB, through a
/// soft-knee curve, with attack and release on the gain, applied to every
/// channel alike.
pub struct Compressor {
    params: Arc<CompressorParams>,
    channels: usize,
    rate: f32,
    /// The detected peak level, linear: instant attack, the release.
    envelope: f32,
    /// Current gain reduction, dB, zero or more.
    reduction: f32,
}

impl Compressor {
    /// A compressor over `channels` channels driven by `params`.
    pub fn new(channels: usize, params: Arc<CompressorParams>) -> Self {
        Compressor {
            params,
            channels,
            rate: 48_000.0,
            envelope: 0.0,
            reduction: 0.0,
        }
    }
}

impl Node for Compressor {
    fn name(&self) -> &str {
        "compressor"
    }

    fn channels(&self) -> NodeChannels {
        NodeChannels::symmetric(self.channels)
    }

    fn prepare(&mut self, sample_rate: SampleRate, _max_frames: usize) {
        self.rate = sample_rate.hz() as f32;
        self.reset();
    }

    fn reset(&mut self) {
        self.envelope = 0.0;
        self.reduction = 0.0;
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus {
        if ctx.input_is_silent {
            self.envelope = 0.0;
            self.reduction = 0.0;
            self.params.reduction_db.set(0.0);
            return ProcessStatus::Silent;
        }
        if output.copy_from(input).is_err() {
            return ProcessStatus::Silent;
        }
        let p = &self.params;
        let (threshold, ratio, knee, makeup) = (
            p.threshold_db.get(),
            p.ratio.get(),
            p.knee_db.get(),
            p.makeup_db.get(),
        );
        let attack = coefficient(p.attack_ms.get(), self.rate);
        let release = coefficient(p.release_ms.get(), self.rate);
        for n in 0..ctx.frames {
            // A peak detector that holds through a tone's zero crossings
            // (instant attack, the release), the curve on it, and the attack
            // and release on the gain it asks for.
            self.envelope = follow(self.envelope, peak_at(input, n), 0.0, release);
            let level = linear_to_db(self.envelope.max(1e-9));
            let wanted = compression_db(level, threshold, ratio, knee);
            self.reduction = follow(self.reduction, wanted, attack, release);
            let gain = db_to_linear(makeup - self.reduction);
            for plane in output.planes_mut() {
                plane[n] *= gain;
            }
        }
        p.reduction_db.set(self.reduction);
        ProcessStatus::Normal
    }
}

// ---------------------------------------------------------------------------
// Limiter

/// A limiter's controls.
#[derive(Debug)]
pub struct LimiterParams {
    ceiling_db: AtomicF32,
    release_ms: AtomicF32,
    reduction_db: AtomicF32,
}

impl LimiterParams {
    /// A limiter that lets nothing past `ceiling_db` (true peak, dBTP);
    /// 50 ms release.
    pub fn new(ceiling_db: f32) -> Self {
        LimiterParams {
            ceiling_db: AtomicF32::new(ceiling_db),
            release_ms: AtomicF32::new(50.0),
            reduction_db: AtomicF32::new(0.0),
        }
    }

    /// The true-peak ceiling, in dBTP.
    pub fn ceiling_db(&self) -> f32 {
        self.ceiling_db.get()
    }

    /// Sets the ceiling.
    pub fn set_ceiling_db(&self, db: f32) {
        self.ceiling_db.set(db.min(0.0));
    }

    /// How fast the gain comes back after a peak, in ms.
    pub fn release_ms(&self) -> f32 {
        self.release_ms.get()
    }

    /// Sets the release.
    pub fn set_release_ms(&self, ms: f32) {
        self.release_ms.set(ms.max(0.0));
    }

    /// How much the limiter took off at the end of the last block, in dB.
    pub fn reduction_db(&self) -> f32 {
        self.reduction_db.get()
    }
}

/// How far ahead the limiter looks, in milliseconds: the gain reaches a
/// peak's reduction as the peak arrives, ramping over this long.
pub const LIMITER_LOOKAHEAD_MS: f32 = 5.0;

/// A true-peak limiter with lookahead. A true-peak detector per channel
/// (four points interpolated between samples) says how much each moment
/// exceeds the ceiling; the largest excess over the next few milliseconds,
/// averaged over the same span, is the reduction applied to the delayed
/// audio, so it reaches a peak's full reduction exactly as the peak passes
/// and never clicks; a release lets it back up afterwards. Reports its
/// delay as latency so the graph lines the other paths up. A final clip at
/// the ceiling catches what rounding leaves.
pub struct Limiter {
    params: Arc<LimiterParams>,
    channels: usize,
    rate: f32,
    /// Lookahead in frames (without the detector's own lag).
    lookahead: usize,
    /// One true-peak detector per channel.
    detectors: Vec<TruePeakMeter>,
    /// The audio, delayed by lookahead + detector lag: one ring per channel.
    delay: Vec<Vec<f32>>,
    /// Required reduction per upcoming frame, a ring the size of the lookahead.
    required: Vec<f32>,
    /// Indices into `required` whose values decrease: the sliding maximum.
    deque: Vec<usize>,
    deque_head: usize,
    deque_len: usize,
    /// The sliding maximum per frame, for the running average.
    maxima: Vec<f32>,
    sum: f32,
    write: usize,
    reduction: f32,
}

impl Limiter {
    /// A limiter over `channels` channels driven by `params`.
    pub fn new(channels: usize, params: Arc<LimiterParams>) -> Self {
        Limiter {
            params,
            channels,
            rate: 48_000.0,
            lookahead: 0,
            detectors: Vec::new(),
            delay: Vec::new(),
            required: Vec::new(),
            deque: Vec::new(),
            deque_head: 0,
            deque_len: 0,
            maxima: Vec::new(),
            sum: 0.0,
            write: 0,
            reduction: 0.0,
        }
    }

    fn total_delay(&self) -> usize {
        self.lookahead + TruePeakMeter::lag()
    }

    /// Pushes this frame's required reduction into the lookahead window
    /// and returns the window's maximum (a monotonic deque over a ring).
    #[inline]
    fn window_max(&mut self, required: f32) -> f32 {
        let l = self.required.len();
        let at = self.write % l;
        // The index leaving the window.
        if self.deque_len > 0 && self.deque[self.deque_head] == at {
            self.deque_head = (self.deque_head + 1) % l;
            self.deque_len -= 1;
        }
        // Drop everything smaller than the newcomer from the back.
        while self.deque_len > 0 {
            let back = (self.deque_head + self.deque_len - 1) % l;
            if self.required[self.deque[back]] <= required {
                self.deque_len -= 1;
            } else {
                break;
            }
        }
        self.required[at] = required;
        let back = (self.deque_head + self.deque_len) % l;
        self.deque[back] = at;
        self.deque_len += 1;
        self.required[self.deque[self.deque_head]]
    }
}

impl Node for Limiter {
    fn name(&self) -> &str {
        "limiter"
    }

    fn channels(&self) -> NodeChannels {
        NodeChannels::symmetric(self.channels)
    }

    fn prepare(&mut self, sample_rate: SampleRate, _max_frames: usize) {
        self.rate = sample_rate.hz() as f32;
        self.lookahead = ((LIMITER_LOOKAHEAD_MS * 0.001 * self.rate) as usize).max(1);
        self.detectors = vec![TruePeakMeter::new(); self.channels];
        self.delay = vec![vec![0.0; self.total_delay()]; self.channels];
        self.required = vec![0.0; self.lookahead];
        self.deque = vec![0; self.lookahead];
        self.maxima = vec![0.0; self.lookahead];
        self.reset();
    }

    fn reset(&mut self) {
        for d in &mut self.detectors {
            *d = TruePeakMeter::new();
        }
        for ring in &mut self.delay {
            ring.iter_mut().for_each(|s| *s = 0.0);
        }
        self.required.iter_mut().for_each(|r| *r = 0.0);
        self.maxima.iter_mut().for_each(|m| *m = 0.0);
        self.deque_head = 0;
        self.deque_len = 0;
        self.sum = 0.0;
        self.write = 0;
        self.reduction = 0.0;
    }

    fn latency_frames(&self) -> usize {
        self.total_delay()
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus {
        if self.required.is_empty() {
            return ProcessStatus::Silent; // not prepared
        }
        if output.set_frames(ctx.frames).is_err() {
            return ProcessStatus::Silent;
        }
        let ceiling_db = self.params.ceiling_db.get();
        let ceiling = db_to_linear(ceiling_db);
        let release = coefficient(self.params.release_ms.get(), self.rate);
        let total = self.total_delay();
        let l = self.lookahead as f32;
        for n in 0..ctx.frames {
            // What this frame will need once it comes out of the delay: the
            // detectors answer for the frame `lag` samples back, which is
            // why the delay is lookahead + lag.
            let mut tp = 0.0f32;
            for (ch, detector) in self.detectors.iter_mut().enumerate() {
                let sample = if ctx.input_is_silent {
                    0.0
                } else {
                    input.channel(ch)[n]
                };
                tp = tp.max(detector.hear(sample));
            }
            let required = if tp > ceiling {
                linear_to_db(tp) - ceiling_db
            } else {
                0.0
            };
            let maximum = self.window_max(required);
            // Running average of the window maxima over the lookahead: a
            // ramp that arrives at the full reduction with the peak.
            let slot = self.write % self.maxima.len();
            self.sum += maximum - self.maxima[slot];
            self.maxima[slot] = maximum;
            let smooth = (self.sum / l).max(0.0);
            // Attack is the ramp; release is a one-pole back toward it.
            self.reduction = if smooth >= self.reduction {
                smooth
            } else {
                smooth + release * (self.reduction - smooth)
            };
            let gain = db_to_linear(-self.reduction);
            let at = self.write % total;
            for (ch, ring) in self.delay.iter_mut().enumerate() {
                let delayed = ring[at];
                ring[at] = if ctx.input_is_silent {
                    0.0
                } else {
                    input.channel(ch)[n]
                };
                output.channel_mut(ch)[n] = (delayed * gain).clamp(-ceiling, ceiling);
            }
            self.write = self.write.wrapping_add(1);
        }
        self.params.reduction_db.set(self.reduction);
        if ctx.input_is_silent && output.is_silent() {
            ProcessStatus::Silent
        } else {
            ProcessStatus::Normal
        }
    }
}

// ---------------------------------------------------------------------------
// Gate

/// A gate's controls.
#[derive(Debug)]
pub struct GateParams {
    threshold_db: AtomicF32,
    ratio: AtomicF32,
    attack_ms: AtomicF32,
    hold_ms: AtomicF32,
    release_ms: AtomicF32,
    range_db: AtomicF32,
    reduction_db: AtomicF32,
}

impl GateParams {
    /// A gate closing below `threshold_db`: 1 ms attack, 50 ms hold,
    /// 100 ms release, 80 dB of attenuation when closed, an infinite
    /// ratio (set one for an expander).
    pub fn new(threshold_db: f32) -> Self {
        GateParams {
            threshold_db: AtomicF32::new(threshold_db),
            ratio: AtomicF32::new(f32::INFINITY),
            attack_ms: AtomicF32::new(1.0),
            hold_ms: AtomicF32::new(50.0),
            release_ms: AtomicF32::new(100.0),
            range_db: AtomicF32::new(80.0),
            reduction_db: AtomicF32::new(0.0),
        }
    }

    /// Below this peak level, in dBFS, the gate closes.
    pub fn threshold_db(&self) -> f32 {
        self.threshold_db.get()
    }

    /// Sets the threshold.
    pub fn set_threshold_db(&self, db: f32) {
        self.threshold_db.set(db);
    }

    /// How many dB the signal drops for each dB it falls under the
    /// threshold: infinity is a gate, 2 to 4 an expander.
    pub fn ratio(&self) -> f32 {
        self.ratio.get()
    }

    /// Sets the ratio.
    pub fn set_ratio(&self, ratio: f32) {
        self.ratio.set(ratio.max(1.0));
    }

    /// How fast the gate opens, in ms.
    pub fn attack_ms(&self) -> f32 {
        self.attack_ms.get()
    }

    /// Sets the attack.
    pub fn set_attack_ms(&self, ms: f32) {
        self.attack_ms.set(ms.max(0.0));
    }

    /// How long the gate stays open after the signal fell under the
    /// threshold, in ms, before it starts to close.
    pub fn hold_ms(&self) -> f32 {
        self.hold_ms.get()
    }

    /// Sets the hold.
    pub fn set_hold_ms(&self, ms: f32) {
        self.hold_ms.set(ms.max(0.0));
    }

    /// How fast the gate closes, in ms.
    pub fn release_ms(&self) -> f32 {
        self.release_ms.get()
    }

    /// Sets the release.
    pub fn set_release_ms(&self, ms: f32) {
        self.release_ms.set(ms.max(0.0));
    }

    /// The most the gate takes off when closed, in dB.
    pub fn range_db(&self) -> f32 {
        self.range_db.get()
    }

    /// Sets the range.
    pub fn set_range_db(&self, db: f32) {
        self.range_db.set(db.max(0.0));
    }

    /// How much the gate is taking off at the end of the last block, in dB.
    pub fn reduction_db(&self) -> f32 {
        self.reduction_db.get()
    }
}

/// A gate, or an expander when its ratio is finite: below the threshold
/// the signal is turned down, by the range (gate) or by the ratio
/// (expander), after a hold; above it the gate opens with the attack.
/// Stereo-linked on the loudest channel's peak.
pub struct Gate {
    params: Arc<GateParams>,
    channels: usize,
    rate: f32,
    /// Smoothed peak level, linear.
    envelope: f32,
    /// Samples of hold left before the gate may close.
    hold_left: usize,
    reduction: f32,
}

impl Gate {
    /// A gate over `channels` channels driven by `params`.
    pub fn new(channels: usize, params: Arc<GateParams>) -> Self {
        Gate {
            params,
            channels,
            rate: 48_000.0,
            envelope: 0.0,
            hold_left: 0,
            reduction: 0.0,
        }
    }
}

impl Node for Gate {
    fn name(&self) -> &str {
        "gate"
    }

    fn channels(&self) -> NodeChannels {
        NodeChannels::symmetric(self.channels)
    }

    fn prepare(&mut self, sample_rate: SampleRate, _max_frames: usize) {
        self.rate = sample_rate.hz() as f32;
        self.reset();
    }

    fn reset(&mut self) {
        self.envelope = 0.0;
        self.hold_left = 0;
        self.reduction = 0.0;
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus {
        let p = &self.params;
        let range = p.range_db.get();
        if ctx.input_is_silent {
            // Nothing to pass; the gate closes behind the silence.
            self.reduction = range;
            self.hold_left = 0;
            p.reduction_db.set(range);
            return ProcessStatus::Silent;
        }
        if output.copy_from(input).is_err() {
            return ProcessStatus::Silent;
        }
        let threshold = p.threshold_db.get();
        let ratio = p.ratio.get();
        let hold = (p.hold_ms.get() * 0.001 * self.rate) as usize;
        let attack = coefficient(p.attack_ms.get(), self.rate);
        let release = coefficient(p.release_ms.get(), self.rate);
        // The detector follows peaks fast and lets go in a millisecond.
        let detector_release = coefficient(1.0, self.rate);
        for n in 0..ctx.frames {
            let peak = peak_at(input, n);
            self.envelope = if peak > self.envelope {
                peak
            } else {
                peak + detector_release * (self.envelope - peak)
            };
            let level = linear_to_db(self.envelope.max(1e-9));
            let wanted = if level >= threshold {
                self.hold_left = hold;
                0.0
            } else if self.hold_left > 0 {
                self.hold_left -= 1;
                0.0
            } else if ratio.is_finite() {
                ((threshold - level) * (ratio - 1.0)).min(range)
            } else {
                range
            };
            // Opening is the attack (reduction shrinking), closing the release.
            self.reduction = follow(self.reduction, wanted, release, attack);
            let gain = db_to_linear(-self.reduction);
            for plane in output.planes_mut() {
                plane[n] *= gain;
            }
        }
        p.reduction_db.set(self.reduction);
        ProcessStatus::Normal
    }
}

// ---------------------------------------------------------------------------
// De-esser

/// A de-esser's controls.
#[derive(Debug)]
pub struct DeEsserParams {
    frequency_hz: AtomicF32,
    threshold_db: AtomicF32,
    ratio: AtomicF32,
    reduction_db: AtomicF32,
}

impl DeEsserParams {
    /// A de-esser listening around `frequency_hz` (6.5 kHz suits most
    /// voices), compressing 4:1 above `threshold_db` of that band.
    pub fn new(frequency_hz: f32, threshold_db: f32) -> Self {
        DeEsserParams {
            frequency_hz: AtomicF32::new(frequency_hz),
            threshold_db: AtomicF32::new(threshold_db),
            ratio: AtomicF32::new(4.0),
            reduction_db: AtomicF32::new(0.0),
        }
    }

    /// The centre of the band the detector listens to, in Hz.
    pub fn frequency_hz(&self) -> f32 {
        self.frequency_hz.get()
    }

    /// Sets the frequency; the filter follows at the next block.
    pub fn set_frequency_hz(&self, hz: f32) {
        self.frequency_hz.set(hz.clamp(1_000.0, 16_000.0));
    }

    /// The level of the band, in dBFS, above which the de-esser acts.
    pub fn threshold_db(&self) -> f32 {
        self.threshold_db.get()
    }

    /// Sets the threshold.
    pub fn set_threshold_db(&self, db: f32) {
        self.threshold_db.set(db);
    }

    /// How hard it acts, as a compression ratio.
    pub fn ratio(&self) -> f32 {
        self.ratio.get()
    }

    /// Sets the ratio.
    pub fn set_ratio(&self, ratio: f32) {
        self.ratio.set(ratio.max(1.0));
    }

    /// How much the de-esser is taking off at the end of the last block.
    pub fn reduction_db(&self) -> f32 {
        self.reduction_db.get()
    }
}

/// A de-esser: a compressor whose detector hears only a band around the
/// sibilance (a band-pass sidechain), turning the whole signal down for
/// the moment an "s" is too loud. Fast: 0.5 ms attack, 40 ms release.
pub struct DeEsser {
    params: Arc<DeEsserParams>,
    channels: usize,
    rate: f32,
    /// The sidechain band-pass, one per channel.
    sidechain: Vec<Biquad>,
    /// The frequency the sidechain was designed for.
    designed_hz: f32,
    /// The band's peak level, linear: instant attack, the release.
    envelope: f32,
    reduction: f32,
}

impl DeEsser {
    /// A de-esser over `channels` channels driven by `params`.
    pub fn new(channels: usize, params: Arc<DeEsserParams>) -> Self {
        DeEsser {
            params,
            channels,
            rate: 48_000.0,
            sidechain: Vec::new(),
            designed_hz: 0.0,
            envelope: 0.0,
            reduction: 0.0,
        }
    }

    fn design(&mut self) {
        let hz = self.params.frequency_hz.get();
        let filter = Biquad::band_pass(f64::from(self.rate), f64::from(hz), 1.0);
        for f in &mut self.sidechain {
            *f = filter;
        }
        self.designed_hz = hz;
    }
}

impl Node for DeEsser {
    fn name(&self) -> &str {
        "de-esser"
    }

    fn channels(&self) -> NodeChannels {
        NodeChannels::symmetric(self.channels)
    }

    fn prepare(&mut self, sample_rate: SampleRate, _max_frames: usize) {
        self.rate = sample_rate.hz() as f32;
        self.sidechain = vec![Biquad::band_pass(48_000.0, 6_500.0, 1.0); self.channels];
        self.design();
        self.reduction = 0.0;
    }

    fn reset(&mut self) {
        for f in &mut self.sidechain {
            f.reset();
        }
        self.envelope = 0.0;
        self.reduction = 0.0;
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus {
        if ctx.input_is_silent {
            self.envelope = 0.0;
            self.reduction = 0.0;
            self.params.reduction_db.set(0.0);
            return ProcessStatus::Silent;
        }
        if output.copy_from(input).is_err() || self.sidechain.is_empty() {
            return ProcessStatus::Silent;
        }
        if self.params.frequency_hz.get() != self.designed_hz {
            self.design(); // a few multiplications, no allocation
        }
        let threshold = self.params.threshold_db.get();
        let ratio = self.params.ratio.get();
        let attack = coefficient(0.5, self.rate);
        let release = coefficient(40.0, self.rate);
        for n in 0..ctx.frames {
            let mut band = 0.0f32;
            for (ch, filter) in self.sidechain.iter_mut().enumerate() {
                let y = filter.process(f64::from(input.channel(ch)[n])) as f32;
                band = band.max(y.abs());
            }
            self.envelope = follow(self.envelope, band, 0.0, release);
            let level = linear_to_db(self.envelope.max(1e-9));
            let wanted = compression_db(level, threshold, ratio, 0.0);
            self.reduction = follow(self.reduction, wanted, attack, release);
            let gain = db_to_linear(-self.reduction);
            for plane in output.planes_mut() {
                plane[n] *= gain;
            }
        }
        self.params.reduction_db.set(self.reduction);
        ProcessStatus::Normal
    }
}

// ---------------------------------------------------------------------------
// Leveler

/// A leveler's controls.
#[derive(Debug)]
pub struct LevelerParams {
    target_db: AtomicF32,
    range_db: AtomicF32,
    speed_db_per_s: AtomicF32,
    floor_db: AtomicF32,
    gain_db: AtomicF32,
}

impl LevelerParams {
    /// A leveler riding the gain toward `target_db` RMS: at most ±12 dB,
    /// 3 dB per second, holding still under −50 dBFS so it never rides
    /// silence up.
    pub fn new(target_db: f32) -> Self {
        LevelerParams {
            target_db: AtomicF32::new(target_db),
            range_db: AtomicF32::new(12.0),
            speed_db_per_s: AtomicF32::new(3.0),
            floor_db: AtomicF32::new(-50.0),
            gain_db: AtomicF32::new(0.0),
        }
    }

    /// The RMS level it rides toward, in dBFS (−18 suits speech).
    pub fn target_db(&self) -> f32 {
        self.target_db.get()
    }

    /// Sets the target.
    pub fn set_target_db(&self, db: f32) {
        self.target_db.set(db);
    }

    /// The most it will turn up or down, in dB.
    pub fn range_db(&self) -> f32 {
        self.range_db.get()
    }

    /// Sets the range.
    pub fn set_range_db(&self, db: f32) {
        self.range_db.set(db.max(0.0));
    }

    /// How fast the gain may move, in dB per second.
    pub fn speed_db_per_s(&self) -> f32 {
        self.speed_db_per_s.get()
    }

    /// Sets the speed.
    pub fn set_speed_db_per_s(&self, db: f32) {
        self.speed_db_per_s.set(db.max(0.0));
    }

    /// Below this peak level, in dBFS, the gain holds still: pauses and
    /// room tone are not turned up.
    pub fn floor_db(&self) -> f32 {
        self.floor_db.get()
    }

    /// Sets the floor.
    pub fn set_floor_db(&self, db: f32) {
        self.floor_db.set(db);
    }

    /// The gain being applied at the end of the last block, in dB.
    pub fn gain_db(&self) -> f32 {
        self.gain_db.get()
    }
}

/// A slow gain rider: measures the RMS level over about 300 ms and moves
/// one gain, a few dB per second at most, toward the target, within a
/// range, holding still during pauses. What a careful engineer does with
/// a fader during a conversation; a compressor it is not.
pub struct Leveler {
    params: Arc<LevelerParams>,
    channels: usize,
    rate: f32,
    /// Mean square across channels, smoothed over about 300 ms.
    mean_square: f32,
    /// The recent peak, linear, letting go in 50 ms: says whether this is
    /// a pause before the slow level could.
    recent: f32,
    gain_db: f32,
}

impl Leveler {
    /// A leveler over `channels` channels driven by `params`.
    pub fn new(channels: usize, params: Arc<LevelerParams>) -> Self {
        Leveler {
            params,
            channels,
            rate: 48_000.0,
            mean_square: 0.0,
            recent: 0.0,
            gain_db: 0.0,
        }
    }
}

impl Node for Leveler {
    fn name(&self) -> &str {
        "leveler"
    }

    fn channels(&self) -> NodeChannels {
        NodeChannels::symmetric(self.channels)
    }

    fn prepare(&mut self, sample_rate: SampleRate, _max_frames: usize) {
        self.rate = sample_rate.hz() as f32;
        self.reset();
    }

    fn reset(&mut self) {
        self.mean_square = 0.0;
        self.recent = 0.0;
        self.gain_db = 0.0;
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus {
        if ctx.input_is_silent {
            // A pause: the level falls, the gain holds.
            self.mean_square = 0.0;
            self.recent = 0.0;
            return ProcessStatus::Silent;
        }
        if output.copy_from(input).is_err() {
            return ProcessStatus::Silent;
        }
        let p = &self.params;
        let (target, range, floor) = (p.target_db.get(), p.range_db.get(), p.floor_db.get());
        let step = p.speed_db_per_s.get() / self.rate;
        let window = coefficient(300.0, self.rate);
        let letting_go = coefficient(50.0, self.rate);
        let channels = input.channels().max(1) as f32;
        for n in 0..ctx.frames {
            let square: f32 = input.planes().map(|p| p[n] * p[n]).sum::<f32>() / channels;
            self.mean_square = square + window * (self.mean_square - square);
            // The recent peak says "pause" long before the 300 ms level could.
            self.recent = follow(self.recent, peak_at(input, n), 0.0, letting_go);
            let level = 10.0 * self.mean_square.max(1e-18).log10();
            if linear_to_db(self.recent.max(1e-9)) >= floor {
                let wanted = (target - level).clamp(-range, range);
                self.gain_db += (wanted - self.gain_db).clamp(-step, step);
            }
            let gain = db_to_linear(self.gain_db);
            for plane in output.planes_mut() {
                plane[n] *= gain;
            }
        }
        p.gain_db.set(self.gain_db);
        ProcessStatus::Normal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::graph::AudioGraph;
    use crate::primitives::midi::MidiEventBuffer;

    const R: SampleRate = SampleRate::HZ_48K;

    fn context<'a>(frames: usize, midi: &'a MidiEventBuffer, silent: bool) -> ProcessContext<'a> {
        ProcessContext {
            sample_rate: R,
            frames,
            time: crate::primitives::graph::BlockTime::default(),
            midi,
            input_is_silent: silent,
            input: None,
        }
    }

    /// Runs `node` over `seconds` of a sine at `hz` and `amplitude` and
    /// returns the output's peak over the last tenth of a second.
    fn steady_peak(node: &mut dyn Node, hz: f32, amplitude: f32, seconds: f32) -> f32 {
        node.prepare(R, 256);
        let midi = MidiEventBuffer::with_capacity(16);
        let mut input = PlanarBuffer::new(2, 256);
        let mut output = PlanarBuffer::new(2, 256);
        let total = (seconds * R.hz() as f32) as usize;
        let mut peak = 0.0f32;
        let mut i = 0usize;
        while i < total {
            let frames = 256.min(total - i);
            input.set_frames(frames).unwrap();
            for n in 0..frames {
                let s = amplitude
                    * (core::f32::consts::TAU * hz * (i + n) as f32 / R.hz() as f32).sin();
                input.channel_mut(0)[n] = s;
                input.channel_mut(1)[n] = s * 0.5;
            }
            output.set_frames(frames).unwrap();
            let ctx = context(frames, &midi, false);
            node.process(&ctx, &input, &mut output);
            if i + frames > total - total / 10 {
                peak = peak.max(output.channel(0).iter().fold(0.0f32, |m, s| m.max(s.abs())));
            }
            i += frames;
        }
        peak
    }

    #[test]
    fn the_static_curve_is_textbook() {
        // Hard knee: nothing under, slope above.
        assert_eq!(compression_db(-30.0, -20.0, 4.0, 0.0), 0.0);
        assert!((compression_db(-8.0, -20.0, 4.0, 0.0) - 9.0).abs() < 1e-6);
        assert_eq!(
            compression_db(-8.0, -20.0, 1.0, 0.0),
            0.0,
            "1:1 does nothing"
        );
        // Soft knee: half the hard-knee reduction at the threshold itself,
        // meeting the hard curve at its edge.
        let at = compression_db(-20.0, -20.0, 4.0, 6.0);
        assert!((at - 0.75 * 3.0 / 4.0).abs() < 1e-6, "{at}");
        assert!((compression_db(-17.0, -20.0, 4.0, 6.0) - 2.25).abs() < 1e-6);
        assert_eq!(compression_db(-24.0, -20.0, 4.0, 6.0), 0.0);
    }

    #[test]
    fn a_compressor_turns_the_loud_down_and_leaves_the_quiet_alone() {
        let params = Arc::new(CompressorParams::new(-20.0, 4.0));
        params.set_knee_db(0.0);
        let mut node = Compressor::new(2, Arc::clone(&params));
        // -8 dBFS in, 12 dB over a 4:1 threshold: 9 dB off, -17 dBFS out.
        let out = linear_to_db(steady_peak(&mut node, 1_000.0, db_to_linear(-8.0), 0.5));
        assert!((out + 17.0).abs() < 0.5, "{out} dBFS");
        assert!((params.reduction_db() - 9.0).abs() < 0.5);
        let quiet = linear_to_db(steady_peak(&mut node, 1_000.0, db_to_linear(-30.0), 0.5));
        assert!((quiet + 30.0).abs() < 0.2, "{quiet} dBFS");
        assert!(params.reduction_db() < 0.1);
        params.set_makeup_db(6.0);
        let made_up = linear_to_db(steady_peak(&mut node, 1_000.0, db_to_linear(-30.0), 0.2));
        assert!((made_up + 24.0).abs() < 0.2, "{made_up} dBFS");
        assert_eq!(node.name(), "compressor");
        assert_eq!(node.channels(), NodeChannels::symmetric(2));
        // The setters clamp what makes no sense.
        params.set_ratio(0.5);
        assert_eq!(params.ratio(), 1.0);
        params.set_attack_ms(-1.0);
        assert_eq!(params.attack_ms(), 0.0);
        params.set_release_ms(20.0);
        assert_eq!(params.release_ms(), 20.0);
        params.set_threshold_db(-12.0);
        assert_eq!(params.threshold_db(), -12.0);
        assert_eq!(params.knee_db(), 0.0);
        assert_eq!(params.makeup_db(), 6.0);
    }

    #[test]
    fn a_limiter_lets_nothing_past_its_ceiling_and_reports_its_delay() {
        let params = Arc::new(LimiterParams::new(-6.0));
        let mut node = Limiter::new(2, Arc::clone(&params));
        node.prepare(R, 256);
        let lookahead = (LIMITER_LOOKAHEAD_MS * 48.0) as usize;
        assert_eq!(node.latency_frames(), lookahead + TruePeakMeter::lag());
        // A tone 6 dB over the ceiling, with a few bursts 12 dB over.
        let midi = MidiEventBuffer::with_capacity(16);
        let mut input = PlanarBuffer::new(2, 256);
        let mut output = PlanarBuffer::new(2, 256);
        let ceiling = db_to_linear(-6.0);
        let mut worst = 0.0f32;
        let mut heard = 0.0f32;
        // A true-peak meter across the whole output, block after block (a
        // single block read on its own would start with a step).
        let mut tp_out = TruePeakMeter::new();
        for b in 0..200 {
            for n in 0..256 {
                let i = b * 256 + n;
                let burst = if (i / 4_800) % 3 == 2 { 2.0 } else { 1.0 };
                let s = burst * (core::f32::consts::TAU * 440.0 * i as f32 / 48_000.0).sin();
                input.channel_mut(0)[n] = s;
                input.channel_mut(1)[n] = -s;
            }
            let ctx = context(256, &midi, false);
            assert_eq!(
                node.process(&ctx, &input, &mut output),
                ProcessStatus::Normal
            );
            // Every block, or the meter would start mid-sine with a step.
            tp_out.push(output.channel(0));
            if b > 2 {
                worst = worst.max(output.peak());
                heard = heard.max(output.peak());
            }
        }
        assert!(
            worst <= ceiling * 1.001,
            "{} over {}",
            linear_to_db(worst),
            -6.0
        );
        assert!(
            heard > ceiling * 0.9,
            "it still passes signal: {}",
            linear_to_db(heard)
        );
        assert!(params.reduction_db() > 5.0, "{}", params.reduction_db());
        // The true peak of the output stays under the ceiling too.
        let tp = tp_out.finish();
        assert!(tp <= ceiling * 1.02, "{} dBTP", linear_to_db(tp));
        // Silence in, after the delay drains, silence out.
        let silence = PlanarBuffer::new(2, 256);
        let ctx = context(256, &midi, true);
        let mut status = ProcessStatus::Normal;
        for _ in 0..4 {
            status = node.process(&ctx, &silence, &mut output);
        }
        assert_eq!(status, ProcessStatus::Silent);
        params.set_ceiling_db(3.0);
        assert_eq!(
            params.ceiling_db(),
            0.0,
            "a ceiling above full scale is full scale"
        );
        params.set_release_ms(100.0);
        assert_eq!(params.release_ms(), 100.0);
        assert_eq!(node.name(), "limiter");
        // Unprepared, it passes nothing rather than panicking.
        let mut raw = Limiter::new(1, Arc::new(LimiterParams::new(-1.0)));
        let one = PlanarBuffer::new(1, 16);
        let mut out = PlanarBuffer::new(1, 16);
        let ctx = context(16, &midi, false);
        assert_eq!(raw.process(&ctx, &one, &mut out), ProcessStatus::Silent);
    }

    #[test]
    fn a_gate_closes_on_the_quiet_and_opens_for_the_loud() {
        let params = Arc::new(GateParams::new(-30.0));
        let mut node = Gate::new(2, Arc::clone(&params));
        let loud = linear_to_db(steady_peak(&mut node, 1_000.0, db_to_linear(-10.0), 0.3));
        assert!((loud + 10.0).abs() < 0.2, "open: {loud}");
        assert!(params.reduction_db() < 0.1);
        let quiet = linear_to_db(steady_peak(&mut node, 1_000.0, db_to_linear(-50.0), 0.6));
        assert!(quiet < -100.0, "closed: {quiet}");
        assert!((params.reduction_db() - 80.0).abs() < 1.0);
        // An expander instead: 2:1 under the threshold, 20 dB under
        // becomes 40 dB under.
        params.set_ratio(2.0);
        let expanded = linear_to_db(steady_peak(&mut node, 1_000.0, db_to_linear(-50.0), 0.6));
        assert!((expanded + 70.0).abs() < 1.0, "{expanded}");
        params.set_range_db(6.0);
        let ranged = linear_to_db(steady_peak(&mut node, 1_000.0, db_to_linear(-50.0), 0.6));
        assert!((ranged + 56.0).abs() < 1.0, "{ranged}");
        params.set_hold_ms(10.0);
        params.set_attack_ms(2.0);
        params.set_release_ms(50.0);
        params.set_threshold_db(-20.0);
        assert_eq!(
            (
                params.hold_ms(),
                params.attack_ms(),
                params.release_ms(),
                params.threshold_db()
            ),
            (10.0, 2.0, 50.0, -20.0)
        );
        assert_eq!(params.ratio(), 2.0);
        assert_eq!(params.range_db(), 6.0);
        assert_eq!(node.name(), "gate");
    }

    #[test]
    fn a_de_esser_hears_only_the_sibilance() {
        let params = Arc::new(DeEsserParams::new(6_500.0, -30.0));
        let mut node = DeEsser::new(2, Arc::clone(&params));
        // A hiss in the band at -10 dBFS is pulled down; a low tone at the
        // same level is left alone.
        let hiss = linear_to_db(steady_peak(&mut node, 6_500.0, db_to_linear(-10.0), 0.3));
        assert!(hiss < -22.0, "pulled down: {hiss}");
        assert!(params.reduction_db() > 10.0);
        let low = linear_to_db(steady_peak(&mut node, 200.0, db_to_linear(-10.0), 0.3));
        assert!((low + 10.0).abs() < 0.5, "left alone: {low}");
        assert!(params.reduction_db() < 0.5);
        // Retune to the low tone and it is the one that gets caught.
        params.set_frequency_hz(1_200.0);
        params.set_threshold_db(-40.0);
        params.set_ratio(10.0);
        let caught = linear_to_db(steady_peak(&mut node, 1_200.0, db_to_linear(-10.0), 0.3));
        assert!(caught < -30.0, "{caught}");
        assert_eq!(params.frequency_hz(), 1_200.0);
        assert_eq!(params.threshold_db(), -40.0);
        assert_eq!(params.ratio(), 10.0);
        params.set_frequency_hz(100.0);
        assert_eq!(params.frequency_hz(), 1_000.0, "clamped to a sensible band");
        assert_eq!(node.name(), "de-esser");
    }

    #[test]
    fn a_leveler_rides_toward_the_target_but_never_rides_silence_up() {
        let params = Arc::new(LevelerParams::new(-18.0));
        params.set_speed_db_per_s(40.0); // quick, for the test
        let mut node = Leveler::new(2, Arc::clone(&params));
        // A -30 dBFS sine on the left and half of it on the right: -33 and
        // -39 dBFS RMS, -35 together. 17 dB to go; the range allows 12.
        let lifted = linear_to_db(steady_peak(&mut node, 1_000.0, db_to_linear(-30.0), 1.0));
        assert!((lifted + 18.0).abs() < 1.0, "{lifted} dBFS peak");
        assert!(
            (params.gain_db() - 12.0).abs() < 0.5,
            "capped at the range: {}",
            params.gain_db()
        );
        params.set_range_db(20.0);
        let lifted = linear_to_db(steady_peak(&mut node, 1_000.0, db_to_linear(-30.0), 1.0));
        assert!(
            (lifted + 13.0).abs() < 1.0,
            "{lifted} dBFS peak: -18 RMS over both channels"
        );
        // Too quiet to count: once the level has fallen under the floor
        // (a few hundred milliseconds), the gain holds where it was.
        params.set_speed_db_per_s(3.0);
        let _ = steady_peak(&mut node, 1_000.0, db_to_linear(-70.0), 0.5);
        let before = params.gain_db();
        let _ = steady_peak(&mut node, 1_000.0, db_to_linear(-70.0), 0.5);
        assert!((params.gain_db() - before).abs() < 1e-3, "held at {before}");
        params.set_speed_db_per_s(40.0);
        // Loud: it comes down, to the same place.
        let lowered = linear_to_db(steady_peak(&mut node, 1_000.0, db_to_linear(-3.0), 1.0));
        assert!((lowered + 13.0).abs() < 1.0, "{lowered}");
        params.set_target_db(-20.0);
        params.set_floor_db(-60.0);
        assert_eq!(
            (params.target_db(), params.floor_db(), params.range_db()),
            (-20.0, -60.0, 20.0)
        );
        assert_eq!(params.speed_db_per_s(), 40.0);
        assert_eq!(node.name(), "leveler");
    }

    #[test]
    fn the_chain_runs_in_a_graph() {
        let mut graph = AudioGraph::new();
        let src = graph.add_node(Box::new(crate::primitives::graph::nodes::SineNode::new(
            440.0, 1.0, 2,
        )));
        let comp = graph.add_node(Box::new(Compressor::new(
            2,
            Arc::new(CompressorParams::new(-12.0, 3.0)),
        )));
        let lim = graph.add_node(Box::new(Limiter::new(
            2,
            Arc::new(LimiterParams::new(-1.0)),
        )));
        graph.connect(src, comp).unwrap();
        graph.connect(comp, lim).unwrap();
        graph.set_output(lim).unwrap();
        graph.prepare(R, 256).unwrap();
        assert!(graph.latency_frames() >= 240);
        let midi = MidiEventBuffer::with_capacity(16);
        let mut peak = 0.0f32;
        for _ in 0..40 {
            graph
                .process(
                    crate::primitives::graph::BlockTime::default(),
                    256,
                    &midi,
                    None,
                )
                .unwrap();
            peak = graph.output().peak();
        }
        assert!(peak <= db_to_linear(-1.0) * 1.001, "{peak}");
        // 0 dBFS in, 12 dB over a 3:1 threshold: 8 dB off.
        assert!((linear_to_db(peak) + 8.0).abs() < 0.5, "{peak}");
    }
}
