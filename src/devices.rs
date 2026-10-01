//! Hardware: which audio and MIDI devices exist, and configurations that
//! match the session.
//!
//! Everything here needs a feature: `devices` for audio, `midi-io` for
//! MIDI ports. Without them the module is empty and the session renders through
//! [`session::steps::render_blocks`](crate::session::steps::render_blocks) or [`export`](crate::export).
//!
//! Made of `superb::primitives::io::device` and `superb::primitives::midi::input`.

#[cfg(feature = "devices")]
use crate::primitives::io::device::{DeviceInfo, InputConfig, OutputConfig};
#[cfg(any(feature = "devices", feature = "midi-io"))]
use crate::primitives::Result;
#[cfg(feature = "devices")]
use crate::primitives::SuperbError;
#[cfg(feature = "devices")]
use crate::session::Session;

/// Audio output devices of the default host.
///
/// Made of: `device::output_devices`.
#[cfg(feature = "devices")]
pub fn outputs() -> Result<Vec<DeviceInfo>> {
    crate::primitives::io::device::output_devices()
}

/// Audio input devices of the default host.
///
/// Made of: `device::input_devices`.
#[cfg(feature = "devices")]
pub fn inputs() -> Result<Vec<DeviceInfo>> {
    crate::primitives::io::device::input_devices()
}

/// An output configuration that matches the session: its sample rate, block
/// size and channel count, on the named device (or the default).
#[cfg(feature = "devices")]
pub fn output_config(session: &Session, device: Option<&str>) -> OutputConfig {
    OutputConfig {
        device_name: device.map(str::to_owned),
        sample_rate: session.document.sample_rate(),
        channels: session.channels,
        buffer_frames: None,
        max_block_frames: session.engine.max_frames(),
    }
}

/// Plays the session through an output device: moves the engine loop into
/// a real output stream and starts it. From then on the device drives the
/// engine, structural edits rebuild the mixer at once, and
/// [`session::steps::render_blocks`](crate::session::steps::render_blocks)
/// is unavailable. Use [`output_config`] for a configuration that matches
/// the session.
///
/// Made of: [`session::steps::rebuild`](crate::session::steps::rebuild)
/// when needed → `EngineLoop::run_on_device`; the stream is kept in
/// [`Session::output`] and stops when dropped.
#[cfg(feature = "devices")]
pub fn output(session: &mut Session, config: &OutputConfig) -> Result<()> {
    if session.needs_rebuild {
        crate::session::steps::rebuild(session)?;
    }
    let engine_loop = session.engine_loop.take().ok_or_else(|| {
        SuperbError::Message("the engine loop is already running on a device stream".into())
    })?;
    match engine_loop.run_on_device(config) {
        Ok(stream) => {
            session.output = Some(stream);
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// An input configuration at the session's sample rate and block size with
/// `channels` channels, on the named device (or the default).
#[cfg(feature = "devices")]
pub fn input_config(session: &Session, device: Option<&str>, channels: usize) -> InputConfig {
    InputConfig {
        device_name: device.map(str::to_owned),
        sample_rate: session.document.sample_rate(),
        channels,
        buffer_frames: None,
        max_block_frames: session.engine.max_frames(),
    }
}

/// Hardware MIDI input ports.
///
/// Made of: `midi::input::list_input_ports`.
#[cfg(feature = "midi-io")]
pub fn midi_inputs() -> Result<Vec<crate::primitives::midi::input::MidiPortInfo>> {
    crate::primitives::midi::input::list_input_ports()
}

#[cfg(all(test, feature = "devices"))]
mod tests {
    use super::*;
    use crate::primitives::SampleRate;
    use crate::session;

    #[test]
    fn configs_follow_the_rig() {
        let session = session::create("dev", SampleRate::CD, 128).unwrap();
        let out = output_config(&session, Some("Speakers"));
        assert_eq!(out.device_name.as_deref(), Some("Speakers"));
        assert_eq!(out.sample_rate, SampleRate::CD);
        assert_eq!(out.max_block_frames, 128);
        assert_eq!(out.channels, 2);
        let inp = input_config(&session, None, 1);
        assert_eq!(inp.channels, 1);
        assert_eq!(inp.max_block_frames, 128);
        // Enumeration must not panic on a machine without hardware.
        let _ = outputs();
        let _ = inputs();
    }
}
