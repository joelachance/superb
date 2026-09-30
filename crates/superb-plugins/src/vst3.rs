//! VST3 hosting — **stub**.
//!
//! Hosting VST3 plugins requires the Steinberg VST 3 SDK (or a compatible
//! re-implementation of its COM-style interfaces). Until that licence is
//! accepted for this project every entry point here returns
//! [`unsupported`], and [`AVAILABLE`] is `false`, so applications can show
//! an honest message instead of an empty plugin list.

use std::path::Path;

use superb_core::{Result, SuperbError};

use crate::PluginInstance;

/// `false`: VST3 hosting is not compiled in.
pub const AVAILABLE: bool = false;

/// The error every VST3 entry point returns.
pub fn unsupported() -> SuperbError {
    SuperbError::Message(
        "VST3 hosting is a stub: the Steinberg VST 3 SDK licence has not been accepted for this project (see docs/superb-plugins.md)".into(),
    )
}

/// Would load a `.vst3` bundle. Always fails with [`unsupported`].
pub fn load(path: impl AsRef<Path>) -> Result<Box<dyn PluginInstance>> {
    let _ = path.as_ref();
    Err(unsupported())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_is_unsupported() {
        assert!(!AVAILABLE);
        let err = load("/nonexistent/plugin.vst3").err().unwrap();
        assert!(err.to_string().contains("VST3"));
        assert_eq!(err, unsupported());
    }
}
