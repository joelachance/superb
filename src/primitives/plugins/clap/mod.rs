//! CLAP host (feature `clap`).
//!
//! [`ClapBundle`] wraps a loaded `.clap` shared library (or an in-process
//! [`clap_plugin_entry`]) and its plugin factory; [`ClapPlugin`] is one
//! instance created from it, implementing [`PluginInstance`].
//!
//! The host follows the CLAP threading model: `ClapBundle::load`,
//! `instantiate`, `activate`, `deactivate`, `save_state`, `load_state` and
//! `param_value` are main-thread calls; `process` is the audio-thread call
//! and never allocates (event lists, channel pointer tables and the
//! transport event are pre-allocated in `activate`). `start_processing` is
//! issued lazily from the first `process` call and `stop_processing` from
//! `deactivate`, so both happen on the thread that drives audio in an
//! offline render and on the callback thread of a live stream.
//!
//! This module is the only place in the SDK with `unsafe` FFI. Every unsafe
//! block names the invariant it relies on; the plugin-side contract is the
//! CLAP ABI itself.

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::graph::{ProcessContext, ProcessStatus};
use crate::primitives::midi::{MidiEvent, MidiMessage};
use crate::primitives::{Result, SampleRate, SuperbError};
use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::{
    clap_event_header, clap_event_midi, clap_event_note, clap_event_param_value,
    clap_event_transport, clap_input_events, clap_output_events, CLAP_CORE_EVENT_SPACE_ID,
    CLAP_EVENT_MIDI, CLAP_EVENT_NOTE_OFF, CLAP_EVENT_NOTE_ON, CLAP_EVENT_PARAM_VALUE,
    CLAP_EVENT_TRANSPORT, CLAP_TRANSPORT_HAS_BEATS_TIMELINE, CLAP_TRANSPORT_HAS_SECONDS_TIMELINE,
    CLAP_TRANSPORT_HAS_TEMPO, CLAP_TRANSPORT_IS_PLAYING,
};
use clap_sys::ext::audio_ports::{
    clap_audio_port_info, clap_plugin_audio_ports, CLAP_EXT_AUDIO_PORTS,
};
use clap_sys::ext::latency::{clap_plugin_latency, CLAP_EXT_LATENCY};
use clap_sys::ext::params::{
    clap_param_info, clap_plugin_params, CLAP_EXT_PARAMS, CLAP_PARAM_IS_AUTOMATABLE,
    CLAP_PARAM_IS_BYPASS, CLAP_PARAM_IS_HIDDEN, CLAP_PARAM_IS_READONLY, CLAP_PARAM_IS_STEPPED,
};
use clap_sys::ext::state::{clap_plugin_state, CLAP_EXT_STATE};
use clap_sys::factory::plugin_factory::{clap_plugin_factory, CLAP_PLUGIN_FACTORY_ID};
use clap_sys::fixedpoint::{CLAP_BEATTIME_FACTOR, CLAP_SECTIME_FACTOR};
use clap_sys::host::clap_host;
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::process::{clap_process, CLAP_PROCESS_ERROR, CLAP_PROCESS_SLEEP};
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::{clap_version_is_compatible, CLAP_VERSION};

use crate::primitives::plugins::{
    AudioPortLayout, ParamInfo, PluginDescriptor, PluginFormat, PluginInstance,
};

#[cfg(test)]
pub(crate) mod test_plugin;

/// Events the host can queue for one block (parameter changes + MIDI).
pub const EVENT_CAPACITY: usize = 1024;
/// Largest channel count the host tables are sized for.
const MAX_CHANNELS: usize = 64;

const HOST_NAME: &CStr = c"Superb SDK";
const HOST_VENDOR: &CStr = c"Superb";
const HOST_URL: &CStr = c"https://github.com/superb-audio/superb-sdk";
const HOST_VERSION: &CStr = c"0.1.0";

fn msg(text: impl Into<String>) -> SuperbError {
    SuperbError::Message(text.into())
}

