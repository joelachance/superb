//! Transport state machine and block segmentation.

use core::fmt;

use superb_core::{BeatPosition, Result, SampleRate, SuperbError, TimePosition};

use crate::{Bbt, TempoMap};

/// Whether the playhead is moving and whether input is being captured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TransportState {
    /// Playhead is stationary.
    #[default]
    Stopped,
    /// Playhead is moving.
    Playing,
    /// Playhead is moving and tracks armed for record capture input.
    Recording,
}

impl TransportState {
    /// `true` for `Playing` and `Recording`.
    pub const fn is_rolling(self) -> bool {
        matches!(self, Self::Playing | Self::Recording)
    }
}

/// A half-open loop range `[start, end)` in samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LoopRange {
    start: TimePosition,
    end: TimePosition,
}

impl LoopRange {
    /// Creates a loop range. Fails unless `start < end`.
    pub fn new(start: TimePosition, end: TimePosition) -> Result<Self> {
        if start >= end {
            return Err(SuperbError::InvalidArgument {
                what: "loop range (start must be < end)",
            });
        }
        Ok(Self { start, end })
    }

    /// Loop start (inclusive).
    pub const fn start(&self) -> TimePosition {
        self.start
    }

    /// Loop end (exclusive).
    pub const fn end(&self) -> TimePosition {
        self.end
    }

    /// Length in samples (always `> 0`).
    pub const fn length(&self) -> i64 {
        self.end.samples() - self.start.samples()
    }

    /// `true` if `pos` is inside `[start, end)`.
    pub fn contains(&self, pos: TimePosition) -> bool {
        pos >= self.start && pos < self.end
    }
}

/// A contiguous slice of the timeline covering part of a process block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    /// Offset of this segment's first frame within the process block.
    pub block_offset: usize,
    /// Number of frames in the segment.
    pub frames: usize,
    /// Timeline position of the segment's first frame.
    pub start: TimePosition,
    /// `true` if the segment starts at the loop start because the previous
    /// segment hit the loop end.
    pub wrapped: bool,
}

impl Segment {
    /// Timeline position one past the last frame.
    pub fn end(&self) -> TimePosition {
        self.start.offset(self.frames as i64)
    }
}

/// Allocation-free iterator over the [`Segment`]s of one process block.
///
/// Produced by [`Transport::advance`]. When the transport is stopped, it
/// yields exactly one segment at the current position (so nodes still see
/// a well-formed block); when looping, it yields one segment per loop pass.
#[derive(Clone, Debug)]
pub struct Segments {
    cursor: TimePosition,
    remaining: usize,
    offset: usize,
    loop_range: Option<LoopRange>,
    wrapped: bool,
}

impl Iterator for Segments {
    type Item = Segment;

    fn next(&mut self) -> Option<Segment> {
        if self.remaining == 0 {
            return None;
        }
        let (frames, next, wraps) = match self.loop_range {
            Some(l) if self.cursor < l.end() => {
                let to_end = (l.end().samples() - self.cursor.samples()) as usize;
                if to_end > self.remaining {
                    (
                        self.remaining,
                        self.cursor.offset(self.remaining as i64),
                        false,
                    )
                } else {
                    (to_end, l.start(), true)
                }
            }
            _ => (
                self.remaining,
                self.cursor.offset(self.remaining as i64),
                false,
            ),
        };
        let seg = Segment {
            block_offset: self.offset,
            frames,
            start: self.cursor,
            wrapped: self.wrapped,
        };
        self.offset += frames;
        self.remaining -= frames;
        self.cursor = next;
        self.wrapped = wraps;
        Some(seg)
    }
}

/// Snapshot of the transport for UI display (cheap to copy across threads).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransportSnapshot {
    /// Current state.
    pub state: TransportState,
    /// Playhead position in samples.
    pub position: TimePosition,
    /// Playhead position in seconds.
    pub seconds: f64,
    /// Playhead position in beats.
    pub beat: BeatPosition,
    /// Bars/beats/ticks at the playhead.
    pub bbt: Bbt,
    /// Tempo at the playhead.
    pub bpm: f64,
    /// `true` if loop playback is enabled and a range is set.
    pub looping: bool,
}

