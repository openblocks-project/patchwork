//! CLAP plugin host — loads .clap files, creates instances, processes audio.
//!
//! Uses clap-sys (raw C bindings) directly. No wrapper crate needed.
//! The CLAP API is small: load → factory → create → activate → process.

use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr;

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::*;
use clap_sys::ext::gui::*;
use clap_sys::ext::params::*;
use clap_sys::factory::plugin_factory::*;
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::process::*;
use clap_sys::version::CLAP_VERSION;

/// Plugin type — determined from audio/note port declarations.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum ClapPluginType {
    /// Has audio in + audio out, no note in (delay, reverb, etc.)
    #[default]
    Effect,
    /// Has note in + audio out, no audio in (synths, samplers)
    Instrument,
}

/// Polymorphic event buffer — holds param events, note events, etc.
/// Events are stored as raw bytes with offsets for random access.
/// The CLAP input_events callbacks read from this.
pub struct EventBuffer {
    data: Vec<u8>,
    offsets: Vec<usize>,
}

impl EventBuffer {
    pub fn new() -> Self {
        Self {
            data: Vec::with_capacity(4096),
            offsets: Vec::with_capacity(64),
        }
    }

    pub fn clear(&mut self) {
        self.data.clear();
        self.offsets.clear();
    }

    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    pub fn get_header(&self, index: usize) -> *const clap_event_header {
        if index < self.offsets.len() {
            unsafe { self.data.as_ptr().add(self.offsets[index]) as *const clap_event_header }
        } else {
            ptr::null()
        }
    }

    pub fn push_param(&mut self, param_id: u32, value: f64) {
        let event = clap_event_param_value {
            header: clap_event_header {
                size: std::mem::size_of::<clap_event_param_value>() as u32,
                time: 0,
                space_id: CLAP_CORE_EVENT_SPACE_ID,
                type_: CLAP_EVENT_PARAM_VALUE,
                flags: 0,
            },
            param_id,
            cookie: ptr::null_mut(),
            note_id: -1,
            port_index: -1,
            channel: -1,
            key: -1,
            value,
        };
        self.push_raw(&event);
    }

    pub fn push_note_on(&mut self, key: i16, velocity: f64, channel: i16) {
        let event = clap_event_note {
            header: clap_event_header {
                size: std::mem::size_of::<clap_event_note>() as u32,
                time: 0,
                space_id: CLAP_CORE_EVENT_SPACE_ID,
                type_: CLAP_EVENT_NOTE_ON,
                flags: 0,
            },
            note_id: -1,
            port_index: 0,
            channel,
            key,
            velocity,
        };
        self.push_raw(&event);
    }

    pub fn push_note_off(&mut self, key: i16, velocity: f64, channel: i16) {
        let event = clap_event_note {
            header: clap_event_header {
                size: std::mem::size_of::<clap_event_note>() as u32,
                time: 0,
                space_id: CLAP_CORE_EVENT_SPACE_ID,
                type_: CLAP_EVENT_NOTE_OFF,
                flags: 0,
            },
            note_id: -1,
            port_index: 0,
            channel,
            key,
            velocity: velocity.max(0.0),
        };
        self.push_raw(&event);
    }

    fn push_raw<T>(&mut self, event: &T) {
        let bytes = unsafe {
            std::slice::from_raw_parts(event as *const T as *const u8, std::mem::size_of::<T>())
        };
        self.offsets.push(self.data.len());
        self.data.extend_from_slice(bytes);
    }
}

/// Info about a single plugin parameter.
#[derive(Clone, Debug)]
pub struct ClapParamInfo {
    pub id: u32,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub flags: u32,
    /// For enum/stepped params: text labels for each integer step value.
    /// Empty for continuous params.
    pub value_labels: Vec<String>,
}

/// Info about a loaded CLAP plugin.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ClapPluginInfo {
    pub name: String,
    pub vendor: String,
    pub id: String,
    pub params: Vec<ClapParamInfo>,
    pub audio_in_channels: u32,
    pub audio_out_channels: u32,
    pub plugin_type: ClapPluginType,
}

