//! An in-process CLAP plugin used to test the host without loading any
//! shared library: a stereo gain with one automatable parameter, a hidden
//! diagnostic parameter, 16 frames of reported latency, and 8-byte state.

use std::ffi::{c_char, c_void, CStr};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::{
    clap_event_note, clap_event_param_value, clap_input_events, clap_output_events,
    CLAP_EVENT_MIDI, CLAP_EVENT_NOTE_ON, CLAP_EVENT_PARAM_VALUE, CLAP_TRANSPORT_IS_PLAYING,
};
use clap_sys::ext::audio_ports::{
    clap_audio_port_info, clap_plugin_audio_ports, CLAP_AUDIO_PORT_IS_MAIN, CLAP_EXT_AUDIO_PORTS,
    CLAP_PORT_STEREO,
};
use clap_sys::ext::latency::{clap_plugin_latency, CLAP_EXT_LATENCY};
use clap_sys::ext::params::{
    clap_param_info, clap_plugin_params, CLAP_EXT_PARAMS, CLAP_PARAM_IS_AUTOMATABLE,
    CLAP_PARAM_IS_HIDDEN, CLAP_PARAM_IS_READONLY,
};
use clap_sys::ext::state::{clap_plugin_state, CLAP_EXT_STATE};
use clap_sys::factory::plugin_factory::{clap_plugin_factory, CLAP_PLUGIN_FACTORY_ID};
use clap_sys::host::clap_host;
use clap_sys::id::CLAP_INVALID_ID;
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::process::{
    clap_process, clap_process_status, CLAP_PROCESS_CONTINUE, CLAP_PROCESS_SLEEP,
};
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::CLAP_VERSION;

/// ID of the test plugin.
pub const TEST_PLUGIN_ID: &str = "aura.test.gain";
const PARAM_GAIN: u32 = 7;
const PARAM_NOTES: u32 = 8;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static BLOCKS: AtomicUsize = AtomicUsize::new(0);
static RAW_MIDI: AtomicUsize = AtomicUsize::new(0);
static TRANSPORT_PLAYING: AtomicBool = AtomicBool::new(false);
static LAST_NOTE: AtomicU32 = AtomicU32::new(0);
static LAST_HOST: AtomicUsize = AtomicUsize::new(0);

/// Instances currently alive (created and not yet destroyed).
pub fn live_instances() -> usize {
    LIVE.load(Ordering::SeqCst)
}
/// Total `process` calls across all instances.
pub fn blocks_processed() -> usize {
    BLOCKS.load(Ordering::SeqCst)
}
/// Raw (non-note) MIDI events seen.
pub fn raw_midi_seen() -> usize {
    RAW_MIDI.load(Ordering::SeqCst)
}
/// `true` if the last processed block's transport said "playing".
pub fn saw_transport_playing() -> bool {
    TRANSPORT_PLAYING.load(Ordering::SeqCst)
}
/// `(channel, key, frame)` of the last note-on.
pub fn last_note() -> (u8, u8, u32) {
    let v = LAST_NOTE.load(Ordering::SeqCst);
    ((v >> 24) as u8, ((v >> 16) & 0xFF) as u8, v & 0xFFFF)
}
/// Makes the most recently created instance call `host.request_restart`.
pub fn ask_for_restart() {
    let host = LAST_HOST.load(Ordering::SeqCst) as *const clap_host;
    if !host.is_null() {
        // SAFETY: the host pointer was handed to `create` by the host and
        // stays valid while the instance lives (tests keep it alive).
        if let Some(f) = unsafe { (*host).request_restart } {
            unsafe { f(host) };
        }
    }
}

struct SyncPtrs<const N: usize>([*const c_char; N]);
// SAFETY: the pointers refer to string literals, which are immutable.
unsafe impl<const N: usize> Sync for SyncPtrs<N> {}

static FEATURES: SyncPtrs<3> =
    SyncPtrs([c"audio-effect".as_ptr(), c"utility".as_ptr(), ptr::null()]);

static DESCRIPTOR: clap_plugin_descriptor = clap_plugin_descriptor {
    clap_version: CLAP_VERSION,
    id: c"aura.test.gain".as_ptr(),
    name: c"Test Gain".as_ptr(),
    vendor: c"Aura".as_ptr(),
    url: c"https://example.invalid".as_ptr(),
    manual_url: c"".as_ptr(),
    support_url: c"".as_ptr(),
    version: c"1.0.0".as_ptr(),
    description: c"Stereo gain used by the aura-plugins host tests".as_ptr(),
    features: FEATURES.0.as_ptr(),
};

