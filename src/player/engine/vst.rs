//! VST and SF2 instrument rendering and state management.

use super::*;
use rayon::prelude::*;
use std::sync::OnceLock;
use crate::hosting::vst::ffi::VstHost;
use crate::mixer::mute;

/// Output below this peak (about -80 dBFS) counts as silent for tail purposes.
const TAIL_SILENCE: f32 = 1.0e-4;

pub(crate) struct HostSlot {
    pub(crate) iid: u16,
    pub(crate) host: VstHost,
    pub(crate) scratch_l: Vec<f32>,
    pub(crate) scratch_r: Vec<f32>,
}

/// Flush-to-zero / denormals-are-zero for the calling thread.  These flags
/// are per-thread, so every thread that runs DSP must call this.
#[inline]
pub(crate) fn enable_ftz_daz() {
    #[cfg(target_arch = "x86_64")]
    #[allow(deprecated)]
    unsafe {
        use std::arch::x86_64::{_mm_getcsr, _mm_setcsr};
        _mm_setcsr(_mm_getcsr() | 0x8040);
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        let mut fpcr: u64;
        core::arch::asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack));
        fpcr |= 1 << 24;
        core::arch::asm!("msr fpcr, {}", in(reg) fpcr, options(nomem, nostack));
    }
}

/// Try to put the calling thread into SCHED_FIFO (real-time priority).
/// Falls back to SCHED_RR, then nice -20, then nothing.
///
/// Requires CAP_SYS_NICE (or membership in the `audio` group with
/// `@audio - rtprio 95` in /etc/security/limits.conf) for SCHED_FIFO
/// to succeed.  Without it, nice -20 is still better than SCHED_OTHER.
#[inline]
pub(crate) fn set_realtime_priority() {
    #[cfg(target_os = "linux")]
    unsafe {
        // Only try once per thread — pthread_setschedparam is cheap but
        // not free, and if it fails once it will fail forever.
        thread_local! {
            static TRIED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        }
        TRIED.with(|t| {
            if t.get() { return; }
            t.set(true);

            let mut param: libc::sched_param = std::mem::zeroed();
            param.sched_priority = 50;

            let rc = libc::pthread_setschedparam(
                libc::pthread_self(),
                                                 libc::SCHED_FIFO,
                                                 &param,
            );
            if rc == 0 { return; }

            // Fallback 1: SCHED_RR (round-robin real-time)
            let rc2 = libc::pthread_setschedparam(
                libc::pthread_self(),
                                                  libc::SCHED_RR,
                                                  &param,
            );
            if rc2 == 0 { return; }

            // Fallback 2: nice -20 (highest normal priority)
            libc::setpriority(libc::PRIO_PROCESS, 0, -20);
        });
    }

    #[cfg(not(target_os = "linux"))]
    {
        // On macOS: would need pthread_set_qos_class_self_np
        // On Windows: would need SetThreadPriority(THREAD_PRIORITY_TIME_CRITICAL)
        // Left as no-op for now.
    }
}

fn audio_pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        let n = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .max(2);
        rayon::ThreadPoolBuilder::new()
        .num_threads(n)
        .thread_name(|i| format!("circolate-vst-{i}"))
        .start_handler(|_| {
            enable_ftz_daz();
            set_realtime_priority();
        })
        .build()
        .expect("failed to build VST thread pool")
    })
}

/// Build the pool up front (call from `Engine::new`) so threads are never
/// spawned inside the audio callback.
pub(crate) fn init_audio_pool() {
    let _ = audio_pool();
}

#[inline]
fn render_host(slot: &mut HostSlot, on: bool, n: usize) {
    slot.scratch_l[..n].fill(0.0);
    slot.scratch_r[..n].fill(0.0);
    if on {
        slot.host.process(&mut slot.scratch_l[..n], &mut slot.scratch_r[..n]);
    }
}

