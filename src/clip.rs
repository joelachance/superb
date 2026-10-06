//! Clips: put audio or MIDI on a track, move and trim it, shape it, archive it.
//!
//! A clip is a region placed on a track. It plays audio the session holds
//! in memory, named by the key it was stored under, or MIDI notes. [`add`]
//! works out which from what you hand it and how long the clip is: the
//! length of the audio, or the end of the last note. [`move_to`] and
//! [`trim`] place its edges, [`gain`], [`fade_in`] and [`fade_out`] shape
//! it, [`mute`] silences it as state. Nothing here deletes: [`archive`]
//! takes a clip off the timeline (it stops playing and no longer counts
//! toward the session's end) and [`restore`] brings it back, each one undo
//! step. Every change here alters what the mixer plays, so it rebuilds the
//! mixer.
//!
//! Made of `superb::primitives::session` (`Clip`, `ClipSource` and the
//! clip commands) and the audio held on the `Session`.

use crate::primitives::session::commands::{self, AddClip, ArchiveClip, MoveClip, TrimClip};
use crate::primitives::session::{Clip, ClipSource};
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

/// Moves a clip to start at `at`. Moves of the same clip within
/// [`Session::COALESCE_WINDOW`] of each other are one undo step, so a drag
/// undoes in one go.
///
/// Made of: `MoveClip` through `session::structural`.
pub fn move_to(session: &mut Session, clip: ClipId, at: TimePosition) -> Result<()> {
    crate::session::structural(session, MoveClip::new(clip, at))
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
        move_to(&mut session, clip, TimePosition::from_samples(100)).unwrap();
        move_to(&mut session, clip, TimePosition::from_samples(200)).unwrap();
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
}
