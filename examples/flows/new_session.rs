//! `session`, `track`, `clip`: start a session, put things in it, save it,
//! open it again.
//!
//! What you'll learn:
//! - A `Session` is the state every flow works on; `session::create` makes one
//! - Tracks come in three kinds: Audio, Midi, Bus
//! - Clips play audio held in memory (a decoded file or, as here, a buffer
//!   you made) or MIDI notes; `clip::add` works out which and how long
//! - Every edit is undoable: `session::undo` / `session::redo`
//! - Save is JSON; open gives you a fresh Session with the same document
//!
//! ```text
//! cargo run --example new_session
//! ```

use superb::prelude::*;
use superb::primitives::session::MidiNote;
use superb::{clip, session, track};

fn main() -> Result<()> {
    // 48 kHz, 256-frame blocks. Nothing is playing yet; this is just the
    // document plus an idle engine.
    let mut session = session::create("My first session", SampleRate::HZ_48K, 256)?;

    // Tracks. `track::add` returns an ID you keep; names are for people.
    let vox = track::add(&mut session, "vox", TrackKind::Audio)?;
    let keys = track::add(&mut session, "keys", TrackKind::Midi)?;

    // Audio for the vox clip. A real program decodes a file (`AudioCache::load`,
    // feature `files`); here we synthesise one second of a soft sine so the
    // example needs no file on disk. The key is the name the clip refers to.
    let rate = session.document.sample_rate();
    let mut take = PlanarBuffer::new(2, rate.hz() as usize);
    for ch in 0..2 {
        for (i, s) in take.channel_mut(ch).iter_mut().enumerate() {
            let t = i as f32 / rate.hz() as f32;
            *s = 0.3 * (t * 220.0 * core::f32::consts::TAU).sin();
        }
    }
    session.cache.insert("vox-take-1.wav", take);
    // Put it on the timeline, starting half a second in. The clip is as long
    // as the audio and named after the key ("vox-take-1").
    let clip = clip::add(
        &mut session,
        vox,
        "vox-take-1.wav",
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

    // Document-level settings.
    session::set_tempo(&mut session, 100.0)?;
    session::add_marker(&mut session, "verse", TimePosition::from_seconds(0.5, rate))?;
    println!(
        "{}: {} tracks, {} clips, {} bpm",
        session.document.name(),
        session.document.track_count(),
        session.document.clip_count(),
        session.document.tempo_map().tempo_at(BeatPosition::ZERO)
    );

    // Undo works on everything above. Move the clip, change your mind.
    clip::move_to(&mut session, clip, TimePosition::from_seconds(2.0, rate))?;
    // Moves of one clip within half a second merge into one undo step, so a
    // drag undoes in one go. Sealing on "mouse up" ends the step at once.
    session.history.seal();
    println!(
        "clip now starts at {:.2} s",
        session.document.clip(clip)?.start().to_seconds(rate)
    );
    session::undo(&mut session)?;
    println!(
        "after undo:      {:.2} s",
        session.document.clip(clip)?.start().to_seconds(rate)
    );
    session::redo(&mut session)?;
    println!(
        "after redo:      {:.2} s",
        session.document.clip(clip)?.start().to_seconds(rate)
    );

    // Save, then open into a second session. Audio is not embedded: the clip
    // still refers to "vox-take-1.wav", which you re-cache on open (a real
    // program decodes the file; we reuse the buffer).
    let path = std::env::temp_dir().join("new_session.superb.json");
    println!("saved? {}", session.history.is_clean());
    session::save(&mut session, &path)?;
    println!(
        "saved? {} -> {}",
        session.history.is_clean(),
        path.display()
    );

    let mut again = session::open(&path, 256)?;
    again.cache.insert(
        "vox-take-1.wav",
        (*session.cache.get("vox-take-1.wav".as_ref()).unwrap()).clone(),
    );
    println!(
        "reopened {:?}: {} tracks, {} clips, marker {:?} at {:.2} s",
        again.document.name(),
        again.document.track_count(),
        again.document.clip_count(),
        again.document.markers()[0].name,
        again.document.markers()[0].position().to_seconds(rate)
    );
    Ok(())
}