/// The playhead.
///
/// The transport owns a [`TempoMap`] so that beat conversions and the sample
/// position stay consistent. It is a plain single-owner value: in a live
/// engine it lives on the audio thread and control messages reach it via a
/// queue (see `superb-rt`).
#[derive(Clone, Debug)]
pub struct Transport {
    state: TransportState,
    position: TimePosition,
    loop_range: Option<LoopRange>,
    loop_enabled: bool,
    tempo_map: TempoMap,
}

impl Transport {
    /// Creates a stopped transport at position zero with a constant tempo.
    pub fn new(rate: SampleRate, bpm: f64) -> Self {
        Self::with_tempo_map(TempoMap::new(rate, bpm))
    }

    /// Creates a stopped transport at position zero using an existing map.
    pub fn with_tempo_map(tempo_map: TempoMap) -> Self {
        Self {
            state: TransportState::Stopped,
            position: TimePosition::ZERO,
            loop_range: None,
            loop_enabled: false,
            tempo_map,
        }
    }

    /// Current state.
    pub fn state(&self) -> TransportState {
        self.state
    }

    /// `true` if playing or recording.
    pub fn is_rolling(&self) -> bool {
        self.state.is_rolling()
    }

    /// Playhead position.
    pub fn position(&self) -> TimePosition {
        self.position
    }

    /// Playhead position in beats.
    pub fn beat_position(&self) -> BeatPosition {
        self.tempo_map.samples_to_beat(self.position)
    }

    /// Sample rate of the underlying tempo map.
    pub fn sample_rate(&self) -> SampleRate {
        self.tempo_map.sample_rate()
    }

    /// The tempo map.
    pub fn tempo_map(&self) -> &TempoMap {
        &self.tempo_map
    }

    /// Mutable access to the tempo map (call off the audio thread).
    pub fn tempo_map_mut(&mut self) -> &mut TempoMap {
        &mut self.tempo_map
    }

    /// Starts playback (no-op if already playing; recording stays recording).
    pub fn play(&mut self) {
        if self.state == TransportState::Stopped {
            self.state = TransportState::Playing;
        }
    }

    /// Starts (or switches to) recording.
    pub fn record(&mut self) {
        self.state = TransportState::Recording;
    }

    /// Stops the playhead where it is.
    pub fn stop(&mut self) {
        self.state = TransportState::Stopped;
    }

    /// Stops and returns to zero.
    pub fn stop_and_rewind(&mut self) {
        self.state = TransportState::Stopped;
        self.position = TimePosition::ZERO;
    }

    /// Toggles between stopped and playing (recording becomes stopped).
    pub fn toggle_play(&mut self) {
        if self.state.is_rolling() {
            self.stop();
        } else {
            self.play();
        }
    }

    /// Moves the playhead. Allowed in any state.
    pub fn seek(&mut self, position: TimePosition) {
        self.position = position;
    }

    /// Moves the playhead to a beat position.
    pub fn seek_beat(&mut self, beat: BeatPosition) {
        self.position = self.tempo_map.beat_to_samples(beat);
    }

    /// Sets (or clears) the loop range. Does not change `loop_enabled`.
    pub fn set_loop(&mut self, range: Option<LoopRange>) {
        self.loop_range = range;
    }

    /// The loop range, if any.
    pub fn loop_range(&self) -> Option<LoopRange> {
        self.loop_range
    }

    /// Enables or disables looping.
    pub fn set_loop_enabled(&mut self, enabled: bool) {
        self.loop_enabled = enabled;
    }

    /// `true` if looping is enabled *and* a range is set.
    pub fn is_looping(&self) -> bool {
        self.loop_enabled && self.loop_range.is_some()
    }

    /// The loop range that will actually be applied by [`advance`](Self::advance).
    fn active_loop(&self) -> Option<LoopRange> {
        if self.loop_enabled {
            self.loop_range
        } else {
            None
        }
    }

