# Flows

The top tier of `superb`: one module per thing you work with, each a set of
free functions over a `Session`. Every page opens with a full example.

| flow | does | page |
|---|---|---|
| `session` | create, open, save; import audio; tempo, meter, loop range; undo | [session](flows/session.md) |
| `track` | add, fader, pan, mute, solo, arm, monitor, route, meter, archive | [track](flows/track.md) |
| `clip` | audio or MIDI on a track: add, move, split, trim, fade, mute; notes | [clip](flows/clip.md) |
| `marker` | named places on the timeline | [marker](flows/marker.md) |
| `transport` | start, stop, seek, loop; the playhead | [transport](flows/transport.md) |
| `send` | a copy of a track into a bus | [send](flows/send.md) |
| `midi` | play notes from code; connect a keyboard | [midi](flows/midi.md) |
| `record` | record onto armed tracks; punch points; recover after a crash | [record](flows/record.md) |
| `metronome` | the click; a count-in | [metronome](flows/metronome.md) |
| `export` | bounce to WAV; a stem set | [export](flows/export.md) |
| `devices` | speakers, inputs, MIDI ports | [devices](flows/devices.md) |

[quick-start.md](quick-start.md) maps tasks to functions. `flows/<flow>.ref.md` is
the generated reference for each page.

## Example

```rust
use superb::primitives::session::TrackKind;
use superb::primitives::{Result, SampleRate, TimePosition};
use superb::{clip, export, session, track, transport};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let mut session = session::create(std::env::temp_dir().join("demo"), rate, 256)?;

    let vox = track::add(&mut session, "vox", TrackKind::Audio)?;
    let take = session::import_audio(&mut session, "/music/take.wav")?;
    clip::add(&mut session, vox, take.key, TimePosition::ZERO)?;
    track::volume(&mut session, vox, -6.0)?;

    transport::start(&mut session)?;
    let heard = session::steps::render_blocks(&mut session, 8)?;   // no sound card needed
    println!("peak {:.3}", heard.peak());

    export::bounce(&mut session, "mix.wav", export::Bounce::default())?;
    session::save(&mut session)?;
    Ok(())
}
```

## How it works

```rust
pub struct Session {
    pub memory: Memory,              // tracks, clips, routing, tempo: what gets saved
    pub history: UndoStack<Memory>,  // every edit goes through here
    pub audio: AudioCache,           // the audio clips play, keyed by path
    pub folder: PathBuf,             // the session folder
    pub engine: EngineHandle,        // control side of the running engine
    pub mixer: MixerHandles,         // live strip and send controls from the last rebuild
    pub recording: Option<Recording>,
    // ...
}
```

* **Verbs take `&mut Session`.** A verb does one job by calling primitives
  in a fixed order; its rustdoc ends with **Made of:** naming them. When a
  verb is not what you want, call the primitives yourself.
* **Edits undo, state does not.** Adding, moving, levels are edits on
  `session.history`. Mute, solo, arm, monitor, input, looping, the click
  and the playhead are state: heard at the next block, never an undo step,
  saved only once `session::state_file` names a file.
* **Drags coalesce.** Moves of one control within half a second
  (`Session::COALESCE_WINDOW`) are one undo step. `session.history.seal()`
  ends the step now.
* **The mixer follows the memory.** Edits that change its shape (a track,
  a clip, a send, a route, monitoring) rebuild it: at once while a device
  plays, otherwise at the next `transport::start` or `render_blocks`.
  Levels and state reach the live controls without a rebuild.
* **Commands land at the next block.** `transport::state` right after
  `transport::start` still says `Stopped`.
* **No hardware needed.** `session::steps::render_blocks` stands in for
  the sound card; `export` renders offline; `record::start` takes a queue
  you fill. The `devices` feature adds real outputs and inputs.
