# clip

Clips: audio or MIDI on a track. Add, move, split, trim, fade, mute, archive; notes in a MIDI clip.

## Example

```rust
use superb::primitives::buffers::PlanarBuffer;
use superb::primitives::session::{MidiNote, TrackKind};
use superb::primitives::{Result, SampleRate, TimeDuration, TimePosition};
use superb::{clip, session, track};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let mut session = session::create(std::env::temp_dir().join("clips"), rate, 256)?;
    let vox = track::add(&mut session, "vox", TrackKind::Audio)?;
    let keys = track::add(&mut session, "keys", TrackKind::Midi)?;

    // Audio: one second held in memory under a key. session::import_audio
    // does this from a file and returns the key.
    let mut audio = PlanarBuffer::new(2, rate.hz() as usize);
    audio.fill(0.2);
    session.audio.insert("audio/vox 1.wav", audio);
    let take = clip::add(&mut session, vox, "audio/vox 1.wav", TimePosition::ZERO)?;   // as long as the audio

    clip::move_to(&mut session, take, vox, TimePosition::from_seconds(0.5, rate))?;
    let tail = clip::split(&mut session, take, TimePosition::from_seconds(1.0, rate))?;   // take | tail
    clip::trim(&mut session, tail, TimePosition::from_seconds(1.1, rate), TimePosition::from_seconds(1.4, rate))?;
    clip::gain(&mut session, take, -3.0)?;
    clip::fade_in(&mut session, take, TimeDuration::from_seconds(0.05, rate))?;
    clip::fade_out(&mut session, tail, TimeDuration::from_seconds(0.2, rate))?;
    clip::mute(&mut session, tail, true)?;          // state, like a track's mute
    clip::rename(&mut session, take, "verse")?;
    clip::archive(&mut session, tail)?;             // silent, still in the session
    session::undo(&mut session)?;                   // back on the timeline

    // MIDI: notes in beats from the clip start; the clip ends with its last note.
    let c = MidiNote { beat: 0.0, length: 1.0, note: 60, velocity: 100, channel: 0 };
    let e = MidiNote { beat: 1.0, length: 1.0, note: 64, velocity: 100, channel: 0 };
    let chord = clip::add(&mut session, keys, vec![c, e], TimePosition::ZERO)?;
    clip::add_note(&mut session, chord, MidiNote { beat: 2.0, note: 67, ..c })?;
    clip::change_note(&mut session, chord, e, MidiNote { beat: 1.5, ..e })?;   // nudge it later
    clip::remove_note(&mut session, chord, c)?;

    println!("{} clips", session.memory.clip_count());
    Ok(())
}
```

## Verbs

| verb | does | undo |
|---|---|---|
| `add(session, track, content, at) -> ClipId` | `content`: an audio key (`&str`, `String`, path) or `Vec<MidiNote>` | one |
| `move_to(session, clip, track, at)` | to a position, on its own track or another | drag |
| `split(session, clip, at) -> ClipId` | the left keeps the id, the right comes back | one |
| `trim(session, clip, start, end)` | move the edges; what is underneath stays put | drag |
| `gain(session, clip, db)` | before the fader | drag |
| `fade_in` / `fade_out(session, clip, length)` | | drag |
| `mute(session, clip, on)` | | state |
| `rename(session, clip, name)` | | merges |
| `archive(session, clip)` / `restore(session, clip)` | off the timeline and back | one |
| `add_note(session, clip, MidiNote)` | a note past the end extends the clip | one |
| `remove_note(session, clip, MidiNote)` | the first note equal to it | one |
| `change_note(session, clip, from, to)` | replace a note | drag |

Every signature, argument, error and undo step: [clip reference](clip.ref.md).

## Notes

* Audio has to be in `session.audio` before a clip can play it:
  `session::import_audio` puts it there and returns the key; a recording
  does the same as it stops.
* A `MidiNote` has no id: it is its beat, length, pitch, velocity and
  channel. Two identical notes are one note twice.
* `ClipSource::Audio { path, offset }` starts a clip partway into its
  audio; pass it as `content`.
* Both halves of a split keep the name and share the audio.