#[repr(C)]
struct Gain {
    plugin: clap_plugin,
    gain: f64,
    notes: u32,
    active: bool,
    processing: bool,
}

/// # Safety
/// `p` must be a plugin created by [`create`].
unsafe fn state<'a>(p: *const clap_plugin) -> &'a mut Gain {
    // SAFETY: `plugin_data` was set to the `Gain` allocation in `create`.
    unsafe { &mut *((*p).plugin_data as *mut Gain) }
}

unsafe extern "C" fn count(_f: *const clap_plugin_factory) -> u32 {
    1
}

unsafe extern "C" fn descriptor(
    _f: *const clap_plugin_factory,
    index: u32,
) -> *const clap_plugin_descriptor {
    if index == 0 {
        &DESCRIPTOR
    } else {
        ptr::null()
    }
}

unsafe extern "C" fn create(
    _f: *const clap_plugin_factory,
    host: *const clap_host,
    id: *const c_char,
) -> *const clap_plugin {
    // SAFETY: the host passes a NUL-terminated id.
    if unsafe { CStr::from_ptr(id) } != c"aura.test.gain" {
        return ptr::null();
    }
    LAST_HOST.store(host as usize, Ordering::SeqCst);
    let boxed = Box::new(Gain {
        plugin: clap_plugin {
            desc: &DESCRIPTOR,
            plugin_data: ptr::null_mut(),
            init: Some(p_init),
            destroy: Some(p_destroy),
            activate: Some(p_activate),
            deactivate: Some(p_deactivate),
            start_processing: Some(p_start),
            stop_processing: Some(p_stop),
            reset: Some(p_reset),
            process: Some(p_process),
            get_extension: Some(p_get_extension),
            on_main_thread: Some(p_on_main_thread),
        },
        gain: 1.0,
        notes: 0,
        active: false,
        processing: false,
    });
    let raw = Box::into_raw(boxed);
    // SAFETY: `raw` is a live allocation; `plugin` is its first field
    // (`repr(C)`), so a pointer to it is the plugin pointer.
    unsafe {
        (*raw).plugin.plugin_data = raw as *mut c_void;
        LIVE.fetch_add(1, Ordering::SeqCst);
        &(*raw).plugin
    }
}

unsafe extern "C" fn p_init(_p: *const clap_plugin) -> bool {
    true
}

unsafe extern "C" fn p_destroy(p: *const clap_plugin) {
    // SAFETY: reclaiming the allocation made in `create`, exactly once.
    unsafe { drop(Box::from_raw((*p).plugin_data as *mut Gain)) };
    LIVE.fetch_sub(1, Ordering::SeqCst);
}

unsafe extern "C" fn p_activate(
    p: *const clap_plugin,
    sample_rate: f64,
    _min: u32,
    max: u32,
) -> bool {
    // SAFETY: valid plugin from `create`.
    let g = unsafe { state(p) };
    if sample_rate <= 0.0 || max == 0 {
        return false;
    }
    g.active = true;
    true
}

unsafe extern "C" fn p_deactivate(p: *const clap_plugin) {
    // SAFETY: valid plugin from `create`.
    unsafe { state(p) }.active = false;
}

unsafe extern "C" fn p_start(p: *const clap_plugin) -> bool {
    // SAFETY: valid plugin from `create`.
    let g = unsafe { state(p) };
    g.processing = g.active;
    g.processing
}

unsafe extern "C" fn p_stop(p: *const clap_plugin) {
    // SAFETY: valid plugin from `create`.
    unsafe { state(p) }.processing = false;
}

unsafe extern "C" fn p_reset(_p: *const clap_plugin) {}

unsafe extern "C" fn p_on_main_thread(_p: *const clap_plugin) {}

/// # Safety
/// `list` must be a valid input event list.
unsafe fn apply_events(g: &mut Gain, list: *const clap_input_events) {
    if list.is_null() {
        return;
    }
    // SAFETY: guaranteed by the caller; the host's list callbacks are valid.
    let l = unsafe { &*list };
    let (Some(size), Some(get)) = (l.size, l.get) else {
        return;
    };
    let n = unsafe { size(list) };
    for i in 0..n {
        let h = unsafe { get(list, i) };
        if h.is_null() {
            continue;
        }
        // SAFETY: every event starts with a header; the type tells us the full struct.
        let hdr = unsafe { &*h };
        match hdr.type_ {
            CLAP_EVENT_PARAM_VALUE => {
                let e = unsafe { &*(h as *const clap_event_param_value) };
                if e.param_id == PARAM_GAIN {
                    g.gain = e.value;
                }
            }
            CLAP_EVENT_NOTE_ON => {
                let e = unsafe { &*(h as *const clap_event_note) };
                g.notes += 1;
                LAST_NOTE.store(
                    ((e.channel as u32) << 24) | ((e.key as u32) << 16) | (hdr.time & 0xFFFF),
                    Ordering::SeqCst,
                );
            }
            CLAP_EVENT_MIDI => {
                RAW_MIDI.fetch_add(1, Ordering::SeqCst);
            }
            _ => {}
        }
    }
}

