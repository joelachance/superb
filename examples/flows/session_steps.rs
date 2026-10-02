//! `play::steps`: build the console by hand and render it offline.
//!
//! Every `play` verb is made of public steps. This example skips the verbs
//! and the `Session`: it takes a `Document`, builds the console with
//! `session::steps::build_mixer`, and runs it through an offline `Engine`.
//! `export::render` does the same thing with inserts attached.
//!
//! What you'll learn:
//! - The document (document) and the mixer (console) are separate; `Wiring`
//!   is the map between them: a strip per track, a node per clip
//! - `AudioCache` holds the audio clips refer to
//! - Bus routing and a pre-fader send come from the document
//! - A prepared mixer runs through `Engine` + `offline::render`
//!
//! ```text
//! cargo run --example console_steps
//! ```

use std::path::PathBuf;

use superb::prelude::*;
use superb::primitives::io::wav::{self, WavFormat};
use superb::primitives::io::{offline, Engine};
use superb::primitives::session::MidiNote;
use superb::session;

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    // The document is the document: tracks, clips, tempo, routing.
    let mut document = Document::new("console-steps", rate);

    // An audio track with half a second of a soft tone from the cache.
    let vox = document.add_track("vox", TrackKind::Audio);
    let mut tone = PlanarBuffer::new(2, 24_000);
    tone.fill(0.2);
    let mut cache = AudioCache::new();
    cache.insert("tone.wav", tone); // the key the clip refers to
    document.add_clip(
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
    let keys = document.add_track("keys", TrackKind::Midi);
    document.track_mut(keys)?.state.volume_db = -3.0;
    document.add_clip(
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

    // Routing lives in the document: keys → room bus (-6 dB) → master, and a
    // quiet pre-fader send from vox into the same bus.
    let room = document.add_track("room", TrackKind::Bus);
    document.track_mut(room)?.state.volume_db = -6.0;
    document.set_track_output(keys, Some(room))?;
    let send = document.add_send(vox, TrackSend::new(room).level_db(-12.0).pre_fader())?;

    // Step one: the console. `Wiring` says what became what.
    let (mixer, wiring) = session::steps::build_mixer(&document, &cache, 2)?;
    println!(
        "{} tracks became {} strips; {} clip node(s); vox send {} is mixer send {:?}",
        document.track_count(),
        wiring.bridge().entries().len(),
        wiring.clips().len(),
        send.raw(),
        wiring.bridge().mixer_send(send).map(|s| s.raw())
    );
    for wired in wiring.clips() {
        println!(
            "  clip {:?} on {} -> node {:?}",
            document.clip(wired.clip)?.name,
            document.track(wired.track)?.name,
            wired.node
        );
    }

    // Step two: run it. The live engine would `publish` the mixer's graph;
    // offline we hand it to an `Engine` with a transport on the document's
    // tempo map and render a second.
    let mut transport = Transport::with_tempo_map(document.tempo_map().clone());
    transport.play();
    let mut engine = Engine::new(mixer, transport);
    let audio = offline::render(&mut engine, rate, 256, 48_000)?;

    let path = std::env::temp_dir().join("console_steps.wav");
    wav::write(&path, &audio, rate, WavFormat::Float32)?;
    println!(
        "wrote {}: {} frames, peak {:.3}",
        path.display(),
        audio.frames(),
        audio.peak()
    );
    Ok(())
}
