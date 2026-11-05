//! Loader worker thread and live-reload watcher.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crate::deps::requires;
use crate::fidelity::FIDELITY_FILENAME;
use crate::player::{self, Cache, Loader, Msg, Project, Transport};

pub struct ReloadSlot {
    pub pending: Mutex<Option<(Arc<Project>, Arc<Mutex<Cache>>)>>,
}

type Signature = Vec<(PathBuf, u64, Option<SystemTime>)>;

fn signature(main: &Path) -> Signature {
    let base = main.parent().unwrap_or_else(|| Path::new("."));
    let mut files: Vec<PathBuf> = vec![
        main.to_path_buf(),
        base.join("samples.json"),
        base.join(FIDELITY_FILENAME),
        base.join(requires::FILENAME),
    ];
    if let Ok(rd) = std::fs::read_dir(base.join("instruments")) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() {
                files.push(p);
            }
        }
    }
    files.sort();
    files
    .into_iter()
    .map(|p| {
        let m = std::fs::metadata(&p).ok();
        let len = m.as_ref().map_or(0, |m| m.len());
        let mtime = m.and_then(|m| m.modified().ok());
        (p, len, mtime)
    })
    .collect()
}

pub fn watch_for_changes(
    path: PathBuf,
    rate: u32,
    use_cache: bool,
    slot: Arc<ReloadSlot>,
    tp: Arc<Transport>,
    _initial_hashes: HashMap<String, u64>,
) {
    const POLL: Duration = Duration::from_millis(100);
    const SETTLE: Duration = Duration::from_millis(120);
    const MAX_SETTLE_ROUNDS: u32 = 20;

    let mut last_sig = signature(&path);

    while !tp.stop.load(Ordering::Relaxed) {
        std::thread::sleep(POLL);
        let sig = signature(&path);
        if sig == last_sig {
            continue;
        }

        let mut cur = sig;
        for _ in 0..MAX_SETTLE_ROUNDS {
            std::thread::sleep(SETTLE);
            if tp.stop.load(Ordering::Relaxed) {
                return;
            }
            let next = signature(&path);
            let stable = next == cur;
            cur = next;
            if stable {
                break;
            }
        }
        last_sig = cur;

        if let Ok((project, cache)) = player::load_project(&path, rate, use_cache, true) {
            // NEW: Sync the new project's width to the transport
            tp.stereo_width.store(project.stereo_width.to_bits(), Ordering::Release);

            if let Ok(mut g) = slot.pending.lock() {
                *g = Some((project, Arc::new(Mutex::new(cache))));
            }
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
                chips: batch.chips,
            };
            if tx.send(msg).is_err() {
                break;
            }
        } else if !wrap_ready {
            let pos = tp.position.load(Ordering::Relaxed);
            let horizon = pos + lookahead;
            let t0 = std::time::Instant::now();
            let batch = loader.load_due(horizon);
            crate::player::perf::record_worker_load(t0.elapsed().as_nanos() as u64);
            let empty = batch.samples.is_empty()
            && batch.vsts.is_empty()
            && batch.chips.is_empty();
            if !empty {
                let msg = Msg::Batch {
                    epoch,
                    samples: batch.samples,
                    vsts: batch.vsts,
                    chips: batch.chips,
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
                    let t1 = std::time::Instant::now();
                    let tail = loader.load_due(i64::MAX);
                    crate::player::perf::record_worker_load(t1.elapsed().as_nanos() as u64);
                    if !(tail.samples.is_empty()
                        && tail.vsts.is_empty()
                        && tail.chips.is_empty())
                    {
                        let msg = Msg::Batch {
                            epoch,
                            samples: tail.samples,
                            vsts: tail.vsts,
                            chips: tail.chips,
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
                        chips: first.chips,
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
