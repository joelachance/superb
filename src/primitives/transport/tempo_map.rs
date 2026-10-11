//! Tempo and meter map: piecewise-constant tempo with optional linear
//! ramps between points, plus meter changes at bar boundaries.

use core::fmt;

use crate::primitives::{AuraError, BeatPosition, Result, SampleRate, TimePosition, TimeSignature};

/// Resolution of [`Bbt::tick`]: ticks per quarter note.
pub const TICKS_PER_QUARTER: u32 = 960;

/// A tempo change: `bpm` applies from `beat` until the next point, either
/// held constant or — if `ramp` is set — ramping linearly (in bpm per beat)
/// to the next point's tempo.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempoPoint {
    /// Where the tempo takes effect.
    pub beat: BeatPosition,
    /// Quarter notes per minute at `beat`.
    pub bpm: f64,
    /// `true` if the tempo ramps linearly from `bpm` to the next point's
    /// `bpm` over the beats between them. Ignored on the last point (the
    /// tempo is held) and before beat zero.
    pub ramp: bool,
    /// Cached: seconds elapsed at `beat` (maintained by [`TempoMap`]).
    seconds: f64,
}

/// Seconds elapsed from `p.beat` to `beat`, which lies in `p`'s segment
/// (`next` is the following point, if any).
fn seconds_in_segment(p: &TempoPoint, next: Option<&TempoPoint>, beat: f64) -> f64 {
    let db = beat - p.beat.beats();
    match next {
        Some(n) if p.ramp && db > 0.0 && n.bpm != p.bpm => {
            let slope = (n.bpm - p.bpm) / (n.beat.beats() - p.beat.beats());
            let bpm_here = p.bpm + slope * db;
            60.0 / slope * (bpm_here / p.bpm).ln()
        }
        _ => db * 60.0 / p.bpm,
    }
}

/// Beats elapsed from `p.beat` after `seconds` seconds inside `p`'s segment.
fn beats_in_segment(p: &TempoPoint, next: Option<&TempoPoint>, seconds: f64) -> f64 {
    match next {
        Some(n) if p.ramp && seconds > 0.0 && n.bpm != p.bpm => {
            let slope = (n.bpm - p.bpm) / (n.beat.beats() - p.beat.beats());
            let bpm_here = p.bpm * (seconds * slope / 60.0).exp();
            (bpm_here - p.bpm) / slope
        }
        _ => seconds * p.bpm / 60.0,
    }
}

/// A time-signature change that takes effect at the start of `bar`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeterPoint {
    /// 1-based bar number where the meter starts.
    pub bar: u32,
    /// The meter from this bar on.
    pub signature: TimeSignature,
    /// Cached: beat position of the start of `bar`.
    beat: BeatPosition,
}

/// Bars / beats / ticks position for display and grid snapping.
///
/// All fields are 1-based except `tick` (0-based within the beat).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Bbt {
    /// 1-based bar number.
    pub bar: u32,
    /// 1-based beat within the bar (in the meter's denominator units).
    pub beat: u32,
    /// 0-based tick within the beat, `0..ticks_per_beat`.
    pub tick: u32,
}

impl fmt::Display for Bbt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{:03}", self.bar, self.beat, self.tick)
    }
}

/// Converts between audio time and musical time.
///
/// Tempo is piecewise constant, with optional linear ramps between points
/// ([`add_tempo_ramp`](TempoMap::add_tempo_ramp)). A map always has a tempo
/// point at beat zero and a meter point at bar 1.
#[derive(Clone, Debug)]
pub struct TempoMap {
    rate: SampleRate,
    tempos: Vec<TempoPoint>,
    meters: Vec<MeterPoint>,
}

