//! Offline: build a tiny session (cached DC audio clip + MIDI note, a "room"
//! bus the keys feed and the vox send into), render a region through the
//! mixer, write `session_playback.wav`.
//!
//! What you'll learn:
//! - Session document vs Mixer (arrangement data → render via `render_region`)
//! - `AudioCache` so we can play without decoding a real file
//! - Track kinds: Audio / Midi / Bus, plus sends and track outputs
//! - `Clip` + `ClipSource` (Audio path or inline MIDI notes)
//!
//! ```text
//! cargo run -p session_playback
//! ```

use std::path::PathBuf;

use superb::io::wav::{self, WavFormat};
use superb::playback::{render_region, AudioCache, RenderOptions};
use superb::prelude::*;
use superb::session::MidiNote;

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    // Session is the editable document (tracks, clips, tempo, routing).
    let mut session = Session::new("playback-demo", rate);

    // Audio track with a 0.5 s DC tone from a cached buffer (no file decode).
    let vox = session.add_track("vox", TrackKind::Audio);
    // 24_000 frames @ 48 kHz = 0.5 s of stereo samples, all 0.2.
    let mut tone = PlanarBuffer::new(2, 24_000);
    tone.fill(0.2);
    let mut cache = AudioCache::new();
    // Key must match ClipSource::Audio { path } below.
    cache.insert("tone.wav", tone);
    // Clip = clip on the timeline: name, start, length, source.
    // Starts at 12_000 samples (0.25 s) so there is silence before the take.
    let take = Clip::new(
        "take",
        TimePosition::from_samples(12_000),
        TimeDuration::from_samples(24_000),
        ClipSource::Audio {
            path: PathBuf::from("tone.wav"),
            offset: TimeDuration::ZERO,
        },
    )?;
    session.add_clip(vox, take)?;

    // MIDI track with one A4 quarter-note at the start.
    let keys = session.add_track("keys", TrackKind::Midi);
    // track_mut borrows the track uniquely so we can set its document gain.
    session.track_mut(keys)?.gain_db = -3.0;
    let note = Clip::new(
        "a4",
        TimePosition::ZERO,
        TimeDuration::from_samples(48_000),
        ClipSource::Midi {
            // note 69 = A4; length 0.5 beats at session tempo.
            notes: vec![MidiNote {
                beat: 0.0,
                length: 0.5,
                note: 69,
                velocity: 100,
                channel: 0,
            }],
        },
    )?;
    session.add_clip(keys, note)?;

    // Routing lives in the document: keys → room bus (−6 dB) → master, and a
    // quiet pre-fader send from vox into the same bus. `render_region`
    // mirrors this onto the mixer (`SessionMixerBridge::apply_routing`).
    let room = session.add_track("room", TrackKind::Bus);
    session.track_mut(room)?.gain_db = -6.0;
    // None would mean "straight to master"; Some(room) feeds the bus first.
    session.set_track_output(keys, Some(room))?;
    // Builder-style: TrackSend::new(dest).level_db(...).pre_fader().
    let send = session.add_send(vox, TrackSend::new(room).level_db(-12.0).pre_fader())?;
    println!(
        "keys → {} → master; vox send {} → {} at {} dB (pre-fader)",
        session.track(room)?.name,
        send.raw(),
        session.track(session.send(send)?.bus())?.name,
        session.send(send)?.level_db
    );

    // One second. render_region wires session → mixer offline and returns a PlanarBuffer.
    let frames = 48_000;
    let audio = render_region(
        &session,
        &cache,
        TimePosition::ZERO,
        frames,
        &RenderOptions {
            block_size: 256,
            channels: 2,
        },
    )?;

    let path = "session_playback.wav";
    wav::write(path, &audio, rate, WavFormat::Float32)?;
    println!(
        "wrote {path}: {} frames, peak {:.3}",
        audio.frames(),
        audio.peak()
    );
    Ok(())
}
