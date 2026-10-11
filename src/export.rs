//! Getting the session out: bounce it, or a range of it, to a WAV file;
//! or bounce every strip to its own file, a stem set.
//!
//! Both build their own copy of the mixer and run it offline, as fast as
//! the machine allows. The running engine is not touched, so you can
//! bounce while listening. The metronome is never in a bounce.
//!
//! Made of [`session::steps::build_mixer`]
//! (the mixer), `aura::primitives::io::offline` (the render loop),
//! `aura::primitives::io::files` (resampling, feature `files`),
//! `aura::primitives::io::wav` (the files) and
//! `aura::primitives::analysis::meter` (what a stem set measures).

use std::path::Path;

use crate::primitives::analysis::meter::{self, Loudness, TruePeakMeter};
use crate::primitives::buffers::gain::linear_to_db;
use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::io::wav::{self, WavFormat};
use crate::primitives::io::{offline, Engine};
use crate::primitives::mixer::StripId;
use crate::primitives::session::TrackKind;
use crate::primitives::transport::Transport;
use crate::primitives::{AuraError, NodeId, Result, SampleRate, TimeDuration, TimePosition};
use crate::session;
use crate::session::Session;

/// What to bounce and how. [`Bounce::default()`] is the whole session,
/// from zero to the end of the last clip, at the session's sample rate as
/// 16-bit PCM.
#[derive(Clone, Debug, PartialEq)]
pub struct Bounce {
    /// Where to start. Zero by default.
    pub start: TimePosition,
    /// How much to render. `None` (the default) runs to the end of the
    /// last clip.
    pub length: Option<TimeDuration>,
    /// Convert to this sample rate before writing. `None` (the default)
    /// keeps the session's rate. Converting needs the `files` feature;
    /// without it any other rate is an error.
    pub sample_rate: Option<SampleRate>,
    /// 16-bit PCM by default.
    pub format: WavFormat,
}

impl Default for Bounce {
    fn default() -> Self {
        Self {
            start: TimePosition::ZERO,
            length: None,
            sample_rate: None,
            format: WavFormat::Pcm16,
        }
    }
}

/// Renders part or all of the session offline and writes it as a WAV
/// file at `path`, making its folder if needed
/// (`session::layout(&session).bounce("mix")` is `bounces/mix.wav`).
/// Returns the audio written. The metronome is left out. Fails when there
/// is nothing to render: no clips after `start` and no `length` given.
///
/// Made of: `Memory::end_position` for the length when none is given →
/// [`session::steps::build_mixer`], the click off → a `Transport` on the
/// session's tempo map, sought to `start` and playing → `Engine::new` →
/// `offline::render` → `files::resample` when a rate is given →
/// `create_dir_all` → `wav::write`.
pub fn bounce(session: &mut Session, path: impl AsRef<Path>, what: Bounce) -> Result<PlanarBuffer> {
    let length = match what.length {
        Some(length) => length,
        None => {
            let end = session.memory.end_position();
            TimeDuration::from_samples(end.samples() - what.start.samples())
        }
    };
    if length.samples() <= 0 {
        return Err(AuraError::Message(
            "nothing to bounce: no clips after the start, and no length given".into(),
        ));
    }
    let audio = render(session, what.start, length.samples() as usize)?;
    let session_rate = session.memory.sample_rate();
    let (audio, rate) = match what.sample_rate {
        Some(rate) if rate != session_rate => (resample(&audio, session_rate, rate)?, rate),
        _ => (audio, session_rate),
    };
    make_parent(path.as_ref())?;
    wav::write(path, &audio, rate, what.format)?;
    Ok(audio)
}

/// Makes the folder `path` goes in, when it has one.
fn make_parent(path: &Path) -> Result<()> {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => std::fs::create_dir_all(dir)
            .map_err(|e| AuraError::Message(format!("cannot create {}: {e}", dir.display()))),
        _ => Ok(()),
    }
}