/// A loaded and activated CLAP plugin instance, ready to process audio.
#[allow(dead_code)]
pub struct ClapInstance {
    _library: libloading::Library,
    plugin: *const clap_plugin,
    host_data: Box<HostData>,
    pub info: ClapPluginInfo,
    /// Params extension pointer (valid for plugin lifetime)
    params_ext: *const clap_plugin_params,
    /// Deinterleaved input buffers (one Vec per channel)
    in_bufs: Vec<Vec<f32>>,
    /// Deinterleaved output buffers
    out_bufs: Vec<Vec<f32>>,
    /// Unified event buffer for process calls (params + notes)
    event_buffer: EventBuffer,
    /// Param changes from plugin GUI (output: plugin → host)
    pub gui_param_changes: Vec<(u32, f64)>,
    /// Steady time counter (sample position)
    steady_time: i64,
    pub activated: bool,
    processing_started: bool,
}

// The host data structure — kept alive as long as the plugin exists.
struct HostData {
    host: clap_host,
}

// ── Host GUI extension callbacks ─────────────────────────────────────────────
// The plugin checks if the host supports GUI before opening a window.
// Without these, the plugin silently refuses to create any window.

static HOST_GUI: clap_host_gui = clap_host_gui {
    resize_hints_changed: Some(host_gui_resize_hints_changed),
    request_resize: Some(host_gui_request_resize),
    request_show: Some(host_gui_request_show),
    request_hide: Some(host_gui_request_hide),
    closed: Some(host_gui_closed),
};

unsafe extern "C" fn host_gui_resize_hints_changed(_host: *const clap_host) {
    crate::system_log::log("HOST_GUI: resize_hints_changed");
}
unsafe extern "C" fn host_gui_request_resize(_host: *const clap_host, w: u32, h: u32) -> bool {
    crate::system_log::log(format!("HOST_GUI: request_resize({}x{})", w, h));
    true
}
unsafe extern "C" fn host_gui_request_show(_host: *const clap_host) -> bool {
    crate::system_log::log("HOST_GUI: request_show");
    true
}
unsafe extern "C" fn host_gui_request_hide(_host: *const clap_host) -> bool {
    crate::system_log::log("HOST_GUI: request_hide");
    true
}
unsafe extern "C" fn host_gui_closed(_host: *const clap_host, was_destroyed: bool) {
    crate::system_log::log(format!("HOST_GUI: closed(was_destroyed={})", was_destroyed));
}

// ── Host thread-check extension ──────────────────────────────────────────────
// Required by many plugins to verify they're on the correct thread for GUI ops.

use clap_sys::ext::thread_check::*;

static HOST_THREAD_CHECK: clap_host_thread_check = clap_host_thread_check {
    is_main_thread: Some(host_is_main_thread),
    is_audio_thread: Some(host_is_audio_thread),
};

// Store the main thread ID at startup so we can reliably check later
static MAIN_THREAD_ID: std::sync::OnceLock<std::thread::ThreadId> = std::sync::OnceLock::new();

/// Call once from main() to record the main thread ID.
pub fn init_main_thread() {
    MAIN_THREAD_ID.get_or_init(|| std::thread::current().id());
}

unsafe extern "C" fn host_is_main_thread(_host: *const clap_host) -> bool {
    MAIN_THREAD_ID
        .get()
        .map(|&main_id| std::thread::current().id() == main_id)
        .unwrap_or(true)
}

unsafe extern "C" fn host_is_audio_thread(_host: *const clap_host) -> bool {
    !unsafe { host_is_main_thread(_host) }
}

// ── Host core callbacks ─────────────────────────────────────────────────────

unsafe extern "C" fn host_get_extension(
    _host: *const clap_host,
    id: *const c_char,
) -> *const c_void {
    let id_str = unsafe { CStr::from_ptr(id) };
    if id_str == CLAP_EXT_GUI {
        return &HOST_GUI as *const clap_host_gui as *const c_void;
    }
    if id_str == CLAP_EXT_THREAD_CHECK {
        return &HOST_THREAD_CHECK as *const clap_host_thread_check as *const c_void;
    }
    ptr::null()
}
unsafe extern "C" fn host_request_restart(_host: *const clap_host) {}
unsafe extern "C" fn host_request_process(_host: *const clap_host) {}
unsafe extern "C" fn host_request_callback(_host: *const clap_host) {}

