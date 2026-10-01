//! The state every flow works on.
//!
//! A [`Rig`] is a plain record of the primitives a running song needs: the
//! document, its undo history, the audio in memory, and the live engine.
//! Flow functions ([`play::start`](crate::play::start),
//! [`record::stop`](crate::record::stop), ...) take `&mut Rig` so you don't
//! pass seven arguments; step functions take the individual fields. Nothing
//! is hidden behind methods: reach into any field and call a primitive
//! directly whenever a flow does not do exactly what you want.
//!
//! Build one with [`project::create`](crate::project::create) or
//! [`project::open`](crate::project::open), or with [`Rig::new`] from a
//! [`Session`] you already have.

use core::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::play::{AudioCache, SessionPlayer};
use crate::primitives::io::live::{live_engine, EngineLoop, LiveEngine};
use crate::primitives::mixer::{SendParams, StripParams};
#[cfg(feature = "clap")]
use crate::primitives::plugins::PluginInstance;
use crate::primitives::plugins::{ParamChange, ParamInfo, PluginDescriptor};
use crate::primitives::realtime::spsc::Producer;
use crate::primitives::session::Session;
use crate::primitives::undo::UndoStack;
use crate::primitives::{InsertId, Result, SendId, TrackId};

/// Everything a session in progress consists of.
pub struct Rig {
    /// The song: tracks, clips, markers, routing, inserts, tempo.
    pub session: Session,
    /// Undo history for `session`. Every flow that changes the song pushes
    /// through this stack, so every change is reversible.
    pub history: UndoStack<Session>,
    /// Audio held in memory, keyed by the path clips refer to.
    pub cache: AudioCache,
    /// Control handle to the engine: commands, graph swaps, position.
    pub engine: LiveEngine,
    /// The engine's audio side. Present until it is moved into a device
    /// stream (`play::start_output`, feature `devices`); while
    /// present, [`play::advance`](crate::play::advance) drives it by hand.
    pub engine_loop: Option<EngineLoop>,
    /// Handles into the console built by the last
    /// [`play::sync`](crate::play::sync).
    pub live: LiveHandles,
    /// A take being recorded, if any.
    pub recording: Option<crate::record::Recording>,
    /// Channels per strip and at the output (2 = stereo).
    pub channels: usize,
    /// Set by every edit that changes the console's structure (a new track,
    /// a moved clip, a send). Cleared by [`play::sync`](crate::play::sync),
    /// which the flows that start audio call for you.
    pub needs_sync: bool,
    /// The running output stream, once started.
    #[cfg(feature = "devices")]
    pub output: Option<crate::primitives::io::device::OutputStream>,
    /// Hardware MIDI inputs connected so far. Dropping one disconnects it.
    #[cfg(feature = "midi-io")]
    pub midi_inputs: Vec<crate::primitives::midi::input::MidiInputBridge>,
    /// Loads plugins for inserts and remembers the bundles it has opened.
    pub plugins: PluginLoader,
}

impl Rig {
    /// A stereo rig around `session`, with an engine that renders blocks of
    /// at most `block_size` frames at the session's sample rate.
    pub fn new(session: Session, block_size: usize) -> Result<Rig> {
        Self::with_channels(session, block_size, 2)
    }

    /// Like [`new`](Self::new) with an explicit channel count.
    pub fn with_channels(session: Session, block_size: usize, channels: usize) -> Result<Rig> {
        let bpm = session
            .tempo_map()
            .tempo_at(crate::primitives::BeatPosition::ZERO);
        let (engine, engine_loop) = live_engine(session.sample_rate(), block_size, bpm)?;
        Ok(Rig {
            session,
            history: UndoStack::new(),
            cache: AudioCache::new(),
            engine,
            engine_loop: Some(engine_loop),
            live: LiveHandles::default(),
            recording: None,
            channels,
            needs_sync: true,
            #[cfg(feature = "devices")]
            output: None,
            #[cfg(feature = "midi-io")]
            midi_inputs: Vec::new(),
            plugins: PluginLoader::default(),
        })
    }
}

impl core::fmt::Debug for Rig {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Rig")
            .field("session", &self.session.name())
            .field("tracks", &self.session.track_count())
            .field("undo_steps", &self.history.undo_len())
            .field("cached_files", &self.cache.len())
            .field("engine", &self.engine)
            .field("engine_loop_attached", &self.engine_loop.is_some())
            .field("recording", &self.recording.is_some())
            .field("channels", &self.channels)
            .field("needs_sync", &self.needs_sync)
            .finish()
    }
}

/// Session track or bus → its live strip parameters.
pub type StripHandles = Vec<(TrackId, Arc<StripParams>)>;
/// Session send → its live level and enable switch.
pub type SendHandles = Vec<(SendId, Arc<SendParams>)>;

