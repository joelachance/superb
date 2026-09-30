//! Non-realtime MIDI hardware input, bridged into an RT-safe SPSC queue.
//!
//! This module wraps [`midir`] (feature `midir`) behind the crate's own
//! types: nothing outside this file knows midir exists. The bridge runs
//! midir's callback on its own (non-realtime) thread, parses each message
//! with [`MidiMessage::from_bytes`] and pushes a [`TimestampedMessage`] into
//! a [`superb_rt::spsc`] queue that the audio thread drains — the standard
//! "non-RT producer, RT consumer" pattern used throughout the SDK.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use midir::{Ignore, MidiInput, MidiInputConnection};
use superb_core::{Result, SuperbError};
use superb_rt::spsc::{self, Consumer};

use crate::event::TimestampedMessage;
use crate::message::MidiMessage;

/// Metadata about a MIDI input port visible to the system.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiPortInfo {
    /// Index into the list returned by [`list_input_ports`], to be passed to
    /// [`MidiInputBridge::connect`].
    ///
    /// This index is a snapshot: it is not stable across hotplug events, so
    /// re-list ports before reconnecting if the device topology may have
    /// changed.
    pub index: usize,
    /// Human-readable port name (e.g. `"IAC Driver Bus 1"`).
    pub name: String,
}

/// Lists the MIDI input ports currently visible to the system.
///
/// # Errors
/// Returns [`SuperbError::Message`] if the underlying MIDI backend could not
/// be initialised (for example, no ALSA sequencer is available). This is
/// expected in headless CI environments.
pub fn list_input_ports() -> Result<Vec<MidiPortInfo>> {
    let midi_in = MidiInput::new("superb-midi-list").map_err(|e| {
        SuperbError::Message(format!("failed to initialise MIDI input backend: {e}"))
    })?;
    let ports = midi_in.ports();
    let mut out = Vec::with_capacity(ports.len());
    for (index, port) in ports.iter().enumerate() {
        let name = midi_in
            .port_name(port)
            .unwrap_or_else(|_| format!("port {index}"));
        out.push(MidiPortInfo { index, name });
    }
    Ok(out)
}

/// A live connection to a hardware or software MIDI input port.
///
/// Owns the midir connection for as long as it is alive; dropping the
/// bridge (or calling [`MidiInputBridge::close`]) closes the connection and
/// stops the callback thread.
pub struct MidiInputBridge {
    connection: MidiInputConnection<()>,
    port_name: String,
    dropped: Arc<AtomicUsize>,
}

impl MidiInputBridge {
    /// Connects to input port `port_index` (as listed by [`list_input_ports`])
    /// and returns the bridge together with the consuming half of a fresh
    /// SPSC queue of capacity `queue_capacity`.
    ///
    /// Every incoming MIDI message is parsed with [`MidiMessage::from_bytes`]
    /// on midir's callback thread. Messages that fail to parse (SysEx, MTC
    /// quarter frames, malformed data) are silently ignored. Messages that
    /// parse but arrive while the queue is full are dropped and counted by
    /// [`MidiInputBridge::dropped`] — the callback thread never blocks.
    ///
    /// # Errors
    /// Returns [`SuperbError::Message`] if the MIDI backend cannot be
    /// initialised or the port cannot be opened, or
    /// [`SuperbError::NotFound`] if `port_index` is out of range.
    pub fn connect(
        port_index: usize,
        queue_capacity: usize,
    ) -> Result<(MidiInputBridge, Consumer<TimestampedMessage>)> {
        let mut midi_in = MidiInput::new("superb-midi-input").map_err(|e| {
            SuperbError::Message(format!("failed to initialise MIDI input backend: {e}"))
        })?;
        midi_in.ignore(Ignore::None);

        let ports = midi_in.ports();
        let port = ports.get(port_index).ok_or(SuperbError::NotFound {
            kind: "midi_input_port",
            id: port_index as u64,
        })?;
        let port_name = midi_in
            .port_name(port)
            .unwrap_or_else(|_| format!("port {port_index}"));

        let (mut producer, consumer) = spsc::channel::<TimestampedMessage>(queue_capacity);
        let dropped = Arc::new(AtomicUsize::new(0));
        let dropped_in_callback = Arc::clone(&dropped);

        let connection = midi_in
            .connect(
                port,
                "superb-midi-in",
                move |timestamp_us, bytes, _data: &mut ()| {
                    if let Ok(message) = MidiMessage::from_bytes(bytes) {
                        let event = TimestampedMessage {
                            timestamp_us,
                            message,
                        };
                        if producer.push(event).is_err() {
                            dropped_in_callback.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                },
                (),
            )
            .map_err(|e| {
                SuperbError::Message(format!("failed to connect to MIDI input port: {e}"))
            })?;

        Ok((
            MidiInputBridge {
                connection,
                port_name,
                dropped,
            },
            consumer,
        ))
    }

    /// Name of the connected port.
    pub fn port_name(&self) -> &str {
        &self.port_name
    }

    /// Number of messages dropped so far because the SPSC queue was full.
    pub fn dropped(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Closes the connection, stopping the callback thread.
    pub fn close(self) {
        let _ = self.connection.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_ports_tolerates_a_missing_backend() {
        // CI has no ALSA sequencer, so this must not panic either way; it
        // only has to report a sensible, human-readable error on failure.
        match list_input_ports() {
            Ok(_ports) => {}
            Err(e) => assert!(
                matches!(e, SuperbError::Message(_)),
                "unexpected error variant: {e:?}"
            ),
        }
    }

    #[test]
    #[ignore = "requires a real MIDI input device and backend, not available in CI"]
    fn connect_to_first_available_port() {
        let ports = list_input_ports().expect("MIDI backend available");
        assert!(!ports.is_empty(), "no MIDI input ports found");
        let (bridge, _consumer) = MidiInputBridge::connect(0, 64).expect("connect to first port");
        assert_eq!(bridge.dropped(), 0);
        assert!(!bridge.port_name().is_empty());
        bridge.close();
    }

    #[test]
    #[ignore = "requires a real MIDI input device and backend, not available in CI"]
    fn connect_rejects_out_of_range_port_index() {
        let err = MidiInputBridge::connect(usize::MAX, 8).err().unwrap();
        assert!(matches!(
            err,
            SuperbError::NotFound {
                kind: "midi_input_port",
                ..
            }
        ));
    }
}