unsafe fn cstr_to_string(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p).to_string_lossy().to_string() }
    }
}

impl ClapInstance {
    /// Load a .clap file and create a plugin instance.
    pub fn load(path: &str, sample_rate: f64, max_block_size: u32) -> Result<Self, String> {
        // On macOS, .clap files are bundles (directories).
        // The actual dylib is at <bundle>/Contents/MacOS/<name>
        let lib_path = {
            let p = std::path::Path::new(path);
            if p.is_dir() {
                // It's a bundle — find the dylib inside
                let stem = p
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let macos_dir = p.join("Contents").join("MacOS");
                if macos_dir.exists() {
                    // Try exact stem name first, then any file in the directory
                    let dylib = macos_dir.join(&stem);
                    if dylib.exists() {
                        dylib.to_string_lossy().to_string()
                    } else {
                        // Pick first file
                        std::fs::read_dir(&macos_dir)
                            .ok()
                            .and_then(|mut d| d.next())
                            .and_then(|e| e.ok())
                            .map(|e| e.path().to_string_lossy().to_string())
                            .unwrap_or_else(|| path.to_string())
                    }
                } else {
                    path.to_string()
                }
            } else {
                path.to_string()
            }
        };

        // Load library
        let library = unsafe { libloading::Library::new(&lib_path) }
            .map_err(|e| format!("Failed to load .clap: {}", e))?;

        // Get entry point
        let entry: &clap_plugin_entry = unsafe {
            let sym: libloading::Symbol<*const clap_plugin_entry> = library
                .get(b"clap_entry")
                .map_err(|e| format!("No clap_entry symbol: {}", e))?;
            &**sym
        };

        // Initialize
        let path_c = CString::new(path).map_err(|_| "Invalid path")?;
        unsafe {
            if let Some(init) = entry.init {
                if !init(path_c.as_ptr()) {
                    return Err("Plugin init() failed".into());
                }
            }
        }

        // Get plugin factory
        let factory: &clap_plugin_factory = unsafe {
            let factory_ptr =
                entry.get_factory.ok_or("No get_factory")?(CLAP_PLUGIN_FACTORY_ID.as_ptr());
            if factory_ptr.is_null() {
                return Err("No plugin factory".into());
            }
            &*(factory_ptr as *const clap_plugin_factory)
        };

        // Get first plugin descriptor
        let plugin_count =
            unsafe { factory.get_plugin_count.ok_or("No get_plugin_count")?(factory) };
        if plugin_count == 0 {
            return Err("No plugins in .clap file".into());
        }

        let desc = unsafe {
            factory
                .get_plugin_descriptor
                .ok_or("No get_plugin_descriptor")?(factory, 0)
        };
        if desc.is_null() {
            return Err("Null plugin descriptor".into());
        }

        let plugin_name = unsafe { cstr_to_string((*desc).name) };
        let plugin_vendor = unsafe { cstr_to_string((*desc).vendor) };
        let plugin_id_str = unsafe { cstr_to_string((*desc).id) };

        // Create host
        let host_name = CString::new("PatchWork").unwrap();
        let host_vendor = CString::new("OpenBlocks").unwrap();
        let host_url = CString::new("https://github.com/nicholasgasior/patchwork").unwrap();
        let host_version = CString::new("0.0.2").unwrap();

        let mut host_data = Box::new(HostData {
            host: clap_host {
                clap_version: CLAP_VERSION,
                host_data: ptr::null_mut(),
                name: host_name.as_ptr(),
                vendor: host_vendor.as_ptr(),
                url: host_url.as_ptr(),
                version: host_version.as_ptr(),
                get_extension: Some(host_get_extension),
                request_restart: Some(host_request_restart),
                request_process: Some(host_request_process),
                request_callback: Some(host_request_callback),
            },
        });
        // Point host_data at itself
        host_data.host.host_data = &mut *host_data as *mut HostData as *mut c_void;

        // Keep CStrings alive by leaking them (they live for the app lifetime)
        std::mem::forget(host_name);
        std::mem::forget(host_vendor);
        std::mem::forget(host_url);
        std::mem::forget(host_version);

        // Create plugin instance
        let plugin_id_c = CString::new(plugin_id_str.as_str()).map_err(|_| "Bad plugin ID")?;
        let plugin = unsafe {
            factory.create_plugin.ok_or("No create_plugin")?(
                factory,
                &host_data.host,
                plugin_id_c.as_ptr(),
            )
        };
        if plugin.is_null() {
            return Err("create_plugin returned null".into());
        }

        // Init
        unsafe {
            if let Some(init) = (*plugin).init {
                if !init(plugin) {
                    return Err("Plugin init() failed".into());
                }
            }
        }

        // Get params extension
        let params_ext = unsafe {
            if let Some(get_ext) = (*plugin).get_extension {
                get_ext(plugin, CLAP_EXT_PARAMS.as_ptr()) as *const clap_plugin_params
            } else {
                ptr::null()
            }
        };

        // Enumerate parameters
        let mut params = Vec::new();
        if !params_ext.is_null() {
            unsafe {
                let count = (*params_ext).count.map(|f| f(plugin)).unwrap_or(0);
                for i in 0..count {
                    let mut info: clap_param_info = std::mem::zeroed();
                    if let Some(get_info) = (*params_ext).get_info {
                        if get_info(plugin, i, &mut info) {
                            let name_bytes: Vec<u8> = info
                                .name
                                .iter()
                                .take_while(|&&c| c != 0)
                                .map(|&c| c as u8)
                                .collect();
                            let name = String::from_utf8_lossy(&name_bytes).to_string();

                            // For enum/stepped params, pre-generate text labels
                            let mut value_labels = Vec::new();
                            let is_stepped = info.flags & CLAP_PARAM_IS_STEPPED != 0;
                            let is_enum = info.flags & CLAP_PARAM_IS_ENUM != 0;
                            if (is_stepped || is_enum) && (info.max_value - info.min_value) < 128.0
                            {
                                if let Some(v2t) = (*params_ext).value_to_text {
                                    let steps =
                                        (info.max_value - info.min_value).round() as i32 + 1;
                                    for step in 0..steps {
                                        let val = info.min_value + step as f64;
                                        let mut buf = [0i8; 256];
                                        if v2t(plugin, info.id, val, buf.as_mut_ptr(), 256) {
                                            let label_bytes: Vec<u8> = buf
                                                .iter()
                                                .take_while(|&&c| c != 0)
                                                .map(|&c| c as u8)
                                                .collect();
                                            value_labels.push(
                                                String::from_utf8_lossy(&label_bytes).to_string(),
                                            );
                                        } else {
                                            value_labels.push(format!("{}", val as i32));
                                        }
                                    }
                                }
                            }

                            params.push(ClapParamInfo {
                                id: info.id,
                                name,
                                min: info.min_value,
                                max: info.max_value,
                                default: info.default_value,
                                flags: info.flags,
                                value_labels,
                            });
                        }
                    }
                }
            }
        }

        // Get audio port info
        let (audio_in_ch, audio_out_ch) = Self::get_audio_ports(plugin);

        // Detect note ports to classify plugin type
        let has_note_input = Self::has_note_input(plugin);
        crate::system_log::log(format!(
            "  audio_in={} audio_out={} note_in={}",
            audio_in_ch, audio_out_ch, has_note_input
        ));
        // If plugin accepts note input → instrument (even if it also has audio inputs for sidechain)
        let plugin_type = if has_note_input {
            ClapPluginType::Instrument
        } else {
            ClapPluginType::Effect
        };

        // Activate (on main thread — allowed by CLAP spec)
        // NOTE: start_processing() must be called from the audio thread,
        // so we defer it to the first process_audio() call.
        unsafe {
            if let Some(activate) = (*plugin).activate {
                if !activate(plugin, sample_rate, 1, max_block_size) {
                    return Err("Plugin activate() failed".into());
                }
            }
        }

        // Allocate deinterleaved buffers
        let bs = max_block_size as usize;
        let in_bufs = (0..audio_in_ch).map(|_| vec![0.0f32; bs]).collect();
        let out_bufs = (0..audio_out_ch).map(|_| vec![0.0f32; bs]).collect();

        let info = ClapPluginInfo {
            name: plugin_name,
            vendor: plugin_vendor,
            id: plugin_id_str,
            params,
            audio_in_channels: audio_in_ch,
            audio_out_channels: audio_out_ch,
            plugin_type,
        };

        let type_str = match plugin_type {
            ClapPluginType::Instrument => "instrument",
            ClapPluginType::Effect => "effect",
        };
        crate::system_log::log(format!(
            "CLAP loaded: {} by {} ({} params, {})",
            info.name,
            info.vendor,
            info.params.len(),
            type_str
        ));

        Ok(Self {
            _library: library,
            plugin,
            host_data,
            info,
            params_ext,
            in_bufs,
            out_bufs,
            event_buffer: EventBuffer::new(),
            gui_param_changes: Vec::new(),
            steady_time: 0,
            activated: true,
            processing_started: false,
        })
    }

