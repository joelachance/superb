//! Ready-made graph nodes.
//!
//! Two groups live here:
//!
//! * **Building blocks** an application can use as they are: [`SineNode`]
//!   (oscillator / test tone), [`GainNode`] (click-free gain ramp),
//!   [`PassThroughNode`] (a summing point; the mixer builds its buses from
//!   it), [`DelayNode`] (a fixed delay that reports itself as latency),
//!   [`MonoSynthNode`] (a one-voice synth driven by live MIDI),
//!   [`BufferPlayerNode`] (plays audio held in memory at a timeline
//!   position), [`ScheduledSynthNode`] (plays a list of notes at
//!   timeline positions), [`LiveInputNode`] (plays the engine's live
//!   input) and [`ClickNode`] (a metronome driven by the block's beat
//!   position, with [`ClickParams`] as its live controls).
//! * **Test fixtures** with no musical use: [`ConstantNode`] emits a DC
//!   value and [`SilenceNode`] is always silent. They make graph behaviour
//!   observable in tests.
//!
//! Every node follows the graph's contract: allocate in `prepare`, never in
//! `process`. Parameters that move while audio runs are shared
//! `Arc<AtomicF32>` handles so a control thread can change them without
//! locks.

use core::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::midi::MidiMessage;
use crate::primitives::realtime::AtomicF32;
use crate::primitives::{SampleRate, TimeDuration, TimePosition};

use crate::primitives::graph::{Node, NodeChannels, ProcessContext, ProcessStatus};

// ---------------------------------------------------------------------------
// SineNode
// ---------------------------------------------------------------------------

/// A phase-continuous sine oscillator written to every output channel.
pub struct SineNode {
    frequency: Arc<AtomicF32>,
    amplitude: Arc<AtomicF32>,
    channels: usize,
    phase: f64,
    sample_rate: f64,
}

impl SineNode {
    /// Creates an oscillator at `frequency` Hz with peak `amplitude`.
    pub fn new(frequency: f32, amplitude: f32, channels: usize) -> Self {
        Self {
            frequency: Arc::new(AtomicF32::new(frequency)),
            amplitude: Arc::new(AtomicF32::new(amplitude)),
            channels,
            phase: 0.0,
            sample_rate: SampleRate::default().as_f64(),
        }
    }

    /// Shared handle to the frequency parameter (Hz).
    pub fn frequency(&self) -> Arc<AtomicF32> {
        Arc::clone(&self.frequency)
    }

    /// Shared handle to the amplitude parameter (linear).
    pub fn amplitude(&self) -> Arc<AtomicF32> {
        Arc::clone(&self.amplitude)
    }
}

impl Node for SineNode {
    fn name(&self) -> &str {
        "sine"
    }

    fn channels(&self) -> NodeChannels {
        NodeChannels::generator(self.channels)
    }

    fn prepare(&mut self, sample_rate: SampleRate, _max_frames: usize) {
        self.sample_rate = sample_rate.as_f64();
    }

    fn reset(&mut self) {
        self.phase = 0.0;
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        _input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus {
        let amp = self.amplitude.get();
        if amp == 0.0 {
            return ProcessStatus::Silent;
        }
        let inc = self.frequency.get() as f64 / self.sample_rate;
        let start_phase = self.phase;
        let mut end_phase = start_phase;
        for plane in output.planes_mut() {
            let mut phase = start_phase;
            for s in plane.iter_mut() {
                *s = (phase * core::f64::consts::TAU).sin() as f32 * amp;
                phase += inc;
            }
            end_phase = phase;
        }
        if self.channels == 0 {
            end_phase += inc * ctx.frames as f64;
        }
        self.phase = end_phase.rem_euclid(1.0);
        ProcessStatus::Normal
    }
}

// ---------------------------------------------------------------------------
// GainNode
// ---------------------------------------------------------------------------

/// Multiplies its input by a gain, ramping linearly across the block when
/// the target changes so parameter moves are click-free.
pub struct GainNode {
    gain: Arc<AtomicF32>,
    current: f32,
    channels: usize,
}

impl GainNode {
    /// Creates a gain stage with an initial linear gain.
    pub fn new(gain: f32, channels: usize) -> Self {
        Self {
            gain: Arc::new(AtomicF32::new(gain)),
            current: gain,
            channels,
        }
    }

    /// Shared handle to the gain parameter (linear).
    pub fn gain(&self) -> Arc<AtomicF32> {
        Arc::clone(&self.gain)
    }
}

impl Node for GainNode {
    fn name(&self) -> &str {
        "gain"
    }

    fn channels(&self) -> NodeChannels {
        NodeChannels::symmetric(self.channels)
    }

    fn reset(&mut self) {
        self.current = self.gain.get();
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus {
        let target = self.gain.get();
        let from = self.current;
        self.current = target;
        if ctx.input_is_silent || (from == 0.0 && target == 0.0) {
            return ProcessStatus::Silent;
        }
        if output.copy_from(input).is_err() {
            return ProcessStatus::Silent;
        }
        if from == target {
            output.apply_gain(target);
        } else {
            output.apply_gain_ramp(from, target);
        }
        ProcessStatus::Normal
    }
}

// ---------------------------------------------------------------------------
// PassThroughNode
// ---------------------------------------------------------------------------

/// Copies input to output. Useful as a bus/summing point.
pub struct PassThroughNode {
    channels: usize,
}

impl PassThroughNode {
    /// Creates a pass-through with `channels` channels.
    pub fn new(channels: usize) -> Self {
        Self { channels }
    }
}

impl Node for PassThroughNode {
    fn name(&self) -> &str {
        "passthrough"
    }

