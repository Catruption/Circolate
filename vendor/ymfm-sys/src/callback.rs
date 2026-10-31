use std::cell::RefCell;

use crate::ffi;

/// User-provided ymfm interface handlers. Each callback is optional; omitted
/// callbacks use the corresponding no-op/default behavior.
#[derive(Default)]
pub struct InterfaceHandler {
    pub advance_clock: Option<Box<dyn FnMut(i64) -> u8>>,
    pub read_data: Option<Box<dyn Fn(ffi::AccessClass, u32, u32) -> Vec<u8>>>,
    pub write_data: Option<Box<dyn FnMut(ffi::AccessClass, u32, &[u8])>>,
    pub ymfm_external_read: Option<Box<dyn FnMut(ffi::AccessClass, u32) -> u8>>,
    pub ymfm_external_write: Option<Box<dyn FnMut(ffi::AccessClass, u32, u8)>>,
    pub ymfm_is_busy: Option<Box<dyn Fn() -> bool>>,
    pub ymfm_set_busy_end: Option<Box<dyn FnMut(u32)>>,
    pub ymfm_set_timer: Option<Box<dyn FnMut(u32, i32)>>,
    pub ymfm_update_irq: Option<Box<dyn FnMut(bool)>>,
}

/// Opaque callback adapter owned by the native chip instance.
pub struct InterfaceCallbacks {
    handler: RefCell<InterfaceHandler>,
}

impl InterfaceCallbacks {
    /// Create a callback adapter containing the supplied host handlers.
    pub fn new(handler: InterfaceHandler) -> Self {
        Self {
            handler: RefCell::new(handler),
        }
    }
}

/// Create an adapter whose callbacks all use their default behavior.
pub(crate) fn default_callbacks() -> Box<InterfaceCallbacks> {
    Box::new(InterfaceCallbacks::new(InterfaceHandler::default()))
}

/// Forward ymfm's IRQ state change to the host callback, if configured.
pub(crate) fn ymfm_update_irq(callbacks: &InterfaceCallbacks, asserted: bool) {
    if let Some(handler) = callbacks.handler.borrow_mut().ymfm_update_irq.as_mut() {
        handler(asserted);
    }
}

/// Advance host-side time and return the timer-expiry bit mask from the callback.
/// Returns `0` when no callback is configured.
pub(crate) fn advance_clock(callbacks: &InterfaceCallbacks, clocks: i64) -> u8 {
    callbacks
        .handler
        .borrow_mut()
        .advance_clock
        .as_mut()
        .map_or(0, |handler| handler(clocks))
}

/// Forward ymfm's BUSY deadline to the host callback, if configured.
pub(crate) fn ymfm_set_busy_end(callbacks: &InterfaceCallbacks, clocks: u32) {
    if let Some(handler) = callbacks.handler.borrow_mut().ymfm_set_busy_end.as_mut() {
        handler(clocks);
    }
}

/// Ask the host whether the emulated device is BUSY.
/// Returns `false` when no callback is configured.
pub(crate) fn ymfm_is_busy(callbacks: &InterfaceCallbacks) -> bool {
    callbacks
        .handler
        .borrow()
        .ymfm_is_busy
        .as_ref()
        .map_or(false, |handler| handler())
}

/// Forward a timer number and duration to the host callback, if configured.
pub(crate) fn ymfm_set_timer(callbacks: &InterfaceCallbacks, tnum: u32, duration_in_clocks: i32) {
    if let Some(handler) = callbacks.handler.borrow_mut().ymfm_set_timer.as_mut() {
        handler(tnum, duration_in_clocks);
    }
}

/// Read one byte from ymfm's external memory or I/O interface.
/// Returns `0` when no callback is configured.
pub(crate) fn ymfm_external_read(
    callbacks: &InterfaceCallbacks,
    access: ffi::AccessClass,
    offset: u32,
) -> u8 {
    callbacks
        .handler
        .borrow_mut()
        .ymfm_external_read
        .as_mut()
        .map_or(0, |handler| handler(access, offset))
}

/// Read a block from host-managed external data.
/// Returns zero-filled data when no callback is configured.
pub(crate) fn read_data(
    callbacks: &InterfaceCallbacks,
    access: ffi::AccessClass,
    base: u32,
    length: u32,
) -> Vec<u8> {
    callbacks.handler.borrow().read_data.as_ref().map_or_else(
        || vec![0; length as usize],
        |handler| handler(access, base, length),
    )
}

/// Forward one byte written through ymfm's external memory or I/O interface.
pub(crate) fn ymfm_external_write(
    callbacks: &InterfaceCallbacks,
    access: ffi::AccessClass,
    offset: u32,
    data: u8,
) {
    if let Some(handler) = callbacks.handler.borrow_mut().ymfm_external_write.as_mut() {
        handler(access, offset, data);
    }
}

/// Forward a block write to host-managed external data, if configured.
pub(crate) fn write_data(
    callbacks: &InterfaceCallbacks,
    access: ffi::AccessClass,
    base: u32,
    data: &[u8],
) {
    if let Some(handler) = callbacks.handler.borrow_mut().write_data.as_mut() {
        handler(access, base, data);
    }
}
