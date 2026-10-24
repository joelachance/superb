//! `session::steps`: build the mixer by hand and render it offline.
//!
//! Every verb is made of public steps. This example skips the verbs and the
//! `Session`: it takes a `Memory`, builds the mixer with
//! `session::steps::build_mixer`, and runs it through an offline `Engine`.
//! `export` does the same thing to bounce a file.
//!
//! What you'll learn:
//! - The memory and the mixer are separate: `build_mixer` makes a strip
//!   per track, a node per clip, and hands back the live controls
//!   (`MixerHandles`) keyed by the memory's ids
//! - `AudioCache` holds the audio clips refer to
//! - Bus routing and a pre-fader send come from the memory
//! - A prepared mixer runs through `Engine` + `offline::render`
//!
//! ```text
//! cargo run --example session_steps
//! ```

use std::path::PathBuf;

use superb::prelude::*;
use superb::primitives::io::wav::{self, WavFormat};
use superb::primitives::io::{offline, Engine};
use superb::primitives::session::MidiNote;
use superb::session;

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    // The memory: tracks, clips, tempo, routing.
    let mut memory = Memory::new("session-steps", rate);

    // An audio track with half a second of a soft tone held in memory.
    let vox = memory.add_track("vox", TrackKind::Audio);
    let mut tone = PlanarBuffer::new(2, 24_000);
    tone.fill(0.2);
    let mut cache = AudioCache::new();
    cache.insert("tone.wav", tone); // the name the clip refers to
    memory.add_clip(
        vox,
        Clip::new(
            "take",
            TimePosition::from_samples(12_000), // 0.25 s of silence first
            TimeDuration::from_samples(24_000),
            ClipSource::Audio {
                path: PathBuf::from("tone.wav"),
                offset: TimeDuration::ZERO,
            },
        )?,
    )?;

    // A MIDI track with one A4 half-beat note; its fader at -3 dB.
    let keys = memory.add_track("keys", TrackKind::Midi);
    memory.track_mut(keys)?.state.volume_db = -3.0;
    memory.add_clip(
        keys,
        Clip::new(
            "a4",
            TimePosition::ZERO,
            TimeDuration::from_samples(48_000),
            ClipSource::Midi {
                notes: vec![MidiNote {
                    beat: 0.0,
                    length: 0.5,
                    note: 69,
                    velocity: 100,
                    channel: 0,
                }],
            },
        )?,
    )?;

    // Routing lives in the memory: keys → room bus (-6 dB) → master, and a
    // quiet pre-fader send from vox into the same bus.
    let room = memory.add_track("room", TrackKind::Bus);
    memory.track_mut(room)?.state.volume_db = -6.0;
    memory.set_track_output(keys, Some(room))?;
    let send = memory.add_send(vox, TrackSend::new(room).level_db(-12.0).pre_fader())?;

    // Step one: the mixer. The handles say what the mixer got for each
    // track and send in the memory; the flows use them to move a fader without
    // a rebuild.
    let (mixer, handles) = session::steps::build_mixer(&memory, &cache, 2)?;
    println!(
        "{} tracks became {} strips, {} send(s); the graph has {} nodes",
        memory.track_count(),
        handles.strips.len(),
        handles.sends.len(),
        mixer.graph().node_count()
    );
    for (track, strip) in &handles.strips {
        println!(
            "  {} at {:.1} dB{}",
            memory.track(*track)?.name,
            strip.volume_db(),
            if strip.is_muted() { " (muted)" } else { "" }
        );
    }
    println!(
        "  vox send at {:.1} dB, on: {}",
        handles.send(send).map_or(0.0, |s| s.level_db()),
        handles.send(send).is_some_and(|s| s.is_enabled())
    );

    // Step two: run it. The live engine would `publish` the mixer's graph;
    // offline we hand it to an `Engine` with a transport on the memory's
    // tempo map and render a second.
    let mut transport = Transport::with_tempo_map(memory.tempo_map().clone());
    transport.play();
    let mut engine = Engine::new(mixer, transport);
    let audio = offline::render(&mut engine, rate, 256, 48_000)?;

    let path = std::env::temp_dir().join("session_steps.wav");
    wav::write(&path, &audio, rate, WavFormat::Float32)?;
    println!(
        "wrote {}: {} frames, peak {:.3}",
        path.display(),
        audio.frames(),
        audio.peak()
    );
    Ok(())
}
