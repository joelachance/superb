# session

Create, open and save a session; tempo, meter and loop range; undo.

## Example

```rust
use superb::primitives::session::TrackKind;
use superb::primitives::transport::LoopRange;
use superb::primitives::{BeatPosition, Result, SampleRate, TimePosition, TimeSignature};
use superb::session::Root;
use superb::{clip, marker, session, track};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let folder = Root::default().folder("My Show");            // ~/superb/My Show/
    let mut session = session::create(&folder, rate, 256)?;

    let vox = track::add(&mut session, "vox", TrackKind::Audio)?;
    let take = session::import_audio(&mut session, "/music/take.wav")?;   // audio/take.wav
    clip::add(&mut session, vox, take.key, TimePosition::ZERO)?;

    session::set_tempo(&mut session, 100.0)?;
    session::tempo_change(&mut session, BeatPosition::new(16.0), Some(120.0))?;   // faster from bar 5
    session::meter_change(&mut session, 9, Some(TimeSignature::new(6, 8)?))?;     // 6/8 from bar 9
    let range = LoopRange::new(TimePosition::ZERO, TimePosition::from_seconds(8.0, rate))?;
    session::set_loop(&mut session, Some(range))?;

    marker::add(&mut session, "verse", TimePosition::from_seconds(8.0, rate))?;
    session::undo(&mut session)?;                                // the marker is gone
    session::redo(&mut session)?;                                // and back

    let layout = session::layout(&session);
    session::state_file(&mut session, Some(layout.state()))?;   // keep mute, solo, arm, the playhead
    session::save(&mut session)?;

    let again = session::open(&folder, 256)?;
    println!("{}: {} tracks", again.memory.name(), again.memory.track_count());
    Ok(())
}
```

Run `cargo run --example new_session` for the same thing without a file on disk.

## Verbs

| verb | does | undo |
|---|---|---|
| `create(folder, rate, block_size) -> Session` | a new session folder, `memory.json` written at once | — |
| `open(folder, block_size) -> Session` | read it back, audio included | — |
| `save(session)` | write `memory.json` and any audio not on disk yet | — |
| `layout(session) -> Layout` | paths in the folder: `memory()`, `state()`, `audio()`, `bounce(name)`, `stem_set(name)` | — |
| `import_audio(session, file) -> Imported` | decode a file into `audio/<name>.wav`; `clip::add` takes `.key` | — |
| `state_file(session, Option<path>)` | where mute, solo, arm and the playhead are kept; unset, they are not saved | — |
| `set_tempo(session, bpm)` | the tempo at the start | drag |
| `tempo_change(session, beat, Option<bpm>)` | add or remove a tempo change | one |
| `meter_change(session, bar, Option<TimeSignature>)` | set or remove the meter from a bar | one |
| `set_loop(session, Option<LoopRange>)` | the loop range; `transport::looping` turns it on | drag |
| `undo(session) -> bool` / `redo(session) -> bool` | `false` when there is nothing to do | |

Every signature, argument, error and undo step: [session reference](session.ref.md).

## Steps

`session::steps` is the engine side. The verbs call these; so can you.

| step | does |
|---|---|
| `rebuild(session)` | build a fresh mixer from the memory and swap it into the engine |
| `render_blocks(session, blocks) -> PlanarBuffer` | tick the engine by hand with no sound card and return the audio |
| `build_mixer(&memory, &audio, channels) -> (Mixer, MixerHandles)` | the mixer alone: a strip per track, a node per clip, the live controls keyed by the memory's ids. Stored insert chains are not loaded |
| `publish(&mut EngineHandle, Mixer)` | prepare and hand a mixer to the engine; waits up to `SWAP_WAIT` (250 ms) while a device plays |

`cargo run --example session_steps` builds a mixer from a `Memory` with no `Session`.

## The folder

```text
~/superb/                 Root::default(); Root::new(path) for anywhere else
  My Show/              Root::folder("My Show"): what create and open take
    memory.json         the edits
    state.json          the state, once state_file names it
    audio/              what clips play: imports and recordings, 32-bit float WAV
    bounces/            export::bounce
    stems/<name>/       export::stems, with manifest.json
```

## Notes

* Beats are quarter notes whatever the meter: a 6/8 bar is three beats.
  Tempo ramps have no verb yet (`SetTempo::ramped` is the command).
* `import_audio` resamples to the session's rate on the way in. With the
  `files` feature it reads WAV, AIFF, FLAC, ALAC, MP3, AAC, Ogg Vorbis, CAF
  and Matroska; without it, a WAV at the session's rate.
* A missing audio file on `open` leaves its clip silent; nothing fails.
* Undoing a recording is the only thing that deletes a file.
* Warnings (a session opened without a state file, a state change that
  will not be saved) go through the `log` facade.
