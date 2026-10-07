//! Clips: put audio or MIDI on a track, move, trim and split it, shape it,
//! archive it.
//!
//! A clip is a region placed on a track. It plays audio the session holds
//! in memory, named by the key it was stored under, or MIDI notes. [`add`]
//! works out which from what you hand it and how long the clip is: the
//! length of the audio, or the end of the last note. [`move_to`], [`trim`]
//! and [`split`] place its edges, [`gain`], [`fade_in`] and [`fade_out`]
//! shape it, [`mute`] silences it as state. Nothing here deletes:
//! [`archive`] takes a clip off the timeline (it stops playing and no
//! longer counts toward the session's end) and [`restore`] brings it back,
//! each one undo step. Every change here alters what the mixer plays, so it
//! rebuilds the mixer.
//!
//! Made of `superb::primitives::session` (`Clip`, `ClipSource` and the
//! clip commands) and the audio held on the `Session`.

use crate::primitives::session::commands::{
    self, AddClip, AddNote, ArchiveClip, ChangeNote, MoveClip, RemoveNote, SplitClip, TrimClip,
};
use crate::primitives::session::{Clip, ClipSource, MidiNote};
use crate::primitives::{
    BeatPosition, ClipId, Result, SuperbError, TimeDuration, TimePosition, TrackId,
};
use crate::session::Session;

/// Places a clip on `track` starting at `at`. `content` is the name audio
/// was stored under in [`Session::audio`] (`&str`, `String` or a path) or
/// the notes of a MIDI clip (`Vec<MidiNote>`, beats relative to `at`). An
/// audio clip is as long as its audio and named after the key's file stem;
/// a MIDI clip ends with its last note and is named after the track.
///
/// Fails when the audio is not in memory, when there are no notes, or when
/// the track is a bus.
///
/// Made of: `AudioCache::get` or `TempoMap::beat_to_samples` for the
/// length → `Memory::allocate_clip_id` → `Clip::with_id` → `AddClip`
/// through `session::structural`.
pub fn add(
    session: &mut Session,
    track: TrackId,
    content: impl Into<ClipSource>,
    at: TimePosition,
) -> Result<ClipId> {
    let source = content.into();
    let (name, length) = match &source {
        ClipSource::Audio { path, offset } => {
            let audio = session.audio.get(path).ok_or_else(|| {
                SuperbError::Message(format!("no audio named {:?} is in memory", path.display()))
            })?;
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "clip".into());
            let frames = audio.frames() as i64 - offset.samples();
            (name, TimeDuration::from_samples(frames))
        }
        ClipSource::Midi { notes } => {
            let tempo = session.memory.tempo_map();
            let start_beat = tempo.samples_to_beat(at).beats();
            let last = notes
                .iter()
                .map(|n| n.beat + n.length)
                .fold(0.0_f64, f64::max);
            let end = tempo.beat_to_samples(BeatPosition::new(start_beat + last));
            let name = session.memory.track(track)?.name.clone();
            (
                name,
                TimeDuration::from_samples(end.samples() - at.samples()),
            )
        }
        ClipSource::Empty => {
            return Err(SuperbError::InvalidArgument {
                what: "an empty clip has no length to infer",
            })
        }
    };
    if length.samples() <= 0 {
        return Err(SuperbError::InvalidArgument {
            what: "a clip needs audio or notes to have a length",
        });
    }
    let id = session.memory.allocate_clip_id();
    let clip = Clip::with_id(id, name, at, length, source)?;
    crate::session::structural(session, AddClip::new(track, clip))?;
    Ok(id)
}

/// Moves a clip to `track`, starting at `at`. Pass the clip's own track to
/// move it in time only. Moves of the same clip within
/// [`Session::COALESCE_WINDOW`] of each other are one undo step, so a drag
/// undoes in one go. Buses take no clips.
///
/// Made of: `MoveClip::to_track` through `session::structural`.
pub fn move_to(
    session: &mut Session,
    clip: ClipId,
    track: TrackId,
    at: TimePosition,
) -> Result<()> {
    crate::session::structural(session, MoveClip::new(clip, at).to_track(track))
}

