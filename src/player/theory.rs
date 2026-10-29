//! theory.rs
//! A deterministic, rule-based music theory engine that translates
//! mathematical features into emotional and psychological effects.

use crate::player::analysis_mir::Features;

pub fn explain_emotional(f: &Features) -> Vec<String> {
    let mut r: Vec<(f32, String)> = vec![];

    // ==========================================
    // 1. HARMONY & CHORD QUALITY
    // ==========================================
    if let Some(c) = &f.chord {
        if c.missing_third {
            r.push((0.9, format!("The {} chord omits its 3rd. By removing the note that defines 'happy' or 'sad', the harmony becomes hollow and ambiguous, creating a strong sense of suspense and unresolved tension.", c.name)));
        } else if c.suffix == "sus2" || c.suffix == "sus4" {
            r.push((0.85, format!("The {} is a suspended chord. The 3rd is replaced by a 2nd or 4th, making the chord feel like it's floating—it actively wants to resolve but is holding back.", c.name)));
        } else if c.minor {
            r.push((0.7, format!("The {} chord is minor. It carries a naturally darker, more introspective weight than a major chord, often evoking melancholy or seriousness.", c.name)));
        } else {
            r.push((0.6, format!("The {} chord is major. Major chords provide a foundational feeling of stability, brightness, or arrival.", c.name)));
        }

        if c.suffix.contains("maj7") {
            r.push((0.8, "The Major 7th extension adds a dreamy, shimmering quality. It softens the major chord, making it feel ethereal or nostalgic.".into()));
        } else if c.suffix.contains("7") && !c.suffix.contains("maj7") {
            r.push((0.8, "The minor 7th (Dominant 7th) creates a gritty, bluesy tension. It contains a tritone that strongly demands resolution to a new chord.".into()));
        } else if c.suffix.contains("9") {
            r.push((0.75, "The 9th extension adds a lush, open, and sophisticated color to the harmony.".into()));
        } else if c.name.contains("add") {
            r.push((0.85, "There are 'outside' notes present in the chord. This chromatic tension against the harmony is what creates that specific feeling of yearning, unease, or bittersweet beauty.".into()));
        }

        if c.span_octaves >= 3.0 {
            r.push((0.85, format!("The voicing spans {:.1} octaves. This massive registral spread creates a cinematic 'wall of sound'—the low end grounds you heavily while the high end floats above.", c.span_octaves)));
        } else if c.span_octaves <= 1.0 && c.span_octaves > 0.0 {
            r.push((0.7, "The chord is voiced very tightly (less than an octave). This creates a claustrophobic, dense, and intense cluster of sound.".into()));
        }

        if c.inverted {
            r.push((0.5, "The chord is inverted (the root is not the lowest note). This weakens the sense of 'home' and makes the harmony feel like it's in motion or passing by.".into()));
        }
    }

    // ==========================================
    // 2. FUNCTIONAL HARMONY
    // ==========================================
    if let Some(rn) = &f.roman {
        match rn.as_str() {
            "I" | "i" => r.push((0.6, "This is the tonic (home) chord. It represents absolute resolution, safety, and arrival.".into())),
            "V" | "v" => r.push((0.7, "This is the dominant chord. It creates maximum tension and instability, making the listener physically yearn for the home chord.".into())),
            "IV" | "iv" => r.push((0.5, "The subdominant chord provides a lift—a feeling of moving away from home without fully departing.".into())),
            "vi" | "VI" => r.push((0.6, "The relative minor. It feels like a bittersweet shadow cast over the major key, offering a moment of introspection.".into())),
            // FIXED: Removed the duplicate strings
            "bVI" => r.push((0.8, "The bVI chord is a chromatic mediant. It appears out of nowhere, creating a magical, cinematic, or slightly devastating shift in the emotional landscape.".into())),
            "bVII" => r.push((0.7, "The bVII (borrowed from the minor key) creates a mixolydian, heroic, or rock-anthem feeling, pulling the music downward in a satisfying way.".into())),
            _ => {}
        }
    }

    // ==========================================
    // 3. MELODY & CONTOUR
    // ==========================================
    // FIXED: Now uses the actual contour string instead of the mean pitch
    if f.contour.contains("Leaping") {
        r.push((0.9, "The melody leaps around unpredictably. Large intervallic jumps create a sense of drama, urgency, or disorientation.".into()));
    } else if f.contour.contains("Stepwise") {
        r.push((0.7, "The melody moves stepwise. Because it mimics the natural cadence of human speech, it feels conversational, hypnotic, and grounded.".into()));
    } else if f.contour.contains("Whole-Tone") {
        r.push((0.85, "The melody uses a whole-tone run. This symmetrical scale creates a floating, dreamlike sensation that never fully resolves.".into()));
    } else if f.contour.contains("Chromatic") {
        r.push((0.85, "The melody crawls up chromatically. This creates intense, climbing tension.".into()));
    }

    // ==========================================
    // 4. REGISTER & TEXTURE
    // ==========================================
    if let Some(avg) = f.average_pitch {
        if avg > 78.0 { // Roughly above F#5
            r.push((0.8, "The melody is in the extreme high register. High frequencies are physically piercing to the human ear—they feel vulnerable, exposed, and demand immediate attention.".into()));
        } else if avg < 48.0 { // Roughly below C3
            r.push((0.8, "The notes are deep in the bass register. Low frequencies are physically felt as much as heard; they provide a heavy, dark, and foundational weight.".into()));
        }
    }

    if f.has_bends {
        r.push((0.75, "Pitch bends are present. Sliding between notes mimics the human voice crying out, adding a wailing, deeply expressive quality.".into()));
    }

    // ==========================================
    // 5. RHYTHM & DENSITY
    // ==========================================
    if f.onsets_per_beat > 2.5 {
        r.push((0.7, "Note density is extremely high. The sheer amount of information creates a frantic, urgent, or chaotic energy.".into()));
    } else if f.onsets_per_beat < 0.3 && f.onsets_per_beat > 0.0 {
        r.push((0.7, "The music is very sparse. The silence between the notes creates a lonely, exposed, and meditative atmosphere.".into()));
    }

    if f.has_motif {
        r.push((0.6, "A short melodic motif keeps repeating. Repetition breeds hypnotism and obsession, locking the listener into a trance.".into()));
    }

    // ==========================================
    // 6. CHROMATICISM
    // ==========================================
    if f.unique_pc_count > 8 {
        r.push((0.8, "There is high chromaticism here (many outside notes). The harmony is bleeding outside the key, creating an anxious, dreamlike, or chaotic dissonance.".into()));
    } else if f.unique_pc_count == 1 {
        r.push((0.9, "A single pitch class is playing. This monolithic, static drone creates a feeling of claustrophobia, intense focus, or trance.".into()));
    }

    r.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let out: Vec<String> = r.into_iter().take(4).map(|(_, s)| s).collect();

    if out.is_empty() {
        vec!["The texture is balanced and neutral. No strong tensions or releases are currently active.".into()]
    } else {
        out
    }
}
