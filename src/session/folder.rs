//! The session folder: `memory.json` beside an `audio/` folder of WAV files.
//!
//! A clip's audio key is a path relative to this folder. Recordings are
//! written there as they stop (`audio/vox 1.wav`), `save` writes whatever a
//! clip refers to that is not on disk yet, and `open` reads it all back.
//! Undoing a recording deletes the files it wrote; nothing else deletes a
//! file.

use std::path::{Path, PathBuf};

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::io::wav::{self, WavFormat};
use crate::primitives::session::ClipSource;
use crate::primitives::{AuraError, Result, SampleRate};
use crate::session::Session;

/// The memory's file inside the session folder.
pub(crate) const MEMORY_FILE: &str = "memory.json";
/// Where recordings go inside the session folder.
pub(crate) const AUDIO_DIR: &str = "audio";

/// Writes `audio` as a 32-bit float WAV at `key` inside `folder`, making
/// the folders on the way.
pub(crate) fn write_audio(
    folder: &Path,
    key: &Path,
    audio: &PlanarBuffer,
    rate: SampleRate,
) -> Result<()> {
    let path = folder.join(key);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AuraError::Message(format!("cannot create {}: {e}", parent.display())))?;
    }
    wav::write(&path, audio, rate, WavFormat::Float32)
}

/// Writes every buffer a clip refers to that is not yet a file in `folder`.
/// `audio/<stem>.wav`, or `audio/<stem> 2.wav` and on, that nothing uses
/// yet in memory or on disk.
pub(crate) fn free_key(session: &Session, stem: &str) -> PathBuf {
    let mut n = 1;
    loop {
        let name = if n == 1 {
            format!("{AUDIO_DIR}/{stem}.wav")
        } else {
            format!("{AUDIO_DIR}/{stem} {n}.wav")
        };
        let key = PathBuf::from(name);
        if session.audio.get(&key).is_none() && !session.folder.join(&key).exists() {
            return key;
        }
        n += 1;
    }
}

pub(crate) fn write_missing_audio(session: &Session, folder: &Path) -> Result<()> {
    let rate = session.memory.sample_rate();
    for track in session.memory.tracks() {
        for clip in track.clips() {
            let ClipSource::Audio { path, .. } = &clip.source else {
                continue;
            };
            if folder.join(path).exists() {
                continue;
            }
            if let Some(audio) = session.audio.get(path) {
                write_audio(folder, path, &audio, rate)?;
            }
        }
    }
    Ok(())
}

/// Loads every WAV file a clip refers to from `folder` into the session's
/// audio. A file that is missing, unreadable or at another sample rate is
/// left out: its clip plays as silence.
pub(crate) fn load_audio(session: &mut Session, folder: &Path) {
    let rate = session.memory.sample_rate();
    let keys: Vec<PathBuf> = session
        .memory
        .tracks()
        .iter()
        .flat_map(|t| t.clips())
        .filter_map(|c| match &c.source {
            ClipSource::Audio { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect();
    for key in keys {
        if session.audio.get(&key).is_some() {
            continue;
        }
        if let Ok((audio, file_rate)) = wav::read(folder.join(&key)) {
            if file_rate == rate {
                session.audio.insert(key, audio);
            }
        }
    }
}