    /// Plans the next `frames` and moves the playhead.
    ///
    /// Returns an iterator of segments covering exactly `frames` frames. If
    /// the transport is stopped, the playhead does not move and a single
    /// segment at the current position is returned. Audio-thread safe.
    pub fn advance(&mut self, frames: usize) -> Segments {
        let loop_range = if self.state.is_rolling() {
            self.active_loop()
        } else {
            None
        };
        let segs = Segments {
            cursor: self.position,
            remaining: frames,
            offset: 0,
            loop_range,
            wrapped: false,
        };
        if self.state.is_rolling() {
            let mut walker = segs.clone();
            let mut end = self.position;
            for s in &mut walker {
                end = s.end();
            }
            // If the last segment ended exactly on the loop end, wrap to start.
            self.position = match loop_range {
                Some(l) if end == l.end() && frames > 0 => l.start(),
                _ => end,
            };
        }
        segs
    }

    /// Cheap snapshot for display.
    pub fn snapshot(&self) -> TransportSnapshot {
        let beat = self.beat_position();
        TransportSnapshot {
            state: self.state,
            position: self.position,
            seconds: self.position.to_seconds(self.sample_rate()),
            beat,
            bbt: self.tempo_map.beat_to_bbt(beat),
            bpm: self.tempo_map.tempo_at(beat),
            looping: self.is_looping(),
        }
    }
}

