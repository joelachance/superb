"""Writes the `## Reference` and `## Types` sections of docs/flows/*.md.

The signatures, doc comments, "Made of:" lines, struct fields, enum
variants and methods come from the source; what each argument means, what
comes back, what fails and how a verb undoes are the notes below. Run it
from the repository root after changing a flow's public functions:

    python3 scripts/reference.py

Everything between `## Reference` and `## Usage` is rewritten, so edit the
notes here, not the generated text."""
import pathlib, re, textwrap

ROOT = pathlib.Path(".")
FLOWS = [
    ("session", "src/session/mod.rs"), ("track", "src/track.rs"), ("clip", "src/clip.rs"),
    ("marker", "src/marker.rs"), ("transport", "src/transport.rs"), ("send", "src/send.rs"),
    ("midi", "src/midi.rs"), ("record", "src/record.rs"), ("metronome", "src/metronome.rs"),
    ("export", "src/export.rs"), ("devices", "src/devices.rs"),
]

def methods_of(lines, name):
    """[(signature, doc)] for every pub fn / pub const in `impl Name {`."""
    out, i, inside = [], 0, False
    while i < len(lines):
        l = lines[i]
        if re.match(rf"impl(<[^>]*>)? {name}\b", l) and l.rstrip().endswith("{"): inside = True; i += 1; continue
        if inside and l.startswith("}"): inside = False
        if inside:
            doc = []
            j = i
            while j < len(lines) and lines[j].strip().startswith("///"):
                doc.append(lines[j].strip()[3:].strip()); j += 1
            while j < len(lines) and lines[j].strip().startswith("#["): j += 1
            if j < len(lines) and re.match(r"\s*pub (fn|const) ", lines[j]):
                sig = lines[j].strip()
                k = j
                while not (sig.rstrip().endswith("{") or sig.rstrip().endswith(";")):
                    k += 1; sig += " " + lines[k].strip()
                sig = re.sub(r"\s+", " ", sig).rstrip(" {;").strip()
                sig = re.sub(r"\s*=\s*.*$", "", sig) if sig.startswith("pub const") else sig
                out.append((sig, " ".join(doc)))
                i = k + 1; continue
            i = j + 1 if j > i else i + 1
            continue
        i += 1
    return out

def parse(path):
    """(verbs, types): verbs = [(name, signature, doc, made_of, cfg)]; types = [(name, kind, doc, fields)]"""
    src = pathlib.Path(path).read_text()
    lines = src.splitlines()
    verbs, types = [], []
    i = 0
    while i < len(lines):
        doc, cfg = [], None
        j = i
        while j < len(lines) and (lines[j].startswith("///") or lines[j].startswith("#[")):
            if lines[j].startswith("///"):
                doc.append(lines[j][3:].strip())
            elif "cfg(" in lines[j]:
                m = re.search(r'feature = "([^"]+)"', lines[j])
                if m: cfg = m.group(1)
            j += 1
        if j < len(lines) and lines[j].startswith("pub fn "):
            sig = lines[j]
            k = j
            while not (sig.rstrip().endswith("{") or sig.rstrip().endswith(";")):
                k += 1; sig += " " + lines[k].strip()
            sig = re.sub(r"\s+", " ", sig).rstrip(" {").strip()
            name = re.match(r"pub fn (\w+)", sig).group(1)
            text = " ".join(doc)
            made = ""
            m = re.search(r"Made of:\s*(.*)$", text)
            if m:
                made = m.group(1).strip(); text = text[:m.start()].strip()
            verbs.append((name, sig, text, made, cfg))
            i = k + 1; continue
        if j < len(lines) and re.match(r"pub (struct|enum) (\w+)", lines[j]):
            m = re.match(r"pub (struct|enum) (\w+)", lines[j]); kind, name = m.group(1), m.group(2)
            fields = []
            k = j + 1; fdoc = []
            if lines[j].rstrip().endswith("{"):
                while k < len(lines) and not lines[k].startswith("}"):
                    l = lines[k].strip()
                    if l.startswith("///"): fdoc.append(l[3:].strip())
                    elif l.startswith("#["): pass
                    elif l and not l.startswith("//"):
                        if kind == "struct":
                            fm = re.match(r"pub (\w+):\s*(.*?),?$", l)
                            if fm: fields.append((fm.group(1), fm.group(2).strip(), " ".join(fdoc)))
                        else:
                            fm = re.match(r"(\w+)(.*?),?$", l)
                            vname, rest = fm.group(1), fm.group(2).strip()
                            if rest.endswith("{"):
                                inner = []
                                k += 1
                                while k < len(lines) and not lines[k].strip().startswith("}"):
                                    il = lines[k].strip()
                                    if il and not il.startswith("///") and not il.startswith("#["): inner.append(il.rstrip(","))
                                    k += 1
                                rest = "{ " + ", ".join(inner) + " }"
                            fields.append((vname, rest, " ".join(fdoc)))
                        fdoc = []
                    k += 1
            types.append((name, kind, " ".join(doc), fields, cfg, methods_of(lines, name)))
            i = k + 1; continue
        i = j + 1 if j > i else i + 1
    return verbs, types