/// Copies a C string into a `String` (empty for null).
///
/// # Safety
/// `p` must be null or point to a NUL-terminated string that outlives the call.
unsafe fn c_to_string(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        // SAFETY: guaranteed by the caller.
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// Converts a NUL-padded fixed array into a `String`.
fn fixed_to_string(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Builds a [`PluginDescriptor`] from a CLAP descriptor.
///
/// # Safety
/// `desc` must be a valid descriptor whose strings outlive the call.
unsafe fn descriptor_from(desc: &clap_plugin_descriptor, path: &Path) -> PluginDescriptor {
    let mut features = Vec::new();
    if !desc.features.is_null() {
        let mut p = desc.features;
        // SAFETY: `features` is a null-terminated array of C strings per the CLAP ABI.
        while !unsafe { *p }.is_null() {
            features.push(unsafe { c_to_string(*p) });
            p = unsafe { p.add(1) };
        }
    }
    // SAFETY: descriptor strings are static per the CLAP ABI.
    unsafe {
        PluginDescriptor {
            format: PluginFormat::Clap,
            id: c_to_string(desc.id),
            name: c_to_string(desc.name),
            vendor: c_to_string(desc.vendor),
            version: c_to_string(desc.version),
            description: c_to_string(desc.description),
            features,
            path: path.to_path_buf(),
        }
    }
}

// ---------------------------------------------------------------------------
// Bundle
// ---------------------------------------------------------------------------

/// A loaded CLAP bundle: entry point + plugin factory.
///
/// Shared by every instance created from it (`Arc`), so the library stays
/// mapped and `deinit` runs only after the last plugin is destroyed.
pub struct ClapBundle {
    path: PathBuf,
    entry: *const clap_plugin_entry,
    factory: *const clap_plugin_factory,
    /// Kept alive as long as any pointer into the library exists. Dropped
    /// after `deinit` (field order).
    _library: Option<libloading::Library>,
}

// SAFETY: the CLAP factory is documented as thread-safe for enumeration; the
// raw pointers are never dereferenced after the bundle is dropped, and
// `create_plugin`/`deinit` are only issued from a single thread at a time by
// this host. Sharing the bundle between threads (via `Arc`) is therefore
// sound as far as this host uses it.
unsafe impl Send for ClapBundle {}
unsafe impl Sync for ClapBundle {}

impl ClapBundle {
    /// Loads a `.clap` shared library (on macOS, a `.clap` bundle directory)
    /// and initialises its entry point.
    pub fn load(path: impl AsRef<Path>) -> Result<Arc<Self>> {
        let path = path.as_ref().to_path_buf();
        let binary = resolve_binary(&path)?;
        // SAFETY: loading a plugin library runs its constructors — inherent
        // to hosting; the caller chose the file.
        let library = unsafe { libloading::Library::new(&binary) }
            .map_err(|e| msg(format!("load {}: {e}", binary.display())))?;
        let entry: *const clap_plugin_entry = {
            // SAFETY: `clap_entry` is the exported `clap_plugin_entry` every
            // CLAP bundle defines; requesting it as that type is the ABI.
            let symbol = unsafe { library.get::<clap_plugin_entry>(b"clap_entry\0") }
                .map_err(|e| msg(format!("{}: no clap_entry symbol: {e}", binary.display())))?;
            &*symbol
        };
        // SAFETY: `entry` points into `library`, which the bundle keeps alive.
        let bundle = unsafe { Self::init(entry, path, Some(library)) }?;
        Ok(Arc::new(bundle))
    }

    /// Wraps an entry point that is already linked into the process
    /// (statically linked plugins, tests). `path` is what the plugin is told
    /// as its bundle path.
    pub fn from_entry(
        entry: &'static clap_plugin_entry,
        path: impl Into<PathBuf>,
    ) -> Result<Arc<Self>> {
        // SAFETY: a `'static` reference is valid for the whole program.
        let bundle = unsafe { Self::init(entry, path.into(), None) }?;
        Ok(Arc::new(bundle))
    }

    /// # Safety
    /// `entry` must stay valid for as long as the returned bundle lives.
    unsafe fn init(
        entry: *const clap_plugin_entry,
        path: PathBuf,
        library: Option<libloading::Library>,
    ) -> Result<Self> {
        // SAFETY: guaranteed by the caller.
        let e = unsafe { &*entry };
        if !clap_version_is_compatible(e.clap_version) {
            return Err(msg(format!(
                "{}: CLAP version {}.{}.{} is not compatible",
                path.display(),
                e.clap_version.major,
                e.clap_version.minor,
                e.clap_version.revision
            )));
        }
        let c_path = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| msg("bundle path contains NUL"))?;
        let init = e.init.ok_or_else(|| msg("clap_entry.init is missing"))?;
        // SAFETY: calling the entry's init with a valid path string, once.
        if !unsafe { init(c_path.as_ptr()) } {
            return Err(msg(format!("{}: clap_entry.init failed", path.display())));
        }
        let get_factory = e
            .get_factory
            .ok_or_else(|| msg("clap_entry.get_factory is missing"))?;
        // SAFETY: the entry is initialised; the factory ID is the standard one.
        let factory =
            unsafe { get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr()) } as *const clap_plugin_factory;
        if factory.is_null() {
            if let Some(deinit) = e.deinit {
                // SAFETY: balancing the successful init above.
                unsafe { deinit() };
            }
            return Err(msg(format!("{}: no plugin factory", path.display())));
        }
        Ok(Self {
            path,
            entry,
            factory,
            _library: library,
        })
    }

    /// Path the bundle was loaded from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every plugin the bundle's factory offers.
    pub fn descriptors(&self) -> Vec<PluginDescriptor> {
        // SAFETY: `factory` is valid while `self` lives (see `init`).
        let f = unsafe { &*self.factory };
        let (Some(count), Some(get)) = (f.get_plugin_count, f.get_plugin_descriptor) else {
            return Vec::new();
        };
        // SAFETY: standard factory calls with our own valid factory pointer.
        let n = unsafe { count(self.factory) };
        (0..n)
            .filter_map(|i| {
                // SAFETY: index within `count`; descriptors are static per the ABI.
                let d = unsafe { get(self.factory, i) };
                if d.is_null() {
                    None
                } else {
                    Some(unsafe { descriptor_from(&*d, &self.path) })
                }
            })
            .collect()
    }

    /// Creates and initialises a plugin instance by ID. Main thread.
    pub fn instantiate(self: &Arc<Self>, plugin_id: &str) -> Result<ClapPlugin> {
        let descriptor = self
            .descriptors()
            .into_iter()
            .find(|d| d.id == plugin_id)
            .ok_or_else(|| {
                msg(format!(
                    "{}: no plugin with id '{plugin_id}'",
                    self.path.display()
                ))
            })?;
        let host_data = Box::new(HostData::default());
        let host = Box::new(clap_host {
            clap_version: CLAP_VERSION,
            host_data: &*host_data as *const HostData as *mut c_void,
            name: HOST_NAME.as_ptr(),
            vendor: HOST_VENDOR.as_ptr(),
            url: HOST_URL.as_ptr(),
            version: HOST_VERSION.as_ptr(),
            get_extension: Some(host_get_extension),
            request_restart: Some(host_request_restart),
            request_process: Some(host_request_process),
            request_callback: Some(host_request_callback),
        });
        let c_id = CString::new(plugin_id).map_err(|_| msg("plugin id contains NUL"))?;
        // SAFETY: `factory` is valid while the bundle lives.
        let f = unsafe { &*self.factory };
        let create = f
            .create_plugin
            .ok_or_else(|| msg("factory.create_plugin is missing"))?;
        // SAFETY: the host struct lives in a `Box` owned by the returned
        // instance, so its address stays valid for the plugin's lifetime.
        let plugin = unsafe { create(self.factory, &*host, c_id.as_ptr()) };
        if plugin.is_null() {
            return Err(msg(format!(
                "{}: create_plugin('{plugin_id}') returned null",
                self.path.display()
            )));
        }
        // SAFETY: non-null plugin returned by the factory.
        let p = unsafe { &*plugin };
        let init = p.init.ok_or_else(|| msg("plugin.init is missing"))?;
        // SAFETY: init is called exactly once, right after creation.
        if !unsafe { init(plugin) } {
            if let Some(destroy) = p.destroy {
                // SAFETY: destroying a plugin whose init failed, as the ABI requires.
                unsafe { destroy(plugin) };
            }
            return Err(msg(format!(
                "{}: plugin '{plugin_id}' failed to init",
                self.path.display()
            )));
        }
        let ext = |id: &CStr| -> *const c_void {
            match p.get_extension {
                // SAFETY: get_extension after init is the documented way to query extensions.
                Some(get) => unsafe { get(plugin, id.as_ptr()) },
                None => ptr::null(),
            }
        };
        let mut instance = ClapPlugin {
            plugin,
            host,
            host_data,
            bundle: Arc::clone(self),
            descriptor,
            ext_params: ext(CLAP_EXT_PARAMS) as *const clap_plugin_params,
            ext_state: ext(CLAP_EXT_STATE) as *const clap_plugin_state,
            ext_latency: ext(CLAP_EXT_LATENCY) as *const clap_plugin_latency,
            ext_audio_ports: ext(CLAP_EXT_AUDIO_PORTS) as *const clap_plugin_audio_ports,
            params: Vec::new(),
            cookies: Vec::new(),
            ports: AudioPortLayout::default(),
            active: false,
            processing: bool::default(),
            sample_rate: SampleRate::default(),
            latency: 0,
            events: Vec::with_capacity(EVENT_CAPACITY),
            in_ptrs: Vec::with_capacity(MAX_CHANNELS),
            out_ptrs: Vec::with_capacity(MAX_CHANNELS),
            steady_time: 0,
            last_status: 0,
            process_errors: 0,
        };
        instance.refresh_params();
        instance.refresh_ports();
        Ok(instance)
    }
}

