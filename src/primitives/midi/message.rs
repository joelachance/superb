//! MIDI message parsing, encoding and construction.

use crate::primitives::{Result, SuperbError};

use crate::primitives::midi::channel::MidiChannel;

/// Highest value representable by a 7-bit MIDI data byte.
const MAX_7BIT: u16 = 127;
/// Highest value representable by a 14-bit MIDI value (pitch bend, session position).
const MAX_14BIT: u16 = 16_383;

/// A single decoded MIDI 1.0 channel-voice or system message.
///
/// This type covers everything a DAW's realtime MIDI path needs: note and
/// controller messages, pitch bend, program/channel pressure, and the
/// system-common / system-realtime messages used for transport sync.
///
/// SysEx (`0xF0`), MIDI Time Code quarter frames (`0xF1`) and running status
/// are **not** supported — see the crate docs for why, and
/// [`MidiMessage::from_bytes`] for the exact error returned.
///
/// The type is `Copy` and contains no heap data, so it is safe to move
/// through an [`spsc`](crate::primitives::realtime::spsc) queue or store in a [`crate::primitives::midi::MidiEventBuffer`]
/// on the audio thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MidiMessage {
    /// Note released. A `NoteOn` with velocity `0` is normalised to this
    /// variant by [`MidiMessage::from_bytes`] and by [`MidiMessage::note_on`].
    NoteOff {
        /// Channel the message applies to.
        channel: MidiChannel,
        /// Note number, `0..=127`.
        note: u8,
        /// Release velocity, `0..=127`.
        velocity: u8,
    },
    /// Note struck. Velocity is never `0`; see [`MidiMessage::NoteOff`].
    NoteOn {
        /// Channel the message applies to.
        channel: MidiChannel,
        /// Note number, `0..=127`.
        note: u8,
        /// Attack velocity, `1..=127`.
        velocity: u8,
    },
    /// Polyphonic (per-note) key pressure, a.k.a. aftertouch.
    PolyPressure {
        /// Channel the message applies to.
        channel: MidiChannel,
        /// Note number, `0..=127`.
        note: u8,
        /// Pressure amount, `0..=127`.
        pressure: u8,
    },
    /// A control change (CC) message.
    ControlChange {
        /// Channel the message applies to.
        channel: MidiChannel,
        /// Controller number, `0..=127`.
        controller: u8,
        /// Controller value, `0..=127`.
        value: u8,
    },
    /// Program (patch/instrument) change.
    ProgramChange {
        /// Channel the message applies to.
        channel: MidiChannel,
        /// Program number, `0..=127`.
        program: u8,
    },
    /// Channel-wide (monophonic) pressure, a.k.a. channel aftertouch.
    ChannelPressure {
        /// Channel the message applies to.
        channel: MidiChannel,
        /// Pressure amount, `0..=127`.
        pressure: u8,
    },
    /// Pitch bend wheel position.
    PitchBend {
        /// Channel the message applies to.
        channel: MidiChannel,
        /// 14-bit bend value, `0..=16383`. [`MidiMessage::pitch_bend_centre`]
        /// (`8192`) is the resting position.
        value: u16,
    },
    /// Document position pointer, in MIDI beats (sixteenth notes) from the start
    /// of the session.
    SongPosition(u16),
    /// Selects a session/sequence number for playback.
    SongSelect(u8),
    /// Requests that an analog synth retune its oscillators.
    TuneRequest,
    /// MIDI clock tick, sent 24 times per quarter note while running.
    TimingClock,
    /// Starts the sequence from the beginning.
    Start,
    /// Resumes a sequence from the current position.
    Continue,
    /// Stops the sequence.
    Stop,
    /// Keep-alive sent by some controllers roughly every 300 ms.
    ActiveSensing,
    /// Requests a full device reset.
    SystemReset,
}

impl MidiMessage {
    /// The centre (resting) value of a 14-bit pitch bend, `8192`.
    pub const fn pitch_bend_centre() -> u16 {
        8192
    }