    fn channels(&self) -> NodeChannels {
        NodeChannels::symmetric(self.channels)
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus {
        if ctx.input_is_silent {
            return ProcessStatus::Silent;
        }
        match output.copy_from(input) {
            Ok(()) => ProcessStatus::Normal,
            Err(_) => ProcessStatus::Silent,
        }
    }
}

// ---------------------------------------------------------------------------
// DelayNode
// ---------------------------------------------------------------------------

/// A pass-through that delays its input by a fixed number of frames and
/// reports that delay as latency — a stand-in for look-ahead limiters,
/// linear-phase EQs and plugins with latency, used to exercise the graph's
/// latency compensation.
pub struct DelayNode {
    channels: usize,
    delay: usize,
    line: crate::primitives::graph::delay::DelayLine,
}

impl DelayNode {
    /// Creates a node that delays `channels` channels by `delay` frames.
    pub fn new(delay: usize, channels: usize) -> Self {
        Self {
            channels,
            delay,
            line: crate::primitives::graph::delay::DelayLine::new(channels, delay, 1),
        }
    }

    /// The delay in frames.
    pub fn delay(&self) -> usize {
        self.delay
    }
}

impl Node for DelayNode {
    fn name(&self) -> &str {
        "delay"
    }

    fn channels(&self) -> NodeChannels {
        NodeChannels::symmetric(self.channels)
    }

    fn prepare(&mut self, _sample_rate: SampleRate, max_frames: usize) {
        self.line =
            crate::primitives::graph::delay::DelayLine::new(self.channels, self.delay, max_frames);
    }

    fn reset(&mut self) {
        self.line.reset();
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus {
        let src = if ctx.input_is_silent {
            None
        } else {
            Some(input)
        };
        if self.line.process(src, ctx.frames, output) {
            ProcessStatus::Silent
        } else {
            ProcessStatus::Normal
        }
    }

    fn latency_frames(&self) -> usize {
        self.delay
    }
}

// ---------------------------------------------------------------------------
// ConstantNode
// ---------------------------------------------------------------------------

/// Emits a DC value on every channel (test fixture).
pub struct ConstantNode {
    value: Arc<AtomicF32>,
    channels: usize,
}

impl ConstantNode {
    /// Creates a generator that outputs `value` on `channels` channels.
    pub fn new(value: f32, channels: usize) -> Self {
        Self {
            value: Arc::new(AtomicF32::new(value)),
            channels,
        }
    }

    /// Shared handle to the value.
    pub fn value(&self) -> Arc<AtomicF32> {
        Arc::clone(&self.value)
    }
}

impl Node for ConstantNode {
    fn name(&self) -> &str {
        "constant"
    }

    fn channels(&self) -> NodeChannels {
        NodeChannels::generator(self.channels)
    }

    fn process(
        &mut self,
        _ctx: &ProcessContext<'_>,
        _input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus {
        let v = self.value.get();
        if v == 0.0 {
            return ProcessStatus::Silent;
        }
        output.fill(v);
        ProcessStatus::Normal
    }
}

// ---------------------------------------------------------------------------
// SilenceNode
// ---------------------------------------------------------------------------

/// A generator that is always silent (test fixture for silence propagation).
pub struct SilenceNode {
    channels: usize,
}

impl SilenceNode {
    /// Creates a silent generator.
    pub fn new(channels: usize) -> Self {
        Self { channels }
    }
}

impl Node for SilenceNode {
    fn name(&self) -> &str {
        "silence"
    }

    fn channels(&self) -> NodeChannels {
        NodeChannels::generator(self.channels)
    }

    fn process(
        &mut self,
        _ctx: &ProcessContext<'_>,
        _input: &PlanarBuffer,
        _output: &mut PlanarBuffer,
    ) -> ProcessStatus {
        ProcessStatus::Silent
    }
}

// ---------------------------------------------------------------------------
// MonoSynthNode
// ---------------------------------------------------------------------------

/// The metronome's live controls, shared between a [`ClickNode`] and
/// whoever sets them: on or off, how loud, and a count-in during which the
/// click sounds even while off.
#[derive(Debug)]
pub struct ClickParams {
    enabled: AtomicBool,
    level: AtomicF32,
    /// Timeline sample the count-in runs until; `i64::MIN` when none.
    until: AtomicI64,
}

impl ClickParams {
    /// Controls that start `enabled` at linear `level`, with no count-in.
    pub fn new(enabled: bool, level: f32) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            level: AtomicF32::new(level),
            until: AtomicI64::new(i64::MIN),
        }
    }

    /// Whether the click sounds whenever the transport rolls.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Turns the click on or off; heard at the next block.
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    /// Click level, linear.
    pub fn level(&self) -> f32 {
        self.level.get()
    }

    /// Sets the click level, linear; heard at the next block.
    pub fn set_level(&self, level: f32) {
        self.level.set(level);
    }

    /// Makes the click sound until the playhead reaches `until`, on or
    /// off: a count-in. `None` ends it.
    pub fn set_count_in_until(&self, until: Option<crate::primitives::TimePosition>) {
        self.until
            .store(until.map_or(i64::MIN, |p| p.samples()), Ordering::Relaxed);
    }

