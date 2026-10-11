//! Where files go: the folder of a session, and the root sessions live
//! under.
//!
//! Every session is a folder with the same shape ([`Layout`]):
//!
//! ```text
//! <session>/
//!   memory.json     the session's edits (session::save)
//!   state.json      its state, once session::state_file names this file
//!   audio/          what clips play: recordings and imports, 32-bit float
//!   bounces/        mixes from export::bounce
//!   stems/<name>/   stem sets from export::stems, each with a manifest.json
//! ```
//!
//! and sessions live side by side under a [`Root`]: `~/aura` unless the
//! application keeps them elsewhere. A clip's audio key is a path relative
//! to the session folder (`audio/vox 1.wav`). Recordings stream into
//! `audio/` as they run, `save` writes whatever a clip refers to that is
//! not on disk yet, and `open` reads it all back. Undoing a recording
//! deletes the files it wrote; nothing else deletes a file.

use std::path::{Path, PathBuf};

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::io::wav::{self, WavFormat};
use crate::primitives::session::ClipSource;
use crate::primitives::{AuraError, Result, SampleRate};
use crate::session::Session;

/// The memory's file inside the session folder.
pub(crate) const MEMORY_FILE: &str = "memory.json";
/// Where state goes once the application names it.
pub(crate) const STATE_FILE: &str = "state.json";
/// Where recordings and imports go inside the session folder.
pub(crate) const AUDIO_DIR: &str = "audio";
/// Where bounces go.
pub(crate) const BOUNCES_DIR: &str = "bounces";
/// Where stem sets go, one folder each.
pub(crate) const STEMS_DIR: &str = "stems";
/// The folder in the user's home folder that sessions live in by default.
pub(crate) const DEFAULT_ROOT: &str = "aura";

/// Where sessions live: one folder per session, side by side. `~/aura` by
/// default ([`Root::default`]); an application that keeps sessions
/// elsewhere (another disk, a folder the user picked) names its own with
/// [`Root::new`]. A root is only a path: nothing is made until a session
/// is created in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    path: PathBuf,
}

impl Root {
    /// A root at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Root { path: path.into() }
    }

    /// The folder sessions live in.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The folder a session called `name` has under this root, to hand to
    /// `session::create` or `session::open`. Characters a file name cannot
    /// hold (`/ \ : * ? " < > |`) become `-`.
    pub fn folder(&self, name: &str) -> PathBuf {
        self.path.join(file_name(name))
    }

    /// The sessions under this root: every folder directly inside it that
    /// holds a `memory.json`, in name order. Empty when the root does not
    /// exist yet.
    ///
    /// # Errors
    /// The root exists but cannot be read.
    pub fn sessions(&self) -> Result<Vec<PathBuf>> {
        let entries = match std::fs::read_dir(&self.path) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(AuraError::Message(format!(
                    "cannot read {}: {e}",
                    self.path.display()
                )))
            }
        };
        let mut sessions: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.join(MEMORY_FILE).is_file())
            .collect();
        sessions.sort();
        Ok(sessions)
    }
}

impl Default for Root {
    /// `aura` in the user's home folder (`$HOME`, or `%USERPROFILE%` on
    /// Windows), or in the current folder when neither is set.
    fn default() -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_default();
        Root::new(home.join(DEFAULT_ROOT))
    }
}

/// Where each part of a session lives inside its folder. Every path is
/// worked out from the folder; nothing here touches the disk.
/// `session::layout` gives the one for an open session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    folder: PathBuf,
}

impl Layout {
    /// The layout of the session folder `folder`.
    pub fn new(folder: impl Into<PathBuf>) -> Self {
        Layout {
            folder: folder.into(),
        }
    }

    /// The session folder.
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// `memory.json`: the session's edits.
    pub fn memory(&self) -> PathBuf {
        self.folder.join(MEMORY_FILE)
    }

    /// `state.json`: where the session's state goes once
    /// `session::state_file` names this file. Nothing writes it before.
    pub fn state(&self) -> PathBuf {
        self.folder.join(STATE_FILE)
    }