impl Drop for ClapBundle {
    fn drop(&mut self) {
        // SAFETY: `entry` is valid until `_library` drops (after this), and
        // every plugin holds an `Arc` to the bundle, so none is alive here.
        if let Some(deinit) = unsafe { (*self.entry).deinit } {
            unsafe { deinit() };
        }
    }
}

impl core::fmt::Debug for ClapBundle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ClapBundle")
            .field("path", &self.path)
            .finish()
    }
}

/// On macOS a `.clap` is a bundle directory; the loadable object is inside.
fn resolve_binary(path: &Path) -> Result<PathBuf> {
    if path.is_dir() {
        let stem = path
            .file_stem()
            .map(|s| s.to_os_string())
            .ok_or_else(|| msg("bundle path has no name"))?;
        let inner = path.join("Contents/MacOS").join(stem);
        if inner.is_file() {
            return Ok(inner);
        }
        return Err(msg(format!(
            "{}: bundle directory has no Contents/MacOS binary",
            path.display()
        )));
    }
    if !path.is_file() {
        return Err(msg(format!("{}: not found", path.display())));
    }
    Ok(path.to_path_buf())
}

// ---------------------------------------------------------------------------
// Host callbacks
// ---------------------------------------------------------------------------

/// Flags a plugin can raise through the host interface.
#[derive(Default)]
struct HostData {
    restart: AtomicBool,
    process: AtomicBool,
    callback: AtomicBool,
}

/// What the plugin asked the host for since the last
/// [`ClapPlugin::take_requests`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostRequests {
    /// `request_restart`: deactivate + activate again (e.g. latency changed).
    pub restart: bool,
    /// `request_process`: wake up processing.
    pub process: bool,
    /// `request_callback`: call `on_main_thread` soon.
    pub callback: bool,
}

/// # Safety
/// `host` must be a `clap_host` created by [`ClapBundle::instantiate`].
unsafe fn host_data<'a>(host: *const clap_host) -> Option<&'a HostData> {
    if host.is_null() {
        return None;
    }
    // SAFETY: guaranteed by the caller; `host_data` points at the boxed HostData.
    unsafe { ((*host).host_data as *const HostData).as_ref() }
}

unsafe extern "C" fn host_get_extension(
    _host: *const clap_host,
    _id: *const c_char,
) -> *const c_void {
    ptr::null()
}

unsafe extern "C" fn host_request_restart(host: *const clap_host) {
    // SAFETY: plugins only receive host pointers we created.
    if let Some(d) = unsafe { host_data(host) } {
        d.restart.store(true, Ordering::Release);
    }
}

unsafe extern "C" fn host_request_process(host: *const clap_host) {
    // SAFETY: as above.
    if let Some(d) = unsafe { host_data(host) } {
        d.process.store(true, Ordering::Release);
    }
}

