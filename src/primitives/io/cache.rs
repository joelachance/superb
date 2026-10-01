//! Audio held in memory for the console to play.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::primitives::buffers::PlanarBuffer;
#[cfg(feature = "files")]
use crate::primitives::{Result, SampleRate};

/// Audio held in memory, keyed by the path clips refer to.
///
/// A clip's `ClipSource::Audio { path }` is looked up here when the console
/// is built; a missing entry plays as silence. Fill it with decoded files
/// (`load`, feature `files`) or with buffers you made.
#[derive(Clone, Debug, Default)]
pub struct AudioCache {
    entries: HashMap<PathBuf, Arc<PlanarBuffer>>,
}

impl AudioCache {
    /// Empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts (or replaces) a buffer for `path`, returning the shared handle.
    pub fn insert(&mut self, path: impl Into<PathBuf>, buffer: PlanarBuffer) -> Arc<PlanarBuffer> {
        let arc = Arc::new(buffer);
        self.entries.insert(path.into(), Arc::clone(&arc));
        arc
    }

    /// Shared buffer for `path`, if present.
    pub fn get(&self, path: &Path) -> Option<Arc<PlanarBuffer>> {
        self.entries.get(path).map(Arc::clone)
    }

    /// Number of cached files.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Decodes `path` at `target_rate` (resampled when needed) and caches it.
    ///
    /// Needs the `files` feature (symphonia + rubato).
    #[cfg(feature = "files")]
    pub fn load(
        &mut self,
        path: impl AsRef<Path>,
        target_rate: SampleRate,
    ) -> Result<Arc<PlanarBuffer>> {
        let path = path.as_ref();
        if let Some(existing) = self.get(path) {
            return Ok(existing);
        }
        let file = crate::primitives::io::files::decode_at_rate(path, target_rate)?;
        Ok(self.insert(path.to_path_buf(), file.buffer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_and_replace() {
        let mut cache = AudioCache::new();
        assert!(cache.is_empty());
        let first = cache.insert("a.wav", PlanarBuffer::new(2, 10));
        assert_eq!(cache.len(), 1);
        assert!(Arc::ptr_eq(&cache.get(Path::new("a.wav")).unwrap(), &first));
        let second = cache.insert("a.wav", PlanarBuffer::new(2, 20));
        assert_eq!(cache.len(), 1, "same key replaces");
        assert_eq!(cache.get(Path::new("a.wav")).unwrap().frames(), 20);
        assert!(!Arc::ptr_eq(&first, &second));
        assert!(cache.get(Path::new("missing.wav")).is_none());
    }
}