    /// Creates a `NoteOn`/`NoteOff` message, validating `note` and `velocity`
    /// are `< 128`.
    ///
    /// A velocity of `0` is normalised to [`MidiMessage::NoteOff`], matching
    /// [`MidiMessage::from_bytes`] and standard MIDI convention.
    pub fn note_on(channel: MidiChannel, note: u8, velocity: u8) -> Result<Self> {
        check_7bit("note", note)?;
        check_7bit("velocity", velocity)?;
        if velocity == 0 {
            Ok(MidiMessage::NoteOff {
                channel,
                note,
                velocity: 0,
            })
        } else {
            Ok(MidiMessage::NoteOn {
                channel,
                note,
                velocity,
            })
        }
    }

    /// Creates a `NoteOff` message, validating `note` and `velocity` are
    /// `< 128`.
    pub fn note_off(channel: MidiChannel, note: u8, velocity: u8) -> Result<Self> {
        check_7bit("note", note)?;
        check_7bit("velocity", velocity)?;
        Ok(MidiMessage::NoteOff {
            channel,
            note,
            velocity,
        })
    }

    /// Creates a `ControlChange` message, validating `controller` and
    /// `value` are `< 128`.
    pub fn control_change(channel: MidiChannel, controller: u8, value: u8) -> Result<Self> {
        check_7bit("controller", controller)?;
        check_7bit("value", value)?;
        Ok(MidiMessage::ControlChange {
            channel,
            controller,
            value,
        })
    }

    /// Creates a `PitchBend` message, validating `value` is `< 16384`.
    pub fn pitch_bend(channel: MidiChannel, value: u16) -> Result<Self> {
        check_14bit("pitch_bend_value", value)?;
        Ok(MidiMessage::PitchBend { channel, value })
    }

    /// Returns the pitch bend value normalised to `-1.0..=1.0`, where `0.0`
    /// is the centre position. Returns `0.0` for every variant other than
    /// [`MidiMessage::PitchBend`].
    pub fn bend_normalized(&self) -> f32 {
        match self {
            MidiMessage::PitchBend { value, .. } => {
                (*value as f32 - Self::pitch_bend_centre() as f32)
                    / Self::pitch_bend_centre() as f32
            }
            _ => 0.0,
        }
    }

    /// The channel this message applies to, or `None` for system messages
    /// (which have no channel).
    pub const fn channel(&self) -> Option<MidiChannel> {
        match self {
            MidiMessage::NoteOff { channel, .. }
            | MidiMessage::NoteOn { channel, .. }
            | MidiMessage::PolyPressure { channel, .. }
            | MidiMessage::ControlChange { channel, .. }
            | MidiMessage::ProgramChange { channel, .. }
            | MidiMessage::ChannelPressure { channel, .. }
            | MidiMessage::PitchBend { channel, .. } => Some(*channel),
            _ => None,
        }
    }

    /// `true` for [`MidiMessage::NoteOn`] and [`MidiMessage::NoteOff`].
    pub const fn is_note(&self) -> bool {
        matches!(
            self,
            MidiMessage::NoteOn { .. } | MidiMessage::NoteOff { .. }
        )
    }

    /// `true` for the single-byte System Real-Time messages (clock,
    /// start/stop/continue, active sensing, system reset).
    pub const fn is_realtime(&self) -> bool {
        matches!(
            self,
            MidiMessage::TimingClock
                | MidiMessage::Start
                | MidiMessage::Continue
                | MidiMessage::Stop
                | MidiMessage::ActiveSensing
                | MidiMessage::SystemReset
        )
    }

    /// The status byte this message encodes to (the first byte written by
    /// [`MidiMessage::to_bytes`]).
    pub const fn status_byte(&self) -> u8 {
        match self {
            MidiMessage::NoteOff { channel, .. } => 0x80 | channel.index(),
            MidiMessage::NoteOn { channel, .. } => 0x90 | channel.index(),
            MidiMessage::PolyPressure { channel, .. } => 0xA0 | channel.index(),
            MidiMessage::ControlChange { channel, .. } => 0xB0 | channel.index(),
            MidiMessage::ProgramChange { channel, .. } => 0xC0 | channel.index(),
            MidiMessage::ChannelPressure { channel, .. } => 0xD0 | channel.index(),
            MidiMessage::PitchBend { channel, .. } => 0xE0 | channel.index(),
            MidiMessage::SongPosition(_) => 0xF2,
            MidiMessage::SongSelect(_) => 0xF3,
            MidiMessage::TuneRequest => 0xF6,
            MidiMessage::TimingClock => 0xF8,
            MidiMessage::Start => 0xFA,
            MidiMessage::Continue => 0xFB,
            MidiMessage::Stop => 0xFC,
            MidiMessage::ActiveSensing => 0xFE,
            MidiMessage::SystemReset => 0xFF,
        }
    }