COMMON = {
    "session": "the open session, borrowed mutably",
    "track": "the track's id, from `track::add` or `session.memory.tracks()`",
    "clip": "the clip's id, from `clip::add` or `track.clips()`",
    "marker": "the marker's id, from `marker::add` or `session.memory.markers()`",
    "send": "the send's id, from `send::add` or `track.sends()`",
    "at": "a position on the timeline, in samples (`TimePosition::from_seconds` converts)",
    "on": "`true` to switch on, `false` to switch off",
    "db": "a level in decibels; `0.0` is unity, negative is quieter",
    "name": "the new name",
    "bpm": "beats (quarter notes) per minute",
}

# flow::verb -> dict(takes={param: text}, returns=..., errors=..., undo=..., example=...)
N = {}
def note(key, returns="", errors="", undo="", example="", takes=None, **kw):
    merged = dict(takes or {})
    merged.update(kw)
    N[key] = dict(takes=merged, returns=returns, errors=errors, undo=undo, example=example)

ONE = "one undo step"
STATE = "none: state, not an edit (saved through the state file, see `session::state_file`)"
DRAG = "one undo step per drag: calls within `Session::COALESCE_WINDOW` (half a second) merge"
NONE = "none: not an edit"
REB = "; rebuilds the mixer (at once while a device plays, otherwise at the next start)"

# ---- session ----
note("session::create", returns="the new `Session`, running an offline engine; nothing plays until `devices::output` or `session::steps::render_blocks`",
     errors="the folder cannot be created or `memory.json` cannot be written; a block size of zero",
     undo=NONE, example='let mut session = session::create("~/songs/demo", SampleRate::HZ_48K, 256)?;',
     folder="where the session lives; created now, with an `audio/` folder inside; the session is named after it",
     sample_rate="the one sample rate everything in the session runs at", block_size="frames per engine block, the largest block any node sees")
note("session::open", returns="the `Session` as saved: memory, routing, tempo, and the audio its clips refer to, read from `audio/`; state comes back only once `state_file` is named",
     errors="no `memory.json` in the folder, a file written by a newer version, or malformed JSON; a missing audio file leaves its clip silent rather than failing",
     undo=NONE, example='let mut session = session::open("~/songs/demo", 256)?;\nsession::state_file(&mut session, Some(session.folder.join("state.json")))?;',
     folder="a session folder, one `create` made", block_size="frames per engine block")
note("session::save", returns="nothing; `session.history.is_clean()` is `true` afterwards", errors="the folder or `memory.json` cannot be written; audio a clip refers to cannot be written",
     undo=NONE, example="session::save(&mut session)?;")
