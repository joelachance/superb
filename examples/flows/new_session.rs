//! `document`: start a document, put things in it, save it, open it again.
//!
//! What you'll learn:
//! - A `Session` is the state every flow works on; `session::create` makes one
//! - Tracks come in three kinds: Audio, Midi, Bus
//! - Clips point at audio in the `AudioCache` (a real file or, as here, a
//!   buffer you made) or hold MIDI notes inline
//! - Every edit is undoable: `session::undo` / `session::redo`
//! - Save is JSON; open gives you a fresh Session with the same document
//!
//! ```text
//! cargo run --example new_session
//! ```

use superb::prelude::*;
use superb::primitives::session::MidiNote;
use superb::session;

fn main() -> Result<()> {
    // 48 kHz, 256-frame blocks. Nothing is playing yet; this is just the
    // document plus an idle engine.
    let mut session = session::create("My first document", SampleRate::HZ_48K, 256)?;

    // Tracks. `add_track` returns an ID you keep; names are for people.
    let vox = session::add_track(&mut session, "vox", TrackKind::Audio)?;
    let keys = session::add_track(&mut session, "keys", TrackKind::Midi)?;

    // Audio for the vox clip. A real program calls `session::import_audio`
    // (feature `files`); here we synthesise one second of a soft sine so the
    // example needs no file on disk. The cache key is the "path" the clip
    // refers to.
    let rate = session.document.sample_rate();
    let mut take = PlanarBuffer::new(2, rate.hz() as usize);
    for ch in 0..2 {
        for (i, s) in take.channel_mut(ch).iter_mut().enumerate() {
            let t = i as f32 / rate.hz() as f32;
            *s = 0.3 * (t * 220.0 * core::f32::consts::TAU).sin();
        }
    }
    session.cache.insert("vox-take-1.wav", take);
    // Put the take on the timeline, starting half a second in.
    let clip = session::add_audio_clip(
        &mut session,
        vox,
        "vox-take-1.wav",
        TimePosition::from_seconds(0.5, rate),
        TimeDuration::from_seconds(1.0, rate),
    )?;

    // A MIDI clip: two bars of a C major arpeggio. Notes are in beats
    // relative to the clip start; the clip's length is in samples.
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
    session::add_midi_clip(
        &mut session,
        keys,
        "arpeggio",
        TimePosition::ZERO,
        TimeDuration::from_seconds(4.0, rate),
        notes,
    )?;

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
    session::move_clip(&mut session, clip, TimePosition::from_seconds(2.0, rate))?;
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
    println!("saved? {}", session::is_saved(&session));
    session::save(&mut session, &path)?;
    println!(
        "saved? {} -> {}",
        session::is_saved(&session),
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