impl fmt::Display for TransportState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Stopped => "stopped",
            Self::Playing => "playing",
            Self::Recording => "recording",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R48: SampleRate = SampleRate::HZ_48K;

    fn pos(s: i64) -> TimePosition {
        TimePosition::from_samples(s)
    }

    fn collect(t: &mut Transport, frames: usize) -> Vec<(i64, usize, usize, bool)> {
        t.advance(frames)
            .map(|s| (s.start.samples(), s.frames, s.block_offset, s.wrapped))
            .collect()
    }

    #[test]
    fn stopped_transport_does_not_move_but_yields_one_segment() {
        let mut t = Transport::new(R48, 120.0);
        t.seek(pos(500));
        let segs = collect(&mut t, 128);
        assert_eq!(segs, vec![(500, 128, 0, false)]);
        assert_eq!(t.position(), pos(500));
        assert_eq!(t.state(), TransportState::Stopped);
    }

    #[test]
    fn playing_advances_by_block_size_monotonically() {
        let mut t = Transport::new(R48, 120.0);
        t.play();
        let mut expected = 0;
        for _ in 0..1000 {
            let segs = collect(&mut t, 64);
            assert_eq!(segs, vec![(expected, 64, 0, false)]);
            expected += 64;
        }
        assert_eq!(t.position(), pos(64_000));
    }

    #[test]
    fn state_transitions() {
        let mut t = Transport::new(R48, 120.0);
        assert!(!t.is_rolling());
        t.toggle_play();
        assert_eq!(t.state(), TransportState::Playing);
        t.record();
        assert_eq!(t.state(), TransportState::Recording);
        t.play(); // play while recording keeps recording
        assert_eq!(t.state(), TransportState::Recording);
        t.toggle_play();
        assert_eq!(t.state(), TransportState::Stopped);
        t.seek(pos(10));
        t.stop_and_rewind();
        assert_eq!(t.position(), pos(0));
        assert_eq!(TransportState::Recording.to_string(), "recording");
    }

    #[test]
    fn loop_wraps_within_a_block() {
        let mut t = Transport::new(R48, 120.0);
        t.set_loop(Some(LoopRange::new(pos(100), pos(1_000)).unwrap()));
        t.set_loop_enabled(true);
        t.seek(pos(900));
        t.play();
        let segs = collect(&mut t, 256);
        assert_eq!(segs, vec![(900, 100, 0, false), (100, 156, 100, true)]);
        assert_eq!(t.position(), pos(256));
        // frames sum to the block
        assert_eq!(segs.iter().map(|s| s.1).sum::<usize>(), 256);
    }

    #[test]
    fn loop_exactly_at_end_wraps_position_to_start() {
        let mut t = Transport::new(R48, 120.0);
        t.set_loop(Some(LoopRange::new(pos(0), pos(512)).unwrap()));
        t.set_loop_enabled(true);
        t.play();
        assert_eq!(collect(&mut t, 256), vec![(0, 256, 0, false)]);
        assert_eq!(collect(&mut t, 256), vec![(256, 256, 0, false)]);
        assert_eq!(t.position(), pos(0)); // wrapped, no zero-length segment emitted
        assert_eq!(collect(&mut t, 256), vec![(0, 256, 0, false)]);
    }

    #[test]
    fn tiny_loop_produces_many_segments() {
        let mut t = Transport::new(R48, 120.0);
        t.set_loop(Some(LoopRange::new(pos(10), pos(13)).unwrap()));
        t.set_loop_enabled(true);
        t.seek(pos(10));
        t.play();
        let segs = collect(&mut t, 10);
        assert_eq!(
            segs,
            vec![
                (10, 3, 0, false),
                (10, 3, 3, true),
                (10, 3, 6, true),
                (10, 1, 9, true)
            ]
        );
        assert_eq!(t.position(), pos(11));
    }

    #[test]
    fn playhead_outside_loop_plays_through_then_catches_loop() {
        let mut t = Transport::new(R48, 120.0);
        t.set_loop(Some(LoopRange::new(pos(1_000), pos(2_000)).unwrap()));
        t.set_loop_enabled(true);
        // before the loop: plain advance until we enter, then wrap at end
        t.seek(pos(800));
        t.play();
        assert_eq!(collect(&mut t, 100), vec![(800, 100, 0, false)]);
        t.seek(pos(1_950));
        assert_eq!(
            collect(&mut t, 100),
            vec![(1_950, 50, 0, false), (1_000, 50, 50, true)]
        );
        // after the loop: never wraps back
        t.seek(pos(2_000));
        assert_eq!(collect(&mut t, 100), vec![(2_000, 100, 0, false)]);
        assert_eq!(t.position(), pos(2_100));
    }

    #[test]
    fn loop_disabled_or_stopped_ignores_range() {
        let mut t = Transport::new(R48, 120.0);
        t.set_loop(Some(LoopRange::new(pos(0), pos(100)).unwrap()));
        t.seek(pos(90));
        t.play();
        assert_eq!(collect(&mut t, 20), vec![(90, 20, 0, false)]);
        assert!(!t.is_looping());
        t.set_loop_enabled(true);
        assert!(t.is_looping());
        t.stop();
        t.seek(pos(90));
        assert_eq!(collect(&mut t, 20), vec![(90, 20, 0, false)]);
        assert_eq!(t.position(), pos(90));
        t.set_loop(None);
        assert!(!t.is_looping());
    }

    #[test]
    fn loop_range_validation() {
        assert!(LoopRange::new(pos(5), pos(5)).is_err());
        assert!(LoopRange::new(pos(6), pos(5)).is_err());
        let l = LoopRange::new(pos(-10), pos(10)).unwrap();
        assert_eq!(l.length(), 20);
        assert!(l.contains(pos(-10)));
        assert!(l.contains(pos(9)));
        assert!(!l.contains(pos(10)));
    }

    #[test]
    fn zero_frame_advance_is_a_no_op() {
        let mut t = Transport::new(R48, 120.0);
        t.set_loop(Some(LoopRange::new(pos(0), pos(100)).unwrap()));
        t.set_loop_enabled(true);
        t.seek(pos(100)); // at loop end exactly
        t.play();
        assert_eq!(collect(&mut t, 0), vec![]);
        assert_eq!(t.position(), pos(100));
    }

    #[test]
    fn seek_and_snapshot_are_consistent_with_tempo_map() {
        let mut t = Transport::new(R48, 120.0);
        t.seek_beat(BeatPosition::new(4.0)); // bar 2 at 120 bpm = 2 s
        assert_eq!(t.position(), pos(96_000));
        let snap = t.snapshot();
        assert_eq!(snap.seconds, 2.0);
        assert!((snap.beat.beats() - 4.0).abs() < 1e-9);
        assert_eq!(
            snap.bbt,
            Bbt {
                bar: 2,
                beat: 1,
                tick: 0
            }
        );
        assert_eq!(snap.bpm, 120.0);
        assert!(!snap.looping);
        assert_eq!(snap.state, TransportState::Stopped);

        t.tempo_map_mut()
            .add_tempo(BeatPosition::new(4.0), 60.0)
            .unwrap();
        t.play();
        let _ = t.advance(48_000); // 1 s at 60 bpm = 1 beat
        assert!((t.beat_position().beats() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn segment_end_helper() {
        let s = Segment {
            block_offset: 0,
            frames: 10,
            start: pos(5),
            wrapped: false,
        };
        assert_eq!(s.end(), pos(15));
    }
}
