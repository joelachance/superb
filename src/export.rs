//! Getting the document out: render a range or the whole document to a file.
//!
//! Export builds its own copy of the console (inserts included) and drives
//! it offline, as fast as the machine allows. The running engine is not
//! touched, so you can bounce while listening.
//!
//! Made of [`session::steps`](crate::session::steps#functions) (the mixer),
//! `superb::primitives::io::offline` (the render loop) and
//! `superb::primitives::io::wav` (the file).

use std::path::Path;

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::io::wav::{self, WavFormat};
use crate::primitives::io::{offline, Engine};
use crate::primitives::transport::Transport;
use crate::primitives::{Result, SuperbError, TimePosition};
use crate::session;
use crate::session::Session;

/// Renders `frames` frames of the document starting at `start` into memory.
///
/// Made of: [`session::steps::build_mixer`] → [`session::steps::attach_inserts`]
/// → a `Transport` on the document's tempo map, sought to `start` and playing →
/// `Engine::new` → `offline::render`.
pub fn render(session: &mut Session, start: TimePosition, frames: usize) -> Result<PlanarBuffer> {
    let (mut mixer, player) =
        session::steps::build_mixer(&session.document, &session.cache, session.channels)?;
    let (_live, failed) = session::steps::attach_inserts(
        &session.document,
        &mut session.plugins,
        &mut mixer,
        &player,
    );
    if let Some((id, why)) = failed.first() {
        return Err(SuperbError::Message(format!(
            "cannot export: insert {id} did not load ({why})"
        )));
    }
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

/// Renders a range and writes it as a WAV file at the document's sample rate.
/// Returns the audio as well.
///
/// Made of: [`render`] → `wav::write`.
pub fn bounce(
    session: &mut Session,
    path: impl AsRef<Path>,
    start: TimePosition,
    frames: usize,
    format: WavFormat,
) -> Result<PlanarBuffer> {
    let audio = render(session, start, frames)?;
    wav::write(path, &audio, session.document.sample_rate(), format)?;
    Ok(audio)
}

/// Bounces the whole document, from the start to the end of the last clip.
/// Fails if the document has no clips.
///
/// Made of: `Document::end_position` → [`bounce`].
pub fn bounce_session(
    session: &mut Session,
    path: impl AsRef<Path>,
    format: WavFormat,
) -> Result<PlanarBuffer> {
    let end = session.document.end_position().samples();
    if end <= 0 {
        return Err(SuperbError::Message(
            "the document has no clips to bounce".into(),
        ));
    }
    bounce(session, path, TimePosition::ZERO, end as usize, format)
}

/// Renders a range, converts it to `rate` and writes it as WAV.
///
/// Made of: [`render`] → `files::resample` → `wav::write`.
#[cfg(feature = "files")]
pub fn bounce_at_rate(
    session: &mut Session,
    path: impl AsRef<Path>,
    start: TimePosition,
    frames: usize,
    rate: crate::primitives::SampleRate,
    format: WavFormat,
) -> Result<PlanarBuffer> {
    let audio = render(session, start, frames)?;
    let converted =
        crate::primitives::io::files::resample(&audio, session.document.sample_rate(), rate)?;
    wav::write(path, &converted, rate, format)?;
    Ok(converted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::session::TrackKind;
    use crate::primitives::{SampleRate, TimeDuration};
    use crate::transport;

    const R: SampleRate = SampleRate::HZ_48K;

    fn tone_rig() -> Session {
        let mut session = session::create("bounce", R, 256).unwrap();
        let vox = session::add_track(&mut session, "vox", TrackKind::Audio).unwrap();
        let mut tone = PlanarBuffer::new(2, 1_000);
        tone.fill(0.5);
        session.cache.insert("tone.wav", tone);
        session::add_audio_clip(
            &mut session,
            vox,
            "tone.wav",
            TimePosition::from_samples(500),
            TimeDuration::from_samples(1_000),
        )
        .unwrap();
        session
    }

    #[test]
    fn render_and_bounce_cover_the_requested_range() {
        let mut session = tone_rig();
        let audio = render(&mut session, TimePosition::ZERO, 2_000).unwrap();
        assert_eq!(audio.frames(), 2_000);
        assert!(audio.channel(0)[..500].iter().all(|&s| s == 0.0));
        assert!(audio.channel(0)[1_000].abs() > 0.3);
        assert!(audio.channel(0)[1_600..].iter().all(|&s| s.abs() < 1e-6));
        // Rendering did not disturb the live engine.
        assert_eq!(transport::playhead(&session), TimePosition::ZERO);

        let dir = std::env::temp_dir().join(format!("superb-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("document.wav");
        let bounced = bounce_session(&mut session, &path, WavFormat::Float32).unwrap();
        assert_eq!(bounced.frames(), 1_500, "to the end of the last clip");
        let (read, rate) = wav::read(&path).unwrap();
        assert_eq!(rate, R);
        assert_eq!(read.channel(0), bounced.channel(0));
        let _ = std::fs::remove_dir_all(&dir);

        let empty = session::create("empty", R, 64).unwrap();
        let mut empty = empty;
        assert!(bounce_session(&mut empty, "unused.wav", WavFormat::Pcm16).is_err());
    }

    #[cfg(feature = "files")]
    #[test]
    fn bounce_at_rate_converts() {
        let mut session = tone_rig();
        let dir = std::env::temp_dir().join(format!("superb-export-rate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cd.wav");
        let out = bounce_at_rate(
            &mut session,
            &path,
            TimePosition::ZERO,
            4_800,
            SampleRate::CD,
            WavFormat::Pcm16,
        )
        .unwrap();
        assert_eq!(out.frames(), 4_410);
        assert_eq!(wav::read(&path).unwrap().1, SampleRate::CD);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
