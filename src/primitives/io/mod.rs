//! # superb-io
//!
//! Where audio leaves (and later enters) the Superb DAW SDK.
//!
//! * [`Engine`] — the block loop: advances a [`Transport`](crate::primitives::transport::Transport),
//!   splits the block at loop points, runs a [`BlockProcessor`](crate::primitives::graph::BlockProcessor)
//!   per segment, delivers queued MIDI, and records DSP load. Its `render`
//!   method is allocation-free. Queued `TimestampedMessage`s are placed
//!   sample-accurately via `superb::primitives::midi::timing::FrameMapper` against the
//!   engine's stream clock ([`MidiTiming::Stream`], the default) or forced
//!   to frame 0 ([`MidiTiming::BlockStart`]).
//! * [`cache`] — audio held in memory, keyed by the path clips refer to.
//! * [`capture`] — a pre-allocated queue of planar blocks for handing
//!   captured audio from a callback to another thread. Pure Rust; the
//!   device layer fills it, tests and offline code can too.
//! * [`live`] — split an engine into a control handle and an audio-side
//!   loop so play/stop/seek and graph swaps can come from another thread.
//! * [`offline`] — drive an [`Engine`] as fast as possible into a buffer
//!   (bounce / test rendering).
//! * [`wav`] — minimal RIFF/WAVE writer and reader (PCM16 and float32).
//! * `files` module (feature `files`) — decode WAV/FLAC (and whatever else
//!   symphonia is built with) into `PlanarBuffer`s, resample with rubato,
//!   and bounce an [`Engine`] straight to a WAV file. See [`FILES_FEATURE`]
//!   for the licence boundary.
//! * `device` module (feature `devices`) — cpal output streams that run an
//!   [`Engine`] on the real-time callback thread, plus input/capture streams
//!   that deliver planar blocks to a closure or a recycle SPSC
//!   (`device::capture_queue` / `device::run_capture`).
//!
//! ```
//! use superb::primitives::SampleRate;
//! use superb::primitives::graph::nodes::SineNode;
//! use superb::primitives::graph::AudioGraph;
//! use superb::primitives::io::{offline, Engine};
//! use superb::primitives::transport::Transport;
//!
//! let mut graph = AudioGraph::new();
//! let sine = graph.add_node(Box::new(SineNode::new(440.0, 0.25, 2)));
//! graph.set_output(sine).unwrap();
//!
//! let mut engine = Engine::new(graph, Transport::new(SampleRate::HZ_48K, 120.0));
//! engine.transport_mut().play();
//! let audio = offline::render(&mut engine, SampleRate::HZ_48K, 256, 4_800).unwrap();
//! assert_eq!(audio.channels(), 2);
//! assert_eq!(audio.frames(), 4_800);
//! assert!((audio.peak() - 0.25).abs() < 1e-3);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cache;
pub mod capture;
#[cfg(feature = "devices")]
pub mod device;
mod engine;
#[cfg(feature = "files")]
pub mod files;
pub mod live;
pub mod offline;
pub mod wav;

pub use engine::{Engine, EngineStats, MidiTiming};

/// Whether this build of `superb-io` includes the `files` module.
///
/// The `files` feature pulls in `symphonia` (MPL-2.0) and `rubato`
/// (MIT OR Apache-2.0). symphonia's file-level copyleft is contained by
/// importing it from exactly one source file, `src/files/decode.rs`; no
/// other file in the workspace names the crate, and
/// `mpl_dependency_is_confined_to_decode_rs` below checks that on every default
/// test run. Enable with `cargo test -p superb-io --features files`.
pub const FILES_FEATURE: bool = cfg!(feature = "files");

#[cfg(test)]
mod licence_boundary {
    use std::path::{Path, PathBuf};

    fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                rust_sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// MPL-2.0 boundary: only `crates/superb-io/src/files/decode.rs` may
    /// mention symphonia. Runs on the default feature set so the check never
    /// depends on the optional dependency being built.
    #[test]
    fn mpl_dependency_is_confined_to_decode_rs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let allowed = root.join("src/primitives/io/files/decode.rs");
        let mut sources = Vec::new();
        for dir in ["src", "tests", "examples"] {
            rust_sources(&root.join(dir), &mut sources);
        }
        assert!(sources.iter().any(|p| p == &allowed), "decode.rs missing");
        // Built at runtime so this file does not trip its own check.
        let needle = ["symph", "onia"].concat();
        let offenders: Vec<_> = sources
            .into_iter()
            .filter(|p| p != &allowed)
            .filter(|p| {
                std::fs::read_to_string(p)
                    .unwrap()
                    .lines()
                    .filter(|l| !l.trim_start().starts_with("//"))
                    .any(|l| l.contains(&needle))
            })
            .collect();
        assert!(
            offenders.is_empty(),
            "{needle} referenced outside decode.rs: {offenders:?}"
        );
    }
}
