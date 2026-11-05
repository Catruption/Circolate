//! IPC socket for broadcasting the live playhead to external editors.

use std::collections::HashMap;
use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::player::{Project, Transport};

pub fn spawn_ipc_server(
    path: String,
    source_file: String,
    tp: Arc<Transport>,
    project: Arc<Project>,
    synesthesia: crate::tui::config::SynesthesiaConfig,
) {
    std::thread::spawn(move || {
        // Clean up stale socket file from previous runs
        let _ = std::fs::remove_file(&path);

        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                crate::log_event!("playhead.ipc_bind_failed", err: format!("{e}"));
                return;
            }
        };

        // Make it non-blocking so we can check the transport in a loop
        if listener.set_nonblocking(true).is_err() {
            return;
        }

        let mut client: Option<UnixStream> = None;
        let mut last_line = usize::MAX;
        let mut last_pos = i64::MIN;
        let mut note_rows_cache = HashMap::new();

        loop {
            if tp.stop.load(Ordering::Relaxed) {
                break;
            }

            // 1. Try to accept a new connection if we don't have one
            if client.is_none() {
                if let Ok((stream, _)) = listener.accept() {
                    let _ = stream.set_nonblocking(true);
                    client = Some(stream);
                    last_line = usize::MAX; // Force a send immediately on connect
                    last_pos = i64::MIN;
                    crate::log_event!("playhead.client_connected");
                }
            }

            // 2. Send updates to the connected client
            if let Some(ref mut stream) = client {
                let pos = tp.position.load(Ordering::Relaxed);
                if let Some(cur) = project.current_line(pos) {
                    // A small, versioned snapshot; `arrangement_line` is deliberately
                    // named so clients don't mistake it for a source-text line.
                    // Throttle unchanged positions to avoid needless socket traffic.
                    let update_interval = (project.sample_rate / 30).max(1) as i64;
                    if cur != last_line
                        || last_pos == i64::MIN
                        || pos.saturating_sub(last_pos) >= update_interval
                    {
                        let time_seconds = pos as f64 / project.sample_rate as f64;
                        let line_start = project.line_start_row(cur);
                        let mut low = line_start;
                        let mut high = line_start + project.rows_per_measure();
                        // Invert the prepared tempo table only within this bar.
                        // A fixed iteration count keeps the socket worker bounded.
                        for _ in 0..20 {
                            let mid = (low + high) * 0.5;
                            if project.tempo.time_at_row(mid) < time_seconds {
                                low = mid;
                            } else {
                                high = mid;
                            }
                        }
                        let row = (low + high) * 0.5;
                        let mut active_notes = Vec::new();
                        for call in project.line_calls(cur) {
                            if !note_rows_cache.contains_key(&call.name) {
                                if let Some(notes) = project.note_rows(&call.name) {
                                    note_rows_cache.insert(call.name.clone(), notes);
                                }
                            }
                            let Some(notes) = note_rows_cache.get(&call.name) else { continue };
                            let call_start = line_start + call.line_offset;
                            let pattern_start = call.pattern_measure_start;
                            let pattern_end = pattern_start + call.pattern_measure_len;
                            for note in notes.iter() {
                                if note.start_row < pattern_start || note.start_row >= pattern_end {
                                    continue;
                                }
                                let start_row = call_start + (note.start_row - pattern_start);
                                let end_row = call_start + (note.end_row - pattern_start);
                                if row < start_row || row >= end_row {
                                    continue;
                                }
                                let part_row = row - call_start + pattern_start;
                                let mut source_lines = vec![note.source_line];
                                let mut source_spans = vec![serde_json::json!({
                                    "line": note.source_line,
                                    "start_byte_column": note.source_byte_column,
                                    "end_byte_column": note.source_end_byte_column,
                                })];
                                if let Some((_, line, start_column, end_column)) = note.waypoint_spans.iter()
                                    .rev()
                                    .find(|(waypoint_row, _, _, _)| *waypoint_row <= part_row)
                                {
                                    if *line != note.source_line
                                        || *start_column != note.source_byte_column
                                        || *end_column != note.source_end_byte_column
                                    {
                                        source_lines.push(*line);
                                        source_spans.push(serde_json::json!({
                                            "line": line,
                                            "start_byte_column": start_column,
                                            "end_byte_column": end_column,
                                        }));
                                    }
                                }
                                active_notes.push(serde_json::json!({
                                    "part": call.name,
                                    "part_row": note.start_row,
                                    "pitch_hz": crate::player::notes::value_at(&note.pitch_points, part_row),
                                    "volume": crate::player::notes::value_at(&note.vol_points, part_row),
                                    "pan": crate::player::notes::value_at(&note.pan_points, part_row),
                                    "source_lines": source_lines,
                                    "source_spans": source_spans,
                                }));
                            }
                        }
                        let msg = serde_json::json!({
                            "version": 1,
                            "type": "playhead",
                            "source_file": source_file,
                            "arrangement_line": cur,
                            "position_samples": pos,
                            "time_seconds": time_seconds,
                            "parts": project.line_part_names(cur),
                            "synesthesia": {
                                "enabled": synesthesia.enabled,
                                "hue_step": synesthesia.hue_step,
                                "saturation_scale": synesthesia.saturation_scale,
                                "value": synesthesia.value,
                            },
                            "active_notes": active_notes,
                        }).to_string() + "\n";

                        if stream.write_all(msg.as_bytes()).is_err() {
                            // Client disconnected
                            client = None;
                            crate::log_event!("playhead.client_disconnected");
                        } else {
                            last_line = cur;
                            last_pos = pos;
                        }
                    }
                }
            }

            // Poll quickly; snapshots are limited to 30 Hz unless the bar changes.
            std::thread::sleep(Duration::from_millis(16));
        }

        let _ = std::fs::remove_file(&path);
    });
}
