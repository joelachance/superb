//! Hardware: which audio and MIDI devices exist, playing through an output,
//! opening the input that monitoring hears and recordings capture.
//!
//! Everything here needs a feature: `devices` for audio, `midi-io` for
//! MIDI ports. Without them the module is empty and the session renders through
//! [`session::steps::render_blocks`](crate::session::steps::render_blocks) or [`export`](crate::export).
//!
//! Made of `superb::primitives::io::device` and `superb::primitives::midi::input`.

#[cfg(feature = "devices")]
use crate::primitives::buffers::PlanarBuffer;
#[cfg(feature = "devices")]
use crate::primitives::io::capture::{capture_queue, CaptureConsumer};
#[cfg(feature = "devices")]
use crate::primitives::io::device::{DeviceInfo, InputConfig, InputStream, OutputConfig};
#[cfg(feature = "devices")]
use crate::primitives::io::{EngineCommand, LiveInput};
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
/// size and channel count, on the named device (or the default). The verbs
/// build this themselves so a stream can never mismatch the engine.
#[cfg(feature = "devices")]
pub(crate) fn output_config(session: &Session, device: Option<&str>) -> OutputConfig {
    OutputConfig {
        device_name: device.map(str::to_owned),
        sample_rate: session.memory.sample_rate(),
        channels: session.channels,
        buffer_frames: None,
        max_block_frames: session.engine.max_frames(),
    }
}

/// Plays the session through an output device (`None` for the default,
/// or a name from [`outputs`]): moves the audio thread into a real output
/// stream at the session's sample rate, block size and channel count, and
/// starts it. From then on the device drives the engine, structural edits
/// rebuild the mixer at once, and
/// [`session::steps::render_blocks`](crate::session::steps::render_blocks)
/// is unavailable.
///
/// Made of: an `OutputConfig` matching the session →
/// [`session::steps::rebuild`](crate::session::steps::rebuild) when needed
/// → `AudioThread::run_on_device`; the stream is kept in
/// [`Session::output`] and stops when dropped.
#[cfg(feature = "devices")]
pub fn output(session: &mut Session, device: Option<&str>) -> Result<()> {
    let config = output_config(session, device);
    if session.needs_rebuild {
        crate::session::steps::rebuild(session)?;
    }
    let audio_thread = session.audio_thread.take().ok_or_else(|| {
        SuperbError::Message("the audio thread is already running on a device stream".into())
    })?;
    match audio_thread.run_on_device(&config) {
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
pub(crate) fn input_config(
    session: &Session,
    device: Option<&str>,
    channels: usize,
) -> InputConfig {
    InputConfig {
        device_name: device.map(str::to_owned),
        sample_rate: session.memory.sample_rate(),
        channels,
        buffer_frames: None,
        max_block_frames: session.engine.max_frames(),
    }
}

/// The session's audio input, once [`input`] opened it: the running stream
/// and what it feeds. Monitoring audio tracks hear it through the engine's
/// live input; a device recording drains its own queue of the same blocks.
#[cfg(feature = "devices")]
pub struct AudioInput {
    stream: InputStream,
    channels: usize,
    recording: std::sync::Arc<core::sync::atomic::AtomicBool>,
    record: Option<CaptureConsumer>,
}

#[cfg(feature = "devices")]
impl AudioInput {
    /// Channels in every captured block.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Name of the device in use.
    pub fn device_name(&self) -> &str {
        self.stream.device_name()
    }

    /// Starts feeding the recording queue and hands out its consumer end,
    /// emptied of anything left from before.
    pub(crate) fn begin_recording(&mut self) -> Result<CaptureConsumer> {
        let mut blocks = self.record.take().ok_or_else(|| {
            SuperbError::Message("a recording is already drawing from the audio input".into())
        })?;
        while let Some(stale) = blocks.pop() {
            blocks.recycle(stale);
        }
        self.recording
            .store(true, core::sync::atomic::Ordering::Release);
        Ok(blocks)
    }

    /// Stops feeding the recording queue and takes its consumer end back.
    pub(crate) fn end_recording(&mut self, blocks: CaptureConsumer) {
        self.recording
            .store(false, core::sync::atomic::Ordering::Release);
        self.record = Some(blocks);
    }
}

#[cfg(feature = "devices")]
impl core::fmt::Debug for AudioInput {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AudioInput")
            .field("device", &self.stream.device_name())
            .field("channels", &self.channels)
            .field(
                "recording",
                &self.recording.load(core::sync::atomic::Ordering::Relaxed),
            )
            .finish()
    }
}

/// Opens the session's audio input (`None` for the default device, or a
/// name from [`inputs`]): a stream at the session's sample rate and block
/// size with as many channels as the audio tracks' inputs name, at least
/// two. From then on a monitoring audio track
/// ([`track::monitor`](crate::track::monitor)) hears it through its strip,
/// and `record::start` with `Source::Device` records from it. One input
/// per session; it stays open for the session's life.
///
/// Made of: an `InputConfig` matching the session → two capture queues
/// (one the engine reads as its live input, one a recording drains) →
/// `InputStream::open_timed` pushing every block to both →
/// `EngineHandle::send(LiveInput)`; the stream is kept in [`Session::input`].
#[cfg(feature = "devices")]
pub fn input(session: &mut Session, device: Option<&str>) -> Result<()> {
    if session.input.is_some() {
        return Err(SuperbError::Message(
            "the audio input is already open".into(),
        ));
    }
    let channels = crate::record::steps::input_width(session).max(2);
    let config = input_config(session, device, channels);
    let max_frames = session.engine.max_frames();
    let (mut live, live_rx) = capture_queue(channels, max_frames, 8)?;
    let (mut record, record_rx) = capture_queue(channels, max_frames, 64)?;
    let recording = std::sync::Arc::new(core::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&recording);
    // The device may give fewer channels than asked for; blocks are
    // widened into this scratch so the queues always see `channels`.
    let mut scratch = PlanarBuffer::new(channels, max_frames);
    let stream = InputStream::open_timed(&config, move |block, at| {
        let block = if block.channels() == channels {
            block
        } else {
            let _ = scratch.set_frames(block.frames());
            let _ = scratch.copy_block_from(0, block, block.frames());
            &scratch
        };
        let _ = live.push(block, at);
        if flag.load(core::sync::atomic::Ordering::Acquire) {
            let _ = record.push(block, at);
        }
    })?;
    session
        .engine
        .send(EngineCommand::LiveInput(LiveInput::new(
            live_rx, channels, max_frames,
        )?))?;
    session.input = Some(AudioInput {
        stream,
        channels,
        recording,
        record: Some(record_rx),
    });
    Ok(())
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
        let session = session::create(session::scratch("dev"), SampleRate::CD, 128).unwrap();
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