    fn get_audio_ports(plugin: *const clap_plugin) -> (u32, u32) {
        use clap_sys::ext::audio_ports::*;
        unsafe {
            let ext = if let Some(get_ext) = (*plugin).get_extension {
                get_ext(plugin, CLAP_EXT_AUDIO_PORTS.as_ptr()) as *const clap_plugin_audio_ports
            } else {
                return (2, 2); // default stereo
            };
            if ext.is_null() {
                return (2, 2);
            }

            let in_count = (*ext).count.map(|f| f(plugin, true)).unwrap_or(0);
            let out_count = (*ext).count.map(|f| f(plugin, false)).unwrap_or(0);

            let in_ch = if in_count > 0 {
                let mut info: clap_audio_port_info = std::mem::zeroed();
                if let Some(get) = (*ext).get {
                    get(plugin, 0, true, &mut info);
                }
                info.channel_count.max(1)
            } else {
                0
            }; // No audio input ports (e.g., instrument/synth)

            let out_ch = if out_count > 0 {
                let mut info: clap_audio_port_info = std::mem::zeroed();
                if let Some(get) = (*ext).get {
                    get(plugin, 0, false, &mut info);
                }
                info.channel_count.max(1)
            } else {
                2
            };

            (in_ch, out_ch)
        }
    }

