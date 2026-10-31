use std::pin::Pin;
use ymfm_sys::ffi;

#[test]
fn omitted_handlers_use_safe_defaults() {
    let mut chip = ffi::create_chip(ffi::ChipType::Ym2612, 7_670_000);
    let mut samples = vec![0i32; chip.channels() as usize];

    chip.pin_mut().write(0, 0x22);
    chip.pin_mut().write(1, 0x00);
    chip.pin_mut().generate(&mut samples);
}

// ymfm's register interface is address/data-port based: write the
// register number to offset 0, then the value to offset 1.
fn write_reg(mut chip: Pin<&mut ffi::Chip>, reg: u8, data: u8) {
    chip.as_mut().write(0, reg);
    chip.as_mut().write(1, data);
}

// minimal 4-operator patch (algorithm 7: all operators are carriers, so
// the output actually oscillates instead of staying silent)
const INIT_REGS: &[(u8, u8)] = &[
    (0x30, 0x01),
    (0x34, 0x01),
    (0x38, 0x01),
    (0x3c, 0x01), // DT/MUL
    (0x40, 0x00),
    (0x44, 0x00),
    (0x48, 0x00),
    (0x4c, 0x00), // TL
    (0x50, 0x1f),
    (0x54, 0x1f),
    (0x58, 0x1f),
    (0x5c, 0x1f), // AR
    (0x60, 0x00),
    (0x64, 0x00),
    (0x68, 0x00),
    (0x6c, 0x00), // D1R
    (0x70, 0x00),
    (0x74, 0x00),
    (0x78, 0x00),
    (0x7c, 0x00), // D2R
    (0x80, 0x0f),
    (0x84, 0x0f),
    (0x88, 0x0f),
    (0x8c, 0x0f), // SL/RR
    (0xb0, 0x07), // algorithm 7, feedback 0
    (0xa4, 0x22),
    (0xa0, 0x69), // frequency
    (0x28, 0xf0), // key on, channel 0
];

fn run_left_channel(mut chip: Pin<&mut ffi::Chip>, channels: usize, samples: usize) -> Vec<i32> {
    let mut buffer = vec![0i32; channels * samples];
    chip.as_mut().generate(&mut buffer);
    buffer.into_iter().step_by(channels).collect()
}

#[test]
fn create_chip_and_generate_samples() {
    let mut chip = ffi::create_chip(ffi::ChipType::Ym2612, 7_670_000);
    assert!(chip.chip_type() == ffi::ChipType::Ym2612);
    assert!(chip.sample_rate() > 0);
    assert_eq!(chip.channels(), 2);

    for &(reg, data) in INIT_REGS {
        write_reg(chip.pin_mut(), reg, data);
    }

    let samples = run_left_channel(chip.pin_mut(), 2, 200);

    // a properly keyed-on, non-degenerate patch should produce an
    // audibly varying (non-constant) waveform
    assert!(samples.iter().any(|&s| s != samples[0]));
}

#[test]
fn all_chip_types_support_basic_lifecycle_operations() {
    let chip_types = [
        ffi::ChipType::Ym2149,
        ffi::ChipType::Ym2151,
        ffi::ChipType::Ym2164,
        ffi::ChipType::Ym2203,
        ffi::ChipType::Ym2413,
        ffi::ChipType::Ym2414,
        ffi::ChipType::Ym2423,
        ffi::ChipType::Ym2608,
        ffi::ChipType::Ym2610,
        ffi::ChipType::Ym2610B,
        ffi::ChipType::Ym2612,
        ffi::ChipType::Ym3438,
        ffi::ChipType::Ymf276,
        ffi::ChipType::Ym3526,
        ffi::ChipType::Ym3533,
        ffi::ChipType::Y8950,
        ffi::ChipType::Ym3812,
        ffi::ChipType::Ymf262,
        ffi::ChipType::Ymf281,
        ffi::ChipType::Ymf278B,
        ffi::ChipType::Ymf289B,
        ffi::ChipType::Ymf288,
        ffi::ChipType::Ym3806,
        ffi::ChipType::Ds1001,
    ];

    for chip_type in chip_types {
        let mut chip = ffi::create_chip(chip_type, 8_000_000);
        assert!(chip.sample_rate() > 0);
        assert!(chip.channels() > 0);
        let _status = chip.pin_mut().read(0);
        chip.pin_mut().reset();
    }
}

#[test]
fn save_and_restore_state_round_trips() {
    let mut chip = ffi::create_chip(ffi::ChipType::Ym2612, 7_670_000);
    let channels = chip.channels() as usize;

    for &(reg, data) in INIT_REGS {
        write_reg(chip.pin_mut(), reg, data);
    }

    // warm up past the initial attack transient
    run_left_channel(chip.pin_mut(), channels, 500);

    let saved = chip.pin_mut().save_state();
    assert!(!saved.is_empty());

    // record the "natural" continuation from the save point
    let expected = run_left_channel(chip.pin_mut(), channels, 50);

    // diverge: change frequency and run forward, so state is now different
    write_reg(chip.pin_mut(), 0xa4, 0x3f);
    write_reg(chip.pin_mut(), 0xa0, 0xff);
    let diverged = run_left_channel(chip.pin_mut(), channels, 50);
    assert_ne!(expected, diverged);

    // rewind to the saved point and confirm we reproduce the original continuation
    chip.pin_mut().restore_state(&saved);
    let replayed = run_left_channel(chip.pin_mut(), channels, 50);
    assert_eq!(expected, replayed);
}

#[test]
fn set_fidelity_changes_sample_rate_where_supported() {
    let clock = 8_000_000;

    // YM2610 supports fidelity selection: MAX (default) divides by 16,
    // MIN/MED divide by 144.
    let mut ym2610 = ffi::create_chip(ffi::ChipType::Ym2610, clock);
    assert_eq!(ym2610.sample_rate(), clock / 16);
    ym2610.pin_mut().set_fidelity(ffi::Fidelity::Min);
    assert_eq!(ym2610.sample_rate(), clock / 144);

    // YM2612 has no fidelity concept; set_fidelity must be a harmless no-op.
    let mut ym2612 = ffi::create_chip(ffi::ChipType::Ym2612, clock);
    let rate_before = ym2612.sample_rate();
    ym2612.pin_mut().set_fidelity(ffi::Fidelity::Min);
    assert_eq!(ym2612.sample_rate(), rate_before);
}

#[test]
fn set_instrument_data_is_supported_by_opll_chips() {
    let instrument_data = vec![0_u8; 0x90];

    let mut ym2413 = ffi::create_chip(ffi::ChipType::Ym2413, 3_579_545);
    assert!(ym2413.pin_mut().set_instrument_data(&instrument_data));
    assert!(
        !ym2413
            .pin_mut()
            .set_instrument_data(&instrument_data[..0x8f])
    );

    let mut ym2612 = ffi::create_chip(ffi::ChipType::Ym2612, 7_670_000);
    assert!(!ym2612.pin_mut().set_instrument_data(&instrument_data));
}