    /// Encodes this message into `out`, returning the number of bytes
    /// written (`1`, `2` or `3`).
    pub fn to_bytes(&self, out: &mut [u8; 3]) -> usize {
        match *self {
            MidiMessage::NoteOff { note, velocity, .. } => {
                out[0] = self.status_byte();
                out[1] = note;
                out[2] = velocity;
                3
            }
            MidiMessage::NoteOn { note, velocity, .. } => {
                out[0] = self.status_byte();
                out[1] = note;
                out[2] = velocity;
                3
            }
            MidiMessage::PolyPressure { note, pressure, .. } => {
                out[0] = self.status_byte();
                out[1] = note;
                out[2] = pressure;
                3
            }
            MidiMessage::ControlChange {
                controller, value, ..
            } => {
                out[0] = self.status_byte();
                out[1] = controller;
                out[2] = value;
                3
            }
            MidiMessage::ProgramChange { program, .. } => {
                out[0] = self.status_byte();
                out[1] = program;
                2
            }
            MidiMessage::ChannelPressure { pressure, .. } => {
                out[0] = self.status_byte();
                out[1] = pressure;
                2
            }
            MidiMessage::PitchBend { value, .. } => {
                out[0] = self.status_byte();
                out[1] = (value & 0x7F) as u8;
                out[2] = ((value >> 7) & 0x7F) as u8;
                3
            }
            MidiMessage::SongPosition(pos) => {
                out[0] = self.status_byte();
                out[1] = (pos & 0x7F) as u8;
                out[2] = ((pos >> 7) & 0x7F) as u8;
                3
            }
            MidiMessage::SongSelect(session) => {
                out[0] = self.status_byte();
                out[1] = session;
                2
            }
            MidiMessage::TuneRequest
            | MidiMessage::TimingClock
            | MidiMessage::Start
            | MidiMessage::Continue
            | MidiMessage::Stop
            | MidiMessage::ActiveSensing
            | MidiMessage::SystemReset => {
                out[0] = self.status_byte();
                1
            }
        }
    }

