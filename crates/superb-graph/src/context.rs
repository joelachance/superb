//! Per-block context handed to nodes.

use superb_core::{BeatPosition, SampleRate, TimePosition};
use superb_midi::MidiEventBuffer;
use superb_transport::{Segment, Transport};

/// Timeline information for one contiguous block (or transport segment).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockTime {
    /// Timeline position of the block's first frame.
    pub start: TimePosition,
    /// `true` if the transport is playing or recording.
    pub rolling: bool,
    /// `true` if this block begins right after a loop wrap.
    pub wrapped: bool,
    /// Tempo at `start`.
    pub bpm: f64,
    /// Musical position of `start`.
    pub beat_start: BeatPosition,
}

impl BlockTime {
    /// A block with no transport: position zero, not rolling, 120 bpm.
    ///
    /// Use for free-running synths, offline unit tests and device warm-up.
    pub const fn free_running() -> Self {
        Self {
            start: TimePosition::ZERO,
            rolling: false,
            wrapped: false,
            bpm: 120.0,
            beat_start: BeatPosition::ZERO,
        }
    }

    /// A stopped block at `position`.
    pub const fn stopped_at(position: TimePosition, bpm: f64) -> Self {
        Self {
            start: position,
            rolling: false,
            wrapped: false,
            bpm,
            beat_start: BeatPosition::ZERO,
        }
    }

    /// Builds block time from a transport and one of the segments it produced.
    pub fn from_transport(transport: &Transport, segment: &Segment) -> Self {
        let map = transport.tempo_map();
        let beat_start = map.samples_to_beat(segment.start);
        Self {
            start: segment.start,
            rolling: transport.is_rolling(),
            wrapped: segment.wrapped,
            bpm: map.tempo_at(beat_start),
            beat_start,
        }
    }

    /// Advances the block time by `frames` samples (beat position is
    /// advanced at the current tempo).
    pub fn advanced_by(&self, frames: usize, rate: SampleRate) -> Self {
        let seconds = frames as f64 / rate.as_f64();
        Self {
            start: self.start.offset(frames as i64),
            rolling: self.rolling,
            wrapped: false,
            bpm: self.bpm,
            beat_start: BeatPosition::new(self.beat_start.beats() + seconds * self.bpm / 60.0),
        }
    }
}

impl Default for BlockTime {
    fn default() -> Self {
        Self::free_running()
    }
}

/// Everything a node needs to process one block.
#[derive(Clone, Copy, Debug)]
pub struct ProcessContext<'a> {
    /// Engine sample rate.
    pub sample_rate: SampleRate,
    /// Frames in this block (`<= max_frames` given to `prepare`).
    pub frames: usize,
    /// Timeline information.
    pub time: BlockTime,
    /// MIDI events for this block, frame offsets relative to the block start.
    pub midi: &'a MidiEventBuffer,
    /// `true` if every producer feeding this node reported silence (or the
    /// node has no producers). Nodes may skip work and report
    /// [`ProcessStatus::Silent`](crate::ProcessStatus::Silent).
    pub input_is_silent: bool,
}

impl<'a> ProcessContext<'a> {
    /// Seconds per frame.
    pub fn sample_period(&self) -> f64 {
        self.sample_rate.period_seconds()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use superb_core::BeatPosition;
    use superb_transport::LoopRange;

    #[test]
    fn from_transport_carries_tempo_and_wrap() {
        let mut t = Transport::new(SampleRate::HZ_48K, 120.0);
        t.tempo_map_mut()
            .add_tempo(BeatPosition::new(2.0), 60.0)
            .unwrap();
        t.set_loop(Some(
            LoopRange::new(
                TimePosition::from_samples(0),
                TimePosition::from_samples(60_000),
            )
            .unwrap(),
        ));
        t.set_loop_enabled(true);
        t.seek(TimePosition::from_samples(59_000));
        t.play();
        let segs: Vec<_> = t.advance(2_000).collect();
        let first = BlockTime::from_transport(&t, &segs[0]);
        assert_eq!(first.start.samples(), 59_000);
        assert!(first.rolling);
        assert!(!first.wrapped);
        // 59 000 smp = 1.229 s -> beat 2.458 -> in the 60 bpm region
        assert_eq!(first.bpm, 60.0);
        let second = BlockTime::from_transport(&t, &segs[1]);
        assert!(second.wrapped);
        assert_eq!(second.start.samples(), 0);
        assert_eq!(second.bpm, 120.0);
        assert_eq!(second.beat_start, BeatPosition::ZERO);
    }

    #[test]
    fn advanced_by_moves_time_and_beats() {
        let bt = BlockTime {
            bpm: 120.0,
            rolling: true,
            ..BlockTime::free_running()
        };
        let next = bt.advanced_by(24_000, SampleRate::HZ_48K);
        assert_eq!(next.start.samples(), 24_000);
        assert!((next.beat_start.beats() - 1.0).abs() < 1e-9);
        assert!(next.rolling);
        assert_eq!(BlockTime::default(), BlockTime::free_running());
        assert_eq!(
            BlockTime::stopped_at(TimePosition::from_samples(7), 90.0).bpm,
            90.0
        );
    }
}