note("session::import_audio", returns="an `Imported`: the key (`audio/<name>.wav`) a clip refers to the audio by, the file's own sample rate and bit depth, channels, frames, and whether it was resampled",
     errors="the file cannot be read or decoded; without the `files` feature, anything but a WAV at the session's rate",
     undo=NONE + "; placing the clip with `clip::add` is the undo step", example='let take = session::import_audio(&mut session, "/music/take.flac")?;\nprintln!("{take}");   // audio/take.wav: 44100 Hz, 24-bit, 2 channel(s), resampled to 48000 Hz, stored as 32-bit float\nclip::add(&mut session, vox, take.key, TimePosition::ZERO)?;',
     file="the audio file to bring in; WAV, AIFF, FLAC, ALAC, MP3, AAC (MP4/M4A), Ogg Vorbis, CAF or Matroska with the `files` feature")
note("session::state_file", returns="nothing", errors="the file exists but cannot be read, is malformed or was written by a newer version; the file cannot be written",
     undo=NONE, example='session::state_file(&mut session, Some(session.folder.join("state.json")))?;   // read now, written from here on\nsession::state_file(&mut session, None)?;                                            // stop writing; the file stays',
     path="`Some(path)`: keep state there, reading it now if it exists; `None`: stop keeping state")
note("session::set_tempo", returns="nothing", errors="a tempo at or below zero", undo=DRAG + REB, example="session::set_tempo(&mut session, 100.0)?;")
note("session::tempo_change", returns="nothing", errors="a beat before zero; removing the change at beat zero (use `set_tempo`); removing a change that is not there",
     undo="one step per beat; changes to the same beat coalesce" + REB, example="session::tempo_change(&mut session, BeatPosition::new(16.0), Some(140.0))?;   // faster from bar 5\nsession::tempo_change(&mut session, BeatPosition::new(16.0), None)?;          // and back",
     beat="where the change takes effect, in quarter notes from the start", bpm="`Some(bpm)` adds or changes the tempo there; `None` removes the change")
note("session::meter_change", returns="nothing", errors="bar 0; removing the meter at bar 1; removing a change that is not there", undo=ONE + REB,
     example="session::meter_change(&mut session, 9, Some(TimeSignature::new(6, 8)?))?;",
     bar="the 1-based bar the new meter starts at", signature="`Some(signature)` sets the meter from that bar on; `None` removes the change there")
note("session::set_loop", returns="nothing", errors="a range whose end is not after its start", undo=DRAG, example="session::set_loop(&mut session, Some(LoopRange::new(TimePosition::ZERO, TimePosition::from_seconds(8.0, rate))?))?;\ntransport::looping(&mut session, true)?;",
     range="`Some(range)` sets the loop range, told to the engine at once; `None` clears it")
note("session::undo", returns="`true` when a step was undone, `false` when there was nothing to undo", errors="a command fails to revert (should not happen)", undo="this is the undo",
     example="if session::undo(&mut session)? { println!(\"undid {:?}\", session.history.redo_label()); }")
note("session::redo", returns="`true` when a step was redone, `false` when there was nothing to redo", errors="a command fails to apply again", undo="this is the redo", example="session::redo(&mut session)?;")

# ---- track ----
note("track::add", returns="the new track's id", errors="none in practice", undo=ONE + REB, example='let vox = track::add(&mut session, "vox", TrackKind::Audio)?;\nlet keys = track::add(&mut session, "keys", TrackKind::Midi)?;\nlet verb = track::add(&mut session, "reverb", TrackKind::Bus)?;',
     name="the track's name; recordings on it are named after it (`vox 1`)", kind="`TrackKind::Audio`, `TrackKind::Midi` or `TrackKind::Bus`")
note("track::archive", returns="nothing", errors="an unknown track; the master", undo=ONE + REB, example="track::archive(&mut session, vox)?;")
note("track::restore", returns="nothing", errors="an unknown track; the master", undo=ONE + REB, example="track::restore(&mut session, vox)?;")
note("track::midi_channel", returns="nothing", errors="an audio track or bus; a channel above 15", undo=STATE + REB, example="track::midi_channel(&mut session, keys, Some(0))?;   // channel 1 only\ntrack::midi_channel(&mut session, keys, None)?;      // all channels",
     channel="`Some(0..=15)` for one MIDI channel (numbered from 0, so channel 1 is `Some(0)`); `None` for all")
