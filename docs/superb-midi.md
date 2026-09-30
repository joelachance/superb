# superb-midi

MIDI 1.0 messages and real-time-safe event plumbing for the Superb DAW SDK.

## Purpose

* [`MidiMessage`] — a decoded channel-voice or system message (`NoteOn`,
  `ControlChange`, `PitchBend`, transport bytes, ...). `Copy`, heap-free,
  with `to_bytes`/`from_bytes` for wire encoding.
* [`MidiEvent`] / [`MidiEventBuffer`] — a message stamped with a sample
  offset (`frame`) within the current process block, stored in a
  fixed-capacity buffer allocated once so pushing on the audio thread never
  allocates.
* [`TimestampedMessage`] / [`timing::FrameMapper`] — the non-RT-thread
  equivalent (microsecond timestamp instead of a frame offset) and the
  converter between the two clocks.
* `input` module (feature `midir`, **off by default**) — lists hardware MIDI
  input ports and bridges them into an SPSC queue.

## Building a buffer and parsing bytes

```rust
use superb_midi::{MidiChannel, MidiEvent, MidiEventBuffer, MidiMessage};

let channel = MidiChannel::new(0).unwrap();

// Decode a wire message.
let note_on = MidiMessage::from_bytes(&[0x90, 60, 100]).unwrap();
assert_eq!(note_on, MidiMessage::NoteOn { channel, note: 60, velocity: 100 });

// Stamp it with a frame offset and store it in a fixed-capacity, allocated-once buffer.
let mut buffer = MidiEventBuffer::with_capacity(256);
buffer.push(MidiEvent { frame: 0, message: note_on }).unwrap();
buffer
    .push_sorted(MidiEvent { frame: 10, message: MidiMessage::note_off(channel, 60, 0).unwrap() })
    .unwrap();

// Only the events landing in this sub-block:
for event in buffer.iter_range(0..8) {
    println!("{:?}", event);
}

// Encode back to bytes.
let mut wire = [0u8; 3];
let n = note_on.to_bytes(&mut wire);
assert_eq!(&wire[..n], &[0x90, 60, 100]);
```

`NoteOn` with velocity `0` is normalised to `NoteOff` by both `from_bytes`
and the `note_on` constructor, matching standard MIDI convention.

## The `midir` feature and the non-RT -> RT queue pattern

The `midir` feature is **not** part of `default-features`, so the workspace
(and CI) builds without linking ALSA/CoreMIDI/WinMM. Enable it explicitly:

```toml
superb-midi = { version = "0.1", features = ["midir"] }
```

Hardware MIDI input runs on midir's own callback thread, which is *not*
real-time safe (it can allocate, lock, etc.), so it must never touch the
audio thread's state directly. `MidiInputBridge::connect` sets up the
standard pattern: the midir callback (non-RT producer) parses bytes into a
`MidiMessage` and pushes a `TimestampedMessage` into a `superb_rt::spsc`
queue; the audio thread (RT consumer) drains it with `Consumer::pop` /
`Consumer::drain`, using `timing::FrameMapper` to turn each
`timestamp_us` into a block-relative `frame`:

```rust,ignore
use superb_midi::{input, timing::FrameMapper};
use superb_core::SampleRate;

let ports = input::list_input_ports()?;
let (bridge, mut consumer) = input::MidiInputBridge::connect(0, /* queue_capacity */ 256)?;

let mut mapper = FrameMapper::new(SampleRate::HZ_48K);

// In the audio callback, once per block:
mapper.set_block(block_start_us, frames_in_block);
while let Some(msg) = consumer.pop() {
    let frame = mapper.frame_for(msg.timestamp_us);
    // event_buffer.push(MidiEvent { frame, message: msg.message })...
}

// If the queue filled up (audio thread fell behind), bridge.dropped()
// counts how many messages were discarded rather than blocking the
// callback thread.
println!("dropped so far: {}", bridge.dropped());
```

Messages that fail to parse (SysEx, MTC quarter frames, malformed bytes) are
silently ignored by the bridge rather than being queued.

`superb_io::Engine` already contains this loop: attach the consumer with
`Engine::set_midi_input` and every `render` runs `set_block` on the engine's
stream clock (or the cpal callback timestamp under `device::run_engine`)
and `frame_for` on each message (`MidiTiming::Stream`, the default). Note
that midir's `timestamp_us` epoch is midir's own connection clock, not
cpal's stream clock — see `docs/superb-io.md` for the consequences.

## Limitations (Phase 2)

* **No SysEx** (`0xF0` .. `0xF7`) — `from_bytes` returns an error.
* **No MIDI Time Code quarter frames** (`0xF1`).
* **No running status** — every call to `from_bytes` must start with a
  status byte; the wire optimisation of omitting repeated status bytes is
  not implemented.
* **No Standard MIDI File (SMF) reading/writing** — that belongs to
  `superb-io`/file I/O in Phase 2, alongside `superb-session` and
  `superb-automation`.

These are deliberately deferred rather than half-implemented; the current
scope covers everything the Phase 1 `superb-graph`/`superb-mixer` realtime
path needs.
