//! Builds a small session through the undo stack, walks the history, saves
//! the document as JSON and reloads it. No audio hardware needed.
//!
//! What you'll learn:
//! - `UndoStack<Session>`: push / undo / redo / seal / begin_group / end_group
//! - Commands as the only way to mutate (so every edit is reversible)
//! - Coalescing rapid property edits (send level drag, move item drag)
//! - `mark_clean` / `is_clean` for "document dirty?" after save
//!
//! ```text
//! cargo run -p session_undo
//! ```

use superb::core::{BeatPosition, Result, SampleRate, TimeDuration, TimePosition};
use superb::session::commands::{
    self, AddItem, AddMarker, AddSend, AddTrack, MoveItem, SetTempo, SetTrackOutput,
};
use superb::session::{Item, ItemSource, Marker, MidiNote, Session, Track, TrackKind, TrackSend};
use superb::undo::UndoStack;

// Helper: print a one-line snapshot of session + undo/redo tip labels.
// `&Session` / `&UndoStack` = shared borrows (read-only).
fn describe(session: &Session, history: &UndoStack<Session>) {
    println!(
        "  {:<12} rev {:>3}  tracks {}  items {}  sends {}  markers {}  tempo {:.0} bpm  end {} smp  | undo: {:?}  redo: {:?}",
        session.name(),
        session.revision(),
        session.track_count(),
        session.item_count(),
        session.send_count(),
        session.markers().len(),
        session.tempo_map().tempo_at(BeatPosition::ZERO),
        session.end_position().samples(),
        // unwrap_or: if the undo/redo stack tip is empty, show "-".
        history.undo_label().unwrap_or("-"),
        history.redo_label().unwrap_or("-"),
    );
}

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let mut session = Session::new("untitled", rate);
    // UndoStack stores applied commands so we can revert/reapply them.
    let mut history = UndoStack::new();

    println!("edits:");
    // push applies the command and records it for undo.
    history.push(&mut session, commands::set_session_name("demo song"))?;
    history.push(&mut session, SetTempo::new(BeatPosition::ZERO, 96.0))?;

    // One undo step for the whole "add drums with a loop" operation.
    // begin_group / end_group nest multiple pushes into a single undo entry.
    history.begin_group("Add drum track");
    // Pre-allocate IDs so the command (and its undo) know the stable identity.
    let drums = session.allocate_track_id();
    history.push(
        &mut session,
        AddTrack::new(Track::with_id(drums, "drums", TrackKind::Audio)),
    )?;
    history.push(
        &mut session,
        commands::set_track_color(drums, Some(0xE0_60_30)),
    )?;
    let loop_clip = session.allocate_clip_id();
    let loop_item = Item::with_id(
        loop_clip,
        "break.wav",
        TimePosition::ZERO,
        TimeDuration::from_seconds(2.0, rate),
        // .into() converts &str → PathBuf for the audio path.
        ItemSource::Audio {
            path: "loops/break.wav".into(),
            offset: TimeDuration::ZERO,
        },
    )?;
    history.push(&mut session, AddItem::new(drums, loop_item))?;
    history.end_group();
    describe(&session, &history);

    let keys = session.allocate_track_id();
    history.push(
        &mut session,
        AddTrack::new(Track::with_id(keys, "keys", TrackKind::Midi)),
    )?;
    let chord = session.allocate_clip_id();
    // C major triad as three MidiNote structs in a vec![...].
    let notes = vec![
        MidiNote {
            beat: 0.0,
            length: 2.0,
            note: 60,
            velocity: 96,
            channel: 0,
        },
        MidiNote {
            beat: 0.0,
            length: 2.0,
            note: 64,
            velocity: 96,
            channel: 0,
        },
        MidiNote {
            beat: 0.0,
            length: 2.0,
            note: 67,
            velocity: 96,
            channel: 0,
        },
    ];
    // TempoMap converts musical beats → sample positions.
    let bar = session.tempo_map().beat_to_samples(BeatPosition::new(4.0));
    let chord_item = Item::with_id(
        chord,
        "C major",
        bar,
        TimeDuration::from_samples(bar.samples()),
        ItemSource::Midi { notes },
    )?;
    history.push(&mut session, AddItem::new(keys, chord_item))?;
    describe(&session, &history);

    // Routing is part of the document: keys feed a reverb bus, drums send
    // into it pre-fader, and the send level drag coalesces into one step.
    let verb = session.allocate_track_id();
    history.push(
        &mut session,
        AddTrack::new(Track::with_id(verb, "verb", TrackKind::Bus)),
    )?;
    history.push(&mut session, SetTrackOutput::new(keys, Some(verb)))?;
    let send = session.allocate_send_id();
    history.push(
        &mut session,
        AddSend::new(drums, TrackSend::with_id(send, verb).pre_fader()),
    )?;
    // Rapid set_send_level_db pushes share a coalesce key → one undo step after seal.
    for db in [-3.0, -6.0, -9.0] {
        history.push(&mut session, commands::set_send_level_db(send, db))?;
    }
    // seal() closes the current coalesce window so the next push is separate.
    history.seal();
    describe(&session, &history);

    // A drag: several small moves coalesce into a single "Move item" step.
    for beat in [5.0, 6.0, 7.0, 8.0] {
        let pos = session.tempo_map().beat_to_samples(BeatPosition::new(beat));
        history.push(&mut session, MoveItem::new(chord, pos))?;
    }
    history.seal();
    let marker = session.allocate_marker_id();
    history.push(
        &mut session,
        AddMarker::new(Marker::with_id(marker, "verse", bar)),
    )?;
    describe(&session, &history);
    // undo_labels() is an iterator; collect::<Vec<_>>() materializes it for Debug.
    println!(
        "  history ({} steps): {:?}",
        history.undo_len(),
        history.undo_labels().collect::<Vec<_>>()
    );

    println!("undo x3:");
    for _ in 0..3 {
        history.undo(&mut session)?;
        describe(&session, &history);
    }
    println!("redo x3:");
    // redo returns Ok(true) while there is something to redo.
    while history.redo(&mut session)? {
        describe(&session, &history);
    }

    let path = "session_undo.superb.json";
    session.save(path)?;
    // mark_clean remembers this revision as the last saved ("not dirty").
    history.mark_clean();
    let reloaded = Session::load(path)?;
    // snapshot() compares document state; metadata().map(...).unwrap_or(0) is soft I/O.
    println!(
        "saved {path} ({} bytes) and reloaded: identical = {}",
        std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        reloaded.snapshot() == session.snapshot()
    );
    println!("clean after save: {}", history.is_clean());
    history.undo(&mut session)?;
    println!("clean after one more undo: {}", history.is_clean());
    Ok(())
}
