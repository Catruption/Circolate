// src/hosting/clap/mod.rs
use std::ffi::{c_void, CString};
use std::os::raw::c_char;
use std::path::Path;
use anyhow::{anyhow, Result};

use clap_sys::entry::clap_plugin_entry;
use clap_sys::host::{clap_host, clap_host_t};
use clap_sys::plugin::clap_plugin;
use clap_sys::factory::plugin_factory::clap_plugin_factory;
use clap_sys::version::CLAP_VERSION;

pub struct ClapHost {
    pub plugin: *const clap_plugin,
    _lib: libloading::Library, // Keep the .clap file loaded
}

impl ClapHost {
    pub fn load(path: &Path) -> Result<Self> {
        unsafe {
            let lib = libloading::Library::new(path)?;

            // 1. Find the entry point
            let entry_fn: libloading::Symbol<*const clap_plugin_entry> = lib.get(b"clap_entry\0")?;
            let entry = *entry_fn;
            if entry.is_null() { return Err(anyhow!("No clap_entry found")); }

            // 2. Initialize plugin
            let path_cstr = CString::new(path.to_str().unwrap())?;
            if !((*entry).init.unwrap())(path_cstr.as_ptr()) {
                return Err(anyhow!("Plugin init failed"));
            }

            // 3. Get factory
            let factory_id = b"clap.plugin-factory\0";
            let factory = ((*entry).get_factory.unwrap())(factory_id.as_ptr() as *const c_char);
            if factory.is_null() { return Err(anyhow!("No plugin factory")); }
            let factory = factory as *const clap_plugin_factory;

            // 4. Create the host callback struct
            let host = clap_host {
                clap_version: CLAP_VERSION,
                host_data: std::ptr::null_mut(),
                name: b"Circolate\0".as_ptr() as *const c_char,
                vendor: b"Cashmere\0".as_ptr() as *const c_char,
                url: b"https://github.com\0".as_ptr() as *const c_char,
                version: b"0.1.0\0".as_ptr() as *const c_char,
                get_extension: Some(host_get_extension),
                request_restart: Some(host_request_restart),
                request_process: Some(host_request_process),
                request_callback: Some(host_request_callback),
            };

            // 5. Instantiate
            let count = ((*factory).get_plugin_count.unwrap())(factory);
            if count == 0 { return Err(anyhow!("No plugins in factory")); }

            let desc = ((*factory).get_plugin_descriptor.unwrap())(factory, 0);
            let plugin = ((*factory).create_plugin.unwrap())(factory, &host, (*desc).id);

            if plugin.is_null() { return Err(anyhow!("create_plugin returned null")); }

            // 6. Activate
            if !((*plugin).init.unwrap())(plugin) {
                return Err(anyhow!("Plugin init failed"));
            }

            Ok(Self { plugin, _lib: lib })
        }
    }
}

// --- CLAP Host Callbacks ---
// Returning null here tells the plugin to use default, headless-safe behavior.
unsafe extern "C" fn host_get_extension(_: *const clap_host_t, _: *const c_char) -> *const c_void {
    std::ptr::null()
}
unsafe extern "C" fn host_request_restart(_: *const clap_host_t) {}
unsafe extern "C" fn host_request_process(_: *const clap_host_t) {}
unsafe extern "C" fn host_request_callback(_: *const clap_host_t) {}
