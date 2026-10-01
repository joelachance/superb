//! MIDI: hearing what you play, sending notes, connecting a keyboard.
//!
//! A MIDI track that is *monitoring* gets a live instrument when the
//! console is built, so notes arriving at the engine sound on that track's
//! strip. Notes can come from your program ([`play_note`], [`send`]) or,
//! with the `midi-io` feature, from a hardware port (`connect_input`).
//!
//! Made of `superb::primitives::midi` (messages), the engine's MIDI queue
//! in `superb::primitives::io::live`, and the live instrument
//! [`play::steps::build_console`](crate::play::steps::build_console) adds.

use crate::primitives::midi::{MidiChannel, MidiMessage, TimestampedMessage};
use crate::primitives::session::TrackKind;
use crate::primitives::{Result, SuperbError, TrackId};
use crate::rig::Rig;

/// Turns live monitoring on or off for a MIDI track. While on, the track
/// carries an instrument that plays whatever MIDI reaches the engine. A
/// switch: saved with the session (as the track's arm flag), not an undo
/// step. It does change the console's shape, so it takes effect at the next
/// rebuild.
///
/// Made of: `Track::armed` through `session::switch`; marks the rig for a
/// rebuild (the instrument is a node in the console).
pub fn monitor(rig: &mut Rig, track: TrackId, on: bool) -> Result<()> {
    if rig.session.track(track)?.kind != TrackKind::Midi {
        return Err(SuperbError::InvalidArgument {
            what: "only MIDI tracks monitor live MIDI",
        });
    }
    crate::session::switch(rig, |s| {
        s.track_mut(track)?.armed = on;
        Ok(())
    })?;
    rig.needs_rebuild = true;
    Ok(())
}

/// `true` if the MIDI track is monitoring.
pub fn is_monitoring(rig: &Rig, track: TrackId) -> bool {
    rig.session
        .track(track)
        .map(|t| t.kind == TrackKind::Midi && t.armed)
        .unwrap_or(false)
}

/// Plays a note on the engine's software input. It sounds at the start of
/// the next block on every monitoring track. `channel` is `0..=15`, `note`
/// and `velocity` are `0..=127`; a velocity of `0` releases the note.
///
/// Made of: `MidiMessage::note_on` → [`send`].
pub fn play_note(rig: &mut Rig, channel: u8, note: u8, velocity: u8) -> Result<()> {
    let channel = MidiChannel::new(channel)?;
    send(rig, MidiMessage::note_on(channel, note, velocity)?)
}

/// Releases a note started with [`play_note`].
///
/// Made of: `MidiMessage::note_off` → [`send`].
pub fn release_note(rig: &mut Rig, channel: u8, note: u8) -> Result<()> {
    let channel = MidiChannel::new(channel)?;
    send(rig, MidiMessage::note_off(channel, note, 0)?)
}

/// Queues any MIDI message for the start of the engine's next block.
///
/// Made of: `EngineHandle::push_midi` with a zero timestamp.
pub fn send(rig: &mut Rig, message: MidiMessage) -> Result<()> {
    rig.engine.push_midi(TimestampedMessage {
        timestamp_us: 0,
        message,
    })
}

/// Lists the hardware MIDI input ports.
///
/// Made of: `midi::input::list_input_ports`.
#[cfg(feature = "midi-io")]
pub fn inputs() -> Result<Vec<crate::primitives::midi::input::MidiPortInfo>> {
    crate::primitives::midi::input::list_input_ports()
}

/// Connects a hardware MIDI input (an index from [`inputs`]) to the
/// engine. Its notes land at the start of the block they arrive in and play
/// on every monitoring track. The connection lives in [`Rig::midi_inputs`]
/// and closes when removed from there.
///
/// Made of: `MidiInputBridge::connect` → `EngineHandle::send(AddMidiInput)`.
#[cfg(feature = "midi-io")]
pub fn connect_input(rig: &mut Rig, port_index: usize) -> Result<()> {
    let (bridge, consumer) =
        crate::primitives::midi::input::MidiInputBridge::connect(port_index, 256)?;
    rig.engine
        .send(crate::primitives::io::live::EngineCommand::AddMidiInput(
            consumer,
        ))?;
    rig.midi_inputs.push(bridge);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::play;
    use crate::primitives::SampleRate;
    use crate::session;

    const R: SampleRate = SampleRate::HZ_48K;

    #[test]
    fn a_monitoring_track_sounds_when_a_note_arrives() {
        let mut rig = session::create("keys", R, 64).unwrap();
        let keys = session::add_track(&mut rig, "keys", TrackKind::Midi).unwrap();
        assert!(!is_monitoring(&rig, keys));
        monitor(&mut rig, keys, true).unwrap();
        assert!(is_monitoring(&rig, keys));
        play::start(&mut rig).unwrap();
        // Nothing played yet: silence.
        assert!(play::drive(&mut rig, 2).unwrap().is_silent());
        play_note(&mut rig, 0, 69, 100).unwrap();
        let sounding = play::drive(&mut rig, 4).unwrap();
        assert!(sounding.peak() > 0.5, "peak {}", sounding.peak());
        release_note(&mut rig, 0, 69).unwrap();
        let released = play::drive(&mut rig, 4).unwrap();
        assert!(released.channel(0)[128..].iter().all(|&s| s == 0.0));
        // Turning monitoring off removes the instrument at the next rebuild.
        // It is a switch: nothing lands in the undo history.
        let steps = rig.history.undo_len();
        monitor(&mut rig, keys, false).unwrap();
        assert_eq!(rig.history.undo_len(), steps);
        play_note(&mut rig, 0, 69, 100).unwrap();
        assert!(play::drive(&mut rig, 4).unwrap().is_silent());
    }

    #[test]
    fn only_midi_tracks_monitor_and_bad_notes_are_refused() {
        let mut rig = session::create("keys", R, 64).unwrap();
        let vox = session::add_track(&mut rig, "vox", TrackKind::Audio).unwrap();
        assert!(matches!(
            monitor(&mut rig, vox, true),
            Err(SuperbError::InvalidArgument { .. })
        ));
        assert!(monitor(&mut rig, TrackId::from_raw(99), true).is_err());
        assert!(
            play_note(&mut rig, 16, 60, 100).is_err(),
            "channel out of range"
        );
        assert!(
            play_note(&mut rig, 0, 200, 100).is_err(),
            "note out of range"
        );
        send(&mut rig, MidiMessage::TimingClock).unwrap();
    }
}
