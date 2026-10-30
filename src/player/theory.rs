//! A deterministic music theory and motion engine.

use crate::player::analysis_mir::Features;

pub fn explain_emotional(f: &Features) -> Vec<String> {
    let mut r: Vec<(f32, String)> = vec![];

    // ==========================================
    // 1. CONTRAST: What changed from the last measure?
    // ==========================================
    let current_is_high = f.motion.highest_pitch > 72;
    let prev_is_high = f.prev_motion.highest_pitch > 72;

    if prev_is_high && !current_is_high && f.motion.lowest_pitch < 48 {
        r.push((0.95, "The piece suddenly drops from the high register into the deep bass. This massive vertical shift feels like a physical fall or landing.".to_string()));
    }

    if f.prev_motion.biggest_leap <= -7 && f.motion.biggest_leap >= 7 {
        r.push((0.9, "After a large downward fall in the previous measure, the melody suddenly rebounds upward. It feels like a bounce or a sudden gasp of air.".to_string()));
    }

    if f.prev_motion.is_pedal && !f.motion.is_pedal {
        r.push((0.85, "The static anchor breaks. The bass note begins to move, removing the heavy, grounded foundation and creating a sense of drifting.".to_string()));
    }

    // ==========================================
    // 2. MOTION: Leaps & Drops
    // ==========================================
    if f.motion.biggest_leap >= 12 {
        r.push((0.95, format!("The melody leaps upward by {} semitones. This is a massive physical jump, creating a feeling of sudden lift.", f.motion.biggest_leap)));
    } else if f.motion.biggest_leap <= -12 {
        r.push((0.95, format!("The melody drops downward by {} semitones. A fall of more than an octave feels like a sudden physical collapse.", -f.motion.biggest_leap)));
    } else if f.motion.biggest_leap.abs() >= 7 {
        let dir = if f.motion.biggest_leap > 0 { "upward" } else { "downward" };
        r.push((0.85, format!("The melody leaps {} by {} semitones. Large intervals are dramatic and draw immediate attention.", dir, f.motion.biggest_leap.abs())));
    }

    // ==========================================
    // 3. MOTION: Pedal Points & Anchors
    // ==========================================
    if f.motion.is_pedal {
        if f.chord.as_ref().map_or(false, |c| c.minor) {
            r.push((0.9, "A minor harmony over a static, unchanging bass note creates a heavy, meditative anchor.".to_string()));
        } else {
            r.push((0.85, "The bass note barely moves, creating a static anchor. The harmony floats above a foundation that refuses to budge.".to_string()));
        }
    }

    // ==========================================
    // 4. THEORY: Chord Quality & Context
    // ==========================================
    if let Some(c) = &f.chord {
        if c.missing_third {
            r.push((0.9, format!("The {} chord omits its 3rd, becoming hollow and ambiguous.", c.name)));
        } else if c.suffix == "sus2" || c.suffix == "sus4" {
            r.push((0.85, format!("The {} is suspended and floating.", c.name)));
        } else if c.minor {
            r.push((0.7, format!("The {} chord is minor, carrying a darker weight.", c.name)));
        }

        if c.suffix.contains("maj7") {
            r.push((0.8, "The Major 7th extension adds a dreamy, shimmering quality.".to_string()));
        } else if c.suffix.contains("7") && !c.suffix.contains("maj7") {
            r.push((0.8, "The minor 7th (Dominant 7th) creates a gritty tension.".to_string()));
        } else if c.name.contains("add") {
            r.push((0.85, "There are 'outside' notes present in the chord, creating chromatic tension.".to_string()));
        }
    }

    // ==========================================
    // 5. TEXTURE & REGISTER
    // ==========================================
    if let Some(avg) = f.average_pitch {
        if avg > 78.0 {
            r.push((0.8, "The notes are in the extreme high register. High frequencies feel vulnerable and exposed.".to_string()));
        } else if avg < 48.0 {
            r.push((0.8, "The notes are deep in the bass register. Low frequencies provide a heavy, foundational weight.".to_string()));
        }
    }

    if f.onsets_per_beat > 2.5 {
        r.push((0.7, "Note density is extremely high, creating a frantic or chaotic energy.".to_string()));
    } else if f.onsets_per_beat < 0.3 && f.onsets_per_beat > 0.0 {
        r.push((0.7, "The music is very sparse. The silence creates a lonely, exposed atmosphere.".to_string()));
    }

    // Sort by priority, take top 4
    r.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let out: Vec<String> = r.into_iter().take(4).map(|(_, s)| s).collect();

    if out.is_empty() {
        vec!["The texture is balanced and neutral.".into()]
    } else {
        out
    }
}
