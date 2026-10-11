//! `midi`: play notes into a monitoring track; connect a keyboard.
//!
//! What you'll learn:
//! - A MIDI track that *monitors* carries a live instrument in the mixer
//! - `midi::play_note` / `release_note` queue notes for the next block
//! - Any `MidiMessage` goes through `midi::send`
//! - With feature `midi-io`, hardware ports are listed and connected to the
//!   engine; their notes play on every monitoring track
//!
//! ```text
//! cargo run --example midi_keys
//! cargo run --example midi_keys --features midi-io   # also lists ports
//! ```

use aura::prelude::*;
use aura::{midi, session, track, transport};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let mut session = session::create(std::env::temp_dir().join("midi_keys.aura"), rate, 128)?;
    let keys = track::add(&mut session, "keys", TrackKind::Midi)?;

    // Monitoring is track state (saved, not an undo step) and changes the
    // mixer: the instrument is a node on the track's strip.
    track::monitor(&mut session, keys, true)?;
    transport::start(&mut session)?;
    println!("monitoring: {}", session.memory.track(keys)?.state.monitor);
    println!(
        "silence before any note: {}",
        session::steps::render_blocks(&mut session, 2)?.is_silent()
    );

    // Middle C, then a chord on top. Channel 0, velocity 100.
    midi::play_note(&mut session, 0, 60, 100)?;
    let audio = session::steps::render_blocks(&mut session, 8)?;
    println!("C4 sounding, peak {:.3}", audio.peak());
    for note in [64, 67] {
        midi::play_note(&mut session, 0, note, 90)?;
    }
    let audio = session::steps::render_blocks(&mut session, 8)?;
    println!("C major chord, peak {:.3}", audio.peak());

    // Release everything. The instrument stops at once (no release tail).
    for note in [60, 64, 67] {
        midi::release_note(&mut session, 0, note)?;
    }
    let audio = session::steps::render_blocks(&mut session, 4)?;
    println!("after release, last block silent: {}", {
        let block = session.engine.max_frames();
        audio.channel(0)[audio.frames() - block..]
            .iter()
            .all(|&s| s == 0.0)
    });

    // Any message can be sent; the instrument ignores what it does not use.
    midi::send(
        &mut session,
        MidiMessage::control_change(MidiChannel::new(0)?, 1, 64)?,
    )?;

    // Hardware keyboards.
    #[cfg(feature = "midi-io")]
    {
        let ports = aura::devices::midi_inputs()?;
        if ports.is_empty() {
            println!("no MIDI input ports");
        }
        for p in &ports {
            println!("MIDI in {}: {}", p.index, p.name);
        }
        if !ports.is_empty() {
            // Connected ports feed the engine; with feature `devices` and
            // `devices::output` you would hear them on the keys track.
            midi::connect_input(&mut session, ports[0].index)?;
            println!(
                "connected {} ({} input(s) held by the session)",
                ports[0].name,
                session.midi_inputs.len()
            );
        }
    }

    // Monitoring off: the instrument leaves the mixer at the next rebuild.
    track::monitor(&mut session, keys, false)?;
    midi::play_note(&mut session, 0, 60, 100)?;
    println!(
        "monitor off, note ignored: {}",
        session::steps::render_blocks(&mut session, 4)?.is_silent()
    );
    Ok(())
}
