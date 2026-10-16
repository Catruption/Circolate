# Native effect models

Binary artifacts produced by the profiling scripts in `profile/`.
Each `.lut.bin` is a flat array of little-endian f32 values, evenly
spaced across the input range [-1.0, +1.0].

Naming convention:

    <effect_slug>.lut.bin    waveshaper transfer curve
    <effect_slug>.ir.wav     impulse response for convolution
    <effect_slug>.coeffs.bin filter coefficients (format TBD)

Slugs match the registry `fallback.name` for that effect:

    fruity_fast_dist        Fast Dist
    fruity_blood_overdrive  Blood Overdrive
    fruity_soft_clipper     Soft Clipper
    emphasis                Emphasis
    fruity_reeverb_2        Reeverb 2

Add new artifacts here as we profile each effect.
