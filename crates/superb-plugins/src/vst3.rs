//! VST3 hosting — **stub**.
//!
//! Hosting VST3 plugins means binding to the format's COM-style interfaces,
//! and shipping such a host means accepting the VST 3 SDK licence. Neither
//! has been done here. Every entry point returns [`unsupported`] and
//! [`AVAILABLE`] is `false`, so an application can show an honest message
//! instead of an empty plugin list.

use std::path::Path;

use superb_core::{Result, SuperbError};

use crate::PluginInstance;

/// `false`: VST3 hosting is not compiled in.
pub const AVAILABLE: bool = false;

/// The error every VST3 entry point returns.
pub fn unsupported() -> SuperbError {
    SuperbError::Message(
        "VST3 hosting is not implemented; only CLAP plugins can be loaded (see docs/superb-plugins.md)".into(),
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
        let err = load("/nonexistent/plugin.vst3").err().unwrap();
        assert!(err.to_string().contains("VST3"));
        assert_eq!(err, unsupported());
    }
}