note("track::move_to", returns="nothing", errors="an index past the end; the master", undo=ONE, example="track::move_to(&mut session, keys, 0)?;   // first",
     index="the new place in `session.memory.tracks()`, 0 first; archived tracks count")
note("track::color", returns="nothing", errors="an unknown track", undo=DRAG, example="track::color(&mut session, vox, Some(0xff8800))?;", color="`Some(0xRRGGBB)` or `None` for no colour")
note("track::rename", returns="nothing", errors="an unknown track", undo=DRAG, example='track::rename(&mut session, vox, "lead vocal")?;')
note("track::volume", returns="nothing", errors="an unknown track", undo=DRAG + "; the live strip hears it at once", example="track::volume(&mut session, vox, -6.0)?;")
note("track::pan", returns="nothing", errors="an unknown track", undo=DRAG + "; the live strip hears it at once", example="track::pan(&mut session, vox, -0.3)?;   // a little left",
     pan="`-1.0` hard left to `1.0` hard right, `0.0` centre")
note("track::mute", returns="nothing", errors="an unknown track", undo=STATE + "; the live strip hears it at once", example="track::mute(&mut session, vox, true)?;")
note("track::solo", returns="nothing", errors="an unknown track; the master", undo=STATE + "; the live strip hears it at once", example="track::solo(&mut session, vox, true)?;   // every other track falls silent")
note("track::arm", returns="nothing", errors="a bus", undo=STATE, example="track::arm(&mut session, vox, true)?;   // the next recording lands here")
note("track::monitor", returns="nothing", errors="a bus", undo=STATE + REB, example="track::monitor(&mut session, vox, true)?;   // hear the input on this strip")
note("track::input", returns="nothing", errors="a MIDI track or bus", undo=STATE, example="track::input(&mut session, vox, Some(Input::Mono(0)))?;        // the first input channel\ntrack::input(&mut session, gtr, Some(Input::Stereo(2, 3)))?;   // a stereo pair",
     input="`Some(Input::Mono(channel))`, `Some(Input::Stereo(left, right))` (0-based device channels) or `None` for everything the recording captures")
note("track::route", returns="nothing", errors="an unknown track; a target that is not a bus; a routing loop; the master (it has no output)", undo=ONE + REB, example="track::route(&mut session, vox, Some(verb))?;   // into the reverb bus\ntrack::route(&mut session, vox, None)?;         // back to the master",
     bus="`Some(bus)` a bus track to feed; `None` the master")
note("track::peak", returns="`Some([left, right])` as linear amplitude since the last call, or `None` before the first rebuild or for an archived track", errors="never; this is a query",
     undo="none: a meter", example="if let Some([l, r]) = track::peak(&session, vox) { draw_meter(l, r); }")

# ---- clip ----
note("clip::add", returns="the new clip's id", errors="audio not in `session.audio` under that key; no notes; a bus", undo=ONE + REB,
     example='let take = session::import_audio(&mut session, "/music/take.wav")?;\nlet c = clip::add(&mut session, vox, take.key, TimePosition::from_seconds(4.0, rate))?;\nlet m = clip::add(&mut session, keys, vec![MidiNote { beat: 0.0, length: 1.0, note: 60, velocity: 100, channel: 0 }], TimePosition::ZERO)?;',
     content="audio: the key the audio is held under in `session.audio` (`&str`, `String`, `PathBuf`), from `session::import_audio` or a recording; MIDI: a `Vec<MidiNote>` with beats from the clip start")
note("clip::move_to", returns="nothing", errors="an unknown clip or track; a bus", undo=DRAG + REB, example="clip::move_to(&mut session, c, vox, TimePosition::from_seconds(8.0, rate))?;   // same track, later\nclip::move_to(&mut session, c, gtr, TimePosition::from_seconds(8.0, rate))?;   // another track",
     track="the track to put it on; its own to move in time only")
