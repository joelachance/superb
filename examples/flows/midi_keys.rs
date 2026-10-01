//! `midi`: play notes into a monitoring track; connect a keyboard.
//!
//! What you'll learn:
//! - A MIDI track that *monitors* carries a live instrument in the console
//! - `midi::play_note` / `release_note` queue notes for the next block
//! - Any `MidiMessage` goes through `midi::send`
//! - With feature `midi-io`, hardware ports are listed and connected to the
//!   engine; their notes play on every monitoring track
//!
//! ```text
//! cargo run --example midi_keys
//! cargo run --example midi_keys --features midi-io   # also lists ports
//! ```

use superb::prelude::*;
use superb::{midi, play, session};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let mut rig = session::create("keys", rate, 128)?;
    let keys = session::add_track(&mut rig, "keys", TrackKind::Midi)?;

    // Monitoring is part of the session (undoable) and changes the console:
    // the instrument is a node on the track's strip.
    midi::monitor(&mut rig, keys, true)?;
    play::start(&mut rig)?;
    println!("monitoring: {}", midi::is_monitoring(&rig, keys));
    println!(
        "silence before any note: {}",
        play::drive(&mut rig, 2)?.is_silent()
    );

    // Middle C, then a chord on top. Channel 0, velocity 100.
    midi::play_note(&mut rig, 0, 60, 100)?;
    let audio = play::drive(&mut rig, 8)?;
    println!("C4 sounding, peak {:.3}", audio.peak());
    for note in [64, 67] {
        midi::play_note(&mut rig, 0, note, 90)?;
    }
    let audio = play::drive(&mut rig, 8)?;
    println!("C major chord, peak {:.3}", audio.peak());

    // Release everything. The instrument stops at once (no release tail).
    for note in [60, 64, 67] {
        midi::release_note(&mut rig, 0, note)?;
    }
    let audio = play::drive(&mut rig, 4)?;
    println!("after release, last block silent: {}", {
        let block = rig.engine.max_frames();
        audio.channel(0)[audio.frames() - block..]
            .iter()
            .all(|&s| s == 0.0)
    });

    // Any message can be sent; the instrument ignores what it does not use.
    midi::send(
        &mut rig,
        MidiMessage::control_change(MidiChannel::new(0)?, 1, 64)?,
    )?;

    // Hardware keyboards.
    #[cfg(feature = "midi-io")]
    {
        let ports = superb::devices::midi_inputs()?;
        if ports.is_empty() {
            println!("no MIDI input ports");
        }
        for p in &ports {
            println!("MIDI in {}: {}", p.index, p.name);
        }
        if !ports.is_empty() {
            // Connected ports feed the engine; with feature `devices` and
            // `play::start_output` you would hear them on the keys track.
            midi::connect_input(&mut rig, ports[0].index)?;
            println!(
                "connected {} ({} input(s) held by the rig)",
                ports[0].name,
                rig.midi_inputs.len()
            );
        }
    }

    // Monitoring off: the instrument leaves the console at the next rebuild.
    midi::monitor(&mut rig, keys, false)?;
    midi::play_note(&mut rig, 0, 60, 100)?;
    println!(
        "monitor off, note ignored: {}",
        play::drive(&mut rig, 4)?.is_silent()
    );
    Ok(())
}
