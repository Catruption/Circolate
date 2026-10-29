//! Thin wrapper around a small C++ shim (vst_shim.cpp) that hosts VST3s.

use anyhow::Result;
use std::ffi::CString;
use std::os::raw::{c_char, c_float, c_int, c_void};

unsafe extern "C" {
    fn vst_shim_create(plugin_path: *const c_char, sample_rate: c_int,
                       block_size: c_int) -> *mut c_void;
                       fn vst_shim_destroy(handle: *mut c_void);
                       fn vst_shim_activate(handle: *mut c_void);
                       fn vst_shim_set_state(handle: *mut c_void,
                                             data: *const u8, len: usize) -> c_int;
                                             fn vst_shim_note_on(handle: *mut c_void,
                                                                 note: c_int, velocity: c_int,
                                                                 sample_offset: c_int);
                                             fn vst_shim_note_off(handle: *mut c_void,
                                                                  note: c_int,
                                                                  sample_offset: c_int);
                                             fn vst_shim_process(handle: *mut c_void,
                                                                 out_l: *mut c_float, out_r: *mut c_float,
                                                                 frames: c_int) -> c_int;
                                                                 fn vst_shim_process_inplace(handle: *mut c_void,
                                                                                             buf_l: *mut c_float, buf_r: *mut c_float,
                                                                                             frames: c_int) -> c_int;
                                                                                             fn vst_shim_set_param_by_index(handle: *mut c_void,
                                                                                                                            index: c_int,
                                                                                                                            normalized: f64) -> c_int;
                                                                                                                            fn vst_shim_dump_params(handle: *mut c_void) -> c_int;
}

pub struct VstHost {
    handle: *mut c_void,
}

impl VstHost {
    pub fn load(plugin_path: &str, state_bytes: &[u8], sr: u32) -> Result<Self> {
        let cpath = CString::new(plugin_path)?;

        // 1. Save the current display environment variables
        let old_display = std::env::var_os("DISPLAY");
        let old_wayland = std::env::var_os("WAYLAND_DISPLAY");

        // 2. Scrub them so JUCE-based plugins (Surge, Vital) think they are headless
        // Note: set_var and remove_var are unsafe in Rust 2024 edition.
        unsafe {
            std::env::remove_var("DISPLAY");
            std::env::remove_var("WAYLAND_DISPLAY");
        }

        // 3. Create the plugin instance (this is where the hang used to happen)
        let handle = unsafe { vst_shim_create(cpath.as_ptr(), sr as c_int, 512) };

        // 4. Restore the environment for Circolate's own TUI usage
        unsafe {
            if let Some(d) = old_display { std::env::set_var("DISPLAY", d); }
            if let Some(w) = old_wayland { std::env::set_var("WAYLAND_DISPLAY", w); }
        }

        if handle.is_null() {
            return Err(anyhow::anyhow!("vst_shim_create failed for {}", plugin_path));
        }

        if !state_bytes.is_empty() {
            let ok = unsafe {
                vst_shim_set_state(handle, state_bytes.as_ptr(), state_bytes.len())
            };
            if ok != 1 {
                unsafe { vst_shim_destroy(handle) };
                return Err(anyhow::anyhow!(
                    "setStateInformation failed for {}",
                    plugin_path
                ));
            }
        }

        unsafe { vst_shim_activate(handle) };

        Ok(VstHost { handle })
    }

    pub fn note_on_at(&mut self, note: u8, vel: u8, sample_offset: i32) {
        unsafe {
            vst_shim_note_on(self.handle, note as c_int, vel as c_int, sample_offset);
        }
    }

    pub fn note_off_at(&mut self, note: u8, sample_offset: i32) {
        unsafe {
            vst_shim_note_off(self.handle, note as c_int, sample_offset);
        }
    }

    pub fn note_on(&mut self, note: u8, vel: u8) {
        self.note_on_at(note, vel, 0);
    }

    pub fn note_off(&mut self, note: u8) {
        self.note_off_at(note, 0);
    }

    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        unsafe {
            vst_shim_process(self.handle, l.as_mut_ptr(), r.as_mut_ptr(),
                             l.len() as c_int);
        }
    }

    pub fn process_inplace(&mut self, l: &mut [f32], r: &mut [f32]) {
        unsafe {
            vst_shim_process_inplace(self.handle, l.as_mut_ptr(), r.as_mut_ptr(),
                                     l.len() as c_int);
        }
    }

    /// Set a parameter by its index in the plugin's controller order
    /// (`IEditController::getParameterInfo` index, 0-based).  `normalized`
    /// is clamped to [0, 1] by the shim.  Returns `true` if the plugin
    /// has a controller and the index was in range.
    ///
    /// The change is queued on the plugin's controller and also forwarded
    /// as an `inputParameterChanges` entry on the next `process` call, so
    /// both DPF-style (shared-state) and host-pipeline plugins observe it.
    pub fn set_param_by_index(&mut self, index: u32, normalized: f64) -> bool {
        unsafe {
            vst_shim_set_param_by_index(self.handle, index as c_int, normalized) == 1
        }
    }

    /// Print the plugin's full parameter table to stderr.  Returns the
    /// parameter count, or `-1` if the plugin has no controller.
    pub fn dump_params(&mut self) -> i32 {
        unsafe { vst_shim_dump_params(self.handle) }
    }
}

impl Drop for VstHost {
    fn drop(&mut self) {
        unsafe { vst_shim_destroy(self.handle) };
    }
}

unsafe impl Send for VstHost {}
