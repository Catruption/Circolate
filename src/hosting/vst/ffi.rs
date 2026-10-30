//! Thin wrapper around a small C++ shim (vst_shim.cpp) that hosts VST3s.

use anyhow::Result;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_float, c_int, c_void};
use std::sync::mpsc;

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

/// Called by shim.cpp to route C++ logs into the Rust log system.
#[unsafe(no_mangle)]
pub extern "C" fn rust_log_callback(msg: *const c_char) {
    if msg.is_null() { return; }
    let c_str = unsafe { CStr::from_ptr(msg) };
    if let Ok(s) = c_str.to_str() {
        let line = s.trim_end_matches('\n');
        if !line.is_empty() {
            crate::log_event!("ffi.stderr", line: line.to_string());
        }
    }
}

pub struct VstHost {
    handle: *mut c_void,
}

impl VstHost {
    pub fn load(plugin_path: &str, state_bytes: &[u8], sr: u32) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let path_str = plugin_path.to_string();
        let state_vec = state_bytes.to_vec();

        std::thread::spawn(move || {
            let res = (|| -> Result<Self> {
                let cpath = CString::new(path_str.as_str())?;

                let old_display = std::env::var_os("DISPLAY");
                let old_wayland = std::env::var_os("WAYLAND_DISPLAY");

                unsafe {
                    std::env::remove_var("DISPLAY");
                    std::env::remove_var("WAYLAND_DISPLAY");
                }

                let handle = unsafe { vst_shim_create(cpath.as_ptr(), sr as c_int, 512) };

                unsafe {
                    if let Some(d) = old_display { std::env::set_var("DISPLAY", d); }
                    if let Some(w) = old_wayland { std::env::set_var("WAYLAND_DISPLAY", w); }
                }

                if handle.is_null() {
                    return Err(anyhow::anyhow!("vst_shim_create failed for {}", path_str));
                }

                if !state_vec.is_empty() {
                    let ok = unsafe {
                        vst_shim_set_state(handle, state_vec.as_ptr(), state_vec.len())
                    };
                    if ok != 1 {
                        unsafe { vst_shim_destroy(handle) };
                        return Err(anyhow::anyhow!("setStateInformation failed for {}", path_str));
                    }
                }

                unsafe { vst_shim_activate(handle) };

                Ok(VstHost { handle })
            })();

            let _ = tx.send(res);
        });

        match rx.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(res) => res,
            Err(_) => Err(anyhow::anyhow!("VST load timed out after 3s (plugin likely hung in createInstance)")),
        }
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
            vst_shim_process(self.handle, l.as_mut_ptr(), r.as_mut_ptr(), l.len() as c_int);
        }
    }

    pub fn process_inplace(&mut self, l: &mut [f32], r: &mut [f32]) {
        unsafe {
            vst_shim_process_inplace(self.handle, l.as_mut_ptr(), r.as_mut_ptr(), l.len() as c_int);
        }
    }

    pub fn set_param_by_index(&mut self, index: u32, normalized: f64) -> bool {
        unsafe {
            vst_shim_set_param_by_index(self.handle, index as c_int, normalized) == 1
        }
    }

    pub fn dump_params(&mut self) -> i32 {
        unsafe { vst_shim_dump_params(self.handle) }
    }
}

impl Drop for VstHost {
    fn drop(&mut self) {
        if self.handle.is_null() { return; }
        let handle = self.handle;
        self.handle = std::ptr::null_mut();

        // Cast *mut c_void → usize so the closure is Send.  The pointer
        // is still valid: we own it, nothing else holds a reference.
        let handle_addr = handle as usize;

        // Spawn destroy on a background thread with a 1s timeout.
        // Floe and other clap-wrapper plugins hang in terminate();
        // blocking shutdown on one plugin is worse than leaking the handle.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let handle = handle_addr as *mut std::ffi::c_void;
            unsafe { vst_shim_destroy(handle) };
            let _ = tx.send(());
        });
        let _ = rx.recv_timeout(std::time::Duration::from_secs(1));
    }
}

unsafe impl Send for VstHost {}
