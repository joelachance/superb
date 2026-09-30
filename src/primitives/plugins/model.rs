//! Format-agnostic plugin model.

use std::path::PathBuf;

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::graph::{ProcessContext, ProcessStatus};
use crate::primitives::{Result, SampleRate};

/// A plugin API.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PluginFormat {
    /// CLAP (`.clap` bundles).
    Clap,
    /// VST3 — stubbed, see [`crate::primitives::plugins::vst3`].
    Vst3,
}

impl PluginFormat {
    /// File extension of a bundle of this format (without the dot).
    pub fn extension(self) -> &'static str {
        match self {
            Self::Clap => "clap",
            Self::Vst3 => "vst3",
        }
    }
}

/// What a plugin says about itself before it is instantiated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginDescriptor {
    /// Format the plugin was found in.
    pub format: PluginFormat,
    /// Stable, unique ID (e.g. `com.vendor.plugin`).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Vendor / author.
    pub vendor: String,
    /// Version string as reported by the plugin.
    pub version: String,
    /// Free-form description.
    pub description: String,
    /// Feature tags (`instrument`, `audio-effect`, `stereo`, ...).
    pub features: Vec<String>,
    /// Bundle the plugin was loaded from.
    pub path: PathBuf,
}

impl PluginDescriptor {
    /// Creates a minimal descriptor (tests, in-process plugins).
    pub fn new(format: PluginFormat, id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            format,
            id: id.into(),
            name: name.into(),
            vendor: String::new(),
            version: String::new(),
            description: String::new(),
            features: Vec::new(),
            path: PathBuf::new(),
        }
    }

    /// `true` if the plugin tags itself as an instrument.
    pub fn is_instrument(&self) -> bool {
        self.features.iter().any(|f| f == "instrument")
    }

    /// `true` if the plugin tags itself as an audio effect.
    pub fn is_audio_effect(&self) -> bool {
        self.features.iter().any(|f| f == "audio-effect")
    }
}

/// One automatable parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamInfo {
    /// Stable parameter ID (format-specific numbering).
    pub id: u32,
    /// Display name.
    pub name: String,
    /// Grouping path (`"Filter/Envelope"`), empty if none.
    pub module: String,
    /// Smallest value.
    pub min: f64,
    /// Largest value.
    pub max: f64,
    /// Value after instantiation.
    pub default: f64,
    /// May be automated by the host.
    pub automatable: bool,
    /// Takes integer steps only.
    pub stepped: bool,
    /// Not meant for the user interface.
    pub hidden: bool,
    /// Cannot be written by the host.
    pub readonly: bool,
    /// This is the plugin's bypass switch.
    pub bypass: bool,
}

/// Channel counts of a plugin's main audio input and output ports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioPortLayout {
    /// Channels on the main input (`0` for instruments / generators).
    pub inputs: usize,
    /// Channels on the main output (`0` for analysers).
    pub outputs: usize,
}

/// A parameter move queued for the audio thread.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamChange {
    /// [`ParamInfo::id`].
    pub id: u32,
    /// New plain value in `[min, max]`.
    pub value: f64,
}

/// A hosted plugin, whatever its format.
///
/// Threading contract (mirrors CLAP's): `activate`, `deactivate`,
/// `param_value`, `save_state`, `load_state` and `params` are main-thread
/// calls; `process`, `reset` and `set_param` (which only queues) are
/// audio-thread safe once active and must not allocate.
pub trait PluginInstance: Send {
    /// Static description.
    fn descriptor(&self) -> &PluginDescriptor;

    /// Main audio port channel counts.
    fn ports(&self) -> AudioPortLayout;

    /// All parameters, in the plugin's order.
    fn params(&self) -> &[ParamInfo];

    /// Current plain value of a parameter.
    fn param_value(&self, id: u32) -> Result<f64>;

    /// Queues a parameter change for the next `process` call (or applies it
    /// immediately while inactive). Fails for unknown IDs or a full queue.
    fn set_param(&mut self, id: u32, value: f64) -> Result<()>;

    /// Prepares for processing at `sample_rate` with blocks of at most
    /// `max_frames`. May allocate.
    fn activate(&mut self, sample_rate: SampleRate, max_frames: usize) -> Result<()>;

    /// Releases processing resources.
    fn deactivate(&mut self);

    /// `true` between a successful `activate` and `deactivate`.
    fn is_active(&self) -> bool;

    /// Clears internal state (voices, delay lines) without reallocating.
    fn reset(&mut self);

    /// Renders one block. `input`/`output` follow [`ports`](Self::ports).
    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus;

    /// Processing latency once active.
    fn latency_frames(&self) -> usize;

    /// Serialises the plugin's full state.
    fn save_state(&mut self) -> Result<Vec<u8>>;

    /// Restores state produced by [`save_state`](Self::save_state).
    fn load_state(&mut self, bytes: &[u8]) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_helpers() {
        let mut d = PluginDescriptor::new(PluginFormat::Clap, "a.b", "AB");
        assert!(!d.is_instrument() && !d.is_audio_effect());
        d.features = vec!["instrument".into(), "synthesizer".into()];
        assert!(d.is_instrument());
        d.features.push("audio-effect".into());
        assert!(d.is_audio_effect());
        assert_eq!(PluginFormat::Clap.extension(), "clap");
        assert_eq!(PluginFormat::Vst3.extension(), "vst3");
        assert_eq!(
            AudioPortLayout::default(),
            AudioPortLayout {
                inputs: 0,
                outputs: 0
            }
        );
    }
}