    /// Check if plugin has note input ports (indicates instrument/synth).
    fn has_note_input(plugin: *const clap_plugin) -> bool {
        use clap_sys::ext::note_ports::*;
        unsafe {
            let ext = if let Some(get_ext) = (*plugin).get_extension {
                get_ext(plugin, CLAP_EXT_NOTE_PORTS.as_ptr()) as *const clap_plugin_note_ports
            } else {
                return false;
            };
            if ext.is_null() {
                return false;
            }
            let count = (*ext).count.map(|f| f(plugin, true)).unwrap_or(0);
            count > 0
        }
    }

    /// Process a block of audio. Mono input → plugin stereo in, plugin stereo out → mono output.
    pub fn process_audio(&mut self, input: &[f32], output: &mut [f32], num_frames: usize) {
        // Lazily call start_processing on audio thread (CLAP requires this on audio thread)
        if !self.processing_started {
            unsafe {
                if let Some(start) = (*self.plugin).start_processing {
                    let ok = start(self.plugin);
                    crate::system_log::log(format!(
                        "CLAP: start_processing() = {} (on audio thread)",
                        ok
                    ));
                }
            }
            self.processing_started = true;
        }

        let in_ch = self.in_bufs.len();
        let out_ch = self.out_bufs.len();

        // Fill input buffers (duplicate mono to all channels)
        for ch in 0..in_ch {
            for i in 0..num_frames {
                self.in_bufs[ch][i] = if i < input.len() { input[i] } else { 0.0 };
            }
        }

        // Zero output buffers
        for ch in 0..out_ch {
            self.out_bufs[ch][..num_frames].fill(0.0);
        }

        // Build CLAP audio buffer pointers
        let mut in_ptrs: Vec<*mut f32> = self.in_bufs.iter_mut().map(|b| b.as_mut_ptr()).collect();
        let mut out_ptrs: Vec<*mut f32> =
            self.out_bufs.iter_mut().map(|b| b.as_mut_ptr()).collect();

        let mut clap_in = clap_audio_buffer {
            data32: in_ptrs.as_mut_ptr(),
            data64: ptr::null_mut(),
            channel_count: in_ch as u32,
            latency: 0,
            constant_mask: 0,
        };

        let mut clap_out = clap_audio_buffer {
            data32: out_ptrs.as_mut_ptr(),
            data64: ptr::null_mut(),
            channel_count: out_ch as u32,
            latency: 0,
            constant_mask: 0,
        };

        // Build input events (params + notes)
        let event_buf_ptr = &self.event_buffer as *const EventBuffer;

        let in_events = clap_input_events {
            ctx: event_buf_ptr as *mut c_void,
            size: Some(input_events_size),
            get: Some(input_events_get),
        };

        // Capture output events (plugin GUI → host param changes)
        let gui_changes_ptr = &mut self.gui_param_changes as *mut Vec<(u32, f64)>;
        let out_events = clap_output_events {
            ctx: gui_changes_ptr as *mut c_void,
            try_push: Some(output_events_try_push),
        };

        let process = clap_process {
            steady_time: self.steady_time,
            frames_count: num_frames as u32,
            transport: ptr::null(),
            audio_inputs: if in_ch > 0 {
                &mut clap_in as *const clap_audio_buffer
            } else {
                ptr::null()
            },
            audio_outputs: &mut clap_out as *mut clap_audio_buffer,
            audio_inputs_count: if in_ch > 0 { 1 } else { 0 },
            audio_outputs_count: 1,
            in_events: &in_events,
            out_events: &out_events,
        };

        // Call plugin process
        let status = unsafe {
            if let Some(proc) = (*self.plugin).process {
                proc(self.plugin, &process)
            } else {
                CLAP_PROCESS_ERROR
            }
        };

        // Clear event buffer for next block
        self.event_buffer.clear();

        // If process failed, copy input to output as pass-through
        if status == CLAP_PROCESS_ERROR {
            output[..num_frames].copy_from_slice(&input[..num_frames.min(input.len())]);
            return;
        }

        self.steady_time += num_frames as i64;

        // Mix output channels to mono
        output[..num_frames].fill(0.0);
        for ch in 0..out_ch {
            for i in 0..num_frames {
                output[i] += self.out_bufs[ch][i];
            }
        }
        if out_ch > 1 {
            let scale = 1.0 / out_ch as f32;
            for i in 0..num_frames {
                output[i] *= scale;
            }
        }
    }