    /// Parses a single, complete MIDI message from `bytes`.
    ///
    /// `bytes` must start with a status byte (`>= 0x80`); running status is
    /// not supported. `bytes` must contain *exactly* the number of bytes the
    /// message needs (no trailing or missing data bytes).
    ///
    /// A `NoteOn` with velocity `0` is normalised to [`MidiMessage::NoteOff`].
    ///
    /// # Errors
    /// Returns [`SuperbError::InvalidArgument`] if:
    /// - `bytes` is empty,
    /// - the first byte is not a status byte (`< 0x80`, i.e. running status),
    /// - any data byte is `>= 0x80`,
    /// - the length does not match the message type, or
    /// - the status byte is SysEx (`0xF0`), MTC quarter frame (`0xF1`), a
    ///   standalone end-of-SysEx (`0xF7`), or an undefined system byte.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let status = *bytes.first().ok_or(SuperbError::InvalidArgument {
            what: "midi_bytes_empty",
        })?;
        if status < 0x80 {
            return Err(SuperbError::InvalidArgument {
                what: "midi_running_status_unsupported",
            });
        }
        let data = &bytes[1..];
        for &b in data {
            if b >= 0x80 {
                return Err(SuperbError::InvalidArgument {
                    what: "midi_data_byte",
                });
            }
        }

        let expect = |n: usize| -> Result<()> {
            if data.len() == n {
                Ok(())
            } else {
                Err(SuperbError::InvalidArgument {
                    what: "midi_message_length",
                })
            }
        };

        match status {
            0x80..=0x8F => {
                expect(2)?;
                let channel = MidiChannel::new(status & 0x0F)?;
                Ok(MidiMessage::NoteOff {
                    channel,
                    note: data[0],
                    velocity: data[1],
                })
            }
            0x90..=0x9F => {
                expect(2)?;
                let channel = MidiChannel::new(status & 0x0F)?;
                let (note, velocity) = (data[0], data[1]);
                if velocity == 0 {
                    Ok(MidiMessage::NoteOff {
                        channel,
                        note,
                        velocity: 0,
                    })
                } else {
                    Ok(MidiMessage::NoteOn {
                        channel,
                        note,
                        velocity,
                    })
                }
            }
            0xA0..=0xAF => {
                expect(2)?;
                let channel = MidiChannel::new(status & 0x0F)?;
                Ok(MidiMessage::PolyPressure {
                    channel,
                    note: data[0],
                    pressure: data[1],
                })
            }
            0xB0..=0xBF => {
                expect(2)?;
                let channel = MidiChannel::new(status & 0x0F)?;
                Ok(MidiMessage::ControlChange {
                    channel,
                    controller: data[0],
                    value: data[1],
                })
            }
            0xC0..=0xCF => {
                expect(1)?;
                let channel = MidiChannel::new(status & 0x0F)?;
                Ok(MidiMessage::ProgramChange {
                    channel,
                    program: data[0],
                })
            }
            0xD0..=0xDF => {
                expect(1)?;
                let channel = MidiChannel::new(status & 0x0F)?;
                Ok(MidiMessage::ChannelPressure {
                    channel,
                    pressure: data[0],
                })
            }
            0xE0..=0xEF => {
                expect(2)?;
                let channel = MidiChannel::new(status & 0x0F)?;
                let value = data[0] as u16 | ((data[1] as u16) << 7);
                Ok(MidiMessage::PitchBend { channel, value })
            }
            0xF0 => Err(SuperbError::InvalidArgument {
                what: "midi_sysex_unsupported",
            }),
            0xF1 => Err(SuperbError::InvalidArgument {
                what: "midi_mtc_quarter_frame_unsupported",
            }),
            0xF2 => {
                expect(2)?;
                let pos = data[0] as u16 | ((data[1] as u16) << 7);
                Ok(MidiMessage::SongPosition(pos))
            }
            0xF3 => {
                expect(1)?;
                Ok(MidiMessage::SongSelect(data[0]))
            }
            0xF4 | 0xF5 | 0xF9 | 0xFD => Err(SuperbError::InvalidArgument {
                what: "midi_undefined_status",
            }),
            0xF6 => {
                expect(0)?;
                Ok(MidiMessage::TuneRequest)
            }
            0xF7 => Err(SuperbError::InvalidArgument {
                what: "midi_standalone_eox_unsupported",
            }),
            0xF8 => {
                expect(0)?;
                Ok(MidiMessage::TimingClock)
            }
            0xFA => {
                expect(0)?;
                Ok(MidiMessage::Start)
            }
            0xFB => {
                expect(0)?;
                Ok(MidiMessage::Continue)
            }
            0xFC => {
                expect(0)?;
                Ok(MidiMessage::Stop)
            }
            0xFE => {
                expect(0)?;
                Ok(MidiMessage::ActiveSensing)
            }
            0xFF => {
                expect(0)?;
                Ok(MidiMessage::SystemReset)
            }
            _ => unreachable!("all u8 values 0x80..=0xFF are covered above"),
        }
    }
}

fn check_7bit(what: &'static str, value: u8) -> Result<()> {
    if value as u16 > MAX_7BIT {
        Err(SuperbError::InvalidArgument { what })
    } else {
        Ok(())
    }
}