/// Cuts a clip in two at `at`, which must fall inside it. The left part
/// keeps the clip's id; the right part is returned. What is underneath
/// stays put, fades stay at the outer edges, gain, mute and archived carry
/// over, and both halves keep the name. One undo step.
///
/// Made of: `Memory::allocate_clip_id` → `SplitClip` through
/// `session::structural`.
pub fn split(session: &mut Session, clip: ClipId, at: TimePosition) -> Result<ClipId> {
    let right = session.memory.allocate_clip_id();
    crate::session::structural(session, SplitClip::new(clip, at, right))?;
    Ok(right)
}

/// Moves the clip's edges to `start` and `end` on the timeline. What is
/// underneath stays where it was: trimming the head shows later audio and
/// MIDI notes keep their place. Edge moves of the same clip within
/// [`Session::COALESCE_WINDOW`] are one undo step. Fails when `end` is not
/// after `start`.
///
/// Made of: `TrimClip` through `session::structural`.
pub fn trim(
    session: &mut Session,
    clip: ClipId,
    start: TimePosition,
    end: TimePosition,
) -> Result<()> {
    crate::session::structural(session, TrimClip::new(clip, start, end))
}

/// Sets the clip's own gain in dB, before the track's fader. Moves within
/// [`Session::COALESCE_WINDOW`] are one undo step; heard at the next
/// rebuild (at once while a device plays).
///
/// Made of: `set_clip_gain_db` through `session::structural`.
pub fn gain(session: &mut Session, clip: ClipId, db: f32) -> Result<()> {
    crate::session::structural(session, commands::set_clip_gain_db(clip, db))
}

/// Mutes or unmutes a clip. State, like a track's mute: saved with the
/// session, not an undo step; the clip stays on the timeline and counts
/// toward the session's end. Rebuilds the mixer.
///
/// Made of: `ClipState::mute` through `session::set_state` → rebuild.
pub fn mute(session: &mut Session, clip: ClipId, on: bool) -> Result<()> {
    crate::session::set_state(session, |m| {
        m.clip_mut(clip)?.state.mute = on;
        Ok(())
    })?;
    crate::session::changed_structure(session)
}

/// Sets the fade-in length from the clip's start. Coalesces like a drag.
///
/// Made of: `set_clip_fade_in` through `session::structural`.
pub fn fade_in(session: &mut Session, clip: ClipId, length: TimeDuration) -> Result<()> {
    crate::session::structural(session, commands::set_clip_fade_in(clip, length))
}

/// Sets the fade-out length to the clip's end. Coalesces like a drag.
///
/// Made of: `set_clip_fade_out` through `session::structural`.
pub fn fade_out(session: &mut Session, clip: ClipId, length: TimeDuration) -> Result<()> {
    crate::session::structural(session, commands::set_clip_fade_out(clip, length))
}

/// Takes a clip off the timeline without deleting it: it stops playing and
/// no longer counts toward the session's end, but keeps its place and its
/// audio. [`restore`] brings it back.
///
/// Made of: `ArchiveClip` through `session::structural`.
pub fn archive(session: &mut Session, clip: ClipId) -> Result<()> {
    crate::session::structural(session, ArchiveClip::new(clip, true))
}

/// Puts an archived clip back on the timeline.
///
/// Made of: `ArchiveClip` (restoring) through `session::structural`.
pub fn restore(session: &mut Session, clip: ClipId) -> Result<()> {
    crate::session::structural(session, ArchiveClip::new(clip, false))
}

/// Adds a note to a MIDI clip. `note.beat` and `note.length` are beats
/// from the clip start; a note that ends past the clip end extends the
/// clip. Notes are their content: two identical notes are one note twice,
/// and [`remove_note`] or [`change_note`] takes the first. One undo step;
/// how notes are picked, drawn or quantised is the caller's business.
///
/// Made of: `AddNote` through `session::structural`.
pub fn add_note(session: &mut Session, clip: ClipId, note: MidiNote) -> Result<()> {
    crate::session::structural(session, AddNote::new(clip, note))
}

/// Removes a note from a MIDI clip; undo puts it back.
///
/// Made of: `RemoveNote` through `session::structural`.
pub fn remove_note(session: &mut Session, clip: ClipId, note: MidiNote) -> Result<()> {
    crate::session::structural(session, RemoveNote::new(clip, note))
}