/// Renders `frames` frames of the session starting at `start` into memory
/// through a fresh offline mixer.
pub(crate) fn render(
    session: &mut Session,
    start: TimePosition,
    frames: usize,
) -> Result<PlanarBuffer> {
    let (mixer, handles) =
        session::steps::build_mixer(&session.memory, &session.audio, session.channels)?;
    handles.click.set_enabled(false);
    let mut transport = Transport::with_tempo_map(session.memory.tempo_map().clone());
    transport.seek(start);
    transport.play();
    let mut engine = Engine::new(mixer, transport);
    offline::render(
        &mut engine,
        session.memory.sample_rate(),
        session.engine.max_frames(),
        frames,
    )
}

/// What a stem set holds and how. [`Stems::default()`] is the whole
/// session, from zero to the end of the last clip, as 24-bit PCM at the
/// session's sample rate.
#[derive(Clone, Debug, PartialEq)]
pub struct Stems {
    /// Where every file starts on the timeline. Zero by default.
    pub start: TimePosition,
    /// How much to render. `None` (the default) runs to the end of the
    /// last clip.
    pub length: Option<TimeDuration>,
    /// 24-bit PCM by default.
    pub format: WavFormat,
}

impl Default for Stems {
    fn default() -> Self {
        Self {
            start: TimePosition::ZERO,
            length: None,
            format: WavFormat::Pcm24,
        }
    }
}

/// A stem set as [`stems`] wrote it: what `manifest.json` beside the files
/// says. Everything a receiving studio, an uploader or a later import
/// needs to know about the files without opening them.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StemSet {
    /// The manifest's format, for readers to check.
    pub format_version: u32,
    /// The set's name, which is also its folder: `stems/<name>/`.
    pub name: String,
    /// Sample rate of every file, in Hz.
    pub sample_rate: u32,
    /// Bits per sample of every file: 16 or 24 (PCM) or 32 (float).
    pub bits: u32,
    /// The timeline sample every file's first frame is at.
    pub start: i64,
    /// Frames in every file.
    pub frames: u64,
    /// One per strip: every track and bus that is not muted, in the
    /// session's order.
    pub stems: Vec<Stem>,
    /// The mix through the master, as [`bounce`] renders it.
    pub mix: Stem,
}

/// The manifest's format.
pub const STEM_SET_FORMAT_VERSION: u32 = 1;

/// One file of a stem set.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Stem {
    /// The file, inside the set's folder (`vox.wav`).
    pub file: String,
    /// The track or bus it is the output of; `mix` for the mix.
    pub name: String,
    /// What it is.
    pub source: StemSource,
    /// Where the strip's output goes: the bus's name, or `None` for the
    /// master (and for the mix itself).
    pub output: Option<String>,
    /// Channels in the file.
    pub channels: usize,
    /// The highest sample, in dBFS; `None` when the file is silent.
    pub peak_dbfs: Option<f32>,
    /// The highest point between the samples too, in dBTP; `None` when
    /// silent.
    pub true_peak_dbtp: Option<f32>,
    /// Integrated loudness, in LUFS; `None` when too quiet or short to
    /// measure.
    pub loudness_lufs: Option<f32>,
}

/// What a stem is the output of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StemSource {
    /// An audio track.
    Audio,
    /// A MIDI track, through its instrument.
    Midi,
    /// A bus: what was routed and sent to it, after its own strip.
    Bus,
    /// The mix through the master.
    Mix,
}