fn check_14bit(what: &'static str, value: u16) -> Result<()> {
    if value > MAX_14BIT {
        Err(SuperbError::InvalidArgument { what })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(n: u8) -> MidiChannel {
        MidiChannel::new(n).unwrap()
    }

    fn round_trip(msg: MidiMessage) -> MidiMessage {
        let mut buf = [0u8; 3];
        let n = msg.to_bytes(&mut buf);
        MidiMessage::from_bytes(&buf[..n]).unwrap()
    }

    #[test]
    fn round_trips_every_variant() {
        let cases = [
            MidiMessage::NoteOff {
                channel: ch(0),
                note: 60,
                velocity: 64,
            },
            MidiMessage::NoteOn {
                channel: ch(1),
                note: 61,
                velocity: 100,
            },
            MidiMessage::PolyPressure {
                channel: ch(2),
                note: 62,
                pressure: 10,
            },
            MidiMessage::ControlChange {
                channel: ch(3),
                controller: 7,
                value: 127,
            },
            MidiMessage::ProgramChange {
                channel: ch(4),
                program: 5,
            },
            MidiMessage::ChannelPressure {
                channel: ch(5),
                pressure: 33,
            },
            MidiMessage::PitchBend {
                channel: ch(6),
                value: 0,
            },
            MidiMessage::PitchBend {
                channel: ch(6),
                value: 8192,
            },
            MidiMessage::PitchBend {
                channel: ch(6),
                value: 16383,
            },
            MidiMessage::SongPosition(1234),
            MidiMessage::SongSelect(9),
            MidiMessage::TuneRequest,
            MidiMessage::TimingClock,
            MidiMessage::Start,
            MidiMessage::Continue,
            MidiMessage::Stop,
            MidiMessage::ActiveSensing,
            MidiMessage::SystemReset,
        ];
        for msg in cases {
            assert_eq!(round_trip(msg), msg, "round trip failed for {msg:?}");
        }
    }

    #[test]
    fn note_on_velocity_zero_normalises_to_note_off() {
        let bytes = [0x90, 60, 0];
        let msg = MidiMessage::from_bytes(&bytes).unwrap();
        assert_eq!(
            msg,
            MidiMessage::NoteOff {
                channel: ch(0),
                note: 60,
                velocity: 0
            }
        );

        let via_ctor = MidiMessage::note_on(ch(0), 60, 0).unwrap();
        assert_eq!(
            via_ctor,
            MidiMessage::NoteOff {
                channel: ch(0),
                note: 60,
                velocity: 0
            }
        );
    }

    #[test]
    fn note_on_nonzero_velocity_stays_note_on() {
        let msg = MidiMessage::from_bytes(&[0x91, 60, 1]).unwrap();
        assert_eq!(
            msg,
            MidiMessage::NoteOn {
                channel: ch(1),
                note: 60,
                velocity: 1
            }
        );
    }

    #[test]
    fn rejects_high_bit_data_bytes() {
        assert!(MidiMessage::from_bytes(&[0x90, 200, 64]).is_err());
        assert!(MidiMessage::from_bytes(&[0x90, 60, 200]).is_err());
    }

    #[test]
    fn rejects_sysex_and_mtc_and_running_status() {
        assert!(MidiMessage::from_bytes(&[0xF0, 0x43, 0x12, 0xF7]).is_err());
        assert!(MidiMessage::from_bytes(&[0xF1, 0x00]).is_err());
        // "running status": a data byte where a status byte is expected.
        assert!(MidiMessage::from_bytes(&[60, 64]).is_err());
        assert!(MidiMessage::from_bytes(&[0xF7]).is_err());
        assert!(MidiMessage::from_bytes(&[0xF4]).is_err());
        assert!(MidiMessage::from_bytes(&[0xF9]).is_err());
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(MidiMessage::from_bytes(&[0x90, 60]).is_err()); // too short
        assert!(MidiMessage::from_bytes(&[0x90, 60, 64, 0]).is_err()); // too long
        assert!(MidiMessage::from_bytes(&[0xC0, 1, 2]).is_err()); // program change takes 1 byte
        assert!(MidiMessage::from_bytes(&[0xF8, 0]).is_err()); // clock takes 0 bytes
        assert!(MidiMessage::from_bytes(&[]).is_err());
    }

    #[test]
    fn pitch_bend_centre_and_normalisation() {
        assert_eq!(MidiMessage::pitch_bend_centre(), 8192);

        let centre = MidiMessage::pitch_bend(ch(0), 8192).unwrap();
        assert_eq!(centre.bend_normalized(), 0.0);

        let min = MidiMessage::pitch_bend(ch(0), 0).unwrap();
        assert_eq!(min.bend_normalized(), -1.0);

        let max = MidiMessage::pitch_bend(ch(0), 16383).unwrap();
        assert!((max.bend_normalized() - 0.99988).abs() < 1e-4);

        // Non-pitch-bend variants normalise to 0.
        assert_eq!(MidiMessage::TuneRequest.bend_normalized(), 0.0);
    }

    #[test]
    fn pitch_bend_rejects_out_of_range() {
        assert!(MidiMessage::pitch_bend(ch(0), 16384).is_err());
    }

    #[test]
    fn constructors_validate_data_ranges() {
        assert!(MidiMessage::note_on(ch(0), 128, 10).is_err());
        assert!(MidiMessage::note_on(ch(0), 10, 128).is_err());
        assert!(MidiMessage::note_off(ch(0), 128, 10).is_err());
        assert!(MidiMessage::control_change(ch(0), 128, 10).is_err());
        assert!(MidiMessage::control_change(ch(0), 10, 128).is_err());
        assert!(MidiMessage::note_on(ch(0), 60, 64).is_ok());
    }

    #[test]
    fn channel_accessor() {
        assert_eq!(
            MidiMessage::note_on(ch(3), 60, 64).unwrap().channel(),
            Some(ch(3))
        );
        assert_eq!(MidiMessage::TimingClock.channel(), None);
        assert_eq!(MidiMessage::SongPosition(0).channel(), None);
    }

    #[test]
    fn is_note_flag() {
        assert!(MidiMessage::note_on(ch(0), 60, 64).unwrap().is_note());
        assert!(MidiMessage::note_off(ch(0), 60, 0).unwrap().is_note());
        assert!(!MidiMessage::control_change(ch(0), 1, 1).unwrap().is_note());
    }

    #[test]
    fn is_realtime_flag() {
        for msg in [
            MidiMessage::TimingClock,
            MidiMessage::Start,
            MidiMessage::Continue,
            MidiMessage::Stop,
            MidiMessage::ActiveSensing,
            MidiMessage::SystemReset,
        ] {
            assert!(msg.is_realtime(), "{msg:?} should be realtime");
        }
        for msg in [
            MidiMessage::TuneRequest,
            MidiMessage::SongSelect(0),
            MidiMessage::SongPosition(0),
        ] {
            assert!(!msg.is_realtime(), "{msg:?} should not be realtime");
        }
        assert!(!MidiMessage::note_on(ch(0), 60, 64).unwrap().is_realtime());
    }

    #[test]
    fn status_byte_matches_to_bytes_first_byte() {
        let msg = MidiMessage::control_change(ch(9), 7, 100).unwrap();
        let mut buf = [0u8; 3];
        let n = msg.to_bytes(&mut buf);
        assert_eq!(buf[0], msg.status_byte());
        assert_eq!(n, 3);
        assert_eq!(msg.status_byte(), 0xB9);
    }

    #[test]
    fn to_bytes_lengths_per_variant() {
        let mut buf = [0u8; 3];
        assert_eq!(
            MidiMessage::note_on(ch(0), 1, 1)
                .unwrap()
                .to_bytes(&mut buf),
            3
        );
        assert_eq!(
            MidiMessage::ProgramChange {
                channel: ch(0),
                program: 1
            }
            .to_bytes(&mut buf),
            2
        );
        assert_eq!(
            MidiMessage::ChannelPressure {
                channel: ch(0),
                pressure: 1
            }
            .to_bytes(&mut buf),
            2
        );
        assert_eq!(MidiMessage::SongSelect(1).to_bytes(&mut buf), 2);
        assert_eq!(MidiMessage::SongPosition(1).to_bytes(&mut buf), 3);
        assert_eq!(MidiMessage::TuneRequest.to_bytes(&mut buf), 1);
        assert_eq!(MidiMessage::SystemReset.to_bytes(&mut buf), 1);
    }
}