note("clip::split", returns="the id of the right half; the left keeps the clip's id", errors="an unknown clip; a position outside the clip", undo=ONE + REB, example="let right = clip::split(&mut session, c, transport::playhead(&session))?;")
note("clip::trim", returns="nothing", errors="an unknown clip; an end not after the start", undo=DRAG + REB, example="clip::trim(&mut session, c, start.offset(4_800), end)?;   // 0.1 s off the front at 48 kHz",
     start="the new start on the timeline; the audio underneath stays put", end="the new end on the timeline")
note("clip::gain", returns="nothing", errors="an unknown clip", undo=DRAG + REB, example="clip::gain(&mut session, c, -3.0)?;")
note("clip::mute", returns="nothing", errors="an unknown clip", undo=STATE + REB, example="clip::mute(&mut session, c, true)?;")
note("clip::fade_in", returns="nothing", errors="an unknown clip", undo=DRAG + REB, example="clip::fade_in(&mut session, c, TimeDuration::from_seconds(0.05, rate))?;", length="how long the fade lasts from the clip start")
note("clip::fade_out", returns="nothing", errors="an unknown clip", undo=DRAG + REB, example="clip::fade_out(&mut session, c, TimeDuration::from_seconds(0.5, rate))?;", length="how long the fade lasts up to the clip end")
note("clip::archive", returns="nothing", errors="an unknown clip", undo=ONE + REB, example="clip::archive(&mut session, c)?;")
note("clip::restore", returns="nothing", errors="an unknown clip", undo=ONE + REB, example="clip::restore(&mut session, c)?;")
note("clip::rename", returns="nothing", errors="an unknown clip", undo=DRAG, example='clip::rename(&mut session, c, "verse vocal")?;')
note("clip::add_note", returns="nothing", errors="an audio clip; a note before the clip start, with no length, or out of MIDI range (note 0..=127, velocity 1..=127, channel 0..=15)", undo=ONE + REB,
     example="clip::add_note(&mut session, m, MidiNote { beat: 2.0, length: 0.5, note: 64, velocity: 90, channel: 0 })?;", note="the note, with `beat` and `length` in quarter notes from the clip start")
note("clip::remove_note", returns="nothing", errors="an audio clip; no note equal to it", undo=ONE + REB, example="clip::remove_note(&mut session, m, note)?;", note="the note to remove; the first one equal to it goes")
note("clip::change_note", returns="nothing", errors="an audio clip; no note equal to `from`; a `to` out of range", undo="one step per drag: note changes in one clip within the coalesce window merge" + REB,
     example="clip::change_note(&mut session, m, note, MidiNote { beat: note.beat + 0.25, ..note })?;   // nudge it later",
     takes={"from": "the note as it is now", "to": "the note as it should be: moved, longer, louder, on another channel, whatever differs"})

# ---- marker ----
note("marker::add", returns="the new marker's id", errors="none in practice", undo=ONE, example='let verse = marker::add(&mut session, "verse", TimePosition::from_seconds(8.0, rate))?;')
note("marker::move_to", returns="nothing", errors="an unknown marker", undo=DRAG, example="marker::move_to(&mut session, verse, TimePosition::from_seconds(8.5, rate))?;")
note("marker::rename", returns="nothing", errors="an unknown marker", undo=DRAG, example='marker::rename(&mut session, verse, "verse 1")?;')
note("marker::remove", returns="nothing", errors="an unknown marker", undo=ONE, example="marker::remove(&mut session, verse)?;")

# ---- transport ----
note("transport::start", returns="nothing", errors="the mixer cannot be built (the memory names audio that is not in memory, say); the command queue is full", undo=NONE,
     example="transport::start(&mut session)?;")