    /// Where the running count-in ends, if one is set.
    pub fn count_in_until(&self) -> Option<crate::primitives::TimePosition> {
        match self.until.load(Ordering::Relaxed) {
            i64::MIN => None,
            samples => Some(crate::primitives::TimePosition::from_samples(samples)),
        }
    }
}

impl Default for ClickParams {
    fn default() -> Self {
        Self::new(false, 1.0)
    }
}

/// A metronome: a short tick on every notated beat while the transport
/// rolls (quarters in 4/4, eighths in 6/8), louder on the first beat of
/// the bar, placed sample-accurately from the block's beat position,
/// tempo and meter. Silent while stopped, and while off unless a count-in
/// is running ([`ClickParams`]).
pub struct ClickNode {
    params: Arc<ClickParams>,
    channels: usize,
    sample_rate: f64,
    /// Sample position of the last tick, so a beat that rounding places in
    /// two neighbouring blocks sounds once.
    last_tick: Option<i64>,
    remaining: usize,
    phase: f64,
    increment: f64,
    amplitude: f32,
    decay: f32,
}

impl ClickNode {
    /// A click writing `channels` output channels.
    pub fn new(channels: usize, params: Arc<ClickParams>) -> Self {
        Self {
            params,
            channels,
            sample_rate: SampleRate::default().as_f64(),
            last_tick: None,
            remaining: 0,
            phase: 0.0,
            increment: 0.0,
            amplitude: 0.0,
            decay: 1.0,
        }
    }

    /// The live controls.
    pub fn params(&self) -> Arc<ClickParams> {
        Arc::clone(&self.params)
    }

    /// Starts a tick: 50 ms of a decaying sine, higher and louder on an
    /// accented beat.
    fn tick(&mut self, accent: bool, level: f32) {
        let hz = if accent { 1500.0 } else { 1000.0 };
        self.increment = hz / self.sample_rate;
        self.phase = 0.0;
        self.amplitude = level * if accent { 1.0 } else { 0.7 };
        self.remaining = (self.sample_rate * 0.05) as usize;
        self.decay = (-6.0 / self.remaining.max(1) as f32).exp();
    }
}

impl Node for ClickNode {
    fn name(&self) -> &str {
        "click"
    }

    fn channels(&self) -> NodeChannels {
        NodeChannels::generator(self.channels)
    }

    fn prepare(&mut self, sample_rate: SampleRate, _max_frames: usize) {
        self.sample_rate = sample_rate.as_f64();
    }

    fn reset(&mut self) {
        self.remaining = 0;
        self.phase = 0.0;
        self.last_tick = None;
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        _input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus {
        let time = ctx.time;
        let counting_in = self
            .params
            .count_in_until()
            .is_some_and(|until| time.start < until);
        if !time.rolling || !(self.params.is_enabled() || counting_in) {
            self.remaining = 0;
            self.last_tick = None;
            return ProcessStatus::Silent;
        }
        let frames = ctx.frames;
        let level = self.params.level();
        let beats_per_sample = time.bpm / 60.0 / self.sample_rate;
        let beat0 = time.beat_start.beats();
        let end = beat0 + frames as f64 * beats_per_sample;
        // Ticks fall on the notated beats of the bar: every `step` quarter
        // notes from the bar start, the first of each bar accented.
        let step = time.signature.beat_length().beats();
        let per_bar = i64::from(time.signature.numerator());
        let bar = time.bar_start.beats();
        // A beat within half a sample of a block edge belongs to the block
        // it is nearest to; `last_tick` keeps it from sounding twice when
        // rounding puts it in both.
        let edge = beats_per_sample / 2.0;
        let mut k = ((beat0 - edge - bar) / step).ceil() as i64;
        let position = |k: i64| bar + k as f64 * step;
        let frame_of = |beat: f64| {
            (((beat - beat0) / beats_per_sample).round().max(0.0) as usize).min(frames - 1)
        };
        let mut next_frame = (position(k) < end - edge).then(|| frame_of(position(k)));
        for i in 0..frames {
            if next_frame == Some(i) {
                let sample = time.start.samples() + i as i64;
                if self.last_tick.is_none_or(|last| (sample - last).abs() > 1) {
                    self.tick(k.rem_euclid(per_bar) == 0, level);
                    self.last_tick = Some(sample);
                }
                k += 1;
                next_frame = (position(k) < end - edge).then(|| frame_of(position(k)));
            }
            let sample = if self.remaining > 0 {
                let s = (self.phase * core::f64::consts::TAU).sin() as f32 * self.amplitude;
                self.phase += self.increment;
                self.amplitude *= self.decay;
                self.remaining -= 1;
                s
            } else {
                0.0
            };
            for ch in 0..self.channels {
                output.channel_mut(ch)[i] = sample;
            }
        }
        ProcessStatus::Normal
    }
}

/// Plays the engine's live audio input ([`ProcessContext::input`]): output
/// channel `i` carries input channel `taps[i]`, so `[0, 0]` puts a mono
/// microphone on both sides of a stereo strip and `[2, 3]` picks a stereo
/// pair. A tap the input does not have is silence, and so is the node
/// while no input is attached. This is how a track monitors what is being
/// recorded on it.
pub struct LiveInputNode {
    taps: Vec<usize>,
}

impl LiveInputNode {
    /// Creates a node with one output channel per tap.
    pub fn new(taps: impl Into<Vec<usize>>) -> Self {
        Self { taps: taps.into() }
    }

    /// Input channel per output channel.
    pub fn taps(&self) -> &[usize] {
        &self.taps
    }
}

impl Node for LiveInputNode {
    fn name(&self) -> &str {
        "live_input"
    }

    fn channels(&self) -> NodeChannels {
        NodeChannels::generator(self.taps.len())
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        _input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus {
        let Some(input) = ctx.input else {
            return ProcessStatus::Silent;
        };
        let frames = ctx.frames.min(input.frames());
        let mut any = false;
        for (i, &tap) in self.taps.iter().enumerate() {
            let out = &mut output.channel_mut(i)[..ctx.frames];
            if tap < input.channels() {
                out[..frames].copy_from_slice(&input.channel(tap)[..frames]);
                out[frames..].fill(0.0);
                any = true;
            } else {
                out.fill(0.0);
            }
        }
        if any {
            ProcessStatus::Normal
        } else {
            ProcessStatus::Silent
        }
    }
}

/// A minimal monophonic MIDI-driven sine synth.
///
/// Note-on sets pitch and amplitude (velocity / 127) at the event's frame
/// offset; note-off for the sounding note silences it. This exists to
/// exercise sample-accurate MIDI delivery end to end, not to sound good.
pub struct MonoSynthNode {
    channels: usize,
    sample_rate: f64,
    phase: f64,
    note: Option<u8>,
    amplitude: f32,
    increment: f64,
}

impl MonoSynthNode {
    /// Creates a synth writing to `channels` output channels.
    pub fn new(channels: usize) -> Self {
        Self {
            channels,
            sample_rate: SampleRate::default().as_f64(),
            phase: 0.0,
            note: None,
            amplitude: 0.0,
            increment: 0.0,
        }
    }