impl Engine {
    pub(crate) fn panic_hosts(&mut self) {
        for (iid, mask) in self.vst_held.iter_mut() {
            if let Some(&idx) = self.iid_to_host_idx.get(iid) {
                let host = &mut self.hosts[idx].host;
                for k in 0..128u8 {
                    if (*mask >> k) & 1 == 1 {
                        host.note_off_at(k, 0);
                    }
                }
            }
            *mask = 0;
        }
        for eng in self.sf2_engines.values_mut() {
            eng.all_notes_off();
        }
        self.vst_active.clear();
    }

    pub(crate) fn resync_vsts(&mut self, pos: i64) {
        self.panic_hosts();
        self.vst_pending.clear();
        self.vst_pending.reserve(self.vst_log.len());
        for n in &self.vst_log {
            if n.end <= pos {
                continue;
            }
            let mut n = n.clone();
            if n.start < pos {
                n.start = pos;
            }
            self.vst_pending.push_back(n);
        }
        self.vst_pending.make_contiguous().sort_unstable_by_key(VstNote::key);
        self.vst_synced = true;
    }

    pub(crate) fn render_vsts(&mut self, out_l: &mut [f32], out_r: &mut [f32], block_start: i64) {
        let block_len = out_l.len() as i64;
        let block_end = block_start + block_len;

        let Self {
            hosts,
            iid_to_host_idx,
            vst_gains,
            iid_to_part_path,
            vst_pending,
            vst_active,
            vst_held,
            vst_tail,
            vst_scratch_l,
            vst_scratch_r,
            vst_hist_l,
            vst_hist_r,
            sf2_engines,
            insert_chains,
            channel_to_insert,
            sf2_scratch,
            vst_events_buf,
            render_flags,
            ..
        } = self;

        if render_flags.len() != hosts.len() {
            render_flags.resize(hosts.len(), false);
        }

        let mut chunk_start = block_start;
        while chunk_start < block_end {
            let chunk_len = ((block_end - chunk_start) as usize).min(VST_BLOCK as usize);
            let chunk_end = chunk_start + chunk_len as i64;
            vst_events_buf.clear();
            let events = &mut *vst_events_buf;

            // 1. Pull pending events
            while vst_pending.front().map_or(false, |n| n.start < chunk_end) {
                let n = vst_pending.pop_front().unwrap();
                if !mute::is_note_active(&n.part_path, n.line as usize, n.source_row as i64) {
                    continue;
                }
                let off = (n.start - chunk_start).max(0) as i32;
                if let Some(i) = vst_active.iter().position(|a| a.iid == n.iid && a.note == n.note) {
                    vst_active.remove(i);
                    events.push((off, 0, n.iid, n.note, 0));
                }
                events.push((off, 1, n.iid, n.note, n.velocity));
                vst_active.push(n);
            }

            let mut i = 0;
            while i < vst_active.len() {
                if vst_active[i].end < chunk_end {
                    let n = vst_active.remove(i);
                    let off = (n.end - chunk_start).max(0) as i32;
                    events.push((off, 0, n.iid, n.note, 0));
                } else {
                    i += 1;
                }
            }

            // 2. Sequential dispatch (mutates hosts, vst_held, vst_tail)
            events.sort_unstable_by_key(|e| (e.0, e.1));
            for &(off, kind, iid, note, vel) in events.iter() {
                if let Some(&idx) = iid_to_host_idx.get(&iid) {
                    let host = &mut hosts[idx].host;
                    let bit = 1u128 << (note.min(127) as u32);
                    let held = vst_held.entry(iid).or_insert(0);
                    if kind == 1 {
                        host.note_on_at(note, vel, off);
                        *held |= bit;
                    } else {
                        host.note_off_at(note, off);
                        *held &= !bit;
                    }
                } else if let Some(eng) = sf2_engines.get_mut(&iid) {
                    if kind == 1 {
                        eng.note_on_at(note, vel, off);
                    } else {
                        eng.note_off_at(note, off);
                    }
                }
                vst_tail.insert(iid, VST_TAIL_BLOCKS);
            }

            // 3. Zero shared scratch (insert chains own their buffers)
            vst_scratch_l[..chunk_len].fill(0.0);
            vst_scratch_r[..chunk_len].fill(0.0);
            for chain in insert_chains.values_mut() {
                chain.buf_l[..chunk_len].fill(0.0);
                chain.buf_r[..chunk_len].fill(0.0);
            }

            // 4. Decide which hosts render this chunk (no allocation)
            let mut n_render = 0usize;
            for (idx, slot) in hosts.iter().enumerate() {
                let has_held = vst_held.get(&slot.iid).copied().unwrap_or(0) != 0;
                let tail = vst_tail.get(&slot.iid).copied().unwrap_or(0);
                let on = has_held || tail > 0;
                render_flags[idx] = on;
                if on {
                    n_render += 1;
                    if tail > 0 {
                        if let Some(t) = vst_tail.get_mut(&slot.iid) {
                            *t -= 1;
                        }
                    }
                }
            }

            // 5. Render hosts.  Parallelize only when there is real work,
            //    and give rayon workers RT priority (see set_realtime_priority).
            if n_render >= 2 {
                let flags: &[bool] = &render_flags[..];
                let hosts_ref = &mut *hosts;
                audio_pool().install(|| {
                    hosts_ref
                    .par_iter_mut()
                    .with_max_len(1)
                    .enumerate()
                    .for_each(|(idx, slot)| render_host(slot, flags[idx], chunk_len));
                });
            } else {
                for (idx, slot) in hosts.iter_mut().enumerate() {
                    render_host(slot, render_flags[idx], chunk_len);
                }
            }

            // 6. Sequential mix pass
            for (idx, slot) in hosts.iter().enumerate() {
                let rendered = render_flags[idx];
                let host_gain = vst_gains.get(&slot.iid).copied().unwrap_or(1.0);
                let part_scale = iid_to_part_path
                .get(&slot.iid)
                .map(|p| crate::mixer::part_volume::get(p) as f32)
                .unwrap_or(1.0);
                let gain = host_gain * part_scale;

                super::super::scope::scope_push_block(
                    slot.iid as usize,
                    &slot.scratch_l[..chunk_len],
                    &slot.scratch_r[..chunk_len],
                    gain,
                );

                if !rendered {
                    continue;
                }

                // Keep the tail alive while the plugin is still audible.
                let peak = slot.scratch_l[..chunk_len]
                .iter()
                .chain(slot.scratch_r[..chunk_len].iter())
                .fold(0.0f32, |m, &s| m.max(s.abs()));
                if peak > TAIL_SILENCE {
                    vst_tail.insert(slot.iid, VST_TAIL_BLOCKS);
                }

                let insert = channel_to_insert.get(&slot.iid).copied();
                match insert.and_then(|ins| insert_chains.get_mut(&ins)) {
                    Some(chain) => super::dsp::mix_add(&mut chain.buf_l[..chunk_len], &mut chain.buf_r[..chunk_len], &slot.scratch_l[..chunk_len], &slot.scratch_r[..chunk_len], gain),
                    None => super::dsp::mix_add(&mut vst_scratch_l[..chunk_len], &mut vst_scratch_r[..chunk_len], &slot.scratch_l[..chunk_len], &slot.scratch_r[..chunk_len], gain),
                }
            }

            // 7. SF2 engines (sequential)
            for (iid, eng) in sf2_engines.iter_mut() {
                let (sl, sr) = sf2_scratch
                .entry(*iid)
                .or_insert_with(|| (vec![0.0; VST_BLOCK as usize], vec![0.0; VST_BLOCK as usize]));
                if sl.len() < chunk_len {
                    sl.resize(chunk_len, 0.0);
                    sr.resize(chunk_len, 0.0);
                }
                sl[..chunk_len].fill(0.0);
                sr[..chunk_len].fill(0.0);
                eng.process(&mut sl[..chunk_len], &mut sr[..chunk_len]);

                let part_scale = iid_to_part_path
                .get(iid)
                .map(|p| crate::mixer::part_volume::get(p) as f32)
                .unwrap_or(1.0);
                super::super::scope::scope_push_block(
                    *iid as usize,
                    &sl[..chunk_len],
                    &sr[..chunk_len],
                    part_scale,
                );

                let insert = channel_to_insert.get(iid).copied();
                match insert.and_then(|ins| insert_chains.get_mut(&ins)) {
                    Some(chain) => super::dsp::mix_add(&mut chain.buf_l[..chunk_len], &mut chain.buf_r[..chunk_len], &sl[..chunk_len], &sr[..chunk_len], part_scale),
                    None => super::dsp::mix_add(&mut vst_scratch_l[..chunk_len], &mut vst_scratch_r[..chunk_len], &sl[..chunk_len], &sr[..chunk_len], part_scale),
                }
            }

            // 8. Insert chains: parallel across chains, then sequential mix-down
            let n_chains = insert_chains.values().filter(|c| !c.is_empty()).count();
            if n_chains >= 2 {
                audio_pool().install(|| {
                    insert_chains.values_mut().par_bridge().for_each(|chain| {
                        if !chain.is_empty() {
                            chain.process_scratch(chunk_len);
                        }
                    });
                });
            } else {
                for chain in insert_chains.values_mut() {
                    if !chain.is_empty() {
                        chain.process_scratch(chunk_len);
                    }
                }
            }
            for chain in insert_chains.values() {
                if chain.is_empty() {
                    continue;
                }
                super::dsp::mix_add(&mut vst_scratch_l[..chunk_len], &mut vst_scratch_r[..chunk_len], &chain.buf_l[..chunk_len], &chain.buf_r[..chunk_len], 1.0);
            }

            let out_off = (chunk_start - block_start) as usize;
            super::dsp::mix_add(&mut out_l[out_off..out_off + chunk_len], &mut out_r[out_off..out_off + chunk_len], &vst_scratch_l[..chunk_len], &vst_scratch_r[..chunk_len], 1.0);

            // History ring: block copies instead of a per-sample modulo.
            let hlen = vst_hist_l.len();
            if hlen > 0 {
                if chunk_len <= hlen {
                    let start = chunk_start.rem_euclid(hlen as i64) as usize;
                    let first = chunk_len.min(hlen - start);
                    vst_hist_l[start..start + first].copy_from_slice(&vst_scratch_l[..first]);
                    vst_hist_r[start..start + first].copy_from_slice(&vst_scratch_r[..first]);
                    if first < chunk_len {
                        let rest = chunk_len - first;
                        vst_hist_l[..rest].copy_from_slice(&vst_scratch_l[first..chunk_len]);
                        vst_hist_r[..rest].copy_from_slice(&vst_scratch_r[first..chunk_len]);
                    }
                } else {
                    for j in 0..chunk_len {
                        let idx = (chunk_start + j as i64).rem_euclid(hlen as i64) as usize;
                        vst_hist_l[idx] = vst_scratch_l[j];
                        vst_hist_r[idx] = vst_scratch_r[j];
                    }
                }
            }

            chunk_start = chunk_end;
        }
    }

    pub(crate) fn render_vsts_history(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        block_start: i64,
        speed: i64,
    ) {
        let head = self.vst_hist_head;
        if head == i64::MIN {
            return;
        }
        let len = self.vst_hist_l.len() as i64;
        if len == 0 {
            return;
        }
        let frames = out_l.len() as i64;
        for i in 0..frames {
            let pos = block_start + i * speed;
            if pos > head || pos <= head - len {
                continue;
            }
            let idx = pos.rem_euclid(len) as usize;
            out_l[i as usize] += self.vst_hist_l[idx];
            out_r[i as usize] += self.vst_hist_r[idx];
        }
    }
}
