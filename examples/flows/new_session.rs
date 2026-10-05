//! `session`, `track`, `clip`: start a session, put things in it, save it,
//! open it again.
//!
//! What you'll learn:
//! - A `Session` is the state every flow works on; `session::create` makes one
//! - Tracks come in three kinds: Audio, Midi, Bus
//! - Clips play audio held in memory (a decoded file or, as here, a buffer
//!   you made) or MIDI notes; `clip::add` works out which and how long
//! - Every edit is undoable: `session::undo` / `session::redo`
//! - A saved session is a folder: `memory.json` plus the audio clips refer to
//!   as WAV files; open reads both back
//!
//! ```text
//! cargo run --example new_session
//! ```

use superb::prelude::*;
use superb::primitives::session::MidiNote;
use superb::{clip, session, track};

fn main() -> Result<()> {
    // 48 kHz, 256-frame blocks. Nothing is playing yet; this is just the
    // memory plus an idle engine.
    let mut session = session::create("My first session", SampleRate::HZ_48K, 256)?;

    // Tracks. `track::add` returns an ID you keep; names are for people.
    let vox = track::add(&mut session, "vox", TrackKind::Audio)?;
    let keys = track::add(&mut session, "keys", TrackKind::Midi)?;

    // Audio for the vox clip. A real program decodes a file (`AudioCache::load`,
    // feature `files`); here we synthesise one second of a soft sine so the
    // example needs no file on disk. The key is the name the clip refers to.
    let rate = session.memory.sample_rate();
    let mut take = PlanarBuffer::new(2, rate.hz() as usize);
    for ch in 0..2 {
        for (i, s) in take.channel_mut(ch).iter_mut().enumerate() {
            let t = i as f32 / rate.hz() as f32;
            *s = 0.3 * (t * 220.0 * core::f32::consts::TAU).sin();
        }
    }
    session.audio.insert("audio/vox-take-1.wav", take);
    // Put it on the timeline, starting half a second in. The clip is as long
    // as the audio and named after the key's stem ("vox-take-1").
    let clip = clip::add(
        &mut session,
        vox,
        "audio/vox-take-1.wav",
        TimePosition::from_seconds(0.5, rate),
    )?;

    // A MIDI clip: a C major arpeggio. Notes are in beats relative to the
    // clip start; the clip ends with its last note.
    let notes: Vec<MidiNote> = [60u8, 64, 67, 72]
        .iter()
        .enumerate()
        .map(|(i, &note)| MidiNote {
            beat: i as f64,
            length: 0.9,
            note,
            velocity: 96,
            channel: 0,
        })
        .collect();
    clip::add(&mut session, keys, notes, TimePosition::ZERO)?;

    // Memory-level settings.
    session::set_tempo(&mut session, 100.0)?;
    session::add_marker(&mut session, "verse", TimePosition::from_seconds(0.5, rate))?;
    println!(
        "{}: {} tracks, {} clips, {} bpm",
        session.memory.name(),
        session.memory.track_count(),
        session.memory.clip_count(),
        session.memory.tempo_map().tempo_at(BeatPosition::ZERO)
    );

    // Undo works on everything above. Move the clip, change your mind.
    clip::move_to(&mut session, clip, TimePosition::from_seconds(2.0, rate))?;
    // Moves of one clip within half a second merge into one undo step, so a
    // drag undoes in one go. Sealing on "mouse up" ends the step at once.
    session.history.seal();
    println!(
        "clip now starts at {:.2} s",
        session.memory.clip(clip)?.start().to_seconds(rate)
    );
    session::undo(&mut session)?;
    println!(
        "after undo:      {:.2} s",
        session.memory.clip(clip)?.start().to_seconds(rate)
    );
    session::redo(&mut session)?;
    println!(
        "after redo:      {:.2} s",
        session.memory.clip(clip)?.start().to_seconds(rate)
    );

    // Save into a session folder, then open it again. The folder gets
    // memory.json and, because a clip refers to it, audio/vox-take-1.wav;
    // open reads both back, so the clip plays without any help.
    let folder = std::env::temp_dir().join("new_session.superb");
    println!("saved? {}", session.history.is_clean());
    session::save(&mut session, &folder)?;
    println!(
        "saved? {} -> {}",
        session.history.is_clean(),
        folder.display()
    );

    let again = session::open(&folder, 256)?;
    println!(
        "reopened {:?}: {} tracks, {} clips, {} audio file(s), marker {:?} at {:.2} s",
        again.memory.name(),
        again.memory.track_count(),
        again.memory.clip_count(),
        again.audio.len(),
        again.memory.markers()[0].name,
        again.memory.markers()[0].position().to_seconds(rate)
    );
    Ok(())
}