note("transport::stop", returns="nothing", errors="the command queue is full", undo=NONE, example="transport::stop(&mut session)?;")
note("transport::seek", returns="nothing", errors="the command queue is full", undo=NONE, example="transport::seek(&mut session, TimePosition::from_seconds(30.0, rate))?;", to="where to put the playhead")
note("transport::rewind", returns="nothing", errors="the command queue is full", undo=NONE, example="transport::rewind(&mut session)?;")
note("transport::looping", returns="nothing", errors="the command queue is full", undo=STATE, example="transport::looping(&mut session, true)?;")
note("transport::playhead", returns="where the playhead is after the last block the engine rendered, in samples", errors="never; a query", undo="none", example="let seconds = transport::playhead(&session).to_seconds(rate);")
note("transport::state", returns="`TransportState::Stopped`, `Playing` or `Recording`, as of the last rendered block", errors="never; a query", undo="none", example="if transport::state(&session).is_rolling() { /* draw a moving playhead */ }")

# ---- send ----
note("send::add", returns="the new send's id", errors="an unknown track or bus; a target that is not a bus; the master as source or target; a bus sending to itself", undo=ONE + REB,
     example="let s = send::add(&mut session, vox, verb, -12.0, SendTap::PostFader)?;",
     bus="the bus track that receives the copy", level_db="the send level in decibels", tap="`SendTap::PostFader` (after the strip's fader and pan) or `SendTap::PreFader` (before)")
note("send::remove", returns="nothing", errors="an unknown send", undo=ONE + REB, example="send::remove(&mut session, s)?;")
note("send::level", returns="nothing", errors="an unknown send", undo=DRAG + "; the live send hears it at once", example="send::level(&mut session, s, -6.0)?;")
note("send::enable", returns="nothing", errors="an unknown send", undo=STATE + "; the live send hears it at once", example="send::enable(&mut session, s, false)?;")

# ---- midi ----
note("midi::play_note", returns="nothing", errors="a channel above 15, a note or velocity above 127; the MIDI queue is full", undo=NONE, example="midi::play_note(&mut session, 0, 60, 100)?;   // middle C on channel 1",
     channel="the MIDI channel, `0..=15`", note="the MIDI note number, `0..=127` (60 is middle C)", velocity="`1..=127`; `0` releases the note")
note("midi::release_note", returns="nothing", errors="a channel above 15 or a note above 127; the MIDI queue is full", undo=NONE, example="midi::release_note(&mut session, 0, 60)?;",
     channel="the MIDI channel, `0..=15`", note="the MIDI note number")
note("midi::send", returns="nothing", errors="the MIDI queue is full", undo=NONE, example="midi::send(&mut session, MidiMessage::control_change(MidiChannel::new(0)?, 1, 64)?)?;",
     message="any `MidiMessage`")
note("midi::connect_input", returns="nothing", errors="no port at that index; the port cannot be opened", undo=NONE, example="for port in devices::midi_inputs()? { midi::connect_input(&mut session, port.index)?; }",
     port_index="the port's `index` from `devices::midi_inputs()`, a snapshot that changes when devices are plugged in")

# ---- record ----
note("record::start", returns="nothing; `session.recording` holds the recording until `stop`", errors="a recording already running; no armed track; the device input cannot be opened; the command queue is full", undo=NONE + " until `stop`",
     example="record::start(&mut session, record::Source::Device(None))?;   // the default input\n// ... play ...\nlet clips = record::stop(&mut session)?;",
     source="`Source::Device(Some(name))` or `Source::Device(None)` for the default input (feature `devices`); `Source::Queue { blocks, channels }` for audio you push yourself")