/// Bounces every strip of the session to its own file and the mix beside
/// them, in one offline pass, into `stems/<name>/` of the session folder,
/// with a `manifest.json` that describes them ([`StemSet`]). Every file
/// starts at the same timeline sample and runs the same length, so they
/// line up in any DAW. Each holds its strip's output after its inserts,
/// fader and pan and before the master, so the stems of the tracks that
/// feed the master and of the buses sum to the mix before the master (a
/// track routed into a bus is in that bus's stem as well as its own). A
/// muted track or bus gets no stem; solo is a listening aid and is
/// ignored; the metronome is left out. Nothing is normalised: the stems
/// must still sum to the mix. A name already used is refused, so a set is
/// never overwritten; a failed bounce removes the folder it made.
///
/// Made of: `Memory::end_position` for the length when none is given →
/// `session::steps::build_mixer` → `Mixer::clear_solo`, the click off → a
/// `Transport` sought to `start` and playing → `Engine::new` → per block
/// `Engine::render`, then `AudioGraph::node_output` of every unmuted strip
/// into a `wav::Writer` and the engine's output into the mix's, each
/// measured by `analysis::meter` → `wav::Writer::finish` → `manifest.json`.
pub fn stems(session: &mut Session, name: &str, what: Stems) -> Result<StemSet> {
    let length = match what.length {
        Some(length) => length,
        None => TimeDuration::from_samples(
            session.memory.end_position().samples() - what.start.samples(),
        ),
    };
    if length.samples() <= 0 {
        return Err(AuraError::Message(
            "no stems to bounce: no clips after the start, and no length given".into(),
        ));
    }
    let folder = session::layout(session).stem_set(name);
    if folder.exists() {
        return Err(AuraError::Message(format!(
            "{} already exists; a stem set is never overwritten, so choose another name",
            folder.display()
        )));
    }
    std::fs::create_dir_all(&folder)
        .map_err(|e| AuraError::Message(format!("cannot create {}: {e}", folder.display())))?;
    let written = write_stems(session, &folder, name, &what, length.samples() as u64);
    if written.is_err() {
        // The folder is this call's own: nothing else was in it.
        let _ = std::fs::remove_dir_all(&folder);
    }
    written
}

/// One file of a stem set while it is written.
struct StemFile {
    /// The graph node whose output it takes; `None` for the mix.
    node: Option<NodeId>,
    /// Frames to skip at the start: the latency of the path to the node.
    latency: u64,
    writer: wav::Writer,
    true_peaks: Vec<TruePeakMeter>,
    loudness: Loudness,
    peak: f32,
    stem: Stem,
}

impl StemFile {
    /// Writes and measures the frames of `planes` that fall inside the
    /// file, where `planes` holds the frames from `rendered` on.
    fn take(&mut self, planes: &[&[f32]], rendered: u64, frames: u64) -> Result<()> {
        let block = planes.first().map_or(0, |p| p.len()) as u64;
        let from = self.latency.max(rendered);
        let until = (self.latency + frames).min(rendered + block);
        if until <= from {
            return Ok(());
        }
        let (a, b) = ((from - rendered) as usize, (until - rendered) as usize);
        let part: Vec<&[f32]> = planes.iter().map(|p| &p[a..b]).collect();
        for (meter, plane) in self.true_peaks.iter_mut().zip(&part) {
            meter.push(plane);
            self.peak = self.peak.max(meter::peak(plane));
        }
        self.loudness.push(&part)?;
        self.writer.write(&part)
    }

    /// Finishes the file and fills in what it measured.
    fn finish(mut self) -> Result<Stem> {
        self.writer.finish()?;
        let true_peak = self
            .true_peaks
            .iter_mut()
            .map(TruePeakMeter::finish)
            .fold(0.0f32, f32::max);
        let db = |linear: f32| (linear > 0.0).then(|| linear_to_db(linear));
        self.stem.peak_dbfs = db(self.peak);
        self.stem.true_peak_dbtp = db(true_peak);
        self.stem.loudness_lufs = self.loudness.integrated();
        Ok(self.stem)
    }
}