unsafe extern "C" fn host_request_callback(host: *const clap_host) {
    // SAFETY: as above.
    if let Some(d) = unsafe { host_data(host) } {
        d.callback.store(true, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// Event lists and streams
// ---------------------------------------------------------------------------

/// One queued input event; every variant starts with a `clap_event_header`.
enum ClapEvent {
    Param(clap_event_param_value),
    Note(clap_event_note),
    Midi(clap_event_midi),
}

impl ClapEvent {
    fn header(&self) -> *const clap_event_header {
        match self {
            ClapEvent::Param(e) => &e.header,
            ClapEvent::Note(e) => &e.header,
            ClapEvent::Midi(e) => &e.header,
        }
    }
}

fn header(size: usize, time: u32, type_: u16) -> clap_event_header {
    clap_event_header {
        size: size as u32,
        time,
        space_id: CLAP_CORE_EVENT_SPACE_ID,
        type_,
        flags: 0,
    }
}

fn event_from_midi(ev: &MidiEvent) -> ClapEvent {
    match ev.message {
        MidiMessage::NoteOn {
            channel,
            note,
            velocity,
        } => ClapEvent::Note(clap_event_note {
            header: header(size_of::<clap_event_note>(), ev.frame, CLAP_EVENT_NOTE_ON),
            note_id: -1,
            port_index: 0,
            channel: channel.index() as i16,
            key: note as i16,
            velocity: velocity as f64 / 127.0,
        }),
        MidiMessage::NoteOff {
            channel,
            note,
            velocity,
        } => ClapEvent::Note(clap_event_note {
            header: header(size_of::<clap_event_note>(), ev.frame, CLAP_EVENT_NOTE_OFF),
            note_id: -1,
            port_index: 0,
            channel: channel.index() as i16,
            key: note as i16,
            velocity: velocity as f64 / 127.0,
        }),
        ref other => {
            let mut data = [0u8; 3];
            other.to_bytes(&mut data);
            ClapEvent::Midi(clap_event_midi {
                header: header(size_of::<clap_event_midi>(), ev.frame, CLAP_EVENT_MIDI),
                port_index: 0,
                data,
            })
        }
    }
}

unsafe extern "C" fn in_events_size(list: *const clap_input_events) -> u32 {
    // SAFETY: `ctx` is the `Vec<ClapEvent>` set by `ClapPlugin::process` for this call.
    let events = unsafe { &*((*list).ctx as *const Vec<ClapEvent>) };
    events.len() as u32
}

unsafe extern "C" fn in_events_get(
    list: *const clap_input_events,
    index: u32,
) -> *const clap_event_header {
    // SAFETY: as in `in_events_size`.
    let events = unsafe { &*((*list).ctx as *const Vec<ClapEvent>) };
    events
        .get(index as usize)
        .map_or(ptr::null(), ClapEvent::header)
}

unsafe extern "C" fn out_events_try_push(
    _list: *const clap_output_events,
    _event: *const clap_event_header,
) -> bool {
    // Output events (parameter gestures, note ends) are accepted and dropped.
    true
}

unsafe extern "C" fn ostream_write(
    stream: *const clap_ostream,
    buffer: *const c_void,
    size: u64,
) -> i64 {
    // SAFETY: `ctx` is the `Vec<u8>` set by `save_state`; the plugin hands
    // us `size` readable bytes.
    let out = unsafe { &mut *((*stream).ctx as *mut Vec<u8>) };
    let bytes = unsafe { std::slice::from_raw_parts(buffer as *const u8, size as usize) };
    out.extend_from_slice(bytes);
    size as i64
}

struct ReadCursor<'a> {
    data: &'a [u8],
    pos: usize,
}

unsafe extern "C" fn istream_read(
    stream: *const clap_istream,
    buffer: *mut c_void,
    size: u64,
) -> i64 {
    // SAFETY: `ctx` is the `ReadCursor` set by `load_state`; the plugin hands
    // us `size` writable bytes.
    let cursor = unsafe { &mut *((*stream).ctx as *mut ReadCursor<'_>) };
    let n = (size as usize).min(cursor.data.len() - cursor.pos);
    let dst = unsafe { std::slice::from_raw_parts_mut(buffer as *mut u8, n) };
    dst.copy_from_slice(&cursor.data[cursor.pos..cursor.pos + n]);
    cursor.pos += n;
    n as i64
}

fn transport_event(ctx: &ProcessContext<'_>) -> clap_event_transport {
    let mut flags = CLAP_TRANSPORT_HAS_TEMPO
        | CLAP_TRANSPORT_HAS_BEATS_TIMELINE
        | CLAP_TRANSPORT_HAS_SECONDS_TIMELINE;
    if ctx.time.rolling {
        flags |= CLAP_TRANSPORT_IS_PLAYING;
    }
    let seconds = ctx.time.start.samples() as f64 / ctx.sample_rate.as_f64();
    clap_event_transport {
        header: header(size_of::<clap_event_transport>(), 0, CLAP_EVENT_TRANSPORT),
        flags,
        song_pos_beats: (ctx.time.beat_start.beats() * CLAP_BEATTIME_FACTOR as f64) as i64,
        song_pos_seconds: (seconds * CLAP_SECTIME_FACTOR as f64) as i64,
        tempo: ctx.time.bpm,
        tempo_inc: 0.0,
        loop_start_beats: 0,
        loop_end_beats: 0,
        loop_start_seconds: 0,
        loop_end_seconds: 0,
        bar_start: 0,
        bar_number: 0,
        tsig_num: 4,
        tsig_denom: 4,
    }
}

// ---------------------------------------------------------------------------
// Instance
// ---------------------------------------------------------------------------

/// A CLAP plugin instance. Created by [`ClapBundle::instantiate`].
pub struct ClapPlugin {
    // Field order matters for `Drop`: the plugin is destroyed in `drop`
    // before `host`/`host_data` (which it may still reference) and before
    // `bundle` (whose library must stay mapped until then).
    plugin: *const clap_plugin,
    /// Never read by the host itself: the plugin keeps the pointer we handed
    /// to `create_plugin`, so the box must live exactly as long as the plugin.
    #[allow(dead_code)]
    host: Box<clap_host>,
    host_data: Box<HostData>,
    bundle: Arc<ClapBundle>,
    descriptor: PluginDescriptor,
    ext_params: *const clap_plugin_params,
    ext_state: *const clap_plugin_state,
    ext_latency: *const clap_plugin_latency,
    ext_audio_ports: *const clap_plugin_audio_ports,
    params: Vec<ParamInfo>,
    cookies: Vec<*mut c_void>,
    ports: AudioPortLayout,
    active: bool,
    processing: bool,
    sample_rate: SampleRate,
    latency: usize,
    events: Vec<ClapEvent>,
    in_ptrs: Vec<*mut f32>,
    out_ptrs: Vec<*mut f32>,
    steady_time: i64,
    last_status: i32,
    process_errors: usize,
}

// SAFETY: a CLAP instance is designed to be driven from one main thread plus
// one audio thread; `ClapPlugin` is moved whole (never shared) between the
// control thread that creates it and the audio thread that processes it,
// which is the ownership model `Node: Send` expresses.
unsafe impl Send for ClapPlugin {}

impl ClapPlugin {
    /// The bundle this instance came from.
    pub fn bundle(&self) -> &Arc<ClapBundle> {
        &self.bundle
    }

    /// Flags the plugin raised through the host interface, cleared on read.
    pub fn take_requests(&self) -> HostRequests {
        HostRequests {
            restart: self.host_data.restart.swap(false, Ordering::AcqRel),
            process: self.host_data.process.swap(false, Ordering::AcqRel),
            callback: self.host_data.callback.swap(false, Ordering::AcqRel),
        }
    }

    /// Runs the plugin's `on_main_thread` callback (call when
    /// [`HostRequests::callback`] was raised).
    pub fn on_main_thread(&mut self) {
        // SAFETY: `plugin` is valid until `drop`.
        if let Some(cb) = unsafe { (*self.plugin).on_main_thread } {
            unsafe { cb(self.plugin) };
        }
    }

    /// Status of the last `process` call (a `CLAP_PROCESS_*` constant).
    pub fn last_process_status(&self) -> i32 {
        self.last_status
    }

    /// Number of `process` calls that returned `CLAP_PROCESS_ERROR`.
    pub fn process_errors(&self) -> usize {
        self.process_errors
    }

    /// `true` if the plugin implements the `clap.state` extension.
    pub fn supports_state(&self) -> bool {
        !self.ext_state.is_null()
    }

    /// Re-reads parameter infos (call after a `params.rescan` request).
    pub fn refresh_params(&mut self) {
        self.params.clear();
        self.cookies.clear();
        if self.ext_params.is_null() {
            return;
        }
        // SAFETY: the extension pointer came from the plugin and lives as long as it.
        let ext = unsafe { &*self.ext_params };
        let (Some(count), Some(get_info)) = (ext.count, ext.get_info) else {
            return;
        };
        // SAFETY: standard extension calls on our valid plugin pointer.
        let n = unsafe { count(self.plugin) };
        for i in 0..n {
            // SAFETY: `info` is a fully initialised out-parameter (zeroed
            // POD) that the plugin fills in.
            let mut info: clap_param_info = unsafe { std::mem::zeroed() };
            if !unsafe { get_info(self.plugin, i, &mut info) } {
                continue;
            }
            self.params.push(ParamInfo {
                id: info.id,
                name: fixed_to_string(&info.name),
                module: fixed_to_string(&info.module),
                min: info.min_value,
                max: info.max_value,
                default: info.default_value,
                automatable: info.flags & CLAP_PARAM_IS_AUTOMATABLE != 0,
                stepped: info.flags & CLAP_PARAM_IS_STEPPED != 0,
                hidden: info.flags & CLAP_PARAM_IS_HIDDEN != 0,
                readonly: info.flags & CLAP_PARAM_IS_READONLY != 0,
                bypass: info.flags & CLAP_PARAM_IS_BYPASS != 0,
            });
            self.cookies.push(info.cookie);
        }
    }

    fn refresh_ports(&mut self) {
        // Without the extension CLAP mandates nothing; assume a stereo effect.
        self.ports = AudioPortLayout {
            inputs: 2,
            outputs: 2,
        };
        if self.ext_audio_ports.is_null() {
            return;
        }
        // SAFETY: extension pointer from the plugin, valid for its lifetime.
        let ext = unsafe { &*self.ext_audio_ports };
        let (Some(count), Some(get)) = (ext.count, ext.get) else {
            return;
        };
        let channels = |is_input: bool| -> usize {
            // SAFETY: standard extension calls; `info` is a zeroed POD out-parameter.
            if unsafe { count(self.plugin, is_input) } == 0 {
                return 0;
            }
            let mut info: clap_audio_port_info = unsafe { std::mem::zeroed() };
            if unsafe { get(self.plugin, 0, is_input, &mut info) } {
                (info.channel_count as usize).min(MAX_CHANNELS)
            } else {
                0
            }
        };
        self.ports = AudioPortLayout {
            inputs: channels(true),
            outputs: channels(false),
        };
    }

    fn refresh_latency(&mut self) {
        self.latency = 0;
        if self.ext_latency.is_null() {
            return;
        }
        // SAFETY: extension pointer from the plugin; `get` is valid once activated.
        if let Some(get) = unsafe { (*self.ext_latency).get } {
            self.latency = unsafe { get(self.plugin) } as usize;
        }
    }

    /// Delivers queued parameter events through `params.flush` (used while
    /// inactive, when no `process` call will pick them up).
    fn flush_params(&mut self) {
        if self.events.is_empty() || self.ext_params.is_null() {
            self.events.clear();
            return;
        }
        // SAFETY: extension pointer from the plugin.
        if let Some(flush) = unsafe { (*self.ext_params).flush } {
            let in_list = clap_input_events {
                ctx: &mut self.events as *mut Vec<ClapEvent> as *mut c_void,
                size: Some(in_events_size),
                get: Some(in_events_get),
            };
            let out_list = clap_output_events {
                ctx: ptr::null_mut(),
                try_push: Some(out_events_try_push),
            };
            // SAFETY: both lists point at stack data that outlives the call.
            unsafe { flush(self.plugin, &in_list, &out_list) };
        }
        self.events.clear();
    }

    fn stop_processing(&mut self) {
        if self.processing {
            // SAFETY: `plugin` is valid; balancing start_processing.
            if let Some(stop) = unsafe { (*self.plugin).stop_processing } {
                unsafe { stop(self.plugin) };
            }
            self.processing = false;
        }
    }
}

impl PluginInstance for ClapPlugin {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn ports(&self) -> AudioPortLayout {
        self.ports
    }

    fn params(&self) -> &[ParamInfo] {
        &self.params
    }

    fn param_value(&self, id: u32) -> Result<f64> {
        if self.ext_params.is_null() {
            return Err(msg("plugin has no params extension"));
        }
        // SAFETY: extension pointer from the plugin.
        let get_value = unsafe { (*self.ext_params).get_value }
            .ok_or_else(|| msg("params.get_value is missing"))?;
        let mut value = 0.0;
        // SAFETY: `value` is a valid out-parameter.
        if unsafe { get_value(self.plugin, id, &mut value) } {
            Ok(value)
        } else {
            Err(SuperbError::NotFound {
                kind: "param",
                id: id as u64,
            })
        }
    }

    fn set_param(&mut self, id: u32, value: f64) -> Result<()> {
        let index = self
            .params
            .iter()
            .position(|p| p.id == id)
            .ok_or(SuperbError::NotFound {
                kind: "param",
                id: id as u64,
            })?;
        if self.events.len() >= self.events.capacity() {
            return Err(SuperbError::CapacityExceeded {
                what: "plugin event queue",
                capacity: self.events.capacity(),
            });
        }
        self.events.push(ClapEvent::Param(clap_event_param_value {
            header: header(
                size_of::<clap_event_param_value>(),
                0,
                CLAP_EVENT_PARAM_VALUE,
            ),
            param_id: id,
            cookie: self.cookies[index],
            note_id: -1,
            port_index: -1,
            channel: -1,
            key: -1,
            value,
        }));
        if !self.active {
            self.flush_params();
        }
        Ok(())
    }

    fn activate(&mut self, sample_rate: SampleRate, max_frames: usize) -> Result<()> {
        if self.active {
            self.deactivate();
        }
        if max_frames == 0 || max_frames > u32::MAX as usize {
            return Err(SuperbError::InvalidArgument { what: "max_frames" });
        }
        // SAFETY: `plugin` is valid until `drop`.
        let activate =
            unsafe { (*self.plugin).activate }.ok_or_else(|| msg("plugin.activate is missing"))?;
        // SAFETY: activate on an inactive plugin, main thread.
        if !unsafe { activate(self.plugin, sample_rate.as_f64(), 1, max_frames as u32) } {
            return Err(msg(format!("{}: activate failed", self.descriptor.name)));
        }
        self.active = true;
        self.sample_rate = sample_rate;
        self.steady_time = 0;
        self.refresh_latency();
        Ok(())
    }

    fn deactivate(&mut self) {
        if !self.active {
            return;
        }
        self.stop_processing();
        // SAFETY: balancing a successful activate.
        if let Some(deactivate) = unsafe { (*self.plugin).deactivate } {
            unsafe { deactivate(self.plugin) };
        }
        self.active = false;
        self.events.clear();
    }

    fn is_active(&self) -> bool {
        self.active
    }

    fn reset(&mut self) {
        if !self.active {
            return;
        }
        // SAFETY: reset is an audio-thread call on an active plugin.
        if let Some(reset) = unsafe { (*self.plugin).reset } {
            unsafe { reset(self.plugin) };
        }
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus {
        if !self.active {
            return ProcessStatus::Silent;
        }
        // SAFETY: `plugin` is valid until `drop`; the calls below are the
        // documented audio-thread sequence on an active plugin.
        let p = unsafe { &*self.plugin };
        if !self.processing {
            match p.start_processing {
                Some(start) if unsafe { start(self.plugin) } => self.processing = true,
                _ => {
                    self.process_errors += 1;
                    return ProcessStatus::Silent;
                }
            }
        }
        let Some(process) = p.process else {
            return ProcessStatus::Silent;
        };

        // MIDI after the (time 0) parameter events; the buffer is already frame-sorted.
        for ev in ctx.midi.iter() {
            if self.events.len() >= self.events.capacity() {
                break;
            }
            self.events.push(event_from_midi(ev));
        }

        self.in_ptrs.clear();
        for ch in 0..input.channels().min(self.ports.inputs).min(MAX_CHANNELS) {
            self.in_ptrs.push(input.channel(ch).as_ptr() as *mut f32);
        }
        self.out_ptrs.clear();
        for plane in output
            .planes_mut()
            .take(self.ports.outputs.min(MAX_CHANNELS))
        {
            self.out_ptrs.push(plane.as_mut_ptr());
        }
        let in_channels = self.in_ptrs.len();
        let constant_mask = if ctx.input_is_silent && in_channels > 0 {
            u64::MAX >> (64 - in_channels)
        } else {
            0
        };
        let in_buf = clap_audio_buffer {
            data32: self.in_ptrs.as_mut_ptr(),
            data64: ptr::null_mut(),
            channel_count: in_channels as u32,
            latency: 0,
            constant_mask,
        };
        let mut out_buf = clap_audio_buffer {
            data32: self.out_ptrs.as_mut_ptr(),
            data64: ptr::null_mut(),
            channel_count: self.out_ptrs.len() as u32,
            latency: 0,
            constant_mask: 0,
        };
        let transport = transport_event(ctx);
        let in_list = clap_input_events {
            ctx: &mut self.events as *mut Vec<ClapEvent> as *mut c_void,
            size: Some(in_events_size),
            get: Some(in_events_get),
        };
        let out_list = clap_output_events {
            ctx: ptr::null_mut(),
            try_push: Some(out_events_try_push),
        };
        let proc_desc = clap_process {
            steady_time: self.steady_time,
            frames_count: ctx.frames as u32,
            transport: &transport,
            audio_inputs: &in_buf,
            audio_outputs: &mut out_buf,
            audio_inputs_count: u32::from(in_channels > 0),
            audio_outputs_count: u32::from(!self.out_ptrs.is_empty()),
            in_events: &in_list,
            out_events: &out_list,
        };
        // SAFETY: every pointer in `proc_desc` refers to stack data or to the
        // caller's buffers, all of which outlive this call; the channel
        // pointer tables were sized in `instantiate` and never grow.
        let status = unsafe { process(self.plugin, &proc_desc) };
        self.events.clear();
        self.steady_time += ctx.frames as i64;
        self.last_status = status;
        match status {
            CLAP_PROCESS_ERROR => {
                self.process_errors += 1;
                ProcessStatus::Silent
            }
            CLAP_PROCESS_SLEEP => ProcessStatus::Silent,
            _ => ProcessStatus::Normal,
        }
    }

    fn latency_frames(&self) -> usize {
        self.latency
    }

    fn save_state(&mut self) -> Result<Vec<u8>> {
        if self.ext_state.is_null() {
            return Err(msg("plugin has no state extension"));
        }
        // SAFETY: extension pointer from the plugin.
        let save = unsafe { (*self.ext_state).save }.ok_or_else(|| msg("state.save is missing"))?;
        let mut out = Vec::new();
        let stream = clap_ostream {
            ctx: &mut out as *mut Vec<u8> as *mut c_void,
            write: Some(ostream_write),
        };
        // SAFETY: the stream points at `out`, which outlives the call.
        if unsafe { save(self.plugin, &stream) } {
            Ok(out)
        } else {
            Err(msg(format!("{}: state.save failed", self.descriptor.name)))
        }
    }

    fn load_state(&mut self, bytes: &[u8]) -> Result<()> {
        if self.ext_state.is_null() {
            return Err(msg("plugin has no state extension"));
        }
        // SAFETY: extension pointer from the plugin.
        let load = unsafe { (*self.ext_state).load }.ok_or_else(|| msg("state.load is missing"))?;
        let mut cursor = ReadCursor {
            data: bytes,
            pos: 0,
        };
        let stream = clap_istream {
            ctx: &mut cursor as *mut ReadCursor<'_> as *mut c_void,
            read: Some(istream_read),
        };
        // SAFETY: the stream points at `cursor`, which outlives the call.
        if unsafe { load(self.plugin, &stream) } {
            Ok(())
        } else {
            Err(msg(format!("{}: state.load failed", self.descriptor.name)))
        }
    }
}

impl Drop for ClapPlugin {
    fn drop(&mut self) {
        self.deactivate();
        // SAFETY: destroy is the last call on a plugin; `plugin` is valid until here.
        if let Some(destroy) = unsafe { (*self.plugin).destroy } {
            unsafe { destroy(self.plugin) };
        }
    }
}

impl core::fmt::Debug for ClapPlugin {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ClapPlugin")
            .field("id", &self.descriptor.id)
            .field("ports", &self.ports)
            .field("params", &self.params.len())
            .field("active", &self.active)
            .field("latency", &self.latency)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::test_plugin::{self, TEST_PLUGIN_ID};
    use super::*;
    use crate::primitives::graph::nodes::ConstantNode;
    use crate::primitives::graph::{AudioGraph, BlockTime, Node};
    use crate::primitives::midi::{MidiChannel, MidiEventBuffer};
    use crate::primitives::plugins::PluginNode;

    const R: SampleRate = SampleRate::HZ_48K;

    /// The in-process test plugin keeps global counters, so tests that
    /// create instances run one at a time.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn bundle() -> Arc<ClapBundle> {
        ClapBundle::from_entry(&test_plugin::TEST_ENTRY, "/virtual/test.clap").unwrap()
    }

    fn ctx<'a>(midi: &'a MidiEventBuffer, frames: usize, silent: bool) -> ProcessContext<'a> {
        ProcessContext {
            sample_rate: R,
            frames,
            time: BlockTime {
                rolling: true,
                ..BlockTime::free_running()
            },
            midi,
            input_is_silent: silent,
        }
    }

    fn ramp(frames: usize) -> PlanarBuffer {
        let mut b = PlanarBuffer::new(2, frames);
        for ch in 0..2 {
            for (f, s) in b.channel_mut(ch).iter_mut().enumerate() {
                *s = (f as f32 + 1.0) * if ch == 0 { 1.0 } else { -1.0 };
            }
        }
        b
    }

    #[test]
    fn bundle_lists_descriptors_and_rejects_unknown_ids() {
        let _guard = serial();
        let b = bundle();
        assert_eq!(b.path(), Path::new("/virtual/test.clap"));
        let descs = b.descriptors();
        assert_eq!(descs.len(), 1);
        let d = &descs[0];
        assert_eq!(d.id, TEST_PLUGIN_ID);
        assert_eq!(d.name, "Test Gain");
        assert_eq!(d.vendor, "Superb");
        assert_eq!(d.version, "1.0.0");
        assert_eq!(d.features, ["audio-effect", "utility"]);
        assert!(d.is_audio_effect() && !d.is_instrument());
        assert_eq!(d.format, PluginFormat::Clap);
        assert!(b.instantiate("nope").is_err());
        assert!(format!("{b:?}").contains("test.clap"));
        assert_eq!(test_plugin::live_instances(), 0);
    }

    #[test]
    fn instance_exposes_params_ports_and_latency() {
        let _guard = serial();
        let b = bundle();
        let mut p = b.instantiate(TEST_PLUGIN_ID).unwrap();
        assert_eq!(test_plugin::live_instances(), 1);
        assert_eq!(
            p.ports(),
            AudioPortLayout {
                inputs: 2,
                outputs: 2
            }
        );
        assert_eq!(p.params().len(), 2);
        let gain = &p.params()[0];
        assert_eq!(
            (
                gain.id,
                gain.name.as_str(),
                gain.min,
                gain.max,
                gain.default
            ),
            (7, "Gain", 0.0, 2.0, 1.0)
        );
        assert!(gain.automatable && !gain.readonly && !gain.hidden);
        assert_eq!(p.params()[1].module, "Diagnostics");
        assert!(p.params()[1].readonly && p.params()[1].hidden);
        assert_eq!(p.param_value(7).unwrap(), 1.0);
        assert!(p.param_value(99).is_err());
        assert!(p.set_param(99, 1.0).is_err());
        assert_eq!(p.latency_frames(), 0, "unknown until activated");
        assert!(!p.is_active());
        p.activate(R, 64).unwrap();
        assert!(p.is_active());
        assert_eq!(p.latency_frames(), 16);
        assert!(p.activate(R, 0).is_err());
        assert!(
            !p.is_active(),
            "a failed re-activate leaves the plugin inactive"
        );
        assert!(p.supports_state());
        assert!(format!("{p:?}").contains(TEST_PLUGIN_ID));
        drop(p);
        assert_eq!(test_plugin::live_instances(), 0, "destroyed on drop");
    }

    #[test]
    fn params_are_flushed_while_inactive_and_applied_in_process() {
        let _guard = serial();
        let blocks_before = test_plugin::blocks_processed();
        let b = bundle();
        let mut p = b.instantiate(TEST_PLUGIN_ID).unwrap();
        p.set_param(7, 0.25).unwrap(); // inactive: flushed immediately
        assert_eq!(p.param_value(7).unwrap(), 0.25);

        p.activate(R, 8).unwrap();
        p.set_param(7, 0.5).unwrap(); // active: queued for the next block
        assert_eq!(p.param_value(7).unwrap(), 0.25);
        let midi = MidiEventBuffer::with_capacity(0);
        let input = ramp(8);
        let mut output = PlanarBuffer::new(2, 8);
        assert_eq!(
            p.process(&ctx(&midi, 8, false), &input, &mut output),
            ProcessStatus::Normal
        );
        assert_eq!(p.param_value(7).unwrap(), 0.5);
        assert_eq!(output.channel(0), &[0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0]);
        assert_eq!(output.channel(1)[7], -4.0);
        assert_eq!(
            p.last_process_status(),
            clap_sys::process::CLAP_PROCESS_CONTINUE
        );
        assert_eq!(p.process_errors(), 0);
        assert!(test_plugin::saw_transport_playing());
        assert_eq!(test_plugin::blocks_processed() - blocks_before, 1);

        // Silent input: constant mask set, plugin reports sleep -> Silent.
        assert_eq!(
            p.process(&ctx(&midi, 8, true), &input, &mut output),
            ProcessStatus::Silent
        );
        assert_eq!(p.last_process_status(), CLAP_PROCESS_SLEEP);

        p.reset();
        p.deactivate();
        assert_eq!(
            p.process(&ctx(&midi, 8, false), &input, &mut output),
            ProcessStatus::Silent
        );
        assert_eq!(p.take_requests(), HostRequests::default());
    }

    #[test]
    fn midi_reaches_the_plugin_as_note_and_raw_events() {
        let _guard = serial();
        let raw_before = test_plugin::raw_midi_seen();
        let b = bundle();
        let mut p = b.instantiate(TEST_PLUGIN_ID).unwrap();
        p.activate(R, 16).unwrap();
        let ch = MidiChannel::new(3).unwrap();
        let mut midi = MidiEventBuffer::with_capacity(8);
        midi.push(MidiEvent {
            frame: 2,
            message: MidiMessage::NoteOn {
                channel: ch,
                note: 60,
                velocity: 127,
            },
        })
        .unwrap();
        midi.push(MidiEvent {
            frame: 5,
            message: MidiMessage::NoteOff {
                channel: ch,
                note: 60,
                velocity: 0,
            },
        })
        .unwrap();
        midi.push(MidiEvent {
            frame: 9,
            message: MidiMessage::ControlChange {
                channel: ch,
                controller: 1,
                value: 64,
            },
        })
        .unwrap();
        let input = ramp(16);
        let mut output = PlanarBuffer::new(2, 16);
        p.process(&ctx(&midi, 16, false), &input, &mut output);
        assert_eq!(
            p.param_value(8).unwrap(),
            1.0,
            "one note-on counted (hidden diagnostic param)"
        );
        assert_eq!(test_plugin::last_note(), (3, 60, 2));
        assert_eq!(test_plugin::raw_midi_seen() - raw_before, 1);
        // Requests raised by the plugin surface through the host flags.
        test_plugin::ask_for_restart();
        let req = p.take_requests();
        assert!(req.restart && !req.process && !req.callback);
        assert_eq!(
            p.take_requests(),
            HostRequests::default(),
            "cleared on read"
        );
        p.on_main_thread();
    }

    #[test]
    fn state_round_trips_between_instances() {
        let _guard = serial();
        let b = bundle();
        let mut a = b.instantiate(TEST_PLUGIN_ID).unwrap();
        a.set_param(7, 1.75).unwrap();
        let state = a.save_state().unwrap();
        assert_eq!(state.len(), 8);
        let mut c = b.instantiate(TEST_PLUGIN_ID).unwrap();
        assert_eq!(c.param_value(7).unwrap(), 1.0);
        c.load_state(&state).unwrap();
        assert_eq!(c.param_value(7).unwrap(), 1.75);
        assert!(
            c.load_state(&state[..3]).is_err(),
            "short stream is rejected by the plugin"
        );
        assert_eq!(test_plugin::live_instances(), 2);
        drop(a);
        drop(c);
        assert_eq!(test_plugin::live_instances(), 0);
    }

    #[test]
    fn plugin_node_runs_inside_a_graph_with_compensation() {
        let _guard = serial();
        let b = bundle();
        let plugin = b.instantiate(TEST_PLUGIN_ID).unwrap();
        let mut node = PluginNode::new(Box::new(plugin));
        assert_eq!(node.name(), "Test Gain");
        let mut tx = node.take_param_sender().unwrap();
        let mut g = AudioGraph::new();
        let src = g.add_node(Box::new(ConstantNode::new(1.0, 2)));
        let fx = g.add_node(Box::new(node));
        g.connect(src, fx).unwrap();
        g.set_output(fx).unwrap();
        g.prepare(R, 32).unwrap();
        assert_eq!(
            g.latency_frames(),
            16,
            "reported through Node::latency_frames after activation"
        );
        tx.push(crate::primitives::plugins::ParamChange { id: 7, value: 2.0 })
            .unwrap();
        g.process(
            BlockTime::free_running(),
            32,
            &MidiEventBuffer::with_capacity(0),
        )
        .unwrap();
        assert_eq!(g.output().channel(1), &[2.0; 32]);
        // The bundle outlives the graph's instance: dropping our Arc first is fine.
        drop(b);
        g.process(
            BlockTime::free_running(),
            32,
            &MidiEventBuffer::with_capacity(0),
        )
        .unwrap();
        drop(g);
        assert_eq!(test_plugin::live_instances(), 0);
    }

    #[test]
    fn loading_missing_or_bogus_files_fails_cleanly() {
        assert!(ClapBundle::load("/nonexistent/thing.clap").is_err());
        let dir = std::env::temp_dir().join(format!("superb-clap-bogus-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("bogus.clap");
        std::fs::write(&file, b"not a shared object").unwrap();
        assert!(ClapBundle::load(&file).is_err());
        let bundle_dir = dir.join("empty.clap");
        std::fs::create_dir_all(&bundle_dir).unwrap();
        assert!(ClapBundle::load(&bundle_dir)
            .unwrap_err()
            .to_string()
            .contains("Contents/MacOS"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
