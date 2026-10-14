//! Thin wrapper around a small C++ shim (vst_shim.cpp) that hosts VST3s.
//!
//! Instruments and effects share the same wrapper.  Callers decide which
//! entry point to use: `process()` for synths (writes output, ignores
//! input), `process_inplace()` for effects (reads and writes the same
//! buffer).
//!
//! Lifecycle: `VstHost::load` creates the plugin with an empty state and
//! then immediately applies the state it was given, then activates.
//! That ordering matters — a sampler sizes its internal playback buffers
//! at activation based on the state it sees.  Loading state after
//! activation leaves those buffers sized for "nothing", and the plugin
//! crashes when it tries to write into them.  See the comment in
//! `vst_shim.cpp` for the full story.

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
}

pub struct VstHost {
    handle: *mut c_void,
}

impl VstHost {
    pub fn load(plugin_path: &str, state_bytes: &[u8], sr: u32) -> Result<Self> {
        let cpath = CString::new(plugin_path)?;

        // Create — the plugin is configured but not yet active.
        let handle = unsafe { vst_shim_create(cpath.as_ptr(), sr as c_int, 512) };
        if handle.is_null() {
            return Err(anyhow::anyhow!("vst_shim_create failed for {}", plugin_path));
        }

        // Apply state while inactive.  The plugin sizes its buffers
        // against this state at activation, so this has to happen
        // before vst_shim_activate.
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

        // Now bring it online.
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

    /// Instrument path: synth writes here, input is ignored.
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        unsafe {
            vst_shim_process(self.handle, l.as_mut_ptr(), r.as_mut_ptr(),
                             l.len() as c_int);
        }
    }

    /// Effect path: reads and writes the same buffer.
    pub fn process_inplace(&mut self, l: &mut [f32], r: &mut [f32]) {
        unsafe {
            vst_shim_process_inplace(self.handle, l.as_mut_ptr(), r.as_mut_ptr(),
                                     l.len() as c_int);
        }
    }
}

impl Drop for VstHost {
    fn drop(&mut self) {
        unsafe { vst_shim_destroy(self.handle) };
    }
}

// SAFETY: the raw handle is only dereferenced through `&mut self`.  The
// engine owns each `VstHost` and only touches it from the audio callback
// (live) or the render loop (offline), never both at once.
unsafe impl Send for VstHost {}
