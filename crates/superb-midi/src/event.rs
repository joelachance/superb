//! Timestamped/frame-stamped MIDI events and a fixed-capacity event buffer.

use core::ops::Range;

use superb_core::{Result, SuperbError};

use crate::message::MidiMessage;

/// A [`MidiMessage`] stamped with its sample offset within the current
/// process block.
///
/// `frame` is always relative to the start of the block currently being
/// processed (i.e. `0..frames_in_block`), never an absolute transport
/// position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MidiEvent {
    /// Sample offset within the current process block at which this event
    /// takes effect.
    pub frame: u32,
    /// The decoded MIDI message.
    pub message: MidiMessage,
}

/// A [`MidiMessage`] stamped with a microsecond timestamp from a free-running
/// clock (e.g. `Instant::now()` on the input thread).
///
/// This is the payload pushed through the non-realtime-to-realtime
/// [`superb_rt::spsc`] queue that carries hardware MIDI input into the audio
/// callback; [`crate::timing::FrameMapper`] converts `timestamp_us` into a
/// [`MidiEvent::frame`] once the receiving block's start time is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimestampedMessage {
    /// Microsecond timestamp from a free-running, monotonic clock.
    pub timestamp_us: u64,
    /// The decoded MIDI message.
    pub message: MidiMessage,
}

/// A fixed-capacity, heap-allocated-once buffer of [`MidiEvent`]s.
///
/// The backing storage is allocated exactly once by [`MidiEventBuffer::with_capacity`]
/// and never grows afterwards: [`MidiEventBuffer::push`] returns
/// [`SuperbError::CapacityExceeded`] instead of reallocating, which makes the
/// buffer safe to `push` into from an audio callback.
#[derive(Clone, Debug)]
pub struct MidiEventBuffer {
    events: Vec<MidiEvent>,
    capacity: usize,
}

impl MidiEventBuffer {
    /// Allocates a buffer that can hold up to `capacity` events. The
    /// allocation happens exactly once, here.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            events: Vec::with_capacity(capacity),
            capacity,
        }
    }

    /// Appends `event` to the end of the buffer.
    ///
    /// # Errors
    /// Returns [`SuperbError::CapacityExceeded`] if the buffer is full; the
    /// buffer is never grown.
    pub fn push(&mut self, event: MidiEvent) -> Result<()> {
        if self.events.len() >= self.capacity {
            return Err(SuperbError::CapacityExceeded {
                what: "midi_event_buffer",
                capacity: self.capacity,
            });
        }
        self.events.push(event);
        Ok(())
    }

    /// Inserts `event` keeping the buffer ordered by ascending `frame`.
    ///
    /// The insertion is stable: among events with an equal `frame`, `event`
    /// is placed after all previously inserted events with that same frame.
    ///
    /// # Errors
    /// Returns [`SuperbError::CapacityExceeded`] if the buffer is full.
    pub fn push_sorted(&mut self, event: MidiEvent) -> Result<()> {
        if self.events.len() >= self.capacity {
            return Err(SuperbError::CapacityExceeded {
                what: "midi_event_buffer",
                capacity: self.capacity,
            });
        }
        let pos = self
            .events
            .iter()
            .position(|e| e.frame > event.frame)
            .unwrap_or(self.events.len());
        // `len() < capacity` was just checked, so this never reallocates.
        self.events.insert(pos, event);
        Ok(())
    }

    /// Number of events currently stored.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// `true` if the buffer holds no events.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Maximum number of events the buffer can hold. Constant for the life
    /// of the buffer.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Removes all events without freeing the backing storage.
    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// Iterates over the stored events in buffer order.
    pub fn iter(&self) -> core::slice::Iter<'_, MidiEvent> {
        self.events.iter()
    }

    /// The stored events as a slice, in buffer order.
    pub fn as_slice(&self) -> &[MidiEvent] {
        &self.events
    }

    /// Sorts the events in place by ascending `frame`, using a stable
    /// insertion sort that performs no allocation.
    ///
    /// Insertion sort is `O(n^2)` but MIDI event counts per block are small
    /// (typically well under a few hundred), and it is one of the few
    /// comparison sorts that is both stable and allocation-free without
    /// relying on `Vec::sort`'s internal scratch buffer.
    pub fn sort_by_frame(&mut self) {
        let n = self.events.len();
        for i in 1..n {
            let mut j = i;
            while j > 0 && self.events[j - 1].frame > self.events[j].frame {
                self.events.swap(j - 1, j);
                j -= 1;
            }
        }
    }

    /// Iterates over events whose `frame` lies in `range`
    /// (`range.start <= frame < range.end`), in buffer order.
    pub fn iter_range(&self, range: Range<u32>) -> impl Iterator<Item = &MidiEvent> {
        self.events
            .iter()
            .filter(move |e| e.frame >= range.start && e.frame < range.end)
    }

    /// Appends every event from `other`, preserving its order.
    ///
    /// # Errors
    /// Returns [`SuperbError::CapacityExceeded`] and leaves `self` unchanged
    /// if there is not enough room for all of `other`'s events.
    pub fn extend_from(&mut self, other: &MidiEventBuffer) -> Result<()> {
        let needed = self.events.len() + other.events.len();
        if needed > self.capacity {
            return Err(SuperbError::CapacityExceeded {
                what: "midi_event_buffer",
                capacity: self.capacity,
            });
        }
        self.events.extend_from_slice(&other.events);
        Ok(())
    }

    /// Keeps only the events for which `f` returns `true`, in place.
    pub fn retain<F>(&mut self, f: F)
    where
        F: FnMut(&MidiEvent) -> bool,
    {
        self.events.retain(f);
    }
}

