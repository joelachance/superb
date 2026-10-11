//! Builds a small memory through the undo stack, walks the history, saves
//! the memory as JSON and reloads it. No audio hardware needed.
//!
//! What you'll learn:
//! - `UndoStack<Memory>`: push / undo / redo / seal / begin_group / end_group
//! - Commands as the only way to mutate (so every edit is reversible)
//! - Coalescing rapid property edits (send level drag, move clip drag)
//! - `mark_clean` / `is_clean` for "memory dirty?" after save
//!
//! ```text
//! cargo run --example session_undo
//! ```

use aura::primitives::session::commands::{
    self, AddClip, AddMarker, AddSend, AddTrack, MoveClip, SetTempo, SetTrackOutput,
};
use aura::primitives::session::{
    Clip, ClipSource, Marker, Memory, MidiNote, Track, TrackKind, TrackSend,
};
use aura::primitives::undo::UndoStack;
use aura::primitives::{BeatPosition, Result, SampleRate, TimeDuration, TimePosition};

// Helper: print a one-line snapshot of memory + undo/redo tip labels.
// `&Memory` / `&UndoStack` = shared borrows (read-only).
fn describe(memory: &Memory, history: &UndoStack<Memory>) {
    println!(
        "  {:<12} rev {:>3}  tracks {}  clips {}  sends {}  markers {}  tempo {:.0} bpm  end {} smp  | undo: {:?}  redo: {:?}",
        memory.name(),
        memory.revision(),
        memory.track_count(),
        memory.clip_count(),
        memory.send_count(),
        memory.markers().len(),
        memory.tempo_map().tempo_at(BeatPosition::ZERO),
        memory.end_position().samples(),
        // unwrap_or: if the undo/redo stack tip is empty, show "-".
        history.undo_label().unwrap_or("-"),
        history.redo_label().unwrap_or("-"),
    );
}

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let mut memory = Memory::new("untitled", rate);
    // UndoStack stores applied commands so we can revert/reapply them.
    let mut history = UndoStack::new();

    println!("edits:");
    // push applies the command and records it for undo.
    history.push(&mut memory, commands::set_session_name("demo memory"))?;
    history.push(&mut memory, SetTempo::new(BeatPosition::ZERO, 96.0))?;

    // One undo step for the whole "add drums with a loop" operation.
    // begin_group / end_group nest multiple pushes into a single undo entry.
    history.begin_group("Add drum track");
    // Pre-allocate IDs so the command (and its undo) know the stable identity.
    let drums = memory.allocate_track_id();
    history.push(
        &mut memory,
        AddTrack::new(Track::with_id(drums, "drums", TrackKind::Audio)),
    )?;
    history.push(
        &mut memory,
        commands::set_track_color(drums, Some(0xE0_60_30)),
    )?;
    let loop_clip = memory.allocate_clip_id();
    let loop_clip = Clip::with_id(
        loop_clip,
        "break.wav",
        TimePosition::ZERO,
        TimeDuration::from_seconds(2.0, rate),
        // .into() converts &str → PathBuf for the audio path.
        ClipSource::Audio {
            path: "loops/break.wav".into(),
            offset: TimeDuration::ZERO,
        },
    )?;
    history.push(&mut memory, AddClip::new(drums, loop_clip))?;
    history.end_group();
    describe(&memory, &history);

    let keys = memory.allocate_track_id();
    history.push(
        &mut memory,
        AddTrack::new(Track::with_id(keys, "keys", TrackKind::Midi)),
    )?;
    let chord = memory.allocate_clip_id();
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
    let bar = memory.tempo_map().beat_to_samples(BeatPosition::new(4.0));
    let chord_clip = Clip::with_id(
        chord,
        "C major",
        bar,
        TimeDuration::from_samples(bar.samples()),
        ClipSource::Midi { notes },
    )?;
    history.push(&mut memory, AddClip::new(keys, chord_clip))?;
    describe(&memory, &history);

    // Routing is part of the memory: keys feed a reverb bus, drums send
    // into it pre-fader, and the send level drag coalesces into one step.
    let verb = memory.allocate_track_id();
    history.push(
        &mut memory,
        AddTrack::new(Track::with_id(verb, "verb", TrackKind::Bus)),
    )?;
    history.push(&mut memory, SetTrackOutput::new(keys, Some(verb)))?;
    let send = memory.allocate_send_id();
    history.push(
        &mut memory,
        AddSend::new(drums, TrackSend::with_id(send, verb).pre_fader()),
    )?;
    // Rapid set_send_level_db pushes share a coalesce key → one undo step after seal.
    for db in [-3.0, -6.0, -9.0] {
        history.push(&mut memory, commands::set_send_level_db(send, db))?;
    }
    // seal() closes the current coalesce window so the next push is separate.
    history.seal();
    describe(&memory, &history);

    // A drag: several small moves coalesce into a single "Move clip" step.
    for beat in [5.0, 6.0, 7.0, 8.0] {
        let pos = memory.tempo_map().beat_to_samples(BeatPosition::new(beat));
        history.push(&mut memory, MoveClip::new(chord, pos))?;
    }
    history.seal();
    let marker = memory.allocate_marker_id();
    history.push(
        &mut memory,
        AddMarker::new(Marker::with_id(marker, "verse", bar)),
    )?;
    describe(&memory, &history);
    // undo_labels() is an iterator; collect::<Vec<_>>() materializes it for Debug.
    println!(
        "  history ({} steps): {:?}",
        history.undo_len(),
        history.undo_labels().collect::<Vec<_>>()
    );

    println!("undo x3:");
    for _ in 0..3 {
        history.undo(&mut memory)?;
        describe(&memory, &history);
    }
    println!("redo x3:");
    // redo returns Ok(true) while there is something to redo.
    while history.redo(&mut memory)? {
        describe(&memory, &history);
    }

    let path = "session_undo.aura.json";
    memory.save(path)?;
    // mark_clean remembers this revision as the last saved ("not dirty").
    history.mark_clean();
    let reloaded = Memory::load(path)?;
    // snapshot() compares memory state; metadata().map(...).unwrap_or(0) is soft I/O.
    println!(
        "saved {path} ({} bytes) and reloaded: identical = {}",
        std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        reloaded.snapshot() == memory.snapshot()
    );
    println!("clean after save: {}", history.is_clean());
    history.undo(&mut memory)?;
    println!("clean after one more undo: {}", history.is_clean());
    Ok(())
}