/// Handles into the console the engine is currently running.
///
/// Rebuilt by every [`play::sync`](crate::play::sync); the atomics behind
/// them are what the audio thread reads, so a change made through them is
/// heard on the next block.
#[derive(Debug, Default)]
pub struct LiveHandles {
    /// Session track → its strip (tracks and buses alike).
    pub strips: StripHandles,
    /// Session send → its level and enable switch.
    pub sends: SendHandles,
    /// Session insert → the loaded plugin's parameter queue and info.
    pub inserts: Vec<LiveInsert>,
    /// Which session objects map to which console objects.
    pub player: Option<SessionPlayer>,
    /// Inserts that could not be loaded, with the reason. The rest of the
    /// console still runs.
    pub failed_inserts: Vec<(InsertId, String)>,
}

impl LiveHandles {
    /// The live strip of a session track or bus.
    pub fn strip(&self, track: TrackId) -> Option<&Arc<StripParams>> {
        self.strips
            .iter()
            .find(|(id, _)| *id == track)
            .map(|(_, p)| p)
    }

    /// The live parameters of a session send.
    pub fn send(&self, send: SendId) -> Option<&Arc<SendParams>> {
        self.sends
            .iter()
            .find(|(id, _)| *id == send)
            .map(|(_, p)| p)
    }

    /// The loaded plugin behind a session insert.
    pub fn insert(&self, insert: InsertId) -> Option<&LiveInsert> {
        self.inserts.iter().find(|i| i.id == insert)
    }

    /// Mutable access to a loaded plugin's handle (to push parameters).
    pub fn insert_mut(&mut self, insert: InsertId) -> Option<&mut LiveInsert> {
        self.inserts.iter_mut().find(|i| i.id == insert)
    }
}

/// Turns an [`Insert`](crate::primitives::session::Insert)'s plugin
/// reference into a running instance, keeping every bundle it has opened so
/// a plugin used on several tracks is loaded from disk once.
///
/// With the `clap` feature off, every request fails with a message saying
/// so; the rest of the console still runs.
#[derive(Default)]
pub struct PluginLoader {
    #[cfg(feature = "clap")]
    bundles: std::collections::HashMap<
        std::path::PathBuf,
        Arc<crate::primitives::plugins::clap::ClapBundle>,
    >,
}

impl PluginLoader {
    /// Opens (or reuses) the bundle at `path`.
    #[cfg(feature = "clap")]
    pub fn load(
        &mut self,
        path: &std::path::Path,
    ) -> Result<Arc<crate::primitives::plugins::clap::ClapBundle>> {
        if let Some(bundle) = self.bundles.get(path) {
            return Ok(Arc::clone(bundle));
        }
        let bundle = crate::primitives::plugins::clap::ClapBundle::load(path)?;
        self.bundles.insert(path.to_path_buf(), Arc::clone(&bundle));
        Ok(bundle)
    }

    /// Makes an already loaded (or in-process) bundle answer for `path`.
    #[cfg(feature = "clap")]
    pub fn register(
        &mut self,
        path: impl Into<std::path::PathBuf>,
        bundle: Arc<crate::primitives::plugins::clap::ClapBundle>,
    ) {
        self.bundles.insert(path.into(), bundle);
    }

    /// Number of bundles held open.
    pub fn loaded(&self) -> usize {
        #[cfg(feature = "clap")]
        {
            self.bundles.len()
        }
        #[cfg(not(feature = "clap"))]
        {
            0
        }
    }

    /// Creates an instance for `insert`, restores its saved state and
    /// applies its stored parameter values.
    pub fn instantiate(
        &mut self,
        insert: &crate::primitives::session::Insert,
    ) -> Result<Box<dyn crate::primitives::plugins::PluginInstance>> {
        use crate::primitives::plugins::PluginFormat;
        match insert.plugin.format {
            PluginFormat::Vst3 => Err(crate::primitives::plugins::vst3::unsupported()),
            #[cfg(feature = "clap")]
            PluginFormat::Clap => {
                let bundle = self.load(&insert.plugin.path)?;
                let mut plugin = bundle.instantiate(&insert.plugin.id)?;
                if !insert.state.is_empty() {
                    plugin.load_state(&insert.state)?;
                }
                for (param, value) in &insert.params {
                    plugin.set_param(*param, *value)?;
                }
                Ok(Box::new(plugin))
            }
            #[cfg(not(feature = "clap"))]
            PluginFormat::Clap => Err(crate::primitives::SuperbError::Message(
                "CLAP hosting is disabled (feature `clap`)".into(),
            )),
        }
    }
}

impl core::fmt::Debug for PluginLoader {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PluginLoader")
            .field("loaded", &self.loaded())
            .finish()
    }
}

/// A plugin instance running inside the console.
#[derive(Debug)]
pub struct LiveInsert {
    /// The session insert this instance was created for.
    pub id: InsertId,
    /// What the plugin says about itself.
    pub descriptor: PluginDescriptor,
    /// Its parameters, in the plugin's order.
    pub params: Vec<ParamInfo>,
    /// Wait-free queue into the running instance; changes apply at the next block.
    pub sender: Producer<ParamChange>,
    /// The node's bypass switch; flips are heard at the next block.
    pub bypass: Arc<AtomicBool>,
}