    /// Queue a parameter change for the next process call.
    pub fn set_param(&mut self, param_id: u32, value: f64) {
        self.event_buffer.push_param(param_id, value);
    }

    /// Queue a note-on event.
    pub fn note_on(&mut self, key: i16, velocity: f64, channel: i16) {
        self.event_buffer.push_note_on(key, velocity, channel);
    }

    /// Queue a note-off event.
    pub fn note_off(&mut self, key: i16, velocity: f64, channel: i16) {
        self.event_buffer.push_note_off(key, velocity, channel);
    }

    /// Create a GUI handle for the UI thread. Call BEFORE moving ClapInstance to audio thread.
    /// The handle shares the plugin pointer — CLAP allows main-thread gui calls + audio-thread process calls.
    pub fn create_gui_handle(&self) -> Option<ClapGuiHandle> {
        let gui_ext = unsafe {
            if let Some(get_ext) = (*self.plugin).get_extension {
                let ptr = get_ext(self.plugin, CLAP_EXT_GUI.as_ptr()) as *const clap_plugin_gui;
                if ptr.is_null() {
                    crate::system_log::log("  Plugin has no GUI extension");
                    return None;
                }
                ptr
            } else {
                crate::system_log::log("  Plugin has no get_extension");
                return None;
            }
        };

        // Check what the plugin supports
        let floating_supported = unsafe {
            if let Some(is_supported) = (*gui_ext).is_api_supported {
                is_supported(self.plugin, CLAP_WINDOW_API_COCOA.as_ptr(), true)
            } else {
                false
            }
        };
        let embedded_supported = unsafe {
            if let Some(is_supported) = (*gui_ext).is_api_supported {
                is_supported(self.plugin, CLAP_WINDOW_API_COCOA.as_ptr(), false)
            } else {
                false
            }
        };

        crate::system_log::log(format!(
            "  GUI support: floating={}, embedded={}",
            floating_supported, embedded_supported
        ));

        // Prefer floating (simplest — plugin creates its own window)
        // Fall back to embedded if floating not supported
        let use_floating = floating_supported;

        if !floating_supported && !embedded_supported {
            return None;
        }

        Some(ClapGuiHandle {
            plugin: self.plugin,
            gui_ext,
            is_open: false,
            is_floating: use_floating,
            plugin_name: self.info.name.clone(),
            #[cfg(target_os = "macos")]
            _window: None,
        })
    }
}

