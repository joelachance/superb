# marker

Named places on the timeline.

## Example

```rust
use superb::primitives::{Result, SampleRate, TimePosition};
use superb::{marker, session};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let mut session = session::create(std::env::temp_dir().join("markers"), rate, 256)?;

    let verse = marker::add(&mut session, "verse", TimePosition::from_seconds(8.0, rate))?;
    let chorus = marker::add(&mut session, "chorus", TimePosition::from_seconds(24.0, rate))?;
    marker::move_to(&mut session, verse, TimePosition::from_seconds(8.5, rate))?;
    marker::rename(&mut session, verse, "verse 1")?;

    for m in session.memory.markers() {                       // in position order
        println!("{} at {:.1} s", m.name, m.position().to_seconds(rate));
    }
    if let Some(next) = session.memory.next_marker(TimePosition::from_seconds(10.0, rate)) {
        println!("next: {}", next.name);                      // chorus
    }

    marker::remove(&mut session, chorus)?;
    session::undo(&mut session)?;                             // chorus is back
    Ok(())
}
```

## Verbs

| verb | does | undo |
|---|---|---|
| `add(session, name, at) -> MarkerId` | | one |
| `move_to(session, marker, at)` | | drag |
| `rename(session, marker, name)` | | merges |
| `remove(session, marker)` | | one |

Every signature, argument, error and undo step: [marker reference](marker.ref.md).

## Notes

* `session.memory.next_marker(at)` and `previous_marker(at)` are what a
  "jump to marker" button is made of.
* Markers have a colour the memory keeps; no verb sets it yet.
