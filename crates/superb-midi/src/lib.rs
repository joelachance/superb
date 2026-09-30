//! # superb-midi
//!
//! MIDI 1.0 messages and real-time-safe event plumbing for the Superb DAW
//! SDK.
//!
//! * [`MidiChannel`] — validated 0-based/1-based MIDI channel newtype.
//! * [`MidiMessage`] — a decoded channel-voice or system message, `Copy` and
//!   heap-free, with [`MidiMessage::from_bytes`] / [`MidiMessage::to_bytes`]
//!   for wire encoding.
//! * [`MidiEvent`] — a [`MidiMessage`] stamped with a sample offset
//!   (`frame`) within the current process block.
//! * [`MidiEventBuffer`] — a fixed-capacity, allocated-once buffer of
//!   [`MidiEvent`]s for the audio thread.
//! * [`TimestampedMessage`] — a [`MidiMessage`] stamped with a microsecond
//!   timestamp, the payload carried from a non-realtime input thread to the
//!   audio thread over a [`superb_rt::spsc`] queue.
//! * [`timing`] — [`timing::FrameMapper`] converts those microsecond
//!   timestamps into block-relative frame offsets.
//! * `input` module (feature `midir`, off by default) — lists hardware MIDI
//!   input ports and bridges them into an SPSC queue via the `midir` crate.
//!
//! SysEx, MIDI Time Code quarter frames and running status are not
//! supported; see [`MidiMessage::from_bytes`] and `docs/superb-midi.md` for
//! details.
//!
//! ```
//! use superb_midi::{MidiChannel, MidiEvent, MidiEventBuffer, MidiMessage};
//!
//! // Decode a Note On from raw bytes.
//! let channel = MidiChannel::new(0).unwrap();
//! let note_on = MidiMessage::from_bytes(&[0x90, 60, 100]).unwrap();
//! assert_eq!(note_on, MidiMessage::NoteOn { channel, note: 60, velocity: 100 });
//!
//! // Stamp it with a frame offset and store it in a fixed-capacity buffer.
//! let mut buffer = MidiEventBuffer::with_capacity(256);
//! buffer.push(MidiEvent { frame: 0, message: note_on }).unwrap();
//! buffer.push_sorted(MidiEvent { frame: 10, message: MidiMessage::note_off(channel, 60, 0).unwrap() }).unwrap();
//! assert_eq!(buffer.len(), 2);
//!
//! // Round trip back to bytes.
//! let mut wire = [0u8; 3];
//! let n = note_on.to_bytes(&mut wire);
//! assert_eq!(&wire[..n], &[0x90, 60, 100]);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod channel;
mod event;
#[cfg(feature = "midir")]
pub mod input;
mod message;
pub mod timing;

pub use channel::MidiChannel;
pub use event::{MidiEvent, MidiEventBuffer, TimestampedMessage};
#[cfg(feature = "midir")]
pub use input::{list_input_ports, MidiInputBridge, MidiPortInfo};
pub use message::MidiMessage;

#[cfg(test)]
mod cross_thread_tests {
    use super::*;

    /// The non-RT input thread pushes `TimestampedMessage`s and the "audio
    /// thread" (this thread) drains them in order — the pattern
    /// [`input::MidiInputBridge`] uses in production, exercised here without
    /// any hardware dependency.
    #[test]
    fn timestamped_messages_cross_threads_in_order() {
        const N: u64 = 5_000;
        let (mut tx, mut rx) = superb_rt::spsc::channel::<TimestampedMessage>(64);
        let channel = MidiChannel::new(0).unwrap();

        let producer = std::thread::spawn(move || {
            for i in 0..N {
                let note = (i % 128) as u8;
                let message = MidiMessage::note_on(channel, note, 100).unwrap();
                let mut event = TimestampedMessage {
                    timestamp_us: i,
                    message,
                };
                loop {
                    match tx.push(event) {
                        Ok(()) => break,
                        Err(back) => {
                            event = back;
                            std::hint::spin_loop();
                        }
                    }
                }
            }
        });

        let mut expected_ts = 0u64;
        while expected_ts < N {
            if let Some(received) = rx.pop() {
                assert_eq!(received.timestamp_us, expected_ts);
                let expected_note = (expected_ts % 128) as u8;
                match received.message {
                    MidiMessage::NoteOn { note, velocity, .. } => {
                        assert_eq!(note, expected_note);
                        assert_eq!(velocity, 100);
                    }
                    other => panic!("unexpected message: {other:?}"),
                }
                expected_ts += 1;
            } else {
                std::hint::spin_loop();
            }
        }

        producer.join().unwrap();
        assert_eq!(rx.pop(), None);
    }
}