note("record::punch_in", returns="nothing", errors="never in practice", undo=DRAG, example="record::punch_in(&mut session, Some(TimePosition::from_seconds(16.0, rate)))?;", at="`Some(position)` where the next recording drops in; `None` clears it")
note("record::punch_out", returns="nothing", errors="never in practice", undo=DRAG, example="record::punch_out(&mut session, Some(TimePosition::from_seconds(24.0, rate)))?;", at="`Some(position)` where the recording drops out; `None` clears it")
note("record::collect", returns="the audio frames gathered so far, or zero when nothing is recording", errors="never", undo="none", example="let frames = record::collect(&mut session);   // call from a timer while a device drives the engine")
note("record::stop", returns="the ids of the clips placed, one per armed track that got something (audio captured, or notes that arrived)", errors="nothing recording; a WAV cannot be written; the collector thread failed", undo="one step labelled \"Record\": undo removes the clips and deletes the files they wrote" + REB,
     example="let clips = record::stop(&mut session)?;")

# ---- metronome ----
note("metronome::enable", returns="nothing", errors="never", undo=STATE + "; heard at the next block", example="metronome::enable(&mut session, true)?;")
note("metronome::level", returns="nothing", errors="never", undo=DRAG + "; heard at once", example="metronome::level(&mut session, -12.0)?;")
note("metronome::count_in", returns="nothing", errors="never", undo=STATE, example="metronome::count_in(&mut session, 2)?;   // two bars of click before a recording at the playhead", bars="bars of click before a recording that starts at the playhead; `0` for none")

# ---- export ----
note("export::bounce", returns="the rendered audio, also written to `path`", errors="nothing to bounce (no clips after `start` and no length given); the file cannot be written; resampling fails",
     undo=NONE, example='let audio = export::bounce(&mut session, "mix.wav", export::Bounce::default())?;                 // the whole session, 16-bit, session rate\nlet range = export::bounce(&mut session, "verse.wav", export::Bounce { start: TimePosition::from_seconds(8.0, rate), length: Some(TimeDuration::from_seconds(16.0, rate)), sample_rate: Some(SampleRate::CD), format: WavFormat::Float32 })?;',
     path="the WAV file to write", what="a `Bounce`: where to start, how long (or to the end), which sample rate (or the session's), 16-bit PCM or 32-bit float")

# ---- devices ----
note("devices::outputs", returns="every audio output device: name, whether it is the default, channels when known", errors="the audio host cannot be reached", undo="none", example="for d in devices::outputs()? { println!(\"{}{}\", d.name, if d.is_default { \" (default)\" } else { \"\" }); }")
note("devices::inputs", returns="every audio input device, the same way", errors="the audio host cannot be reached", undo="none", example="let inputs = devices::inputs()?;")
note("devices::output", returns="nothing; `session.output` holds the stream and the audio thread inside it", errors="the audio thread is already on a device (`close_output` first); the device refuses the session's rate, block size or channel count; the mixer cannot be built",
     undo="none", example="devices::output(&mut session, None)?;                  // the default output drives the engine from here\ndevices::output(&mut session, Some(\"Scarlett 2i2\"))?;", device="`Some(name)` from `outputs()`, or `None` for the default")
note("devices::close_output", returns="nothing; `session.audio_thread` has the thread back", errors="no device output open; a recording running", undo="none", example="devices::close_output(&mut session)?;\ndevices::output(&mut session, Some(\"Headphones\"))?;   // move to another device")
note("devices::input", returns="nothing; `session.input` holds the stream", errors="an input already open; the device cannot be opened at the session's rate and block size", undo="none",
     example="devices::input(&mut session, None)?;\ntrack::monitor(&mut session, vox, true)?;   // hear it", device="`Some(name)` from `inputs()`, or `None` for the default")
note("devices::close_input", returns="nothing", errors="no input open; a device recording running", undo="none", example="devices::close_input(&mut session)?;")
note("devices::midi_inputs", returns="every MIDI input port: index and name", errors="the MIDI backend cannot be reached", undo="none", example="for p in devices::midi_inputs()? { println!(\"{}: {}\", p.index, p.name); }")