/// The render behind [`stems`], into a folder that exists and is empty.
fn write_stems(
    session: &mut Session,
    folder: &Path,
    name: &str,
    what: &Stems,
    frames: u64,
) -> Result<StemSet> {
    let rate = session.memory.sample_rate();
    let channels = session.channels;
    let block = session.engine.max_frames();
    let (mixer, handles, strips) =
        session::steps::build_mixer_with_strips(&session.memory, &session.audio, channels)?;
    mixer.clear_solo();
    handles.click.set_enabled(false);
    let master = session.memory.master().id();
    // Which strip node each stem reads, named after its track; the mix
    // keeps its name.
    let mut taken = vec!["mix".to_string()];
    let mut taps = Vec::new();
    for (track_id, strip) in &strips {
        let track = session.memory.track(*track_id)?;
        if *track_id == master || track.state.mute {
            continue;
        }
        let node = match strip {
            StripId::Track(id) => mixer.track(*id)?.strip_node,
            StripId::Bus(id) => mixer.bus(*id)?.strip_node,
        };
        let source = match track.kind {
            TrackKind::Audio => StemSource::Audio,
            TrackKind::Midi => StemSource::Midi,
            TrackKind::Bus => StemSource::Bus,
        };
        let output = track
            .output()
            .and_then(|bus| session.memory.track(bus).ok())
            .map(|bus| bus.name.clone());
        let stem_name = unique(&mut taken, &session::folder::file_name(&track.name));
        taps.push((node, track.name.clone(), stem_name, source, output));
    }
    let mut transport = Transport::with_tempo_map(session.memory.tempo_map().clone());
    transport.seek(what.start);
    transport.play();
    let mut engine = Engine::new(mixer, transport);
    engine.prepare(rate, block)?;

    let mut files = Vec::with_capacity(taps.len() + 1);
    let mut open = |node: Option<NodeId>, latency: u64, stem: Stem| -> Result<()> {
        files.push(StemFile {
            node,
            latency,
            writer: wav::Writer::create(folder.join(&stem.file), channels, rate, what.format)?,
            true_peaks: vec![TruePeakMeter::new(); channels],
            loudness: Loudness::new(rate, channels)?,
            peak: 0.0,
            stem,
        });
        Ok(())
    };
    let stem = |file: String, name: String, source: StemSource, output: Option<String>| Stem {
        file,
        name,
        source,
        output,
        channels,
        peak_dbfs: None,
        true_peak_dbtp: None,
        loudness_lufs: None,
    };
    for (node, track_name, stem_name, source, output) in taps {
        let latency = engine.processor().graph().node_output_latency(node)? as u64;
        open(
            Some(node),
            latency,
            stem(format!("{stem_name}.wav"), track_name, source, output),
        )?;
    }
    let mix_latency = engine.processor().graph().latency_frames() as u64;
    open(
        None,
        mix_latency,
        stem("mix.wav".into(), "mix".into(), StemSource::Mix, None),
    )?;

    // Render until the file with the longest path is full.
    let total = frames + files.iter().map(|f| f.latency).max().unwrap_or(0);
    let silence = vec![0.0f32; block];
    let mut out = PlanarBuffer::new(channels, block);
    let mut rendered = 0u64;
    while rendered < total {
        let n = (block as u64).min(total - rendered) as usize;
        out.set_frames(n)?;
        engine.render(&mut out)?;
        let graph = engine.processor().graph();
        for file in &mut files {
            match file.node {
                Some(node) => {
                    let planes: Vec<&[f32]> = if graph.node_is_silent(node)? {
                        vec![&silence[..n]; channels]
                    } else {
                        let buffer = graph.node_output(node)?;
                        (0..channels).map(|c| &buffer.channel(c)[..n]).collect()
                    };
                    file.take(&planes, rendered, frames)?;
                }
                None => {
                    let planes: Vec<&[f32]> = out.planes().collect();
                    file.take(&planes, rendered, frames)?;
                }
            }
        }
        rendered += n as u64;
    }

    let mut stems = Vec::with_capacity(files.len());
    for file in files {
        stems.push(file.finish()?);
    }
    let mix = stems.pop().expect("the mix is always last");
    let set = StemSet {
        format_version: STEM_SET_FORMAT_VERSION,
        name: name.to_string(),
        sample_rate: rate.hz(),
        bits: what.format.bits(),
        start: what.start.samples(),
        frames,
        stems,
        mix,
    };
    let json = serde_json::to_string_pretty(&set)
        .map_err(|e| AuraError::Message(format!("cannot encode the stem manifest: {e}")))?;
    let manifest = folder.join("manifest.json");
    std::fs::write(&manifest, json)
        .map_err(|e| AuraError::Message(format!("cannot write {}: {e}", manifest.display())))?;
    Ok(set)
}

