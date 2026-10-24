# Quick start

Which function to call. Each flow page opens with a full example; the
linked reference has every signature, argument, error and undo step.
[ARCHITECTURE.md](ARCHITECTURE.md) is the rules, [TERMINOLOGY.md](TERMINOLOGY.md)
the words.

## Do this

| you want | call | reference |
|---|---|---|
| a new session | `session::create` | [session](flows/session.ref.md) |
| open or save | `session::open`, `session::save` | [session](flows/session.ref.md) |
| where files go | `session::layout`, `Root`, `Layout` | [session](flows/session.ref.md) |
| bring in an audio file | `session::import_audio`, then `clip::add` with `.key` | [session](flows/session.ref.md), [clip](flows/clip.ref.md) |
| keep mute, solo, arm, the playhead | `session::state_file` | [session](flows/session.ref.md) |
| tempo, a tempo change, a meter | `session::set_tempo`, `tempo_change`, `meter_change` | [session](flows/session.ref.md) |
| a loop range | `session::set_loop`, then `transport::looping` | [session](flows/session.ref.md), [transport](flows/transport.ref.md) |
| undo or redo | `session::undo`, `session::redo` | [session](flows/session.ref.md) |
| a track | `track::add`; `archive` / `restore`; `rename`, `move_to`, `color` | [track](flows/track.ref.md) |
| mute, solo, arm, monitor, input | `track::mute`, `solo`, `arm`, `monitor`, `input`, `midi_channel` | [track](flows/track.ref.md) |
| volume, pan, route, a meter | `track::volume`, `pan`, `route`, `peak` | [track](flows/track.ref.md) |
| a clip | `clip::add`, `move_to`, `split`, `trim`, `gain`, `fade_in`, `fade_out`, `mute`, `rename`, `archive`, `restore` | [clip](flows/clip.ref.md) |
| notes in a MIDI clip | `clip::add_note`, `remove_note`, `change_note` | [clip](flows/clip.ref.md) |
| a marker | `marker::add`, `move_to`, `rename`, `remove` | [marker](flows/marker.ref.md) |
| play, stop, locate | `transport::start`, `stop`, `seek`, `rewind`, `looping`, `playhead`, `state` | [transport](flows/transport.ref.md) |
| a send | `send::add`, `remove`, `level`, `enable` | [send](flows/send.ref.md) |
| play notes from code | `midi::play_note`, `release_note`, `send` | [midi](flows/midi.ref.md) |
| a hardware keyboard | `devices::midi_inputs`, `midi::connect_input` | [devices](flows/devices.ref.md), [midi](flows/midi.ref.md) |
| record | `record::start`, `punch_in`, `punch_out`, `collect`, `stop` | [record](flows/record.ref.md) |
| a crash left files | `record::unfinished`, `record::recover` | [record](flows/record.ref.md) |
| the click, a count-in | `metronome::enable`, `level`, `count_in` | [metronome](flows/metronome.ref.md) |
| a mix, or stems | `export::bounce`, `export::stems` | [export](flows/export.ref.md) |
| speakers and inputs | `devices::outputs`, `output`, `close_output`, `inputs`, `input`, `close_input` | [devices](flows/devices.ref.md) |
| hear with no sound card | `session::steps::render_blocks` | [session](flows/session.md#steps) |
| rebuild the mixer now | `session::steps::rebuild` | [session](flows/session.md#steps) |

State (mute, solo, arm, monitor, input, clip mute, send on/off, looping,
the click, the count-in, the playhead) is heard at the next block, is not
an undo step, and is saved only after `session::state_file`. Volume, pan,
send level and the other drags are edits: calls within
`Session::COALESCE_WINDOW` (half a second) merge into one undo step.

## Not a flow

These exist in the crate. A session does not apply them for you.

| you want | call | heard when you use the flows? |
|---|---|---|
| an insert or a plugin on a track | store it with `AddInsert` / `RemoveInsert` / `MoveInsert` / `set_insert_bypass` / `set_insert_param` on `Memory`. To hear it, load a `PluginNode` and call `Mixer::add_insert` on a mixer you built | no. `build_mixer` does not read insert chains |
| automation | `primitives::automation`: `AutomationLane::apply_block`, `AutomationBank` | no. The memory does not own lanes |
| a compressor, limiter, gate, de-esser or leveler | `primitives::graph::dynamics` | only if you place the node yourself |
| loudness, true peak, an offset, silence | `primitives::analysis::{meter, offset, silence}` | `export::stems` reports loudness of each file. The meters are yours to call |
| a tempo ramp | `commands::SetTempo::ramped` | no verb. `session::set_tempo` and `tempo_change` are steps, not ramps |
| VST3 | `primitives::plugins::vst3` | `AVAILABLE` is `false`. Every call returns `unsupported()` |

`Memory` is the data (`superb::primitives::session::Memory`). `Session` is
the open session (`session::Session`). Commands are `Command<Memory>`.
