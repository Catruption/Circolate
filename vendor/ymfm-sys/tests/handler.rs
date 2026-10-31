use std::cell::RefCell;
use std::pin::Pin;
use std::rc::Rc;
use ymfm_sys::{InterfaceCallbacks, InterfaceHandler, ffi};

#[derive(Clone)]
struct TestState {
    irq: Rc<RefCell<bool>>,
    busy: Rc<RefCell<bool>>,
    data: Rc<RefCell<[Vec<u8>; 4]>>,
    current_clock: Rc<RefCell<i64>>,
    busy_until: Rc<RefCell<i64>>,
    timers: Rc<RefCell<[i64; 2]>>,
}

fn handler() -> (InterfaceHandler, TestState) {
    let state = TestState {
        irq: Rc::new(RefCell::new(false)),
        busy: Rc::new(RefCell::new(false)),
        data: Rc::new(RefCell::new(std::array::from_fn(|_| Vec::new()))),
        current_clock: Rc::new(RefCell::new(0)),
        busy_until: Rc::new(RefCell::new(0)),
        timers: Rc::new(RefCell::new([-1; 2])),
    };
    let update_irq_state = state.clone();
    let advance_state = state.clone();
    let busy_end_state = state.clone();
    let timer_state = state.clone();
    let external_read_state = state.clone();
    let read_data_state = state.clone();
    let external_write_state = state.clone();
    let write_data_state = state.clone();
    let handler = InterfaceHandler {
        advance_clock: Some(Box::new(move |clocks| {
            let mut current = advance_state.current_clock.borrow_mut();
            *current += clocks;
            *advance_state.busy.borrow_mut() = *current < *advance_state.busy_until.borrow();
            let mut timers = advance_state.timers.borrow_mut();
            let mut expired = 0;
            for (index, deadline) in timers.iter_mut().enumerate() {
                if *deadline >= 0 && *current >= *deadline {
                    *deadline = -1;
                    expired |= 1 << index;
                }
            }
            expired
        })),
        read_data: Some(Box::new(move |access, base, length| {
            read_bytes(&read_data_state, access, base, length)
        })),
        write_data: Some(Box::new(move |access, base, values| {
            for (index, value) in values.iter().copied().enumerate() {
                write_byte(&write_data_state, access, base + index as u32, value);
            }
        })),
        ymfm_external_read: Some(Box::new(move |access, offset| {
            read_byte(&external_read_state, access, offset)
        })),
        ymfm_external_write: Some(Box::new(move |access, offset, value| {
            write_byte(&external_write_state, access, offset, value);
        })),
        ymfm_is_busy: Some(Box::new({
            let state = state.clone();
            move || *state.busy.borrow()
        })),
        ymfm_set_busy_end: Some(Box::new(move |clocks| {
            let current = *busy_end_state.current_clock.borrow();
            *busy_end_state.busy_until.borrow_mut() = current + i64::from(clocks);
            *busy_end_state.busy.borrow_mut() = true;
        })),
        ymfm_set_timer: Some(Box::new(move |tnum, duration| {
            if let Some(deadline) = timer_state.timers.borrow_mut().get_mut(tnum as usize) {
                let current = *timer_state.current_clock.borrow();
                *deadline = if duration < 0 {
                    -1
                } else {
                    current + i64::from(duration)
                };
            }
        })),
        ymfm_update_irq: Some(Box::new(move |asserted| {
            *update_irq_state.irq.borrow_mut() = asserted;
        })),
    };
    (handler, state)
}

fn data_index(access: ffi::AccessClass) -> usize {
    match access {
        ffi::AccessClass::Io => 0,
        ffi::AccessClass::AdpcmA => 1,
        ffi::AccessClass::AdpcmB => 2,
        ffi::AccessClass::Pcm => 3,
        _ => 0,
    }
}

fn read_byte(state: &TestState, access: ffi::AccessClass, offset: u32) -> u8 {
    state.data.borrow()[data_index(access)]
        .get(offset as usize)
        .copied()
        .unwrap_or(0)
}

fn read_bytes(state: &TestState, access: ffi::AccessClass, base: u32, length: u32) -> Vec<u8> {
    (0..length)
        .map(|index| read_byte(state, access, base + index))
        .collect()
}

fn write_byte(state: &TestState, access: ffi::AccessClass, offset: u32, value: u8) {
    let buffer = &mut state.data.borrow_mut()[data_index(access)];
    let index = offset as usize;
    if buffer.len() <= index {
        buffer.resize(index + 1, 0);
    }
    buffer[index] = value;
}

fn write_reg(mut chip: Pin<&mut ffi::Chip>, reg: u8, data: u8) {
    chip.as_mut().write(0, reg);
    chip.as_mut().write(1, data);
}

#[test]
fn timer_a_expiry_asserts_irq() {
    let (handler, callback_state) = handler();
    let mut chip = ffi::create_chip_with_callbacks(
        ffi::ChipType::Ym2612,
        7_670_000,
        Box::new(InterfaceCallbacks::new(handler)),
    );
    let channels = chip.channels() as usize;
    assert!(!*callback_state.irq.borrow());

    write_reg(chip.pin_mut(), 0x24, 0xff);
    write_reg(chip.pin_mut(), 0x25, 0x03);
    write_reg(chip.pin_mut(), 0x27, 0x05);

    let mut buffer = vec![0i32; channels];
    let fired = (0..1000).any(|_| {
        chip.pin_mut().generate(&mut buffer);
        *callback_state.irq.borrow()
    });
    assert!(fired, "timer A should have expired and asserted IRQ");

    let status = chip.pin_mut().read(0);
    assert!(
        status & 0x01 != 0,
        "status register should report TIMERA (bit 0)"
    );
}

#[test]
fn irq_callback_is_forwarded_to_rust() {
    let (handler, callback_state) = handler();
    let mut chip = ffi::create_chip_with_callbacks(
        ffi::ChipType::Ym2612,
        7_670_000,
        Box::new(InterfaceCallbacks::new(handler)),
    );
    let channels = chip.channels() as usize;

    write_reg(chip.pin_mut(), 0x24, 0xff);
    write_reg(chip.pin_mut(), 0x25, 0x03);
    write_reg(chip.pin_mut(), 0x27, 0x05);

    let mut buffer = vec![0i32; channels];
    let fired = (0..1000).any(|_| {
        chip.pin_mut().generate(&mut buffer);
        *callback_state.irq.borrow()
    });
    assert!(
        fired,
        "ymfm IRQ notification should reach the Rust callback"
    );
}

#[test]
fn busy_callback_is_owned_by_rust() {
    let (handler, callback_state) = handler();
    let mut chip = ffi::create_chip_with_callbacks(
        ffi::ChipType::Ym2612,
        7_670_000,
        Box::new(InterfaceCallbacks::new(handler)),
    );

    write_reg(chip.pin_mut(), 0x22, 0x00);
    assert!(*callback_state.busy.borrow());

    let mut buffer = vec![0i32; chip.channels() as usize];
    let cleared = (0..1000).any(|_| {
        chip.pin_mut().generate(&mut buffer);
        !*callback_state.busy.borrow()
    });
    assert!(
        cleared,
        "Rust-owned BUSY state should expire as samples advance"
    );
    assert!(!*callback_state.busy.borrow());
}
