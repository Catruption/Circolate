// src/convert/song.rs
//! Shared Trackscript assembly.
//!
//! Both FLP and IT converters build a `Song` and call `write()`.  The
//! writer emits one part per channel with N measures, addressed from
//! BlockList by 1-based measure index:
//!
//!     BlockList()
//!     {
//!         ƒpianoer(1);
//!         ;
//!         ƒpianoer(2); ƒZENOLOGY(1);
//!     }
//!
//!     BlockArray()
//!     {
//!         ƒpianoer(<|4/4, ƒ'samples/05_pianoer.wav'>)[
//!             <§1/1,#C5,~14,%78.1>
//!             <§1/1,#C5,~14.55,%78.1>
//!         ]
//!         ;
//!     }
//!
//! No more `ZENOLOGY02` suffixes, no more `|4/4[...]` per measure.

use std::collections::HashMap;

use super::fmt;
use crate::spec;

pub(super) struct Part {
    pub(super) name: String,
    /// Pre-formatted source argument, e.g. `ƒ'samples/05_pianoer.wav'`.
    pub(super) source_arg: String,
    /// One rendered body per unique measure.  Deduped by content.
    pub(super) sections: Vec<String>,
}

pub(super) struct Call {
    pub(super) part_idx: usize,
    /// 0-based; written as 1-based in the file.
    pub(super) section_idx: usize,
    /// Optional per-measure override block (raw text, no brackets),
    /// e.g. `Some("<%50>")`.
    pub(super) overrides: Option<String>,
}

pub(super) struct MeasureLine {
    pub(super) calls: Vec<Call>,
    /// Shim events, tempo lines, etc.  Written after the calls.
    pub(super) extra: Vec<String>,
}

pub(super) struct Song {
    pub(super) title: String,
    pub(super) source_name: String,
    pub(super) source_format: &'static str,
    pub(super) tempo: f64,
    pub(super) subs: u32,
    pub(super) time_signature: (u32, u32),
    pub(super) song_volume_pct: f64,
    pub(super) panning_law: &'static str,
    pub(super) pitch_default: &'static str,

    pub(super) parts: Vec<Part>,
    pub(super) part_index: HashMap<String, usize>,

    pub(super) measures: Vec<MeasureLine>,

    pub(super) post_comments: Vec<String>,

    /// Value for the `SoundFont:` config key.  `None` omits the key.
    /// `Some("ask".into())` marks this trackscript as "prompt at play
    /// time"; `run.sh` greps for it.  `Some("'path'")` (with the quotes
    /// baked in) pins a specific SF2.
    pub(super) soundfont: Option<String>,
}

impl Song {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        title: &str,
        source_name: &str,
        source_format: &'static str,
        tempo: f64,
        subs: u32,
        time_signature: (u32, u32),
        song_volume_pct: f64,
        panning_law: &'static str,
        pitch_default: &'static str,
    ) -> Self {
        Song {
            title: title.to_string(),
            source_name: source_name.to_string(),
            source_format,
            tempo,
            subs,
            time_signature,
            song_volume_pct,
            panning_law,
            pitch_default,
            parts: Vec::new(),
            part_index: HashMap::new(),
            measures: Vec::new(),
            post_comments: Vec::new(),
            soundfont: None,
        }
    }

    pub(super) fn get_or_create_part(&mut self, name: &str, source_arg: String) -> usize {
        if let Some(&idx) = self.part_index.get(name) {
            return idx;
        }
        let idx = self.parts.len();
        self.parts.push(Part {
            name: name.to_string(),
            source_arg,
            sections: Vec::new(),
        });
        self.part_index.insert(name.to_string(), idx);
        idx
    }

    /// Dedup by content; returns the 0-based section index.
    pub(super) fn add_section(&mut self, part_idx: usize, body: String) -> usize {
        let sections = &mut self.parts[part_idx].sections;
        if let Some(pos) = sections.iter().position(|s| s == &body) {
            return pos;
        }
        sections.push(body);
        sections.len() - 1
    }

    /// Grow `measures` if needed and return the requested line.
    pub(super) fn measure_line(&mut self, m: usize) -> &mut MeasureLine {
        while self.measures.len() <= m {
            self.measures.push(MeasureLine { calls: Vec::new(), extra: Vec::new() });
        }
        &mut self.measures[m]
    }

    pub(super) fn write(self) -> String {
        let mut out = String::new();
        let (num, den) = self.time_signature;

        // ── Config ────────────────────────────────────────────────
        out.push_str("Config\n{\n");
        out.push_str("    TrackscriptVersion: 1.10;\n");
        out.push_str(&format!("    BeatsPerMinute: {};\n", fmt(self.tempo, 2)));
        out.push_str(&format!("    TimeSignature: {}/{};\n", num, den));
        out.push_str(&format!("    Subdivisions: {};\n", self.subs));
        out.push_str(&format!(
            "    SongVolume: {}%;\n",
            fmt(self.song_volume_pct, 2)
        ));
        out.push_str(&format!("    PanningLaw: {};\n", self.panning_law));
        out.push_str("    Overlap: stack;\n");
        out.push_str(&format!("    PitchDefault: {};\n", self.pitch_default));
        if let Some(sf) = &self.soundfont {
            out.push_str(&format!("    SoundFont: {};\n", sf));
        }
        out.push_str("}\n\n");

        // ── BlockList ─────────────────────────────────────────────
        //
        // Each line is a call to one measure of one part, written as
        // `ƒname(N);` where N is the 1-based measure index.
        out.push_str("BlockList()\n{\n");
        for line in &self.measures {
            if line.calls.is_empty() && line.extra.is_empty() {
                out.push_str("    ;\n");
                continue;
            }
            out.push_str("    ");
            let mut first = true;
            for c in &line.calls {
                if !first {
                    out.push(' ');
                }
                first = false;
                out.push_str(spec::FIELD_SOURCE);
                out.push_str(&self.parts[c.part_idx].name);
                out.push('(');
                out.push_str(&(c.section_idx + 1).to_string());
                if let Some(ov) = &c.overrides {
                    out.push_str(ov);
                }
                out.push_str(");");
            }
            for extra in &line.extra {
                if !first {
                    out.push(' ');
                }
                first = false;
                out.push_str(extra);
            }
            out.push('\n');
        }
        out.push_str("}\n\n");

        // ── BlockArray ────────────────────────────────────────────
        //
        // A part is `ƒname(<|4/4, ƒ'path'>)[ measures ] ;` where each
        // measure is one line of the body.  The frame `|4/4` lives
        // inside the header args, not in per-measure section headers.
        out.push_str("BlockArray()\n{\n");
        let frame = spec::section_frame(num, self.subs); // "|4/4"
        for part in &self.parts {
             out.push_str(&format!(
                 "    ƒ{}(<{}, {}>)[\n",
                 part.name, frame, part.source_arg
             ));
            for section in &part.sections {
                out.push_str(section);
                out.push('\n');
            }
            out.push_str("    ]\n");
            out.push_str("    ;\n\n");
        }
        // Drop the final blank line before the closing brace.
        if out.ends_with("\n\n") {
            out.pop();
        }
        out.push_str("}\n");

        // ── Post-comments ─────────────────────────────────────────
        for c in &self.post_comments {
            out.push_str(c);
            if !c.ends_with('\n') {
                out.push('\n');
            }
        }

        out
    }
}