/// `base`, or `base 2`, `base 3` ... when `taken` has it; the name returned
/// is added to `taken`.
fn unique(taken: &mut Vec<String>, base: &str) -> String {
    let mut name = base.to_string();
    let mut n = 1;
    while taken.iter().any(|t| t.eq_ignore_ascii_case(&name)) {
        n += 1;
        name = format!("{base} {n}");
    }
    taken.push(name.clone());
    name
}

#[cfg(feature = "files")]
fn resample(audio: &PlanarBuffer, from: SampleRate, to: SampleRate) -> Result<PlanarBuffer> {
    crate::primitives::io::files::resample(audio, from, to)
}

#[cfg(not(feature = "files"))]
fn resample(_audio: &PlanarBuffer, _from: SampleRate, _to: SampleRate) -> Result<PlanarBuffer> {
    Err(AuraError::Message(
        "converting a bounce to another sample rate needs the `files` feature".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::session::TrackKind;
    use crate::{clip, track, transport};

    const R: SampleRate = SampleRate::HZ_48K;

    fn tone_rig() -> Session {
        let mut session = session::create(session::scratch("bounce"), R, 256).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        let mut tone = PlanarBuffer::new(2, 1_000);
        tone.fill(0.5);
        session.audio.insert("tone.wav", tone);
        clip::add(
            &mut session,
            vox,
            "tone.wav",
            TimePosition::from_samples(500),
        )
        .unwrap();
        session
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("aura-export-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_default_bounce_covers_the_session_and_a_range_covers_itself() {
        let mut session = tone_rig();
        let dir = temp_dir("range");
        let path = dir.join("session.wav");
        let whole = bounce(&mut session, &path, Bounce::default()).unwrap();
        assert_eq!(whole.frames(), 1_500, "to the end of the last clip");
        assert!(whole.channel(0)[..500].iter().all(|&s| s == 0.0));
        assert!(whole.channel(0)[1_000].abs() > 0.3);
        let (read, rate) = wav::read(&path).unwrap();
        assert_eq!(rate, R);
        assert_eq!(read.frames(), 1_500);
        // Rendering did not disturb the live engine.
        assert_eq!(transport::playhead(&session), TimePosition::ZERO);

        let part = bounce(
            &mut session,
            dir.join("part.wav"),
            Bounce {
                start: TimePosition::from_samples(1_000),
                length: Some(TimeDuration::from_samples(2_000)),
                format: WavFormat::Float32,
                ..Bounce::default()
            },
        )
        .unwrap();
        assert_eq!(part.frames(), 2_000, "a given length runs past the clips");
        assert!(part.channel(0)[100].abs() > 0.3);
        assert!(part.channel(0)[600..].iter().all(|&s| s.abs() < 1e-6));
        let _ = std::fs::remove_dir_all(&dir);

        let mut empty = session::create(session::scratch("empty"), R, 64).unwrap();
        assert!(bounce(&mut empty, "unused.wav", Bounce::default()).is_err());
        let mut late = tone_rig();
        assert!(
            bounce(
                &mut late,
                "unused.wav",
                Bounce {
                    start: TimePosition::from_samples(5_000),
                    ..Bounce::default()
                }
            )
            .is_err(),
            "nothing after the start"
        );
    }

    #[test]
    fn a_bounce_leaves_the_click_out_and_makes_its_folder() {
        let mut session = tone_rig();
        crate::metronome::enable(&mut session, true).unwrap();
        let path = session::layout(&session)
            .bounces()
            .join("nested")
            .join("mix.wav");
        let audio = bounce(&mut session, &path, Bounce::default()).unwrap();
        assert!(
            audio.channel(0)[..500].iter().all(|&s| s == 0.0),
            "no click before the clip"
        );
        assert!(path.is_file());
    }

    /// vox (0.5 from 500) and gtr (0.25 from 0, into the bus "group") feed
    /// the master; "spare" is muted.
    fn stem_rig() -> (Session, crate::primitives::TrackId) {
        let mut session = session::create(session::scratch("stems"), R, 256).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        let gtr = track::add(&mut session, "gtr", TrackKind::Audio).unwrap();
        let spare = track::add(&mut session, "spare", TrackKind::Audio).unwrap();
        let group = track::add(&mut session, "group", TrackKind::Bus).unwrap();
        let mut half = PlanarBuffer::new(2, 1_000);
        half.fill(0.5);
        session.audio.insert("half.wav", half);
        let mut quarter = PlanarBuffer::new(2, 2_000);
        quarter.fill(0.25);
        session.audio.insert("quarter.wav", quarter);
        clip::add(
            &mut session,
            vox,
            "half.wav",
            TimePosition::from_samples(500),
        )
        .unwrap();
        clip::add(&mut session, gtr, "quarter.wav", TimePosition::ZERO).unwrap();
        clip::add(&mut session, spare, "half.wav", TimePosition::ZERO).unwrap();
        track::route(&mut session, gtr, Some(group)).unwrap();
        track::mute(&mut session, spare, true).unwrap();
        (session, vox)
    }

    #[test]
    fn stems_line_up_and_sum_to_the_mix() {
        let (mut session, _) = stem_rig();
        let set = stems(&mut session, "final", Stems::default()).unwrap();
        let folder = session::layout(&session).stem_set("final");
        assert_eq!(set.name, "final");
        assert_eq!(
            (set.sample_rate, set.bits, set.start, set.frames),
            (48_000, 24, 0, 2_000)
        );
        let names: Vec<&str> = set.stems.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            ["vox", "gtr", "group"],
            "the muted track has no stem"
        );
        assert!(!folder.join("spare.wav").exists());
        assert_eq!(set.stems[0].output, None, "vox feeds the master");
        assert_eq!(set.stems[1].output.as_deref(), Some("group"));
        assert_eq!(set.stems[2].source, StemSource::Bus);
        assert_eq!(set.mix.file, "mix.wav");
        assert_eq!(set.mix.source, StemSource::Mix);

        // The manifest on disk is what came back.
        let json = std::fs::read_to_string(folder.join("manifest.json")).unwrap();
        let read: StemSet = serde_json::from_str(&json).unwrap();
        assert_eq!(read, set);
        assert!(json.contains("\"source\": \"audio\""), "{json}");

        // Every file is as long as the set; vox starts where its clip does.
        let load = |file: &str| {
            let (audio, rate, format) = wav::read_with_format(folder.join(file)).unwrap();
            assert_eq!((rate, format), (R, WavFormat::Pcm24));
            assert_eq!(audio.frames(), 2_000, "{file}");
            audio
        };
        let (vox, group, mix) = (load("vox.wav"), load("group.wav"), load("mix.wav"));
        assert!(vox.channel(0)[..500].iter().all(|&s| s == 0.0));
        assert!(vox.channel(0)[600] > 0.3);
        // The stems that feed the master sum to the mix.
        for n in 0..2_000 {
            let sum = vox.channel(0)[n] + group.channel(0)[n];
            assert!(
                (sum - mix.channel(0)[n]).abs() < 1e-5,
                "frame {n}: {sum} vs {}",
                mix.channel(0)[n]
            );
        }
        // Measured: vox is louder than the guitar bus, and nothing clips.
        let (v, g) = (&set.stems[0], &set.stems[2]);
        assert!(v.peak_dbfs.unwrap() > g.peak_dbfs.unwrap());
        assert!(set.mix.true_peak_dbtp.unwrap() < 0.0);
        assert_eq!(
            set.mix.loudness_lufs, None,
            "2 000 frames is under the 400 ms loudness needs"
        );

        // A name is used once; a cleaned name lands in its cleaned folder.
        assert!(stems(&mut session, "final", Stems::default()).is_err());
        assert!(
            read == serde_json::from_str::<StemSet>(
                &std::fs::read_to_string(folder.join("manifest.json")).unwrap()
            )
            .unwrap(),
            "untouched"
        );
        let other = stems(
            &mut session,
            "v2/short",
            Stems {
                start: TimePosition::from_samples(400),
                length: Some(TimeDuration::from_samples(300)),
                format: WavFormat::Float32,
            },
        )
        .unwrap();
        assert_eq!((other.start, other.frames, other.bits), (400, 300, 32));
        let short = session::layout(&session).stem_set("v2-short");
        let (vox, _) = wav::read(short.join("vox.wav")).unwrap();
        assert_eq!(vox.frames(), 300);
        assert!(
            vox.channel(0)[..100].iter().all(|&s| s == 0.0),
            "the clip starts at 500"
        );
        assert!(vox.channel(0)[150] > 0.3);
        let _ = std::fs::remove_dir_all(&session.folder);
    }

    #[test]
    fn stems_ignore_solo_and_take_unique_file_names() {
        let (mut session, vox) = stem_rig();
        track::solo(&mut session, vox, true).unwrap();
        // A second track called "vox", and one called "mix".
        let again = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        let mix = track::add(&mut session, "mix", TrackKind::Audio).unwrap();
        clip::add(&mut session, again, "half.wav", TimePosition::ZERO).unwrap();
        clip::add(&mut session, mix, "half.wav", TimePosition::ZERO).unwrap();
        let set = stems(&mut session, "solo", Stems::default()).unwrap();
        let files: Vec<&str> = set.stems.iter().map(|s| s.file.as_str()).collect();
        assert_eq!(
            files,
            ["vox.wav", "gtr.wav", "group.wav", "vox 2.wav", "mix 2.wav"]
        );
        let gtr = &set.stems[1];
        assert!(gtr.peak_dbfs.is_some(), "solo did not silence the guitar");
        // Nothing to bounce is refused, and leaves no folder behind.
        assert!(stems(
            &mut session,
            "late",
            Stems {
                start: TimePosition::from_samples(10_000),
                ..Stems::default()
            }
        )
        .is_err());
        assert!(!session::layout(&session).stem_set("late").exists());
        assert_eq!(unique(&mut vec!["a".into(), "A 2".into()], "a"), "a 3");
        let _ = std::fs::remove_dir_all(&session.folder);
    }

    #[cfg(feature = "files")]
    #[test]
    fn a_bounce_converts_to_the_given_rate() {
        let mut session = tone_rig();
        let dir = temp_dir("rate");
        let path = dir.join("cd.wav");
        let out = bounce(
            &mut session,
            &path,
            Bounce {
                length: Some(TimeDuration::from_samples(4_800)),
                sample_rate: Some(SampleRate::CD),
                ..Bounce::default()
            },
        )
        .unwrap();
        assert_eq!(out.frames(), 4_410);
        assert_eq!(wav::read(&path).unwrap().1, SampleRate::CD);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(feature = "files"))]
    #[test]
    fn converting_without_the_files_feature_is_refused() {
        let mut session = tone_rig();
        assert!(bounce(
            &mut session,
            "unused.wav",
            Bounce {
                sample_rate: Some(SampleRate::CD),
                ..Bounce::default()
            }
        )
        .is_err());
        // The session's own rate is not a conversion.
        let dir = temp_dir("same-rate");
        assert!(bounce(
            &mut session,
            dir.join("same.wav"),
            Bounce {
                sample_rate: Some(R),
                ..Bounce::default()
            }
        )
        .is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