impl TempoMap {
    /// Creates a map with a single constant tempo in `4/4`.
    pub fn new(rate: SampleRate, bpm: f64) -> Self {
        let mut map = Self {
            rate,
            tempos: vec![TempoPoint {
                beat: BeatPosition::ZERO,
                bpm: 120.0,
                ramp: false,
                seconds: 0.0,
            }],
            meters: vec![MeterPoint {
                bar: 1,
                signature: TimeSignature::FOUR_FOUR,
                beat: BeatPosition::ZERO,
            }],
        };
        map.set_initial_tempo(bpm).expect("invalid initial tempo");
        map
    }

    /// The sample rate used for sample conversions.
    pub fn sample_rate(&self) -> SampleRate {
        self.rate
    }

    /// Changes the sample rate (beats/seconds relationships are unaffected).
    pub fn set_sample_rate(&mut self, rate: SampleRate) {
        self.rate = rate;
    }

    // ------------------------------------------------------------------
    // Tempo editing
    // ------------------------------------------------------------------

    fn validate_bpm(bpm: f64) -> Result<()> {
        if !(bpm.is_finite() && bpm > 0.0 && bpm <= 2_000.0) {
            return Err(AuraError::InvalidArgument { what: "bpm" });
        }
        Ok(())
    }

    /// Sets the tempo at beat zero.
    pub fn set_initial_tempo(&mut self, bpm: f64) -> Result<()> {
        Self::validate_bpm(bpm)?;
        self.tempos[0].bpm = bpm;
        self.rebuild();
        Ok(())
    }

    /// Adds (or replaces) a constant tempo change at `beat`.
    ///
    /// `beat` must be finite and `>= 0`. Replacing an existing point clears
    /// its ramp flag.
    pub fn add_tempo(&mut self, beat: BeatPosition, bpm: f64) -> Result<()> {
        self.upsert_tempo(beat, bpm, false)
    }

    /// Adds (or replaces) a tempo point at `beat` that ramps linearly from
    /// `bpm` to the next point's tempo. If no later point exists (yet) the
    /// tempo is held until one is added.
    ///
    /// ```
    /// use aura::primitives::{BeatPosition, SampleRate};
    /// use aura::primitives::transport::TempoMap;
    ///
    /// let mut map = TempoMap::new(SampleRate::HZ_48K, 120.0);
    /// map.set_tempo_ramp(BeatPosition::ZERO, true).unwrap(); // ramp from beat 0 ...
    /// map.add_tempo(BeatPosition::new(4.0), 60.0).unwrap();  // ... down to 60 at beat 4
    /// assert_eq!(map.tempo_at(BeatPosition::new(2.0)), 90.0);
    /// // Slower than 2 s (all at 120) and faster than 4 s (all at 60).
    /// let s = map.beat_to_seconds(BeatPosition::new(4.0));
    /// assert!(s > 2.0 && s < 4.0);
    /// assert!((map.seconds_to_beat(s).beats() - 4.0).abs() < 1e-9);
    /// ```
    pub fn add_tempo_ramp(&mut self, beat: BeatPosition, bpm: f64) -> Result<()> {
        self.upsert_tempo(beat, bpm, true)
    }

    /// Sets or clears the ramp flag of the tempo point at exactly `beat`.
    pub fn set_tempo_ramp(&mut self, beat: BeatPosition, ramp: bool) -> Result<()> {
        match self
            .tempos
            .binary_search_by(|p| p.beat.partial_cmp(&beat).expect("finite"))
        {
            Ok(i) => {
                self.tempos[i].ramp = ramp;
                self.rebuild();
                Ok(())
            }
            Err(_) => Err(AuraError::NotFound {
                kind: "tempo point",
                id: 0,
            }),
        }
    }

