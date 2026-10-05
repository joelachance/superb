//! Getting the session out: bounce it, or a range of it, to a WAV file.
//!
//! A bounce builds its own copy of the mixer and runs it offline, as fast
//! as the machine allows. The running engine is not touched, so you can
//! bounce while listening.
//!
//! Made of [`session::steps::build_mixer`]
//! (the mixer), `superb::primitives::io::offline` (the render loop),
//! `superb::primitives::io::files` (resampling, feature `files`) and
//! `superb::primitives::io::wav` (the file).

use std::path::Path;

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::io::wav::{self, WavFormat};
use crate::primitives::io::{offline, Engine};
use crate::primitives::transport::Transport;
use crate::primitives::{Result, SampleRate, SuperbError, TimeDuration, TimePosition};
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
/// file at `path`. Returns the audio written. Fails when there is nothing
/// to render: no clips after `start` and no `length` given.
///
/// Made of: `Document::end_position` for the length when none is given →
/// [`session::steps::build_mixer`] → a `Transport` on the session's tempo
/// map, sought to `start` and playing → `Engine::new` → `offline::render`
/// → `files::resample` when a rate is given → `wav::write`.
pub fn bounce(session: &mut Session, path: impl AsRef<Path>, what: Bounce) -> Result<PlanarBuffer> {
    let length = match what.length {
        Some(length) => length,
        None => {
            let end = session.document.end_position();
            TimeDuration::from_samples(end.samples() - what.start.samples())
        }
    };
    if length.samples() <= 0 {
        return Err(SuperbError::Message(
            "nothing to bounce: no clips after the start, and no length given".into(),
        ));
    }
    let audio = render(session, what.start, length.samples() as usize)?;
    let session_rate = session.document.sample_rate();
    let (audio, rate) = match what.sample_rate {
        Some(rate) if rate != session_rate => (resample(&audio, session_rate, rate)?, rate),
        _ => (audio, session_rate),
    };
    wav::write(path, &audio, rate, what.format)?;
    Ok(audio)
}

/// Renders `frames` frames of the session starting at `start` into memory
/// through a fresh offline mixer.
pub(crate) fn render(
    session: &mut Session,
    start: TimePosition,
    frames: usize,
) -> Result<PlanarBuffer> {
    let (mixer, _handles) =
        session::steps::build_mixer(&session.document, &session.audio, session.channels)?;
    let mut transport = Transport::with_tempo_map(session.document.tempo_map().clone());
    transport.seek(start);
    transport.play();
    let mut engine = Engine::new(mixer, transport);
    offline::render(
        &mut engine,
        session.document.sample_rate(),
        session.engine.max_frames(),
        frames,
    )
}

#[cfg(feature = "files")]
fn resample(audio: &PlanarBuffer, from: SampleRate, to: SampleRate) -> Result<PlanarBuffer> {
    crate::primitives::io::files::resample(audio, from, to)
}

#[cfg(not(feature = "files"))]
fn resample(_audio: &PlanarBuffer, _from: SampleRate, _to: SampleRate) -> Result<PlanarBuffer> {
    Err(SuperbError::Message(
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
        let mut session = session::create("bounce", R, 256).unwrap();
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
        let dir = std::env::temp_dir().join(format!("superb-export-{tag}-{}", std::process::id()));
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

        let mut empty = session::create("empty", R, 64).unwrap();
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
