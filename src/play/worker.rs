//! Loader worker thread and live-reload watcher.

use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::player::{self, Cache, Loader, Msg, Project, Transport};

use super::audio::Silence;

pub struct ReloadSlot {
    pub pending: Mutex<Option<(Arc<Project>, Arc<Mutex<Cache>>)>>,
}

pub fn watch_for_changes(
    path: std::path::PathBuf,
    rate: u32,
    use_cache: bool,
    slot: Arc<ReloadSlot>,
    tp: Arc<Transport>,
    mut last_hashes: std::collections::HashMap<String, u64>,
) {
    let mut last_mtime = std::fs::metadata(&path)
    .and_then(|m| m.modified())
    .ok();

    while !tp.stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(400));
        let mtime = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok();
        if mtime == last_mtime {
            continue;
        }
        std::thread::sleep(Duration::from_millis(300));
        let settled = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok();
        if settled != mtime {
            continue;
        }
        let _s = Silence::new();
        match player::load_project(&path, rate, use_cache, true) {
            Ok((project, cache)) => {
                let new_hashes = project.part_hash_map().clone();
                if new_hashes == last_hashes {
                    last_mtime = mtime;
                    continue;
                }
                last_hashes = new_hashes;
                if let Ok(mut g) = slot.pending.lock() {
                    *g = Some((project, Arc::new(Mutex::new(cache))));
                }
                last_mtime = mtime;
            }
            Err(_) => {}
        }
    }
}

pub fn worker(
    mut loader: Loader,
    tx: Sender<Msg>,
    tp: Arc<Transport>,
    lookahead: i64,
    slot: Option<Arc<ReloadSlot>>,
    total: i64,
    wrap_tx: Sender<Msg>,
) {
    let mut epoch = 0u64;
    let mut wrap_ready = false;
    let mut last_wraps = tp.loop_wraps.load(Ordering::Acquire);
    let tail_margin = lookahead * 8;
    while !tp.stop.load(Ordering::Relaxed) {
        let wraps = tp.loop_wraps.load(Ordering::Acquire);
        if wraps != last_wraps {
            last_wraps = wraps;
            wrap_ready = false;
        }
        if let Some(slot) = &slot {
            let pending = slot.pending.lock().ok().and_then(|mut g| g.take());
            if let Some((new_project, new_cache)) = pending {
                wrap_ready = false;
                loader = Loader::with_shared_cache(new_project, new_cache);
                let cur = tp.position.load(Ordering::Relaxed);
                tp.seek_to.store(cur, Ordering::Release);
                tp.reload_count.fetch_add(1, Ordering::Relaxed);
            }
        }
        let req = tp.seek_to.swap(i64::MIN, Ordering::AcqRel);
        if req != i64::MIN {
            wrap_ready = false;
            epoch += 1;
            let batch = loader.seek(req, req + lookahead);
            let msg = Msg::Seek {
                epoch,
                pos: req,
                samples: batch.samples,
                vsts: batch.vsts,
            };
            if tx.send(msg).is_err() {
                break;
            }
        } else if !wrap_ready {
            let pos = tp.position.load(Ordering::Relaxed);
            let horizon = pos + lookahead;
            let batch = loader.load_due(horizon);
            let empty = batch.samples.is_empty() && batch.vsts.is_empty();
            if !empty {
                let msg = Msg::Batch {
                    epoch,
                    samples: batch.samples,
                    vsts: batch.vsts,
                };
                if tx.send(msg).is_err() {
                    break;
                }
            }
            tp.loaded_lines
            .store(loader.cursor as i64, Ordering::Relaxed);

            if tp.loop_next.load(Ordering::Acquire)
                && tp.speed.load(Ordering::Relaxed) == 1
                && total > 0
                && pos + tail_margin >= total
                {
                    let tail = loader.load_due(i64::MAX);
                    if !(tail.samples.is_empty() && tail.vsts.is_empty()) {
                        let msg = Msg::Batch {
                            epoch,
                            samples: tail.samples,
                            vsts: tail.vsts,
                        };
                        if tx.send(msg).is_err() {
                            break;
                        }
                    }
                    epoch += 1;
                    let first = loader.seek(0, lookahead);
                    let wrap = Msg::Seek {
                        epoch,
                        pos: 0,
                        samples: first.samples,
                        vsts: first.vsts,
                    };
                    if wrap_tx.send(wrap).is_err() {
                        break;
                    }
                    wrap_ready = true;
                }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