    /// `audio/`: what clips play, recordings and imports.
    pub fn audio(&self) -> PathBuf {
        self.folder.join(AUDIO_DIR)
    }

    /// `bounces/`: mixes.
    pub fn bounces(&self) -> PathBuf {
        self.folder.join(BOUNCES_DIR)
    }

    /// `bounces/<name>.wav`: a bounce called `name`, for `export::bounce`.
    pub fn bounce(&self, name: &str) -> PathBuf {
        self.bounces().join(format!("{}.wav", file_name(name)))
    }

    /// `stems/`: stem sets, one folder each.
    pub fn stems(&self) -> PathBuf {
        self.folder.join(STEMS_DIR)
    }

    /// `stems/<name>/`: the stem set called `name`, as `export::stems`
    /// writes it.
    pub fn stem_set(&self, name: &str) -> PathBuf {
        self.stems().join(file_name(name))
    }
}

/// `name` as a file name: characters a file name cannot hold on any of
/// the systems sessions move between become `-`, and an empty name is
/// `untitled`.
pub(crate) fn file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').trim();
    if cleaned.is_empty() {
        "untitled".into()
    } else {
        cleaned.to_string()
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session;

    #[test]
    fn a_layout_names_every_part_of_a_session() {
        let layout = Layout::new("/music/My Show");
        assert_eq!(layout.folder(), Path::new("/music/My Show"));
        assert_eq!(layout.memory(), Path::new("/music/My Show/memory.json"));
        assert_eq!(layout.state(), Path::new("/music/My Show/state.json"));
        assert_eq!(layout.audio(), Path::new("/music/My Show/audio"));
        assert_eq!(layout.bounces(), Path::new("/music/My Show/bounces"));
        assert_eq!(
            layout.bounce("rough mix"),
            Path::new("/music/My Show/bounces/rough mix.wav")
        );
        assert_eq!(layout.stems(), Path::new("/music/My Show/stems"));
        assert_eq!(
            layout.stem_set("final"),
            Path::new("/music/My Show/stems/final")
        );
        assert_eq!(
            layout.stem_set("v2/final"),
            Path::new("/music/My Show/stems/v2-final")
        );
    }

    #[test]
    fn names_become_file_names() {
        assert_eq!(file_name("My Show"), "My Show");
        assert_eq!(file_name("a/b\\c:d*e?f\"g<h>i|j"), "a-b-c-d-e-f-g-h-i-j");
        assert_eq!(file_name("  ..hidden.. "), "hidden");
        assert_eq!(file_name(""), "untitled");
        assert_eq!(file_name("..."), "untitled");
        assert_eq!(file_name("tab\there"), "tab-here");
    }

    #[test]
    fn a_root_holds_sessions_side_by_side() {
        let default = Root::default();
        assert!(
            default.path().ends_with("aura"),
            "{}",
            default.path().display()
        );
        assert_eq!(default, Root::default());

        let root = Root::new(session::scratch("root"));
        assert!(
            root.sessions().unwrap().is_empty(),
            "no root folder yet: no sessions"
        );
        assert_eq!(root.folder("Ep: 1"), root.path().join("Ep- 1"));
        let b = session::create(root.folder("B side"), SampleRate::HZ_48K, 64).unwrap();
        let a = session::create(root.folder("A side"), SampleRate::HZ_48K, 64).unwrap();
        // A folder without a memory is not a session.
        std::fs::create_dir_all(root.path().join("not a session")).unwrap();
        std::fs::write(root.path().join("notes.txt"), "hi").unwrap();
        assert_eq!(
            root.sessions().unwrap(),
            vec![a.folder.clone(), b.folder.clone()]
        );
        // A session is made with every folder of its layout.
        let layout = session::layout(&a);
        for dir in [layout.audio(), layout.bounces(), layout.stems()] {
            assert!(dir.is_dir(), "{}", dir.display());
        }
        assert!(layout.memory().is_file());
        assert!(
            !layout.state().exists(),
            "state is not written until it is named"
        );
        let _ = std::fs::remove_dir_all(root.path());
    }
}
