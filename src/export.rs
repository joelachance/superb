//! Getting the song out: render a range or the whole song to a file.
//!
//! Export builds its own copy of the console (inserts included) and drives
//! it offline, as fast as the machine allows. The running engine is not
//! touched, so you can bounce while listening.
//!
//! Made of [`play::steps`](crate::play::steps#functions) (the console),
//! `superb::primitives::io::offline` (the render loop) and
//! `superb::primitives::io::wav` (the file).

use std::path::Path;

use crate::play;
use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::io::wav::{self, WavFormat};
use crate::primitives::io::{offline, Engine};
use crate::primitives::transport::Transport;
use crate::primitives::{Result, SuperbError, TimePosition};
use crate::rig::Rig;

/// Renders `frames` frames of the song starting at `start` into memory.
///
/// Made of: [`play::steps::build_console`] → [`play::steps::attach_inserts`]
/// → a `Transport` on the song's tempo map, sought to `start` and playing →
/// `Engine::new` → `offline::render`.
pub fn render(rig: &mut Rig, start: TimePosition, frames: usize) -> Result<PlanarBuffer> {
    let (mut mixer, player) = play::steps::build_console(&rig.session, &rig.cache, rig.channels)?;
    let (_live, failed) =
        play::steps::attach_inserts(&rig.session, &mut rig.plugins, &mut mixer, &player);
    if let Some((id, why)) = failed.first() {
        return Err(SuperbError::Message(format!(
            "cannot export: insert {id} did not load ({why})"
        )));
    }
    let mut transport = Transport::with_tempo_map(rig.session.tempo_map().clone());
    transport.seek(start);
    transport.play();
    let mut engine = Engine::new(mixer, transport);
    offline::render(
        &mut engine,
        rig.session.sample_rate(),
        rig.engine.max_frames(),
        frames,
    )
}

/// Renders a range and writes it as a WAV file at the song's sample rate.
/// Returns the audio as well.
///
/// Made of: [`render`] → `wav::write`.
pub fn bounce(
    rig: &mut Rig,
    path: impl AsRef<Path>,
    start: TimePosition,
    frames: usize,
    format: WavFormat,
) -> Result<PlanarBuffer> {
    let audio = render(rig, start, frames)?;
    wav::write(path, &audio, rig.session.sample_rate(), format)?;
    Ok(audio)
}

/// Bounces the whole song, from the start to the end of the last clip.
/// Fails if the song has no clips.
///
/// Made of: `Session::end_position` → [`bounce`].
pub fn bounce_song(
    rig: &mut Rig,
    path: impl AsRef<Path>,
    format: WavFormat,
) -> Result<PlanarBuffer> {
    let end = rig.session.end_position().samples();
    if end <= 0 {
        return Err(SuperbError::Message(
            "the song has no clips to bounce".into(),
        ));
    }
    bounce(rig, path, TimePosition::ZERO, end as usize, format)
}

/// Renders a range, converts it to `rate` and writes it as WAV.
///
/// Made of: [`render`] → `files::resample` → `wav::write`.
#[cfg(feature = "files")]
pub fn bounce_at_rate(
    rig: &mut Rig,
    path: impl AsRef<Path>,
    start: TimePosition,
    frames: usize,
    rate: crate::primitives::SampleRate,
    format: WavFormat,
) -> Result<PlanarBuffer> {
    let audio = render(rig, start, frames)?;
    let converted =
        crate::primitives::io::files::resample(&audio, rig.session.sample_rate(), rate)?;
    wav::write(path, &converted, rate, format)?;
    Ok(converted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::session::TrackKind;
    use crate::primitives::{SampleRate, TimeDuration};
    use crate::project;

    const R: SampleRate = SampleRate::HZ_48K;

    fn tone_rig() -> Rig {
        let mut rig = project::create("bounce", R, 256).unwrap();
        let vox = project::add_track(&mut rig, "vox", TrackKind::Audio).unwrap();
        let mut tone = PlanarBuffer::new(2, 1_000);
        tone.fill(0.5);
        rig.cache.insert("tone.wav", tone);
        project::add_audio_clip(
            &mut rig,
            vox,
            "tone.wav",
            TimePosition::from_samples(500),
            TimeDuration::from_samples(1_000),
        )
        .unwrap();
        rig
    }

    #[test]
    fn render_and_bounce_cover_the_requested_range() {
        let mut rig = tone_rig();
        let audio = render(&mut rig, TimePosition::ZERO, 2_000).unwrap();
        assert_eq!(audio.frames(), 2_000);
        assert!(audio.channel(0)[..500].iter().all(|&s| s == 0.0));
        assert!(audio.channel(0)[1_000].abs() > 0.3);
        assert!(audio.channel(0)[1_600..].iter().all(|&s| s.abs() < 1e-6));
        // Rendering did not disturb the live engine.
        assert_eq!(play::position(&rig), TimePosition::ZERO);

        let dir = std::env::temp_dir().join(format!("superb-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("song.wav");
        let bounced = bounce_song(&mut rig, &path, WavFormat::Float32).unwrap();
        assert_eq!(bounced.frames(), 1_500, "to the end of the last clip");
        let (read, rate) = wav::read(&path).unwrap();
        assert_eq!(rate, R);
        assert_eq!(read.channel(0), bounced.channel(0));
        let _ = std::fs::remove_dir_all(&dir);

        let empty = project::create("empty", R, 64).unwrap();
        let mut empty = empty;
        assert!(bounce_song(&mut empty, "unused.wav", WavFormat::Pcm16).is_err());
    }

    #[cfg(feature = "files")]
    #[test]
    fn bounce_at_rate_converts() {
        let mut rig = tone_rig();
        let dir = std::env::temp_dir().join(format!("superb-export-rate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cd.wav");
        let out = bounce_at_rate(
            &mut rig,
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