unsafe extern "C" fn p_process(
    p: *const clap_plugin,
    process: *const clap_process,
) -> clap_process_status {
    // SAFETY: valid plugin from `create`; `process` is the host's descriptor for this call.
    let g = unsafe { state(p) };
    let pr = unsafe { &*process };
    BLOCKS.fetch_add(1, Ordering::SeqCst);
    if !pr.transport.is_null() {
        TRANSPORT_PLAYING.store(
            unsafe { (*pr.transport).flags } & CLAP_TRANSPORT_IS_PLAYING != 0,
            Ordering::SeqCst,
        );
    }
    unsafe { apply_events(g, pr.in_events) };
    if pr.audio_inputs_count == 0 || pr.audio_outputs_count == 0 {
        return CLAP_PROCESS_SLEEP;
    }
    let input: &clap_audio_buffer = unsafe { &*pr.audio_inputs };
    let output: &mut clap_audio_buffer = unsafe { &mut *pr.audio_outputs };
    let frames = pr.frames_count as usize;
    let channels = input.channel_count.min(output.channel_count) as usize;
    let all_constant =
        input.channel_count > 0 && input.constant_mask == (u64::MAX >> (64 - input.channel_count));
    for ch in 0..channels {
        // SAFETY: the host guarantees `channel_count` valid channel pointers
        // of `frames_count` samples each.
        unsafe {
            let src = *input.data32.add(ch);
            let dst = *output.data32.add(ch);
            for f in 0..frames {
                *dst.add(f) = if all_constant {
                    0.0
                } else {
                    *src.add(f) * g.gain as f32
                };
            }
        }
    }
    if all_constant {
        CLAP_PROCESS_SLEEP
    } else {
        CLAP_PROCESS_CONTINUE
    }
}

// --- extensions -------------------------------------------------------------

fn write_fixed(dst: &mut [c_char], text: &str) {
    for (d, s) in dst.iter_mut().zip(text.bytes().chain(std::iter::once(0))) {
        *d = s as c_char;
    }
}

unsafe extern "C" fn params_count(_p: *const clap_plugin) -> u32 {
    2
}

unsafe extern "C" fn params_get_info(
    _p: *const clap_plugin,
    index: u32,
    info: *mut clap_param_info,
) -> bool {
    // SAFETY: the host passes a writable `clap_param_info`.
    let info = unsafe { &mut *info };
    match index {
        0 => {
            info.id = PARAM_GAIN;
            info.flags = CLAP_PARAM_IS_AUTOMATABLE;
            info.cookie = ptr::null_mut();
            write_fixed(&mut info.name, "Gain");
            write_fixed(&mut info.module, "");
            info.min_value = 0.0;
            info.max_value = 2.0;
            info.default_value = 1.0;
            true
        }
        1 => {
            info.id = PARAM_NOTES;
            info.flags = CLAP_PARAM_IS_READONLY | CLAP_PARAM_IS_HIDDEN;
            info.cookie = ptr::null_mut();
            write_fixed(&mut info.name, "Notes seen");
            write_fixed(&mut info.module, "Diagnostics");
            info.min_value = 0.0;
            info.max_value = 1e9;
            info.default_value = 0.0;
            true
        }
        _ => false,
    }
}

unsafe extern "C" fn params_get_value(p: *const clap_plugin, id: u32, out: *mut f64) -> bool {
    // SAFETY: valid plugin; `out` is a writable f64.
    let g = unsafe { state(p) };
    let v = match id {
        PARAM_GAIN => g.gain,
        PARAM_NOTES => g.notes as f64,
        _ => return false,
    };
    unsafe { *out = v };
    true
}

unsafe extern "C" fn params_value_to_text(
    _p: *const clap_plugin,
    _id: u32,
    _v: f64,
    _buf: *mut c_char,
    _cap: u32,
) -> bool {
    false
}

unsafe extern "C" fn params_text_to_value(
    _p: *const clap_plugin,
    _id: u32,
    _text: *const c_char,
    _out: *mut f64,
) -> bool {
    false
}