    fn upsert_tempo(&mut self, beat: BeatPosition, bpm: f64, ramp: bool) -> Result<()> {
        Self::validate_bpm(bpm)?;
        if !beat.is_finite() || beat.beats() < 0.0 {
            return Err(AuraError::InvalidArgument { what: "tempo beat" });
        }
        match self
            .tempos
            .binary_search_by(|p| p.beat.partial_cmp(&beat).expect("finite"))
        {
            Ok(i) => {
                self.tempos[i].bpm = bpm;
                self.tempos[i].ramp = ramp;
            }
            Err(i) => self.tempos.insert(
                i,
                TempoPoint {
                    beat,
                    bpm,
                    ramp,
                    seconds: 0.0,
                },
            ),
        }
        self.rebuild();
        Ok(())
    }

    /// Removes the tempo change at exactly `beat`. The point at beat zero cannot be removed.
    pub fn remove_tempo(&mut self, beat: BeatPosition) -> Result<()> {
        if beat == BeatPosition::ZERO {
            return Err(AuraError::InvalidArgument {
                what: "cannot remove initial tempo",
            });
        }
        match self
            .tempos
            .binary_search_by(|p| p.beat.partial_cmp(&beat).expect("finite"))
        {
            Ok(i) => {
                self.tempos.remove(i);
                self.rebuild();
                Ok(())
            }
            Err(_) => Err(AuraError::NotFound {
                kind: "tempo point",
                id: 0,
            }),
        }
    }

    /// All tempo points in beat order.
    pub fn tempo_points(&self) -> &[TempoPoint] {
        &self.tempos
    }

    /// Tempo in effect at `beat` (interpolated inside a ramp).
    pub fn tempo_at(&self, beat: BeatPosition) -> f64 {
        let i = self.tempo_index_for_beat(beat);
        let p = &self.tempos[i];
        match self.tempos.get(i + 1) {
            Some(n) if p.ramp => {
                let t = ((beat.beats() - p.beat.beats()) / (n.beat.beats() - p.beat.beats()))
                    .clamp(0.0, 1.0);
                p.bpm + (n.bpm - p.bpm) * t
            }
            _ => p.bpm,
        }
    }

    // ------------------------------------------------------------------
    // Meter editing
    // ------------------------------------------------------------------

    /// Adds (or replaces) a meter change starting at 1-based `bar`.
    pub fn add_meter(&mut self, bar: u32, signature: TimeSignature) -> Result<()> {
        if bar == 0 {
            return Err(AuraError::InvalidArgument {
                what: "bar (1-based)",
            });
        }
        match self.meters.binary_search_by_key(&bar, |m| m.bar) {
            Ok(i) => self.meters[i].signature = signature,
            Err(i) => self.meters.insert(
                i,
                MeterPoint {
                    bar,
                    signature,
                    beat: BeatPosition::ZERO,
                },
            ),
        }
        self.rebuild();
        Ok(())
    }

    /// Removes the meter change at `bar`. Bar 1 cannot be removed.
    pub fn remove_meter(&mut self, bar: u32) -> Result<()> {
        if bar <= 1 {
            return Err(AuraError::InvalidArgument {
                what: "cannot remove initial meter",
            });
        }
        match self.meters.binary_search_by_key(&bar, |m| m.bar) {
            Ok(i) => {
                self.meters.remove(i);
                self.rebuild();
                Ok(())
            }
            Err(_) => Err(AuraError::NotFound {
                kind: "meter point",
                id: bar as u64,
            }),
        }
    }

    /// All meter points in bar order.
    pub fn meter_points(&self) -> &[MeterPoint] {
        &self.meters
    }

    /// Time signature in effect at `beat`.
    pub fn meter_at(&self, beat: BeatPosition) -> TimeSignature {
        self.meters[self.meter_index_for_beat(beat)].signature
    }

    // ------------------------------------------------------------------
    // Conversions
    // ------------------------------------------------------------------

    /// Seconds elapsed at `beat` (negative beats extrapolate the first tempo).
    pub fn beat_to_seconds(&self, beat: BeatPosition) -> f64 {
        let i = self.tempo_index_for_beat(beat);
        let p = &self.tempos[i];
        p.seconds + seconds_in_segment(p, self.tempos.get(i + 1), beat.beats())
    }

