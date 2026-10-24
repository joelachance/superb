# metronome

A click on every beat while the transport rolls, and the count-in before a
recording.

## Purpose

The click is a node on the master bus, built with the mixer and driven by
the memory's metronome state: whether it is on and how loud, both reaching
the live node at once, and how many bars of count-in a recording that
starts at the playhead gets. It ticks on the notated beat of the meter in
force (quarters in 4/4, eighths in 6/8) and accents the first beat of
every bar, following tempo and meter changes along the timeline.
During a count-in the click sounds even when the metronome is off, and
stops at the punch-in.

## Verbs

| verb | does | undo |
|---|---|---|
| `enable(session, on)` | the click while rolling; state | — |
| `level(session, db)` | click level; heard at once | drag (coalesces) |
| `count_in(session, bars)` | bars the transport rolls early, clicking, before a recording that starts at the playhead; `0` for none; state | — |

## Reference

Every function of the `metronome` flow: what it takes, what it returns, what it refuses, and how it undoes. A `Result<T>` is `Ok(T)` or a `SuperbError`; the errors listed are the ones you can cause.

### `enable`

```rust
pub fn enable(session: &mut Session, on: bool) -> Result<()>
```

Turns the click on or off. State, like mute: saved with the session, heard
at the next block, not an undo step.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `on: bool` — `true` to switch on, `false` to switch off

**Returns** `Result<()>` — nothing

**Errors** never.

**Undo** none: state, not an edit (saved through the state file, see `session::state_file`); heard at the next block.

**Made of** `MetronomeState::enabled` through `session::set_state` → `ClickParams::set_enabled`.

**Example**

```rust
metronome::enable(&mut session, true)?;
```

### `level`

```rust
pub fn level(session: &mut Session, db: f32) -> Result<()>
```

Sets the click level in dB. Moves within `Session::COALESCE_WINDOW` are one
undo step; heard at once.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `db: f32` — a level in decibels; `0.0` is unity, negative is quieter

**Returns** `Result<()>` — nothing

**Errors** never.

**Undo** one undo step per drag: calls within `Session::COALESCE_WINDOW` (half a second) merge; heard at once.

**Made of** `set_metronome_level_db` through `session::edit` → `ClickParams::set_level`.

**Example**

```rust
metronome::level(&mut session, -12.0)?;
```

### `count_in`

```rust
pub fn count_in(session: &mut Session, bars: u32) -> Result<()>
```

Sets how many bars of count-in a recording gets when it starts at the
playhead: `record::start` rolls the transport that many bars early with the
click sounding, and drops into record at the playhead. `0` for none. State,
saved with the session.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `bars: u32` — bars of click before a recording that starts at the playhead; `0` for none

**Returns** `Result<()>` — nothing

**Errors** never.

**Undo** none: state, not an edit (saved through the state file, see `session::state_file`).

**Made of** `MetronomeState::count_in` through `session::set_state`.

**Example**

```rust
metronome::count_in(&mut session, 2)?;   // two bars of click before a recording at the playhead
```

## Usage

```rust
use superb::{metronome, record, session, transport};

metronome::enable(&mut session, true)?;
metronome::level(&mut session, -6.0)?;
metronome::count_in(&mut session, 1)?;
transport::seek(&mut session, TimePosition::from_seconds(2.0, rate))?;
record::start(&mut session, record::Source::Device(None))?;   // rolls from 0 s, records from 2 s
```

## Made of

`enable`: `MetronomeState::enabled` through `session::set_state` →
`ClickParams::set_enabled`. `level`: `set_metronome_level_db` through
`session::edit` → `ClickParams::set_level`. `count_in`:
`MetronomeState::count_in` through `session::set_state`; `record::start`
reads it, seeks the transport back by that many bars of the meter at the
punch-in and sets `ClickParams::set_count_in_until(punch_in)`, which
`record::stop` clears. The node is `graph::nodes::ClickNode`, placed on the
master bus by `session::steps::build_mixer`, with `session.mixer.click` as
its controls.

## Notes

* The click follows the block's beat position, tempo and meter, so it
  stays on the beat through tempo and meter changes; a tempo ramp inside
  one block uses the tempo at the block's start.
* The count-in applies when a recording begins at the playhead. With a
  punch-in point ahead of the playhead the transport rolls from where it
  is instead, and the click sounds only if the metronome is on.
* A count-in that would start before zero is shortened to start at zero.