/// GUI handle for the UI thread — opens/closes the plugin's floating window.
/// Shares the plugin pointer with ClapProcessor (audio thread).
/// CLAP guarantees thread safety: gui calls on main thread, process on audio thread.
pub struct ClapGuiHandle {
    plugin: *const clap_plugin,
    gui_ext: *const clap_plugin_gui,
    pub is_open: bool,
    is_floating: bool,
    pub plugin_name: String,
    /// Retained NSWindow for embedded mode (kept alive while GUI is open)
    #[cfg(target_os = "macos")]
    _window: Option<objc2::rc::Retained<objc2_app_kit::NSWindow>>,
}

// Safety: The plugin pointer is valid for the plugin's lifetime.
// GUI calls happen on main thread only.
unsafe impl Send for ClapGuiHandle {}
unsafe impl Sync for ClapGuiHandle {}

impl ClapGuiHandle {
    pub fn open(&mut self) -> bool {
        if self.is_open {
            return true;
        }

        crate::system_log::log(format!("GUI: opening (floating={})", self.is_floating));

        unsafe {
            // Step 1: create
            if let Some(create) = (*self.gui_ext).create {
                let ok = create(
                    self.plugin,
                    CLAP_WINDOW_API_COCOA.as_ptr(),
                    self.is_floating,
                );
                crate::system_log::log(format!("GUI: create() = {}", ok));
                if !ok {
                    return false;
                }
            } else {
                crate::system_log::error("GUI: no create() function");
                return false;
            }

            // Step 2: get size
            let (mut w, mut h) = (600u32, 400u32);
            if let Some(get_size) = (*self.gui_ext).get_size {
                get_size(self.plugin, &mut w, &mut h);
                crate::system_log::log(format!("GUI: size = {}x{}", w, h));
            }

            // Step 3: for embedded mode, create NSWindow and call set_parent
            #[cfg(target_os = "macos")]
            if !self.is_floating {
                use objc2_app_kit::{NSBackingStoreType, NSWindow, NSWindowStyleMask};
                use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};

                let mtm = MainThreadMarker::new().expect("Must be called from main thread");

                let frame =
                    NSRect::new(NSPoint::new(200.0, 200.0), NSSize::new(w as f64, h as f64));
                // No Closable — user closes via "Close UI" button in the node.
                // macOS close button would release the window while the plugin
                // still references its NSView, causing a crash.
                let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Miniaturizable;

                let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                    mtm.alloc(),
                    frame,
                    style,
                    NSBackingStoreType::NSBackingStoreBuffered,
                    false,
                );

                let title = NSString::from_str(&self.plugin_name);
                window.setTitle(&title);

                // Get the content view (NSView) to pass to the plugin
                if let Some(content_view) = window.contentView() {
                    let nsview_ptr = objc2::rc::Retained::as_ptr(&content_view) as *mut c_void;

                    let clap_win = clap_window {
                        api: CLAP_WINDOW_API_COCOA.as_ptr(),
                        specific: clap_window_handle { cocoa: nsview_ptr },
                    };
                    if let Some(set_parent) = (*self.gui_ext).set_parent {
                        let ok = set_parent(self.plugin, &clap_win);
                        crate::system_log::log(format!("GUI: set_parent() = {}", ok));
                        if !ok {
                            if let Some(destroy) = (*self.gui_ext).destroy {
                                destroy(self.plugin);
                            }
                            return false;
                        }
                    }
                }

                window.makeKeyAndOrderFront(None);
                self._window = Some(window);
            }