    /// The note currently sounding, if any.
    pub fn current_note(&self) -> Option<u8> {
        self.note
    }

    /// Equal-tempered frequency of a MIDI note number (A4 = 69 = 440 Hz).
    pub fn note_to_hz(note: u8) -> f64 {
        440.0 * 2f64.powf((note as f64 - 69.0) / 12.0)
    }

    fn apply(&mut self, msg: &MidiMessage) {
        match *msg {
            MidiMessage::NoteOn { note, velocity, .. } => {
                self.note = Some(note);
                self.amplitude = velocity as f32 / 127.0;
                self.increment = Self::note_to_hz(note) / self.sample_rate;
            }
            MidiMessage::NoteOff { note, .. } if self.note == Some(note) => {
                self.note = None;
                self.amplitude = 0.0;
            }
            _ => {}
        }
    }

    fn render(&mut self, output: &mut PlanarBuffer, from: usize, to: usize) {
        let start_phase = self.phase;
        let mut end_phase = start_phase;
        for plane in output.planes_mut() {
            let mut phase = start_phase;
            for s in &mut plane[from..to] {
                *s = if self.amplitude > 0.0 {
                    (phase * core::f64::consts::TAU).sin() as f32 * self.amplitude
                } else {
                    0.0
                };
                phase += self.increment;
            }
            end_phase = phase;
        }
        self.phase = end_phase.rem_euclid(1.0);
    }
}

impl Node for MonoSynthNode {
    fn name(&self) -> &str {
        "monosynth"
    }

    fn channels(&self) -> NodeChannels {
        NodeChannels::generator(self.channels)
    }

    fn prepare(&mut self, sample_rate: SampleRate, _max_frames: usize) {
        self.sample_rate = sample_rate.as_f64();
        if let Some(n) = self.note {
            self.increment = Self::note_to_hz(n) / self.sample_rate;
        }
    }

    fn reset(&mut self) {
        self.phase = 0.0;
        self.note = None;
        self.amplitude = 0.0;
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        _input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus {
        let mut cursor = 0usize;
        let mut any_sound = self.amplitude > 0.0;
        for ev in ctx.midi.iter() {
            let frame = (ev.frame as usize).min(ctx.frames);
            if frame > cursor {
                self.render(output, cursor, frame);
                cursor = frame;
            }
            self.apply(&ev.message);
            any_sound |= self.amplitude > 0.0;
        }
        if cursor < ctx.frames {
            self.render(output, cursor, ctx.frames);
        }
        if any_sound {
            ProcessStatus::Normal
        } else {
            ProcessStatus::Silent
        }
    }
}

// ---------------------------------------------------------------------------
// BufferPlayerNode
// ---------------------------------------------------------------------------

/// Timeline-aware playback of a cached planar buffer.
///
/// While the transport is rolling, frames whose absolute sample position
/// falls in `[timeline_start, timeline_start + length)` are read from
/// `buffer` at `buffer_offset + (pos - timeline_start)`, scaled by `gain`,
/// with optional linear fade-in / fade-out. Outside that window (or when
/// stopped / muted) the node reports [`ProcessStatus::Silent`].
///
/// Allocated offline: the buffer is an [`Arc`] so many players can share one
/// decode. `process` does not allocate.
pub struct BufferPlayerNode {
    buffer: Arc<PlanarBuffer>,
    channels: usize,
    timeline_start: i64,
    length: i64,
    buffer_offset: i64,
    gain: f32,
    muted: bool,
    fade_in: i64,
    fade_out: i64,
}

impl BufferPlayerNode {
    /// Creates a player for `buffer` writing `channels` output channels.
    ///
    /// Placement defaults to starting at timeline 0 for the full buffer
    /// length (or zero length if the buffer is empty — such a player is
    /// always silent until [`set_region`](Self::set_region) is called).
    pub fn new(buffer: Arc<PlanarBuffer>, channels: usize) -> Self {
        let frames = buffer.frames() as i64;
        Self {
            buffer,
            channels,
            timeline_start: 0,
            length: frames,
            buffer_offset: 0,
            gain: 1.0,
            muted: false,
            fade_in: 0,
            fade_out: 0,
        }
    }

    /// Places the player on the timeline.
    ///
    /// * `timeline_start` — absolute sample where playback of
    ///   `buffer[buffer_offset]` begins.
    /// * `length` — how many timeline frames to emit (`<= 0` → always silent).
    /// * `buffer_offset` — start frame inside the buffer.
    pub fn set_region(
        &mut self,
        timeline_start: TimePosition,
        length: TimeDuration,
        buffer_offset: TimeDuration,
    ) {
        self.timeline_start = timeline_start.samples();
        self.length = length.samples();
        self.buffer_offset = buffer_offset.samples().max(0);
    }

    /// Linear gain (1.0 = unity). Prefer [`set_gain_db`](Self::set_gain_db).
    pub fn set_gain(&mut self, gain: f32) {
        self.gain = gain;
    }