def param_list(sig):
    start = sig.index("(")
    depth, end = 0, start
    for idx in range(start, len(sig)):
        if sig[idx] == "(": depth += 1
        elif sig[idx] == ")":
            depth -= 1
            if depth == 0: end = idx; break
    inside = sig[start + 1: end]
    out, depth, cur = [], 0, ""
    for ch in inside:
        if ch in "<([": depth += 1
        if ch in ">)]": depth -= 1
        if ch == "," and depth == 0:
            out.append(cur.strip()); cur = ""
        else: cur += ch
    if cur.strip(): out.append(cur.strip())
    return [(p.split(":")[0].strip(), p.split(":", 1)[1].strip()) for p in out if ":" in p]

def ret_type(sig):
    return sig.split("->", 1)[1].strip() if "->" in sig else "()"

def render_verb(flow, name, sig, doc, made, cfg):
    key = f"{flow}::{name}"
    n = N.get(key, dict(takes={}, returns="", errors="", undo="", example=""))
    out = [f"### `{name}`", "", "```rust", sig, "```", ""]
    if cfg: out += [f"*Feature `{cfg}`.*", ""]
    if doc: out += [textwrap.fill(doc, 76), ""]
    params = param_list(sig)
    if params:
        out.append("**Takes**")
        for p, t in params:
            text = n["takes"].get(p) or COMMON.get(p) or ""
            if p == "session" and not t.startswith("&mut"): text = "the open session, borrowed"
            out.append(f"- `{p}: {t}` — {text}" if text else f"- `{p}: {t}`")
        out.append("")
    rt = ret_type(sig)
    out += [f"**Returns** `{rt}`" + (f" — {n['returns']}" if n["returns"] else ""), ""]
    if n["errors"]: out += [f"**Errors** {n['errors']}.", ""]
    if n["undo"]: out += [f"**Undo** {n['undo']}.", ""]
    if made: out += [f"**Made of** {made}", ""]
    if n["example"]: out += ["**Example**", "", "```rust", n["example"], "```", ""]
    return "\n".join(out)

def render_types(types):
    if not types: return ""
    out = ["## Types", ""]
    for name, kind, doc, fields, cfg, methods in types:
        out += [f"### `{name}`" + (f" *(feature `{cfg}`)*" if cfg else ""), ""]
        if doc: out += [textwrap.fill(doc, 76), ""]
        if fields:
            out.append("| " + ("field" if kind == "struct" else "variant") + " | type | meaning |")
            out.append("|---|---|---|")
            for f, t, d in fields:
                out.append(f"| `{f}` | `{t}` | {d} |" if t else f"| `{f}` | | {d} |")
            out.append("")
        if methods:
            out.append("| method | meaning |")
            out.append("|---|---|")
            for sig, d in methods:
                out.append(f"| `{sig.replace('pub ', '', 1)}` | {d} |")
            out.append("")
    return "\n".join(out)

for flow, path in FLOWS:
    verbs, types = parse(path)
    doc_path = ROOT / "docs" / "flows" / f"{flow}.md"
    text = doc_path.read_text()
    ref = ["## Reference", "", f"Every function of the `{flow}` flow: what it takes, what it returns, what it refuses, and how it undoes. A `Result<T>` is `Ok(T)` or a `SuperbError`; the errors listed are the ones you can cause.", ""]
    for v in verbs: ref.append(render_verb(flow, *v))
    ref_text = "\n".join(ref) + "\n" + render_types(types)
    # drop an older reference, then insert before "## Usage" (or append)
    text = re.sub(r"\n## Reference\n.*?(?=\n## Usage|\n## Made of|\Z)", "\n", text, flags=re.S)
    text = re.sub(r"\n## Types\n.*?(?=\n## Usage|\n## Made of|\Z)", "\n", text, flags=re.S)
    if "\n## Usage" in text:
        text = text.replace("\n## Usage", "\n" + ref_text.rstrip("\n") + "\n\n## Usage", 1)
    else:
        text = text.rstrip("\n") + "\n\n" + ref_text
    text = re.sub(r"\n{3,}", "\n\n", text)
    doc_path.write_text(text)
    print(f"{flow}: {len(verbs)} verbs, {len(types)} types")