/// Replaces a note of a MIDI clip with another: moved, lengthened, louder,
/// on another channel, whatever differs. Changes to the same note within
/// [`Session::COALESCE_WINDOW`] are one undo step, so a drag that calls
/// this as it goes undoes in one. A note that ends past the clip end
/// extends the clip.
///
/// Made of: `ChangeNote` through `session::structural`.
pub fn change_note(
    session: &mut Session,
    clip: ClipId,
    from: MidiNote,
    to: MidiNote,
) -> Result<()> {
    crate::session::structural(session, ChangeNote::new(clip, from, to))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::buffers::PlanarBuffer;
    use crate::primitives::session::{Memory, MidiNote, TrackKind};
    use crate::primitives::{SampleRate, TimeDuration};
    use crate::{session, track, transport};

    const R: SampleRate = SampleRate::HZ_48K;

    fn note(beat: f64, length: f64) -> MidiNote {
        MidiNote {
            beat,
            length,
            note: 60,
            velocity: 100,
            channel: 0,
        }
    }

    #[test]
    fn audio_clips_take_their_length_and_name_from_the_audio() {
        let mut session = session::create(session::scratch("clips"), R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        session
            .audio
            .insert("takes/take 1.wav", PlanarBuffer::new(2, 480));
        let clip = add(
            &mut session,
            vox,
            "takes/take 1.wav",
            TimePosition::from_samples(100),
        )
        .unwrap();
        let c = session.memory.clip(clip).unwrap();
        assert_eq!(c.name, "take 1");
        assert_eq!(c.start().samples(), 100);
        assert_eq!(c.length().samples(), 480);
        assert_eq!(session.memory.clip_track(clip).unwrap(), vox);
        assert!(session.needs_rebuild);
        assert!(
            add(&mut session, vox, "missing.wav", TimePosition::ZERO).is_err(),
            "audio must be in memory"
        );
        let bus = track::add(&mut session, "bus", TrackKind::Bus).unwrap();
        let before = session.history.undo_len();
        assert!(add(&mut session, bus, "takes/take 1.wav", TimePosition::ZERO).is_err());
        assert_eq!(before, session.history.undo_len(), "nothing recorded");
    }

    #[test]
    fn midi_clips_end_with_their_last_note() {
        let mut session = session::create(session::scratch("clips"), R, 64).unwrap();
        let keys = track::add(&mut session, "keys", TrackKind::Midi).unwrap();
        // 120 bpm: a beat is 24 000 samples. Notes to beat 2.5 → 60 000.
        let clip = add(
            &mut session,
            keys,
            vec![note(0.0, 1.0), note(1.5, 1.0)],
            TimePosition::from_samples(48_000),
        )
        .unwrap();
        let c = session.memory.clip(clip).unwrap();
        assert_eq!(c.name, "keys");
        assert_eq!(c.length().samples(), 60_000);
        assert!(matches!(c.source, ClipSource::Midi { .. }));
        assert!(
            add(
                &mut session,
                keys,
                Vec::<MidiNote>::new(),
                TimePosition::ZERO
            )
            .is_err(),
            "no notes, no length"
        );
        assert!(add(&mut session, keys, ClipSource::Empty, TimePosition::ZERO).is_err());
    }

    #[test]
    fn a_drag_is_one_step_and_archiving_silences_without_deleting() {
        let mut session = session::create(session::scratch("clips"), R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        let mut tone = PlanarBuffer::new(2, 400);
        tone.fill(0.5);
        session.audio.insert("tone.wav", tone);
        let clip = add(&mut session, vox, "tone.wav", TimePosition::ZERO).unwrap();
        let steps = session.history.undo_len();
        move_to(&mut session, clip, vox, TimePosition::from_samples(100)).unwrap();
        move_to(&mut session, clip, vox, TimePosition::from_samples(200)).unwrap();
        assert_eq!(session.history.undo_len(), steps + 1, "a drag coalesces");
        assert_eq!(session.memory.clip(clip).unwrap().start().samples(), 200);
        assert_eq!(session.memory.end_position().samples(), 600);
        session.history.seal();

        transport::start(&mut session).unwrap();
        assert!(
            session::steps::render_blocks(&mut session, 6)
                .unwrap()
                .peak()
                > 0.3
        );

        archive(&mut session, clip).unwrap();
        assert!(session.memory.clip(clip).unwrap().state.archived);
        assert_eq!(session.memory.clip_count(), 1, "still there");
        assert_eq!(
            session.memory.end_position().samples(),
            0,
            "an archived clip does not extend the session"
        );
        transport::rewind(&mut session).unwrap();
        assert!(session::steps::render_blocks(&mut session, 6)
            .unwrap()
            .is_silent());
        assert_eq!(session.history.undo_len(), steps + 2, "one step");

        // Undo restores it; so does `restore`.
        assert!(session::undo(&mut session).unwrap());
        assert!(!session.memory.clip(clip).unwrap().state.archived);
        archive(&mut session, clip).unwrap();
        archive(&mut session, clip).unwrap(); // already archived: no new step
        assert_eq!(session.history.undo_len(), steps + 3);
        restore(&mut session, clip).unwrap();
        assert!(!session.memory.clip(clip).unwrap().state.archived);
        transport::rewind(&mut session).unwrap();
        assert!(
            session::steps::render_blocks(&mut session, 6)
                .unwrap()
                .peak()
                > 0.3
        );

        // Archived survives a save.
        archive(&mut session, clip).unwrap();
        let json = session.memory.to_json().unwrap();
        let copy = Memory::from_json(&json).unwrap();
        assert!(copy.clip(clip).unwrap().state.archived);
    }

    #[test]
    fn trims_keep_the_audio_in_place_and_fades_gain_and_mute_shape_the_clip() {
        let mut session = session::create(session::scratch("clips"), R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        let mut tone = PlanarBuffer::new(2, 400);
        tone.fill(0.5);
        session.audio.insert("tone.wav", tone);
        let clip = add(
            &mut session,
            vox,
            "tone.wav",
            TimePosition::from_samples(100),
        )
        .unwrap();

        // Head trim by 50: starts later, is shorter, and plays from 50 into the audio.
        trim(
            &mut session,
            clip,
            TimePosition::from_samples(150),
            TimePosition::from_samples(500),
        )
        .unwrap();
        let c = session.memory.clip(clip).unwrap();
        assert_eq!(c.start().samples(), 150);
        assert_eq!(c.length().samples(), 350);
        assert!(matches!(
            &c.source,
            ClipSource::Audio { offset, .. } if offset.samples() == 50
        ));
        // Tail trim coalesces with the head trim: one step so far.
        let steps = session.history.undo_len();
        trim(
            &mut session,
            clip,
            TimePosition::from_samples(150),
            TimePosition::from_samples(300),
        )
        .unwrap();
        assert_eq!(session.history.undo_len(), steps);
        assert_eq!(session.memory.clip(clip).unwrap().length().samples(), 150);
        assert!(trim(
            &mut session,
            clip,
            TimePosition::from_samples(300),
            TimePosition::from_samples(300)
        )
        .is_err());
        session.history.seal();
        assert!(session::undo(&mut session).unwrap());
        let c = session.memory.clip(clip).unwrap();
        assert_eq!((c.start().samples(), c.length().samples()), (100, 400));

        // Gain and fades are edits heard at the rebuild; mute is state.
        gain(&mut session, clip, -6.0206).unwrap();
        fade_in(&mut session, clip, TimeDuration::from_samples(200)).unwrap();
        fade_out(&mut session, clip, TimeDuration::from_samples(100)).unwrap();
        let c = &session.memory.clip(clip).unwrap().state;
        assert!((c.gain_db + 6.0206).abs() < 1e-6);
        assert_eq!(c.fade_in.samples(), 200);
        assert_eq!(c.fade_out.samples(), 100);
        transport::start(&mut session).unwrap();
        let out = session::steps::render_blocks(&mut session, 8).unwrap();
        let early = out.channel(0)[110].abs();
        let late = out.channel(0)[350].abs();
        assert!(early < late * 0.2, "fading in: {early} vs {late}");
        assert!(late > 0.1 && late < 0.25, "half the level: {late}");
        let steps = session.history.undo_len();
        mute(&mut session, clip, true).unwrap();
        assert_eq!(session.history.undo_len(), steps, "state, not an edit");
        assert!(!session.history.is_clean());
        transport::rewind(&mut session).unwrap();
        assert!(session::steps::render_blocks(&mut session, 8)
            .unwrap()
            .is_silent());
        assert_eq!(
            session.memory.end_position().samples(),
            500,
            "muted still counts"
        );
        mute(&mut session, clip, false).unwrap();

        // Trimming a MIDI clip's head keeps the notes on the timeline.
        let keys = track::add(&mut session, "keys", TrackKind::Midi).unwrap();
        let midi = add(
            &mut session,
            keys,
            vec![note(0.0, 1.0), note(2.0, 1.0)],
            TimePosition::ZERO,
        )
        .unwrap();
        // A beat is 24 000 samples at 120 bpm: trim one beat off the head.
        trim(
            &mut session,
            midi,
            TimePosition::from_samples(24_000),
            TimePosition::from_samples(72_000),
        )
        .unwrap();
        let c = session.memory.clip(midi).unwrap();
        let ClipSource::Midi { notes } = &c.source else {
            panic!("midi")
        };
        assert!((notes[0].beat + 1.0).abs() < 1e-9 && (notes[1].beat - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_split_leaves_two_clips_in_place_and_a_move_can_change_track() {
        let mut session = session::create(session::scratch("split"), R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        let alt = track::add(&mut session, "alt", TrackKind::Audio).unwrap();
        let bus = track::add(&mut session, "bus", TrackKind::Bus).unwrap();
        let mut tone = PlanarBuffer::new(2, 400);
        tone.fill(0.5);
        session.audio.insert("tone.wav", tone);
        let left = add(
            &mut session,
            vox,
            "tone.wav",
            TimePosition::from_samples(100),
        )
        .unwrap();
        fade_in(&mut session, left, TimeDuration::from_samples(10)).unwrap();
        fade_out(&mut session, left, TimeDuration::from_samples(20)).unwrap();
        gain(&mut session, left, -3.0).unwrap();
        session.history.seal();

        assert!(split(&mut session, left, TimePosition::from_samples(100)).is_err());
        assert!(split(&mut session, left, TimePosition::from_samples(500)).is_err());
        let steps = session.history.undo_len();
        let right = split(&mut session, left, TimePosition::from_samples(250)).unwrap();
        assert_eq!(session.history.undo_len(), steps + 1);
        let l = session.memory.clip(left).unwrap();
        let r = session.memory.clip(right).unwrap();
        assert_eq!((l.start().samples(), l.length().samples()), (100, 150));
        assert_eq!((r.start().samples(), r.length().samples()), (250, 250));
        assert_eq!(r.name, "tone");
        assert!(matches!(&r.source, ClipSource::Audio { offset, .. } if offset.samples() == 150));
        assert_eq!(l.state.fade_in.samples(), 10);
        assert_eq!(l.state.fade_out.samples(), 0);
        assert_eq!(r.state.fade_in.samples(), 0);
        assert_eq!(r.state.fade_out.samples(), 20);
        assert_eq!(r.state.gain_db, -3.0);
        assert_eq!(session.memory.clip_track(right).unwrap(), vox);
        // The two halves play as one: the sound is continuous across the cut.
        transport::start(&mut session).unwrap();
        let out = session::steps::render_blocks(&mut session, 8).unwrap();
        assert!((out.channel(0)[245].abs() - out.channel(0)[255].abs()).abs() < 1e-3);

        // Undo puts the one clip back; redo splits again.
        assert!(session::undo(&mut session).unwrap());
        assert!(session.memory.clip(right).is_err());
        let l = session.memory.clip(left).unwrap();
        assert_eq!(l.length().samples(), 400);
        assert_eq!(l.state.fade_out.samples(), 20);
        assert!(session::redo(&mut session).unwrap());
        assert!(session.memory.clip(right).is_ok());

        // Move the right half to another track; buses take no clips.
        move_to(&mut session, right, alt, TimePosition::from_samples(0)).unwrap();
        assert_eq!(session.memory.clip_track(right).unwrap(), alt);
        assert_eq!(session.memory.clip(right).unwrap().start().samples(), 0);
        assert!(move_to(&mut session, right, bus, TimePosition::ZERO).is_err());

        // A MIDI split partitions the notes.
        let keys = track::add(&mut session, "keys", TrackKind::Midi).unwrap();
        let midi = add(
            &mut session,
            keys,
            vec![note(0.0, 1.0), note(2.0, 1.0)],
            TimePosition::ZERO,
        )
        .unwrap();
        // One beat is 24 000 samples at 120 bpm: split after the first beat.
        let right = split(&mut session, midi, TimePosition::from_samples(24_000)).unwrap();
        let ClipSource::Midi { notes: l } = &session.memory.clip(midi).unwrap().source else {
            panic!()
        };
        let ClipSource::Midi { notes: r } = &session.memory.clip(right).unwrap().source else {
            panic!()
        };
        assert_eq!(
            l.len(),
            2,
            "the left keeps its notes; the scheduler cuts them at its end"
        );
        assert_eq!(r.len(), 1);
        assert!((r[0].beat - 1.0).abs() < 1e-9);
    }

    #[test]
    fn notes_are_added_changed_and_removed_with_undo() {
        let mut session = session::create(session::scratch("notes"), R, 64).unwrap();
        let keys = track::add(&mut session, "keys", TrackKind::Midi).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        let clip = add(&mut session, keys, vec![note(0.0, 1.0)], TimePosition::ZERO).unwrap();
        let one_beat = session.memory.clip(clip).unwrap().length();
        let steps = session.history.undo_len();

        // Adding past the end grows the clip; undo shrinks it back.
        let late = MidiNote {
            note: 64,
            ..note(2.0, 1.0)
        };
        add_note(&mut session, clip, late).unwrap();
        let notes = |s: &Session| match &s.memory.clip(clip).unwrap().source {
            ClipSource::Midi { notes } => notes.clone(),
            _ => unreachable!(),
        };
        assert_eq!(notes(&session).len(), 2);
        assert_eq!(
            session.memory.clip(clip).unwrap().length(),
            TimeDuration::from_samples(one_beat.samples() * 3)
        );
        assert_eq!(session.history.undo_len(), steps + 1);
        assert!(session::undo(&mut session).unwrap());
        assert_eq!(notes(&session).len(), 1);
        assert_eq!(session.memory.clip(clip).unwrap().length(), one_beat);
        assert!(session::redo(&mut session).unwrap());

        // A drag: several changes to the same note are one undo step.
        let mut moved = late;
        for beat in [2.5, 3.0, 3.5] {
            let next = MidiNote { beat, ..moved };
            change_note(&mut session, clip, moved, next).unwrap();
            moved = next;
        }
        assert_eq!(session.history.undo_len(), steps + 2);
        assert!(notes(&session).contains(&moved));
        // Another note changed within the window joins the step; one undo
        // takes the whole drag and it back together.
        let first = note(0.0, 1.0);
        let louder = MidiNote {
            velocity: 120,
            ..first
        };
        change_note(&mut session, clip, first, louder).unwrap();
        assert_eq!(session.history.undo_len(), steps + 2);
        assert!(session::undo(&mut session).unwrap());
        assert!(notes(&session).contains(&first));
        assert!(
            notes(&session).contains(&late),
            "the whole drag undone at once"
        );

        remove_note(&mut session, clip, late).unwrap();
        assert_eq!(notes(&session).len(), 1);
        assert!(session::undo(&mut session).unwrap());
        assert_eq!(notes(&session).len(), 2);

        // Rules: a MIDI clip only, a real note, a note that exists.
        let audio = {
            let mut tone = PlanarBuffer::new(1, 4_800);
            tone.fill(0.1);
            session.audio.insert("tone.wav", tone);
            add(&mut session, vox, "tone.wav", TimePosition::ZERO).unwrap()
        };
        assert!(add_note(&mut session, audio, note(0.0, 1.0)).is_err());
        assert!(add_note(&mut session, clip, note(-1.0, 1.0)).is_err());
        assert!(add_note(&mut session, clip, note(0.0, 0.0)).is_err());
        assert!(add_note(
            &mut session,
            clip,
            MidiNote {
                velocity: 0,
                ..note(0.0, 1.0)
            }
        )
        .is_err());
        assert!(remove_note(&mut session, clip, note(7.0, 1.0)).is_err());
        assert!(change_note(&mut session, clip, note(7.0, 1.0), note(8.0, 1.0)).is_err());

        // The schedule follows: the new note sounds when played.
        transport::seek(&mut session, TimePosition::ZERO).unwrap();
        transport::start(&mut session).unwrap();
        let heard = session::steps::render_blocks(&mut session, 4).unwrap();
        assert!(heard.frames() > 0);
    }
}