    /// Gain in dB (0 = unity).
    pub fn set_gain_db(&mut self, db: f32) {
        self.gain = crate::primitives::buffers::gain::db_to_linear(db);
    }

    /// Mute the player (always silent).
    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    /// Fade lengths in samples (clamped to the region). `0` disables.
    pub fn set_fades(&mut self, fade_in: TimeDuration, fade_out: TimeDuration) {
        self.fade_in = fade_in.samples().max(0);
        self.fade_out = fade_out.samples().max(0);
    }

    /// Shared buffer handle.
    pub fn buffer(&self) -> &Arc<PlanarBuffer> {
        &self.buffer
    }

    fn envelope(&self, into_clip: i64) -> f32 {
        let mut env = 1.0f32;
        if self.fade_in > 0 && into_clip < self.fade_in {
            env *= into_clip as f32 / self.fade_in as f32;
        }
        if self.fade_out > 0 {
            let from_end = self.length - into_clip;
            if from_end < self.fade_out {
                env *= from_end as f32 / self.fade_out as f32;
            }
        }
        env
    }

    fn sample_at(&self, src_frame: i64, out_ch: usize) -> f32 {
        if src_frame < 0 {
            return 0.0;
        }
        let src = src_frame as usize;
        if src >= self.buffer.frames() {
            return 0.0;
        }
        let buf_ch = self.buffer.channels();
        if buf_ch == 0 {
            return 0.0;
        }
        if buf_ch == 1 {
            self.buffer.channel(0)[src]
        } else if out_ch < buf_ch {
            self.buffer.channel(out_ch)[src]
        } else {
            // Fan last buffer channel into extra outs (rare).
            self.buffer.channel(buf_ch - 1)[src]
        }
    }
}

impl Node for BufferPlayerNode {
    fn name(&self) -> &str {
        "buffer_player"
    }

    fn channels(&self) -> NodeChannels {
        NodeChannels::generator(self.channels)
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        _input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus {
        if self.muted || self.gain == 0.0 || self.length <= 0 || !ctx.time.rolling {
            return ProcessStatus::Silent;
        }
        let block_start = ctx.time.start.samples();
        let block_end = block_start + ctx.frames as i64;
        let clip_start = self.timeline_start;
        let clip_end = self.timeline_start + self.length;
        if block_end <= clip_start || block_start >= clip_end {
            return ProcessStatus::Silent;
        }

        let mut any = false;
        for ch in 0..self.channels {
            let plane = &mut output.channel_mut(ch)[..ctx.frames];
            for (i, out) in plane.iter_mut().enumerate() {
                let pos = block_start + i as i64;
                if pos < clip_start || pos >= clip_end {
                    *out = 0.0;
                    continue;
                }
                let into = pos - clip_start;
                let src = self.buffer_offset + into;
                let s = self.sample_at(src, ch) * self.gain * self.envelope(into);
                *out = s;
                if s != 0.0 {
                    any = true;
                }
            }
        }
        if any {
            ProcessStatus::Normal
        } else {
            ProcessStatus::Silent
        }
    }
}

// ---------------------------------------------------------------------------
// ScheduledSynthNode
// ---------------------------------------------------------------------------

/// A sample-absolute MIDI note event for [`ScheduledSynthNode`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScheduledNoteEvent {
    /// Absolute timeline sample where the event fires.
    pub sample: i64,
    /// `true` = note on, `false` = note off.
    pub on: bool,
    /// MIDI note number.
    pub note: u8,
    /// Velocity (`1..=127` for note-on; ignored for note-off).
    pub velocity: u8,
    /// MIDI channel `0..=15`.
    pub channel: u8,
}

/// Monophonic sine synth driven by a sorted list of absolute-time note events.
///
/// Built for session MIDI clips: convert the clip's notes through a
/// [`TempoMap`](crate::primitives::transport::TempoMap) once (off the audio thread), then
/// this node fires them sample-accurately from [`BlockTime`](crate::primitives::graph::BlockTime).
pub struct ScheduledSynthNode {
    channels: usize,
    sample_rate: f64,
    events: Arc<[ScheduledNoteEvent]>,
    phase: f64,
    note: Option<u8>,
    amplitude: f32,
    increment: f64,
}

impl ScheduledSynthNode {
    /// Creates a synth that plays `events` (must be sorted by `sample`).
    pub fn new(channels: usize, events: Arc<[ScheduledNoteEvent]>) -> Self {
        Self {
            channels,
            sample_rate: SampleRate::default().as_f64(),
            events,
            phase: 0.0,
            note: None,
            amplitude: 0.0,
            increment: 0.0,
        }
    }

    /// Empty synth (always silent until replaced — construct a new node).
    pub fn silent(channels: usize) -> Self {
        Self::new(channels, Arc::from([]))
    }

    /// The scheduled events.
    pub fn events(&self) -> &[ScheduledNoteEvent] {
        &self.events
    }

    fn apply_event(&mut self, ev: &ScheduledNoteEvent) {
        if ev.on {
            self.note = Some(ev.note);
            self.amplitude = (ev.velocity as f32 / 127.0).clamp(0.0, 1.0);
            self.increment = MonoSynthNode::note_to_hz(ev.note) / self.sample_rate;
        } else if self.note == Some(ev.note) {
            self.note = None;
            self.amplitude = 0.0;
        }
    }

    fn render(&mut self, output: &mut PlanarBuffer, from: usize, to: usize) {
        let start_phase = self.phase;
        let mut end_phase = start_phase;
        for plane in output.planes_mut() {
            let mut phase = start_phase;
            for s in &mut plane[from..to] {
                *s = if self.amplitude > 0.0 {
                    (phase * core::f64::consts::TAU).sin() as f32 * self.amplitude
                } else {
                    0.0
                };
                phase += self.increment;
            }
            end_phase = phase;
        }
        self.phase = end_phase.rem_euclid(1.0);
    }