unsafe extern "C" fn params_flush(
    p: *const clap_plugin,
    input: *const clap_input_events,
    _out: *const clap_output_events,
) {
    // SAFETY: valid plugin; the host's list is valid for the call.
    let g = unsafe { state(p) };
    unsafe { apply_events(g, input) };
}

static PARAMS_EXT: clap_plugin_params = clap_plugin_params {
    count: Some(params_count),
    get_info: Some(params_get_info),
    get_value: Some(params_get_value),
    value_to_text: Some(params_value_to_text),
    text_to_value: Some(params_text_to_value),
    flush: Some(params_flush),
};

unsafe extern "C" fn state_save(p: *const clap_plugin, stream: *const clap_ostream) -> bool {
    // SAFETY: valid plugin; the host's stream is valid for the call.
    let g = unsafe { state(p) };
    let s = unsafe { &*stream };
    let Some(write) = s.write else { return false };
    let bytes = g.gain.to_le_bytes();
    unsafe { write(stream, bytes.as_ptr() as *const c_void, 8) == 8 }
}

unsafe extern "C" fn state_load(p: *const clap_plugin, stream: *const clap_istream) -> bool {
    // SAFETY: valid plugin; the host's stream is valid for the call.
    let g = unsafe { state(p) };
    let s = unsafe { &*stream };
    let Some(read) = s.read else { return false };
    let mut bytes = [0u8; 8];
    if unsafe { read(stream, bytes.as_mut_ptr() as *mut c_void, 8) } != 8 {
        return false;
    }
    g.gain = f64::from_le_bytes(bytes);
    true
}

static STATE_EXT: clap_plugin_state = clap_plugin_state {
    save: Some(state_save),
    load: Some(state_load),
};

unsafe extern "C" fn latency_get(_p: *const clap_plugin) -> u32 {
    16
}

static LATENCY_EXT: clap_plugin_latency = clap_plugin_latency {
    get: Some(latency_get),
};

unsafe extern "C" fn ports_count(_p: *const clap_plugin, _is_input: bool) -> u32 {
    1
}

unsafe extern "C" fn ports_get(
    _p: *const clap_plugin,
    index: u32,
    _is_input: bool,
    info: *mut clap_audio_port_info,
) -> bool {
    if index != 0 {
        return false;
    }
    // SAFETY: the host passes a writable `clap_audio_port_info`.
    let info = unsafe { &mut *info };
    info.id = 0;
    write_fixed(&mut info.name, "main");
    info.flags = CLAP_AUDIO_PORT_IS_MAIN;
    info.channel_count = 2;
    info.port_type = CLAP_PORT_STEREO.as_ptr();
    info.in_place_pair = CLAP_INVALID_ID;
    true
}

static PORTS_EXT: clap_plugin_audio_ports = clap_plugin_audio_ports {
    count: Some(ports_count),
    get: Some(ports_get),
};

unsafe extern "C" fn p_get_extension(_p: *const clap_plugin, id: *const c_char) -> *const c_void {
    // SAFETY: the host passes a NUL-terminated extension id.
    let id = unsafe { CStr::from_ptr(id) };
    if id == CLAP_EXT_PARAMS {
        &PARAMS_EXT as *const _ as *const c_void
    } else if id == CLAP_EXT_STATE {
        &STATE_EXT as *const _ as *const c_void
    } else if id == CLAP_EXT_LATENCY {
        &LATENCY_EXT as *const _ as *const c_void
    } else if id == CLAP_EXT_AUDIO_PORTS {
        &PORTS_EXT as *const _ as *const c_void
    } else {
        ptr::null()
    }
}

// --- entry ------------------------------------------------------------------

static FACTORY: clap_plugin_factory = clap_plugin_factory {
    get_plugin_count: Some(count),
    get_plugin_descriptor: Some(descriptor),
    create_plugin: Some(create),
};

unsafe extern "C" fn entry_init(_path: *const c_char) -> bool {
    true
}

unsafe extern "C" fn entry_deinit() {}

unsafe extern "C" fn entry_get_factory(id: *const c_char) -> *const c_void {
    // SAFETY: the host passes a NUL-terminated factory id.
    if unsafe { CStr::from_ptr(id) } == CLAP_PLUGIN_FACTORY_ID {
        &FACTORY as *const _ as *const c_void
    } else {
        ptr::null()
    }
}

/// The plugin's entry point, as a `.clap` would export it under `clap_entry`.
pub static TEST_ENTRY: clap_plugin_entry = clap_plugin_entry {
    clap_version: CLAP_VERSION,
    init: Some(entry_init),
    deinit: Some(entry_deinit),
    get_factory: Some(entry_get_factory),
};
