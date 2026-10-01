//! Inserts: the effects in a track's chain, as the document remembers them.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::primitives::plugins::{PluginDescriptor, PluginFormat};
use crate::primitives::InsertId;

/// Which plugin an insert refers to: enough to load it again from disk.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PluginRef {
    /// The plugin API the bundle speaks.
    pub format: PluginFormat,
    /// Bundle the plugin lives in.
    pub path: PathBuf,
    /// The plugin's stable ID inside that bundle (for example `com.vendor.plugin`).
    pub id: String,
}

impl From<&PluginDescriptor> for PluginRef {
    fn from(desc: &PluginDescriptor) -> Self {
        Self {
            format: desc.format,
            path: desc.path.clone(),
            id: desc.id.clone(),
        }
    }
}

/// One effect in a track's insert chain.
///
/// The document stores what is needed to recreate the effect: which plugin,
/// whether it is bypassed, the parameter values the user has set, and the
/// plugin's own saved state. Turning this into a running node is the job of
/// the playback layer, not the document.
#[derive(Clone, Debug, PartialEq)]
pub struct Insert {
    pub(crate) id: InsertId,
    /// The plugin to load.
    pub plugin: PluginRef,
    /// A bypassed insert passes its input through unchanged.
    pub bypassed: bool,
    /// Parameter values to apply after loading, as `(parameter id, plain
    /// value)`. Parameters not listed keep the plugin's defaults.
    pub params: Vec<(u32, f64)>,
    /// Opaque state saved by the plugin itself, or empty.
    pub state: Vec<u8>,
}

impl Insert {
    /// An insert for `plugin` with no ID yet (the document assigns one).
    pub fn new(plugin: PluginRef) -> Self {
        Self::with_id(InsertId::INVALID, plugin)
    }

    /// An insert with a pre-allocated ID (see
    /// [`Document::allocate_insert_id`](crate::primitives::session::Document::allocate_insert_id)).
    pub fn with_id(id: InsertId, plugin: PluginRef) -> Self {
        Self {
            id,
            plugin,
            bypassed: false,
            params: Vec::new(),
            state: Vec::new(),
        }
    }

    /// The insert's ID (`InsertId::INVALID` until added to a document).
    pub fn id(&self) -> InsertId {
        self.id
    }

    /// The stored value of a parameter, if the user has set one.
    pub fn param(&self, param: u32) -> Option<f64> {
        self.params
            .iter()
            .find(|(p, _)| *p == param)
            .map(|(_, v)| *v)
    }

    /// Stores a parameter value, replacing any earlier value for it.
    pub fn set_param(&mut self, param: u32, value: f64) {
        match self.params.iter_mut().find(|(p, _)| *p == param) {
            Some(entry) => entry.1 = value,
            None => self.params.push((param, value)),
        }
    }

    /// Forgets a stored parameter value so the plugin's default applies.
    /// Returns whether one was stored.
    pub fn clear_param(&mut self, param: u32) -> bool {
        let before = self.params.len();
        self.params.retain(|(p, _)| *p != param);
        self.params.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin() -> PluginRef {
        PluginRef {
            format: PluginFormat::Clap,
            path: "/plugins/verb.clap".into(),
            id: "com.example.verb".into(),
        }
    }

    #[test]
    fn params_are_upserted_and_cleared() {
        let mut insert = Insert::new(plugin());
        assert_eq!(insert.id(), InsertId::INVALID);
        assert_eq!(insert.param(7), None);
        insert.set_param(7, 0.5);
        insert.set_param(9, 1.0);
        insert.set_param(7, 0.25);
        assert_eq!(insert.param(7), Some(0.25));
        assert_eq!(insert.params.len(), 2);
        assert!(insert.clear_param(7));
        assert!(!insert.clear_param(7));
        assert_eq!(insert.param(9), Some(1.0));
    }

    #[test]
    fn plugin_ref_comes_from_a_descriptor() {
        let mut desc = PluginDescriptor::new(PluginFormat::Clap, "com.example.verb", "Verb");
        desc.path = "/plugins/verb.clap".into();
        assert_eq!(PluginRef::from(&desc), plugin());
        let insert = Insert::with_id(InsertId::from_raw(3), plugin());
        assert_eq!(insert.id(), InsertId::from_raw(3));
        assert!(!insert.bypassed && insert.state.is_empty());
    }
}