    fn first_index_at_or_after(events: &[ScheduledNoteEvent], sample: i64) -> usize {
        events.partition_point(|e| e.sample < sample)
    }
}

impl Node for ScheduledSynthNode {
    fn name(&self) -> &str {
        "scheduled_synth"
    }

    fn channels(&self) -> NodeChannels {
        NodeChannels::generator(self.channels)
    }

    fn prepare(&mut self, sample_rate: SampleRate, _max_frames: usize) {
        self.sample_rate = sample_rate.as_f64();
        if let Some(n) = self.note {
            self.increment = MonoSynthNode::note_to_hz(n) / self.sample_rate;
        }
    }

    fn reset(&mut self) {
        self.phase = 0.0;
        self.note = None;
        self.amplitude = 0.0;
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        _input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus {
        if !ctx.time.rolling || self.events.is_empty() {
            if self.amplitude == 0.0 {
                return ProcessStatus::Silent;
            }
            // The transport stopped while a note was sounding: cut it rather
            // than let it ring, so stop always means silence.
            self.reset();
            return ProcessStatus::Silent;
        }
        if ctx.time.wrapped {
            // Loop wrap: drop hanging notes so the next lap starts clean.
            self.reset();
        }

        let block_start = ctx.time.start.samples();
        let block_end = block_start + ctx.frames as i64;
        let mut idx = Self::first_index_at_or_after(&self.events, block_start);
        let mut cursor = 0usize;
        let mut any_sound = self.amplitude > 0.0;

        while idx < self.events.len() && self.events[idx].sample < block_end {
            let ev = self.events[idx];
            let frame = (ev.sample - block_start).clamp(0, ctx.frames as i64) as usize;
            if frame > cursor {
                self.render(output, cursor, frame);
                cursor = frame;
            }
            self.apply_event(&ev);
            any_sound |= self.amplitude > 0.0;
            idx += 1;
        }
        if cursor < ctx.frames {
            self.render(output, cursor, ctx.frames);
        }
        if any_sound {
            ProcessStatus::Normal
        } else {
            ProcessStatus::Silent
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::graph::BlockTime;
    use crate::primitives::midi::{MidiChannel, MidiEvent, MidiEventBuffer};

    fn ctx<'a>(frames: usize, midi: &'a MidiEventBuffer, silent_in: bool) -> ProcessContext<'a> {
        ProcessContext {
            sample_rate: SampleRate::HZ_48K,
            frames,
            time: BlockTime::free_running(),
            midi,
            input_is_silent: silent_in,
            input: None,
        }
    }

    #[test]
    fn sine_is_phase_continuous_and_reacts_to_parameter_handles() {
        let midi = MidiEventBuffer::with_capacity(0);
        let mut node = SineNode::new(480.0, 1.0, 2); // exactly 100 samples per cycle
        node.prepare(SampleRate::HZ_48K, 64);
        let input = PlanarBuffer::new(0, 64);
        let mut out = PlanarBuffer::new(2, 64);
        let mut samples = Vec::new();
        for _ in 0..4 {
            assert_eq!(
                node.process(&ctx(64, &midi, true), &input, &mut out),
                ProcessStatus::Normal
            );
            samples.extend_from_slice(out.channel(0));
            assert_eq!(out.channel(0), out.channel(1));
        }
        // sample 100 is one full cycle: back to ~0 and rising
        assert!(samples[100].abs() < 1e-4);
        assert!(samples[101] > 0.0);
        assert!((samples[25] - 1.0).abs() < 1e-4);
        node.amplitude().set(0.0);
        assert_eq!(
            node.process(&ctx(64, &midi, true), &input, &mut out),
            ProcessStatus::Silent
        );
        assert_eq!(node.channels(), NodeChannels::generator(2));
    }

    #[test]
    fn gain_ramps_between_targets_without_jumps() {
        let midi = MidiEventBuffer::with_capacity(0);
        let mut node = GainNode::new(1.0, 1);
        let mut input = PlanarBuffer::new(1, 8);
        input.fill(1.0);
        let mut out = PlanarBuffer::new(1, 8);
        node.process(&ctx(8, &midi, false), &input, &mut out);
        assert_eq!(out.channel(0), &[1.0; 8]);
        node.gain().set(0.0);
        node.process(&ctx(8, &midi, false), &input, &mut out);
        let ch = out.channel(0);
        assert_eq!(ch[0], 1.0);
        assert_eq!(ch[7], 0.0);
        for w in ch.windows(2) {
            assert!(w[1] < w[0]);
        }
        // fully at zero now -> silent shortcut
        assert_eq!(
            node.process(&ctx(8, &midi, false), &input, &mut out),
            ProcessStatus::Silent
        );
        // silent input -> silent output regardless of gain
        node.gain().set(1.0);
        assert_eq!(
            node.process(&ctx(8, &midi, true), &input, &mut out),
            ProcessStatus::Silent
        );
        node.reset();
        assert_eq!(node.name(), "gain");
    }

    #[test]
    fn passthrough_constant_and_silence() {
        let midi = MidiEventBuffer::with_capacity(0);
        let mut input = PlanarBuffer::new(1, 4);
        input.fill(0.3);
        let mut out = PlanarBuffer::new(1, 4);
        let mut p = PassThroughNode::new(1);
        assert_eq!(
            p.process(&ctx(4, &midi, false), &input, &mut out),
            ProcessStatus::Normal
        );
        assert_eq!(out.channel(0), &[0.3; 4]);
        assert_eq!(
            p.process(&ctx(4, &midi, true), &input, &mut out),
            ProcessStatus::Silent
        );

        let mut c = ConstantNode::new(0.7, 1);
        assert_eq!(
            c.process(&ctx(4, &midi, true), &input, &mut out),
            ProcessStatus::Normal
        );
        assert_eq!(out.channel(0), &[0.7; 4]);
        c.value().set(0.0);
        assert_eq!(
            c.process(&ctx(4, &midi, true), &input, &mut out),
            ProcessStatus::Silent
        );

        let mut s = SilenceNode::new(1);
        assert_eq!(
            s.process(&ctx(4, &midi, true), &input, &mut out),
            ProcessStatus::Silent
        );
        assert_eq!(s.name(), "silence");
        assert_eq!(c.name(), "constant");
    }

    #[test]
    fn monosynth_starts_and_stops_at_event_frames() {
        let ch = MidiChannel::new(0).unwrap();
        let mut midi = MidiEventBuffer::with_capacity(4);
        midi.push(MidiEvent {
            frame: 10,
            message: MidiMessage::NoteOn {
                channel: ch,
                note: 69,
                velocity: 127,
            },
        })
        .unwrap();
        midi.push(MidiEvent {
            frame: 30,
            message: MidiMessage::NoteOff {
                channel: ch,
                note: 69,
                velocity: 0,
            },
        })
        .unwrap();
        let mut synth = MonoSynthNode::new(1);
        synth.prepare(SampleRate::HZ_48K, 64);
        let input = PlanarBuffer::new(0, 64);
        let mut out = PlanarBuffer::new(1, 64);
        assert_eq!(
            synth.process(&ctx(64, &midi, true), &input, &mut out),
            ProcessStatus::Normal
        );
        let s = out.channel(0);
        assert!(s[..10].iter().all(|&x| x == 0.0), "silent before note-on");
        assert!(
            s[11..30].iter().any(|&x| x.abs() > 0.01),
            "sounding after note-on"
        );
        assert!(s[30..].iter().all(|&x| x == 0.0), "silent after note-off");
        assert_eq!(synth.current_note(), None);

        // a note-off for a different note is ignored
        let mut midi = MidiEventBuffer::with_capacity(4);
        midi.push(MidiEvent {
            frame: 0,
            message: MidiMessage::NoteOn {
                channel: ch,
                note: 60,
                velocity: 64,
            },
        })
        .unwrap();
        midi.push(MidiEvent {
            frame: 5,
            message: MidiMessage::NoteOff {
                channel: ch,
                note: 61,
                velocity: 0,
            },
        })
        .unwrap();
        synth.process(&ctx(64, &midi, true), &input, &mut out);
        assert_eq!(synth.current_note(), Some(60));
        let peak = out.peak();
        assert!((peak - 64.0 / 127.0).abs() < 0.05, "peak {peak}");

        let empty = MidiEventBuffer::with_capacity(0);
        assert_eq!(
            synth.process(&ctx(64, &empty, true), &input, &mut out),
            ProcessStatus::Normal
        );
        synth.reset();
        assert_eq!(
            synth.process(&ctx(64, &empty, true), &input, &mut out),
            ProcessStatus::Silent
        );
        assert!((MonoSynthNode::note_to_hz(69) - 440.0).abs() < 1e-9);
        assert!((MonoSynthNode::note_to_hz(81) - 880.0).abs() < 1e-9);
    }

    #[test]
    fn buffer_player_reads_region_with_gain_and_respects_transport() {
        let mut buf = PlanarBuffer::new(1, 100);
        for (i, s) in buf.channel_mut(0).iter_mut().enumerate() {
            *s = i as f32 * 0.01;
        }
        let mut player = BufferPlayerNode::new(Arc::new(buf), 1);
        player.set_region(
            TimePosition::from_samples(50),
            TimeDuration::from_samples(40),
            TimeDuration::from_samples(10),
        );
        player.set_gain(2.0);
        let midi = MidiEventBuffer::with_capacity(0);
        let input = PlanarBuffer::new(0, 64);
        let mut out = PlanarBuffer::new(1, 64);

        // Not rolling → silent
        assert_eq!(
            player.process(&ctx(64, &midi, true), &input, &mut out),
            ProcessStatus::Silent
        );

        let rolling = ProcessContext {
            sample_rate: SampleRate::HZ_48K,
            frames: 64,
            time: BlockTime {
                start: TimePosition::from_samples(40),
                rolling: true,
                wrapped: false,
                bpm: 120.0,
                beat_start: Default::default(),
                signature: Default::default(),
                bar_start: Default::default(),
            },
            midi: &midi,
            input_is_silent: true,
            input: None,
        };
        assert_eq!(
            player.process(&rolling, &input, &mut out),
            ProcessStatus::Normal
        );
        let s = out.channel(0);
        // frames 0..10 are timeline 40..50 → before clip → 0
        assert!(s[..10].iter().all(|&x| x == 0.0));
        // frame 10 = timeline 50 = buffer[10] * 2 = 0.10 * 2 = 0.20
        assert!((s[10] - 0.20).abs() < 1e-5);
        // frame 49 = timeline 89 = buffer[49] * 2 = 0.49 * 2 = 0.98
        assert!((s[49] - 0.98).abs() < 1e-5);
        // frame 50 = timeline 90 = past clip end (50+40=90) → 0
        assert_eq!(s[50], 0.0);
        assert_eq!(player.name(), "buffer_player");
    }

    #[test]
    fn scheduled_synth_fires_notes_at_absolute_samples() {
        let events: Arc<[ScheduledNoteEvent]> = Arc::from([
            ScheduledNoteEvent {
                sample: 10,
                on: true,
                note: 69,
                velocity: 127,
                channel: 0,
            },
            ScheduledNoteEvent {
                sample: 30,
                on: false,
                note: 69,
                velocity: 0,
                channel: 0,
            },
        ]);
        let mut synth = ScheduledSynthNode::new(1, events);
        synth.prepare(SampleRate::HZ_48K, 64);
        let midi = MidiEventBuffer::with_capacity(0);
        let input = PlanarBuffer::new(0, 64);
        let mut out = PlanarBuffer::new(1, 64);
        let rolling = ProcessContext {
            sample_rate: SampleRate::HZ_48K,
            frames: 64,
            time: BlockTime {
                start: TimePosition::ZERO,
                rolling: true,
                wrapped: false,
                bpm: 120.0,
                beat_start: Default::default(),
                signature: Default::default(),
                bar_start: Default::default(),
            },
            midi: &midi,
            input_is_silent: true,
            input: None,
        };
        assert_eq!(
            synth.process(&rolling, &input, &mut out),
            ProcessStatus::Normal
        );
        let s = out.channel(0);
        assert!(s[..10].iter().all(|&x| x == 0.0));
        assert!(s[11..30].iter().any(|&x| x.abs() > 0.01));
        assert!(s[30..].iter().all(|&x| x == 0.0));
        assert_eq!(synth.name(), "scheduled_synth");
    }

    #[test]
    fn live_input_node_copies_its_taps_and_is_silent_without_an_input() {
        let midi = MidiEventBuffer::with_capacity(0);
        let mut node = LiveInputNode::new([1, 0, 5]);
        assert_eq!(node.channels().outputs, 3);
        let mut out = PlanarBuffer::new(3, 8);
        let none = ctx(8, &midi, true);
        assert_eq!(
            node.process(&none, &PlanarBuffer::new(1, 8), &mut out),
            ProcessStatus::Silent
        );
        let mut live = PlanarBuffer::new(2, 8);
        live.channel_mut(0).fill(0.25);
        live.channel_mut(1).fill(0.5);
        let with = ProcessContext {
            input: Some(&live),
            ..ctx(8, &midi, true)
        };
        assert_eq!(
            node.process(&with, &PlanarBuffer::new(1, 8), &mut out),
            ProcessStatus::Normal
        );
        assert!(out.channel(0).iter().all(|&s| s == 0.5));
        assert!(out.channel(1).iter().all(|&s| s == 0.25));
        assert!(out.channel(2).iter().all(|&s| s == 0.0), "no such tap");
    }

    #[test]
    fn the_click_ticks_on_the_beat_and_obeys_its_switch_and_count_in() {
        let midi = MidiEventBuffer::with_capacity(0);
        let params = Arc::new(ClickParams::new(true, 0.5));
        let mut node = ClickNode::new(1, Arc::clone(&params));
        node.prepare(SampleRate::HZ_48K, 64);
        let mut out = PlanarBuffer::new(1, 64);
        let input = PlanarBuffer::new(1, 64);
        // Stopped: nothing.
        let stopped = ctx(64, &midi, true);
        assert_eq!(
            node.process(&stopped, &input, &mut out),
            ProcessStatus::Silent
        );
        // Rolling at 120 bpm from beat 0.999: the beat lands 24 frames in
        // (a beat is 24 000 samples), so the first 24 frames stay silent.
        let rolling = ProcessContext {
            time: BlockTime {
                rolling: true,
                beat_start: crate::primitives::BeatPosition::new(0.999),
                ..BlockTime::free_running()
            },
            ..ctx(64, &midi, true)
        };
        assert_eq!(
            node.process(&rolling, &input, &mut out),
            ProcessStatus::Normal
        );
        assert!(out.channel(0)[..24].iter().all(|&s| s == 0.0));
        assert!(
            out.channel(0)[24..].iter().any(|&s| s.abs() > 0.1),
            "a tick"
        );
        // Beat 1 of the bar is quieter than beat 0 (the accent).
        let mut accented = PlanarBuffer::new(1, 64);
        let on_beat_zero = ProcessContext {
            time: BlockTime {
                rolling: true,
                beat_start: crate::primitives::BeatPosition::new(4.0),
                ..BlockTime::free_running()
            },
            ..ctx(64, &midi, true)
        };
        node.process(&on_beat_zero, &input, &mut accented);
        assert!(accented.peak() > out.peak());
        // The click ticks the notated beat. At 45 000 bpm a 64-frame block is
        // one quarter note: in 4/4 one tick (frame 0), in 6/8 two (an eighth
        // apart, frame 32).
        let quarter_block = |signature| ProcessContext {
            time: BlockTime {
                rolling: true,
                bpm: 45_000.0,
                beat_start: crate::primitives::BeatPosition::new(8.0),
                bar_start: crate::primitives::BeatPosition::new(8.0),
                signature,
                ..BlockTime::free_running()
            },
            ..ctx(64, &midi, true)
        };
        node.reset();
        node.process(
            &quarter_block(crate::primitives::TimeSignature::FOUR_FOUR),
            &input,
            &mut out,
        );
        assert_eq!(node.last_tick, Some(0));
        node.reset();
        node.process(
            &quarter_block(crate::primitives::TimeSignature::new(6, 8).unwrap()),
            &input,
            &mut out,
        );
        assert_eq!(node.last_tick, Some(32));
        // Off: silent, unless a count-in is running and the block is before its end.
        params.set_enabled(false);
        assert_eq!(
            node.process(&on_beat_zero, &input, &mut out),
            ProcessStatus::Silent
        );
        params.set_count_in_until(Some(crate::primitives::TimePosition::from_samples(1_000)));
        assert_eq!(
            node.process(&on_beat_zero, &input, &mut out),
            ProcessStatus::Normal
        );
        params.set_count_in_until(None);
        assert_eq!(
            node.process(&on_beat_zero, &input, &mut out),
            ProcessStatus::Silent
        );
        assert!(node.params().count_in_until().is_none());
    }
}