    /// Beat position at `seconds` (negative seconds extrapolate the first tempo).
    pub fn seconds_to_beat(&self, seconds: f64) -> BeatPosition {
        let idx = match self
            .tempos
            .binary_search_by(|p| p.seconds.partial_cmp(&seconds).expect("finite"))
        {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        };
        let p = &self.tempos[idx];
        BeatPosition::new(
            p.beat.beats() + beats_in_segment(p, self.tempos.get(idx + 1), seconds - p.seconds),
        )
    }

    /// Sample position of `beat` (rounded to the nearest sample).
    pub fn beat_to_samples(&self, beat: BeatPosition) -> TimePosition {
        TimePosition::from_seconds(self.beat_to_seconds(beat), self.rate)
    }

    /// Beat position of a sample position.
    pub fn samples_to_beat(&self, position: TimePosition) -> BeatPosition {
        self.seconds_to_beat(position.to_seconds(self.rate))
    }

    /// Samples per quarter note at `beat`'s tempo (fractional).
    pub fn samples_per_beat_at(&self, beat: BeatPosition) -> f64 {
        60.0 / self.tempo_at(beat) * self.rate.as_f64()
    }

    /// Converts a beat position to bars/beats/ticks. Negative beats clamp to `1.1.000`.
    pub fn beat_to_bbt(&self, beat: BeatPosition) -> Bbt {
        if beat.beats() <= 0.0 {
            return Bbt {
                bar: 1,
                beat: 1,
                tick: 0,
            };
        }
        let m = &self.meters[self.meter_index_for_beat(beat)];
        let bar_len = m.signature.bar_length().beats();
        let beat_len = m.signature.beat_length().beats();
        let offset = beat.beats() - m.beat.beats();
        let bars = (offset / bar_len).floor();
        let within_bar = offset - bars * bar_len;
        let beats = (within_bar / beat_len).floor();
        let within_beat = within_bar - beats * beat_len;
        let ticks_per_beat = (TICKS_PER_QUARTER as f64 * beat_len).round();
        let mut tick = (within_beat / beat_len * ticks_per_beat).round() as u32;
        let mut beat_idx = beats as u32;
        let mut bar_idx = bars as u32;
        // Carry rounding overflow forward so `tick` stays in range.
        if tick as f64 >= ticks_per_beat {
            tick = 0;
            beat_idx += 1;
            if beat_idx >= m.signature.numerator() as u32 {
                beat_idx = 0;
                bar_idx += 1;
            }
        }
        Bbt {
            bar: m.bar + bar_idx,
            beat: beat_idx + 1,
            tick,
        }
    }

