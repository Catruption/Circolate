//! Thin wrapper around a small C++ shim (vst_shim.cpp) that hosts VST3s.

use anyhow::Result;
use std::ffi::CString;
use std::os::raw::{c_char, c_float, c_int, c_void};

extern "C" {
    fn vst_shim_create(plugin_path: *const c_char, sample_rate: c_int,
                       block_size: c_int) -> *mut c_void;
                       fn vst_shim_destroy(handle: *mut c_void);
                       fn vst_shim_set_state(handle: *mut c_void,
                                             data: *const u8, len: usize) -> c_int;
                                             fn vst_shim_note_on(handle: *mut c_void, note: c_int, velocity: c_int);
                                             fn vst_shim_note_off(handle: *mut c_void, note: c_int);
                                             fn vst_shim_process(handle: *mut c_void,
                                                                 out_l: *mut c_float, out_r: *mut c_float,
                                                                 frames: c_int) -> c_int;
}

pub struct VstHost {
    handle: *mut c_void,
}

impl VstHost {
    pub fn load(plugin_path: &str, state_bytes: &[u8], sr: u32) -> Result<Self> {
        let cpath = CString::new(plugin_path)?;
        let handle = unsafe { vst_shim_create(cpath.as_ptr(), sr as c_int, 512) };
        if handle.is_null() {
            return Err(anyhow::anyhow!("vst_shim_create failed for {}", plugin_path));
        }
        let ok = unsafe {
            vst_shim_set_state(handle, state_bytes.as_ptr(), state_bytes.len())
        };
        if ok != 1 {
            unsafe { vst_shim_destroy(handle) };
            return Err(anyhow::anyhow!("setStateInformation failed"));
        }
        Ok(VstHost { handle })
    }

    pub fn note_on(&mut self, note: u8, vel: u8) {
        unsafe { vst_shim_note_on(self.handle, note as c_int, vel as c_int) };
    }
    pub fn note_off(&mut self, note: u8) {
        unsafe { vst_shim_note_off(self.handle, note as c_int) };
    }
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        unsafe {
            vst_shim_process(self.handle, l.as_mut_ptr(), r.as_mut_ptr(),
                             l.len() as c_int);
        }
    }
}

impl Drop for VstHost {
    fn drop(&mut self) {
        unsafe { vst_shim_destroy(self.handle) };
    }
}
