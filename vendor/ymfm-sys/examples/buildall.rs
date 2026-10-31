use ymfm_sys::ffi::{self, ChipType};

fn exercise_chip(chip_type: ChipType) {
    let mut chip = ffi::create_chip(chip_type, 8_000_000);

    chip.pin_mut().reset();

    let saved = chip.pin_mut().save_state();
    assert!(!saved.is_empty());
    chip.pin_mut().restore_state(&saved);

    chip.pin_mut().read(0);
    chip.pin_mut().write(0, 0);

    let channels = chip.channels() as usize;
    let mut samples = vec![0_i32; channels * 20];
    chip.pin_mut().generate(&mut samples);
}

fn main() {
    let chip_types = [
        ChipType::Ym2149,
        ChipType::Ym2151,
        ChipType::Ym2203,
        ChipType::Ym2164,
        ChipType::Ym2413,
        ChipType::Ym2414,
        ChipType::Ym2423,
        ChipType::Ym2608,
        ChipType::Ym2610,
        ChipType::Ym2610B,
        ChipType::Ym2612,
        ChipType::Ym3526,
        ChipType::Ym3438,
        ChipType::Ymf276,
        ChipType::Y8950,
        ChipType::Ym3533,
        ChipType::Ym3812,
        ChipType::Ymf262,
        ChipType::Ymf278B,
        ChipType::Ymf281,
        ChipType::Ymf289B,
        ChipType::Ym3806,
        ChipType::Ymf288,
        ChipType::Ds1001,
    ];

    for chip_type in chip_types {
        exercise_chip(chip_type);
    }

    println!("Done");
}