    /// Converts bars/beats/ticks to a beat position.
    pub fn bbt_to_beat(&self, bbt: Bbt) -> Result<BeatPosition> {
        if bbt.bar == 0 || bbt.beat == 0 {
            return Err(AuraError::InvalidArgument {
                what: "bbt (1-based bar/beat)",
            });
        }
        let idx = match self.meters.binary_search_by_key(&bbt.bar, |m| m.bar) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        };
        let m = &self.meters[idx];
        let bar_len = m.signature.bar_length().beats();
        let beat_len = m.signature.beat_length().beats();
        let ticks_per_beat = TICKS_PER_QUARTER as f64 * beat_len;
        let beats = m.beat.beats()
            + (bbt.bar - m.bar) as f64 * bar_len
            + (bbt.beat - 1) as f64 * beat_len
            + bbt.tick as f64 / ticks_per_beat * beat_len;
        Ok(BeatPosition::new(beats))
    }

    /// Beat position of the start of the bar containing `beat`.
    pub fn bar_start(&self, beat: BeatPosition) -> BeatPosition {
        let bbt = self.beat_to_bbt(beat);
        self.bbt_to_beat(Bbt {
            bar: bbt.bar,
            beat: 1,
            tick: 0,
        })
        .expect("valid bbt")
    }

    // ------------------------------------------------------------------
    // Internals
    // ------------------------------------------------------------------

    fn tempo_index_for_beat(&self, beat: BeatPosition) -> usize {
        match self
            .tempos
            .binary_search_by(|p| p.beat.partial_cmp(&beat).expect("finite"))
        {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        }
    }

    fn meter_index_for_beat(&self, beat: BeatPosition) -> usize {
        match self
            .meters
            .binary_search_by(|m| m.beat.partial_cmp(&beat).expect("finite"))
        {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        }
    }

    /// Recomputes cached seconds for tempo points and beats for meter points.
    fn rebuild(&mut self) {
        let mut seconds = 0.0;
        for i in 0..self.tempos.len() {
            if i > 0 {
                let prev = self.tempos[i - 1];
                let here = self.tempos[i];
                seconds += seconds_in_segment(&prev, Some(&here), here.beat.beats());
            }
            self.tempos[i].seconds = seconds;
        }
        let mut beat = 0.0;
        for i in 0..self.meters.len() {
            if i > 0 {
                let prev = self.meters[i - 1];
                beat +=
                    (self.meters[i].bar - prev.bar) as f64 * prev.signature.bar_length().beats();
            }
            self.meters[i].beat = BeatPosition::new(beat);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R48: SampleRate = SampleRate::HZ_48K;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn constant_tempo_conversions() {
        let map = TempoMap::new(R48, 120.0);
        assert!(approx(map.beat_to_seconds(BeatPosition::new(4.0)), 2.0));
        assert!(approx(map.seconds_to_beat(2.0).beats(), 4.0));
        assert_eq!(
            map.beat_to_samples(BeatPosition::new(1.0)).samples(),
            24_000
        );
        assert!(approx(
            map.samples_to_beat(TimePosition::from_samples(96_000))
                .beats(),
            4.0
        ));
        assert!(approx(
            map.samples_per_beat_at(BeatPosition::ZERO),
            24_000.0
        ));
        assert_eq!(map.tempo_at(BeatPosition::new(1e9)), 120.0);
    }

    #[test]
    fn tempo_change_is_piecewise_constant() {
        let mut map = TempoMap::new(R48, 120.0);
        map.add_tempo(BeatPosition::new(4.0), 60.0).unwrap();
        // 4 beats at 120 = 2 s, then 4 beats at 60 = 4 s
        assert!(approx(map.beat_to_seconds(BeatPosition::new(8.0)), 6.0));
        assert!(approx(map.seconds_to_beat(6.0).beats(), 8.0));
        assert!(approx(map.seconds_to_beat(3.0).beats(), 5.0));
        assert_eq!(map.tempo_at(BeatPosition::new(3.999)), 120.0);
        assert_eq!(map.tempo_at(BeatPosition::new(4.0)), 60.0);

        // replace instead of duplicate
        map.add_tempo(BeatPosition::new(4.0), 240.0).unwrap();
        assert_eq!(map.tempo_points().len(), 2);
        assert!(approx(map.beat_to_seconds(BeatPosition::new(8.0)), 3.0));

        map.remove_tempo(BeatPosition::new(4.0)).unwrap();
        assert_eq!(map.tempo_points().len(), 1);
        assert!(map.remove_tempo(BeatPosition::ZERO).is_err());
        assert!(map.remove_tempo(BeatPosition::new(9.0)).is_err());
    }

    #[test]
    fn round_trip_through_many_tempo_changes() {
        let mut map = TempoMap::new(R48, 100.0);
        for i in 1..50 {
            map.add_tempo(
                BeatPosition::new(i as f64 * 3.0),
                60.0 + (i % 7) as f64 * 17.0,
            )
            .unwrap();
        }
        for i in 0..2000 {
            let beat = BeatPosition::new(i as f64 * 0.0773);
            let back = map.seconds_to_beat(map.beat_to_seconds(beat));
            assert!((back.beats() - beat.beats()).abs() < 1e-9, "beat {beat}");
        }
        // seconds are monotonic in beats
        let mut last = f64::NEG_INFINITY;
        for i in 0..500 {
            let s = map.beat_to_seconds(BeatPosition::new(i as f64 * 0.5));
            assert!(s > last);
            last = s;
        }
    }

    #[test]
    fn linear_ramp_integrates_exactly() {
        let mut map = TempoMap::new(R48, 120.0);
        map.set_tempo_ramp(BeatPosition::ZERO, true).unwrap();
        map.add_tempo(BeatPosition::new(4.0), 60.0).unwrap();
        assert!(map.tempo_points()[0].ramp);
        assert!(!map.tempo_points()[1].ramp);
        // Instantaneous tempo is linear in beats.
        assert!(approx(map.tempo_at(BeatPosition::ZERO), 120.0));
        assert!(approx(map.tempo_at(BeatPosition::new(1.0)), 105.0));
        assert!(approx(map.tempo_at(BeatPosition::new(2.0)), 90.0));
        assert!(approx(map.tempo_at(BeatPosition::new(4.0)), 60.0));
        assert!(
            approx(map.tempo_at(BeatPosition::new(9.0)), 60.0),
            "held after the last point"
        );
        assert!(
            approx(map.tempo_at(BeatPosition::new(-3.0)), 120.0),
            "held before beat zero"
        );
        // ∫0^4 60 / (120 - 15β) dβ = 4 ln 2 seconds.
        let s4 = map.beat_to_seconds(BeatPosition::new(4.0));
        assert!(approx(s4, 4.0 * 2f64.ln()), "{s4}");
        assert!(approx(
            map.beat_to_seconds(BeatPosition::new(8.0)),
            s4 + 4.0
        ));
        assert!(approx(map.seconds_to_beat(s4).beats(), 4.0));
        assert!(approx(map.seconds_to_beat(s4 + 4.0).beats(), 8.0));
        // Midway in time is *later* than midway in beats (tempo is slowing).
        assert!(map.seconds_to_beat(s4 / 2.0).beats() > 2.0);
        // Negative time still extrapolates the initial tempo linearly.
        assert!(approx(map.beat_to_seconds(BeatPosition::new(-2.0)), -1.0));
        assert!(approx(map.seconds_to_beat(-1.0).beats(), -2.0));
        // Ramp up as well as down, and a "ramp" between equal tempos is constant.
        map.add_tempo_ramp(BeatPosition::new(4.0), 60.0).unwrap();
        map.add_tempo(BeatPosition::new(8.0), 120.0).unwrap();
        assert!(approx(
            map.beat_to_seconds(BeatPosition::new(8.0)),
            2.0 * s4
        ));
        map.add_tempo(BeatPosition::new(8.0), 60.0).unwrap();
        assert!(approx(
            map.beat_to_seconds(BeatPosition::new(8.0)),
            s4 + 4.0
        ));
        // Clearing the flag restores the piecewise-constant behaviour.
        map.set_tempo_ramp(BeatPosition::ZERO, false).unwrap();
        assert!(approx(map.beat_to_seconds(BeatPosition::new(4.0)), 2.0));
        assert!(map.set_tempo_ramp(BeatPosition::new(1.5), true).is_err());
        // Replacing a ramp point with `add_tempo` clears its flag.
        map.add_tempo_ramp(BeatPosition::new(4.0), 60.0).unwrap();
        map.add_tempo(BeatPosition::new(4.0), 60.0).unwrap();
        assert!(!map.tempo_points()[1].ramp);
        assert!(map.add_tempo_ramp(BeatPosition::new(-1.0), 100.0).is_err());
        assert!(map.add_tempo_ramp(BeatPosition::new(1.0), 0.0).is_err());
    }

    #[test]
    fn ramps_round_trip_and_stay_monotonic() {
        let mut map = TempoMap::new(R48, 100.0);
        map.set_tempo_ramp(BeatPosition::ZERO, true).unwrap();
        for i in 1..40 {
            let bpm = 50.0 + ((i * 37) % 150) as f64;
            if i % 3 == 0 {
                map.add_tempo(BeatPosition::new(i as f64 * 2.5), bpm)
                    .unwrap();
            } else {
                map.add_tempo_ramp(BeatPosition::new(i as f64 * 2.5), bpm)
                    .unwrap();
            }
        }
        let mut last_s = f64::NEG_INFINITY;
        for i in 0..4000 {
            let beat = BeatPosition::new(i as f64 * 0.0311);
            let s = map.beat_to_seconds(beat);
            assert!(s > last_s, "seconds must increase with beats");
            last_s = s;
            let back = map.seconds_to_beat(s);
            assert!(
                (back.beats() - beat.beats()).abs() < 1e-8,
                "beat {beat} -> {s} s -> {back}"
            );
            let samples = map.beat_to_samples(beat);
            assert!((map.samples_to_beat(samples).beats() - beat.beats()).abs() < 1e-3);
        }
    }

    #[test]
    fn negative_time_extrapolates_first_tempo() {
        let map = TempoMap::new(R48, 120.0);
        assert!(approx(map.beat_to_seconds(BeatPosition::new(-2.0)), -1.0));
        assert!(approx(map.seconds_to_beat(-1.0).beats(), -2.0));
        assert_eq!(
            map.beat_to_bbt(BeatPosition::new(-1.0)),
            Bbt {
                bar: 1,
                beat: 1,
                tick: 0
            }
        );
    }

    #[test]
    fn invalid_tempos_are_rejected() {
        let mut map = TempoMap::new(R48, 120.0);
        assert!(map.add_tempo(BeatPosition::new(1.0), 0.0).is_err());
        assert!(map.add_tempo(BeatPosition::new(1.0), -5.0).is_err());
        assert!(map.add_tempo(BeatPosition::new(1.0), f64::NAN).is_err());
        assert!(map.add_tempo(BeatPosition::new(-1.0), 120.0).is_err());
        assert!(map.set_initial_tempo(f64::INFINITY).is_err());
        assert_eq!(map.tempo_points().len(), 1);
    }

    #[test]
    fn bbt_in_four_four() {
        let map = TempoMap::new(R48, 120.0);
        assert_eq!(
            map.beat_to_bbt(BeatPosition::ZERO),
            Bbt {
                bar: 1,
                beat: 1,
                tick: 0
            }
        );
        assert_eq!(
            map.beat_to_bbt(BeatPosition::new(4.0)),
            Bbt {
                bar: 2,
                beat: 1,
                tick: 0
            }
        );
        assert_eq!(
            map.beat_to_bbt(BeatPosition::new(5.5)),
            Bbt {
                bar: 2,
                beat: 2,
                tick: 480
            }
        );
        assert_eq!(
            map.beat_to_bbt(BeatPosition::new(7.9994)),
            Bbt {
                bar: 2,
                beat: 4,
                tick: 959
            }
        );
        // rounding at the very end of a beat carries into the next bar
        assert_eq!(
            map.beat_to_bbt(BeatPosition::new(7.999_999_9)),
            Bbt {
                bar: 3,
                beat: 1,
                tick: 0
            }
        );
        let b = map
            .bbt_to_beat(Bbt {
                bar: 3,
                beat: 4,
                tick: 240,
            })
            .unwrap();
        assert!(approx(b.beats(), 8.0 + 3.0 + 0.25));
        assert_eq!(map.bar_start(BeatPosition::new(9.7)).beats(), 8.0);
        assert!(map
            .bbt_to_beat(Bbt {
                bar: 0,
                beat: 1,
                tick: 0
            })
            .is_err());
        assert_eq!(
            Bbt {
                bar: 3,
                beat: 4,
                tick: 7
            }
            .to_string(),
            "3.4.007"
        );
    }

    #[test]
    fn meter_changes_shift_bar_boundaries() {
        let mut map = TempoMap::new(R48, 120.0);
        // bars 1-2 in 4/4 (8 quarter notes), bar 3 onwards in 3/4
        map.add_meter(3, TimeSignature::new(3, 4).unwrap()).unwrap();
        assert_eq!(
            map.meter_at(BeatPosition::new(7.9)),
            TimeSignature::FOUR_FOUR
        );
        assert_eq!(map.meter_at(BeatPosition::new(8.0)).numerator(), 3);
        assert_eq!(
            map.beat_to_bbt(BeatPosition::new(8.0)),
            Bbt {
                bar: 3,
                beat: 1,
                tick: 0
            }
        );
        assert_eq!(
            map.beat_to_bbt(BeatPosition::new(11.0)),
            Bbt {
                bar: 4,
                beat: 1,
                tick: 0
            }
        );
        assert_eq!(
            map.beat_to_bbt(BeatPosition::new(13.0)),
            Bbt {
                bar: 4,
                beat: 3,
                tick: 0
            }
        );
        assert!(approx(
            map.bbt_to_beat(Bbt {
                bar: 5,
                beat: 1,
                tick: 0
            })
            .unwrap()
            .beats(),
            14.0
        ));

        // 6/8 counts eighth-note beats with 480 ticks each
        map.add_meter(5, TimeSignature::new(6, 8).unwrap()).unwrap();
        assert_eq!(
            map.beat_to_bbt(BeatPosition::new(14.5)),
            Bbt {
                bar: 5,
                beat: 2,
                tick: 0
            }
        );
        assert_eq!(
            map.beat_to_bbt(BeatPosition::new(14.25)),
            Bbt {
                bar: 5,
                beat: 1,
                tick: 240
            }
        );
        assert_eq!(
            map.beat_to_bbt(BeatPosition::new(17.0)),
            Bbt {
                bar: 6,
                beat: 1,
                tick: 0
            }
        );

        assert!(map.add_meter(0, TimeSignature::FOUR_FOUR).is_err());
        assert!(map.remove_meter(1).is_err());
        assert!(map.remove_meter(9).is_err());
        map.remove_meter(5).unwrap();
        assert_eq!(map.meter_points().len(), 2);
        assert_eq!(
            map.beat_to_bbt(BeatPosition::new(17.0)),
            Bbt {
                bar: 6,
                beat: 1,
                tick: 0
            }
        );
    }

    #[test]
    fn bbt_round_trip_across_meters() {
        let mut map = TempoMap::new(R48, 90.0);
        map.add_meter(3, TimeSignature::new(7, 8).unwrap()).unwrap();
        map.add_meter(6, TimeSignature::new(5, 4).unwrap()).unwrap();
        for i in 0..400 {
            let beat = BeatPosition::new(i as f64 * 0.125);
            let bbt = map.beat_to_bbt(beat);
            let back = map.bbt_to_beat(bbt).unwrap();
            assert!(
                (back.beats() - beat.beats()).abs() < 1e-9,
                "{beat} -> {bbt} -> {back}"
            );
        }
    }

    #[test]
    fn sample_rate_change_keeps_musical_time() {
        let mut map = TempoMap::new(R48, 120.0);
        let one_beat = map.beat_to_samples(BeatPosition::new(1.0));
        map.set_sample_rate(SampleRate::HZ_96K);
        assert_eq!(
            map.beat_to_samples(BeatPosition::new(1.0)).samples(),
            one_beat.samples() * 2
        );
        assert_eq!(map.sample_rate(), SampleRate::HZ_96K);
    }
}