impl<'a> IntoIterator for &'a MidiEventBuffer {
    type Item = &'a MidiEvent;
    type IntoIter = core::slice::Iter<'a, MidiEvent>;

    fn into_iter(self) -> Self::IntoIter {
        self.events.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::MidiChannel;

    fn ev(frame: u32) -> MidiEvent {
        MidiEvent {
            frame,
            message: MidiMessage::note_on(MidiChannel::new(0).unwrap(), 60, 64).unwrap(),
        }
    }

    #[test]
    fn capacity_is_fixed_and_never_exceeded() {
        let mut buf = MidiEventBuffer::with_capacity(3);
        assert_eq!(buf.capacity(), 3);
        buf.push(ev(0)).unwrap();
        buf.push(ev(1)).unwrap();
        buf.push(ev(2)).unwrap();
        assert_eq!(buf.capacity(), 3, "capacity must not change after pushes");
        assert_eq!(buf.len(), 3);

        let err = buf.push(ev(3)).unwrap_err();
        assert_eq!(
            err,
            SuperbError::CapacityExceeded {
                what: "midi_event_buffer",
                capacity: 3
            }
        );
        assert_eq!(buf.capacity(), 3, "a failed push must not grow the buffer");
        assert_eq!(buf.len(), 3);
    }

    #[test]
    fn push_sorted_never_grows_capacity() {
        let mut buf = MidiEventBuffer::with_capacity(2);
        buf.push_sorted(ev(5)).unwrap();
        buf.push_sorted(ev(1)).unwrap();
        assert_eq!(buf.capacity(), 2);
        let err = buf.push_sorted(ev(3)).unwrap_err();
        assert_eq!(
            err,
            SuperbError::CapacityExceeded {
                what: "midi_event_buffer",
                capacity: 2
            }
        );
        assert_eq!(buf.capacity(), 2);
    }

    #[test]
    fn push_sorted_keeps_ascending_order() {
        let mut buf = MidiEventBuffer::with_capacity(5);
        for f in [5u32, 1, 3, 1, 0] {
            buf.push_sorted(ev(f)).unwrap();
        }
        let frames: Vec<u32> = buf.iter().map(|e| e.frame).collect();
        assert_eq!(frames, vec![0, 1, 1, 3, 5]);
    }

    #[test]
    fn push_sorted_is_stable_for_equal_frames() {
        let mut buf = MidiEventBuffer::with_capacity(4);
        let ch = MidiChannel::new(0).unwrap();
        let a = MidiEvent {
            frame: 2,
            message: MidiMessage::note_on(ch, 1, 1).unwrap(),
        };
        let b = MidiEvent {
            frame: 2,
            message: MidiMessage::note_on(ch, 2, 1).unwrap(),
        };
        let c = MidiEvent {
            frame: 2,
            message: MidiMessage::note_on(ch, 3, 1).unwrap(),
        };
        buf.push_sorted(a).unwrap();
        buf.push_sorted(b).unwrap();
        buf.push_sorted(c).unwrap();
        let notes: Vec<u8> = buf
            .iter()
            .map(|e| match e.message {
                MidiMessage::NoteOn { note, .. } => note,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(
            notes,
            vec![1, 2, 3],
            "equal-frame events must keep insertion order"
        );
    }

    #[test]
    fn sort_by_frame_is_stable_and_allocation_free_in_place() {
        let mut buf = MidiEventBuffer::with_capacity(6);
        let ch = MidiChannel::new(0).unwrap();
        // Two events share frame 2; tag them via note number to detect order.
        let events = [
            MidiEvent {
                frame: 4,
                message: MidiMessage::note_on(ch, 40, 1).unwrap(),
            },
            MidiEvent {
                frame: 2,
                message: MidiMessage::note_on(ch, 21, 1).unwrap(),
            },
            MidiEvent {
                frame: 1,
                message: MidiMessage::note_on(ch, 10, 1).unwrap(),
            },
            MidiEvent {
                frame: 2,
                message: MidiMessage::note_on(ch, 22, 1).unwrap(),
            },
            MidiEvent {
                frame: 0,
                message: MidiMessage::note_on(ch, 0, 1).unwrap(),
            },
        ];
        for e in events {
            buf.push(e).unwrap();
        }
        let cap_before = buf.capacity();
        buf.sort_by_frame();
        assert_eq!(buf.capacity(), cap_before, "sort must not reallocate");

        let frames: Vec<u32> = buf.iter().map(|e| e.frame).collect();
        assert_eq!(frames, vec![0, 1, 2, 2, 4]);
        let notes: Vec<u8> = buf
            .iter()
            .map(|e| match e.message {
                MidiMessage::NoteOn { note, .. } => note,
                _ => unreachable!(),
            })
            .collect();
        // 21 was pushed before 22, both at frame 2: stability keeps that order.
        assert_eq!(notes, vec![0, 10, 21, 22, 40]);
    }

    #[test]
    fn iter_range_boundaries() {
        let mut buf = MidiEventBuffer::with_capacity(8);
        for f in [0u32, 1, 2, 3, 4, 5] {
            buf.push(ev(f)).unwrap();
        }
        let frames: Vec<u32> = buf.iter_range(2..4).map(|e| e.frame).collect();
        assert_eq!(
            frames,
            vec![2, 3],
            "range end is exclusive, start is inclusive"
        );

        let none: Vec<u32> = buf.iter_range(10..20).map(|e| e.frame).collect();
        assert!(none.is_empty());

        let all: Vec<u32> = buf.iter_range(0..6).map(|e| e.frame).collect();
        assert_eq!(all.len(), 6);

        let empty_range: Vec<u32> = buf.iter_range(3..3).map(|e| e.frame).collect();
        assert!(empty_range.is_empty(), "an empty range must yield nothing");
    }

    #[test]
    fn extend_from_respects_capacity_and_order() {
        let mut a = MidiEventBuffer::with_capacity(4);
        a.push(ev(0)).unwrap();
        a.push(ev(1)).unwrap();

        let mut b = MidiEventBuffer::with_capacity(2);
        b.push(ev(9)).unwrap();
        b.push(ev(10)).unwrap();

        a.extend_from(&b).unwrap();
        let frames: Vec<u32> = a.iter().map(|e| e.frame).collect();
        assert_eq!(frames, vec![0, 1, 9, 10]);
        assert_eq!(a.capacity(), 4);

        let err = a.extend_from(&b).unwrap_err();
        assert_eq!(
            err,
            SuperbError::CapacityExceeded {
                what: "midi_event_buffer",
                capacity: 4
            }
        );
        // Unchanged on failure.
        assert_eq!(a.len(), 4);
    }

    #[test]
    fn retain_filters_in_place() {
        let mut buf = MidiEventBuffer::with_capacity(5);
        for f in [0u32, 1, 2, 3, 4] {
            buf.push(ev(f)).unwrap();
        }
        buf.retain(|e| e.frame % 2 == 0);
        let frames: Vec<u32> = buf.iter().map(|e| e.frame).collect();
        assert_eq!(frames, vec![0, 2, 4]);
        assert_eq!(buf.capacity(), 5, "retain must not change capacity");
    }

    #[test]
    fn clear_keeps_capacity() {
        let mut buf = MidiEventBuffer::with_capacity(3);
        buf.push(ev(0)).unwrap();
        buf.push(ev(1)).unwrap();
        buf.clear();
        assert!(buf.is_empty());
        assert_eq!(buf.capacity(), 3);
        buf.push(ev(0)).unwrap();
        buf.push(ev(1)).unwrap();
        buf.push(ev(2)).unwrap();
        assert!(buf.push(ev(3)).is_err());
    }

    #[test]
    fn as_slice_and_into_iter_agree() {
        let mut buf = MidiEventBuffer::with_capacity(3);
        buf.push(ev(0)).unwrap();
        buf.push(ev(1)).unwrap();
        let from_slice: Vec<u32> = buf.as_slice().iter().map(|e| e.frame).collect();
        let from_into_iter: Vec<u32> = (&buf).into_iter().map(|e| e.frame).collect();
        assert_eq!(from_slice, from_into_iter);
    }
}
