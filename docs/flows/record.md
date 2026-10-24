# record

Start a recording, stop it, get a clip on every armed track.

## Example

From a real input, with the `devices` feature:

```rust
use superb::primitives::session::{Input, TrackKind};
use superb::primitives::{Result, SampleRate, TimePosition};
use superb::{devices, record, session, track, transport};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let mut session = session::create(std::env::temp_dir().join("take"), rate, 256)?;
    let vox = track::add(&mut session, "vox", TrackKind::Audio)?;
    track::arm(&mut session, vox, true)?;
    track::input(&mut session, vox, Some(Input::Mono(0)))?;   // the first input channel, mono
    track::monitor(&mut session, vox, true)?;                  // hear yourself

    devices::output(&mut session, None)?;
    record::punch_in(&mut session, Some(TimePosition::from_seconds(4.0, rate)))?;
    record::start(&mut session, record::Source::Device(None))?;   // rolls now, records from 4 s
    std::thread::sleep(std::time::Duration::from_secs(10));
    if let Some(r) = &session.recording {
        println!("{} frames, {} dropped, {} disk errors", r.frames(), r.dropped(), r.disk_errors());
    }

    let clips = record::stop(&mut session)?;              // one clip per armed track, one undo step
    let clip = session.memory.clip(clips[0])?;
    println!("{} at {:.1} s, {:.1} s long", clip.name,
        clip.start().to_seconds(rate), clip.length().to_seconds(rate));
    Ok(())
}
```

Offline, from a queue you fill yourself:

```rust
use superb::primitives::io::capture::capture_queue;

track::arm(&mut session, vox, true)?;
transport::start(&mut session)?;
let (mut mic, blocks) = capture_queue(1, 256, 64)?;
record::start(&mut session, record::Source::Queue { blocks, channels: 1 })?;
for (n, block) in my_blocks.iter().enumerate() {
    mic.push(block, (n * 256) as u64 * 1_000_000 / rate.hz() as u64);
    session::steps::render_blocks(&mut session, 1)?;     // renders and collects
}
let clips = record::stop(&mut session)?;
```

`cargo run --example record_take` runs the offline version end to end.

## Verbs

| verb | does |
|---|---|
| `start(session, Source)` | record onto every armed track from the playhead, or from `punch_in` when it lies ahead |
| `stop(session) -> Vec<ClipId>` | stop, finish the files, place the clips; one undo step |
| `punch_in` / `punch_out(session, Option<at>)` | where the next recording drops in and out; edits |
| `collect(session) -> frames` | drain the queue into the files; `render_blocks` calls it, a device recording has its own thread |
| `unfinished(session) -> Vec<PathBuf>` | files a crash left behind |
| `recover(session) -> Vec<ClipId>` | finish them and place their clips |

| `Source` | records from |
|---|---|
| `Source::Device(Option<name>)` | the audio input (`devices::input`, opened if needed); feature `devices` |
| `Source::Queue { blocks, channels }` | a capture queue you push into (`capture_queue` gives both ends) |

`session.recording` holds the `Recording` while it runs: `frames()`,
`sealed_frames()`, `dropped()`, `disk_errors()`, `peaks()`, `files()`,
`started_at()`, `punch_in()`, `punch_out()`.

Every signature, argument, error and undo step: [record reference](record.ref.md).

## Notes

* Each armed audio track gets its own 32-bit float WAV in `audio/`,
  written block by block from the first moment, so a crash costs about a
  second. Armed MIDI tracks get a clip of the notes that arrived.
* With `metronome::count_in` set, a recording that starts at the playhead
  rolls that many bars early with the click.
* A device take is lined up with the transport by the device clock, so
  the buffers in and out never shift it.
* Undoing a recording removes its clips and deletes its files; redo writes
  them again. `recover` deletes nothing.
* Loop recording is not there yet.