            // Step 4: show
            if let Some(show) = (*self.gui_ext).show {
                let ok = show(self.plugin);
                crate::system_log::log(format!("GUI: show() = {}", ok));
            }
        }

        self.is_open = true;
        crate::system_log::log("Plugin GUI opened");
        true
    }

    pub fn close(&mut self) {
        if !self.is_open {
            return;
        }
        // IMPORTANT: destroy plugin GUI BEFORE closing the NSWindow.
        // The plugin's GUI references the NSView inside our window.
        // If we close the window first, the plugin accesses freed memory → segfault.
        unsafe {
            if let Some(hide) = (*self.gui_ext).hide {
                hide(self.plugin);
            }
            if let Some(destroy) = (*self.gui_ext).destroy {
                destroy(self.plugin);
            }
        }
        #[cfg(target_os = "macos")]
        {
            if let Some(window) = self._window.take() {
                window.orderOut(None); // hide window without releasing
                // Window is dropped here, releasing it
            }
        }
        self.is_open = false;
    }
}

impl Drop for ClapGuiHandle {
    fn drop(&mut self) {
        // Only close if we actually have an open GUI.
        // The plugin instance might already be destroyed by the audio thread,
        // so we can't safely call hide/destroy on a dead pointer.
        if self.is_open {
            self.close();
        }
    }
}

impl ClapInstance {
    /// Call from audio thread before dropping the instance.
    #[allow(dead_code)]
    pub fn stop_processing_if_needed(&mut self) {
        if self.processing_started {
            unsafe {
                if let Some(stop) = (*self.plugin).stop_processing {
                    stop(self.plugin);
                }
            }
            self.processing_started = false;
        }
    }
}

impl Drop for ClapInstance {
    fn drop(&mut self) {
        if !self.plugin.is_null() && self.activated {
            // Check if we're on the main thread. CLAP requires deactivate/destroy
            // on the main thread. If we're on the audio thread (engine dropped us),
            // we must still clean up but the plugin will log a warning.
            let on_main = MAIN_THREAD_ID
                .get()
                .map(|&id| std::thread::current().id() == id)
                .unwrap_or(true);

            unsafe {
                if self.processing_started {
                    // stop_processing should be on audio thread — we might be there
                    if let Some(stop) = (*self.plugin).stop_processing {
                        stop(self.plugin);
                    }
                }
                if on_main {
                    if let Some(deactivate) = (*self.plugin).deactivate {
                        deactivate(self.plugin);
                    }
                    if let Some(destroy) = (*self.plugin).destroy {
                        destroy(self.plugin);
                    }
                } else {
                    // Not on main thread — can't safely call deactivate/destroy.
                    // TODO: defer to main thread via callback. For now, call anyway
                    // to avoid resource leak, accepting the thread violation warning.
                    if let Some(deactivate) = (*self.plugin).deactivate {
                        deactivate(self.plugin);
                    }
                    if let Some(destroy) = (*self.plugin).destroy {
                        destroy(self.plugin);
                    }
                }
            }
        }
    }
}

// Safety: ClapInstance is Send because the plugin pointer is only accessed
// from the audio thread after being moved there.
unsafe impl Send for ClapInstance {}

// ── Input events callback functions ──────────────────────────────────────────

unsafe extern "C" fn input_events_size(list: *const clap_input_events) -> u32 {
    unsafe {
        let buf = &*((*list).ctx as *const EventBuffer);
        buf.len() as u32
    }
}

unsafe extern "C" fn input_events_get(
    list: *const clap_input_events,
    index: u32,
) -> *const clap_event_header {
    unsafe {
        let buf = &*((*list).ctx as *const EventBuffer);
        buf.get_header(index as usize)
    }
}

unsafe extern "C" fn output_events_try_push(
    list: *const clap_output_events,
    event: *const clap_event_header,
) -> bool {
    unsafe {
        if event.is_null() {
            return true;
        }
        let header = &*event;
        // Capture param value changes from plugin GUI
        if header.space_id == CLAP_CORE_EVENT_SPACE_ID && header.type_ == CLAP_EVENT_PARAM_VALUE {
            let param_event = &*(event as *const clap_event_param_value);
            let ctx = (*list).ctx as *mut Vec<(u32, f64)>;
            if !ctx.is_null() {
                (*ctx).push((param_event.param_id, param_event.value));
            }
        }
    }
    true
}
