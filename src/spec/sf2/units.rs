//! SF2 unit conversions (§8.1.2).
//!
//! Timecents: cents relative to a 1-second reference; `-12000` means
//! "instant". Attenuation: centibels; `-10 dB = 200 cb`, silence at
//! `1440 cb`.

pub fn timecents_to_secs(tc: i16) -> f32 {
    if tc <= -12000 {
        return 0.0;
    }
    (2f32).powf(tc as f32 / 1200.0)
}

pub fn attenuation_cb_to_linear(cb: i16) -> f32 {
    if cb <= 0 {
        return 1.0;
    }
    if cb >= 1440 {
        return 0.0;
    }
    (10f32).powf(-(cb as f32) / 200.0)
}