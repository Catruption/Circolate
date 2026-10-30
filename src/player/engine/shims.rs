//! Shim host management and processing.

use super::*;
use crate::hosting::vst::ffi::VstHost;

pub(crate) struct ShimHost {
    pub(crate) host: VstHost,
    pub(crate) mix: f64,
    pub(crate) gain: f64,
    pub(crate) pan: f64,
}

impl Engine {
    pub(crate) fn install_shim(
        &mut self,
        id: u32,
        host: VstHost,
        mix: f64,
        gain: f64,
        pan: f64,
    ) {
        self.shims.insert(id, ShimHost {
            host,
            mix: mix.clamp(0.0, 1.0),
            gain,
            pan: pan.clamp(-1.0, 1.0),
        });
        self.shim_scratch.insert(
            id,
            (vec![0.0f32; VST_BLOCK as usize], vec![0.0f32; VST_BLOCK as usize]),
        );
    }

    pub(crate) fn attach_shim_line(&mut self, line: u32, shim_id: u32) {
        let chain = self.shim_by_line.entry(line).or_default();
        if !chain.contains(&shim_id) {
            chain.push(shim_id);
        }
        let mut all: Vec<Vec<u32>> = self.shim_by_line.values().cloned().collect();
        all.sort();
        all.dedup();
        self.shim_chains_cache = all;
    }

    pub(crate) fn shim_count(&self) -> usize {
        self.shims.len()
    }

    pub(crate) fn process_shims(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let len = out_l.len();

        if self.shim_dry_l.len() < len {
            self.shim_dry_l.resize(len, 0.0);
            self.shim_dry_r.resize(len, 0.0);
        }
        for buf in self.shim_scratch.values_mut() {
            if buf.0.len() < len {
                buf.0.resize(len, 0.0);
                buf.1.resize(len, 0.0);
            }
        }

        let mut dry_l = std::mem::take(&mut self.shim_dry_l);
        let mut dry_r = std::mem::take(&mut self.shim_dry_r);

        let block = VST_BLOCK as usize;
        let num_chains = self.shim_chains_cache.len();

        let mut chunk_start = 0usize;
        while chunk_start < len {
            let chunk_end = (chunk_start + block).min(len);
            let chunk_len = chunk_end - chunk_start;

            if self.shim_link_l.len() < chunk_len {
                self.shim_link_l.resize(chunk_len, 0.0);
                self.shim_link_r.resize(chunk_len, 0.0);
            }
            self.shim_processed.clear();

            for chain_idx in 0..num_chains {
                let chain_len = self.shim_chains_cache[chain_idx].len();
                for i in 0..chain_len {
                    let shim_id = self.shim_chains_cache[chain_idx][i];
                    if self.shim_processed.contains(&shim_id) {
                        continue;
                    }
                    self.shim_processed.push(shim_id);

                    let Some(shim) = self.shims.get_mut(&shim_id) else { continue; };
                    let Some(buf) = self.shim_scratch.get_mut(&shim_id) else { continue; };

                    dry_l[chunk_start..chunk_end]
                        .copy_from_slice(&buf.0[chunk_start..chunk_end]);
                    dry_r[chunk_start..chunk_end]
                        .copy_from_slice(&buf.1[chunk_start..chunk_end]);

                    shim.host.process_inplace(
                        &mut buf.0[chunk_start..chunk_end],
                        &mut buf.1[chunk_start..chunk_end],
                    );

                    let mix = shim.mix as f32;
                    let dry_amt = 1.0 - mix;
                    let theta = ((shim.pan + 1.0) * std::f64::consts::FRAC_PI_4) as f32;
                    let gl = (shim.gain as f32) * theta.cos();
                    let gr = (shim.gain as f32) * theta.sin();

                    for j in chunk_start..chunk_end {
                        buf.0[j] = dry_l[j] * dry_amt + buf.0[j] * mix;
                        buf.1[j] = dry_r[j] * dry_amt + buf.1[j] * mix;
                        buf.0[j] *= gl;
                        buf.1[j] *= gr;
                    }

                    if let Some(next_id) = self.shim_chains_cache[chain_idx].get(i + 1).copied() {
                        {
                            let Some(buf) = self.shim_scratch.get(&shim_id) else { continue; };
                            self.shim_link_l[..chunk_len]
                                .copy_from_slice(&buf.0[chunk_start..chunk_end]);
                            self.shim_link_r[..chunk_len]
                                .copy_from_slice(&buf.1[chunk_start..chunk_end]);
                        }
                        if let Some(next) = self.shim_scratch.get_mut(&next_id) {
                            for j in 0..chunk_len {
                                next.0[chunk_start + j] += self.shim_link_l[j];
                                next.1[chunk_start + j] += self.shim_link_r[j];
                            }
                        }
                    } else if let Some(b) = self.shim_scratch.get(&shim_id) {
                        for j in chunk_start..chunk_end {
                            out_l[j] += b.0[j];
                            out_r[j] += b.1[j];
                        }
                    }
                }
            }

            chunk_start = chunk_end;
        }

        self.shim_dry_l = dry_l;
        self.shim_dry_r = dry_r;
    }
}