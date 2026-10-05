//! Inserts: the effects in a track's chain, as the memory remembers them.

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
/// The memory stores what is needed to recreate the effect: which plugin,
/// its state (bypass and the parameter values the user has set), and the
/// plugin's own saved preset. Turning this into a running node is the job
/// of the session's steps, not the memory.
#[derive(Clone, Debug, PartialEq)]
pub struct Insert {
    pub(crate) id: InsertId,
    /// The plugin to load.
    pub plugin: PluginRef,
    /// Bypass and parameter values.
    pub state: InsertState,
    /// Opaque bytes the plugin saved for itself, or empty. Restored before
    /// the parameter values are applied.
    pub preset: Vec<u8>,
}

/// An insert's state: whether it is bypassed and the parameter values the
/// user has set.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct InsertState {
    /// A bypassed insert passes its input through unchanged.
    pub bypass: bool,
    /// Parameter values to apply after loading, as `(parameter id, plain
    /// value)`. Parameters not listed keep the plugin's defaults.
    pub params: Vec<(u32, f64)>,
}

impl Insert {
    /// An insert for `plugin` with no ID yet (the memory assigns one).
    pub fn new(plugin: PluginRef) -> Self {
        Self::with_id(InsertId::INVALID, plugin)
    }

    /// An insert with a pre-allocated ID (see
    /// [`Memory::allocate_insert_id`](crate::primitives::session::Memory::allocate_insert_id)).
    pub fn with_id(id: InsertId, plugin: PluginRef) -> Self {
        Self {
            id,
            plugin,
            state: InsertState::default(),
            preset: Vec::new(),
        }
    }

    /// The insert's ID (`InsertId::INVALID` until added to a memory).
    pub fn id(&self) -> InsertId {
        self.id
    }

    /// The stored value of a parameter, if the user has set one.
    pub fn param(&self, param: u32) -> Option<f64> {
        self.state
            .params
            .iter()
            .find(|(p, _)| *p == param)
            .map(|(_, v)| *v)
    }

    /// Stores a parameter value, replacing any earlier value for it.
    pub fn set_param(&mut self, param: u32, value: f64) {
        match self.state.params.iter_mut().find(|(p, _)| *p == param) {
            Some(entry) => entry.1 = value,
            None => self.state.params.push((param, value)),
        }
    }

    /// Forgets a stored parameter value so the plugin's default applies.
    /// Returns whether one was stored.
    pub fn clear_param(&mut self, param: u32) -> bool {
        let before = self.state.params.len();
        self.state.params.retain(|(p, _)| *p != param);
        self.state.params.len() != before
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
        assert_eq!(insert.state.params.len(), 2);
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
        assert!(!insert.state.bypass && insert.preset.is_empty());
    }
}
