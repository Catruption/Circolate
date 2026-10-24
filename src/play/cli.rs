//! Flag parsing for `circolate play`.

use anyhow::{anyhow, bail, Result};
use std::path::PathBuf;

pub struct Args {
    pub song: Option<PathBuf>,
    pub render: Option<PathBuf>,
    pub rate: u32,
    pub start: f64,
    pub no_cache: bool,
    pub verify: bool,
    pub samples_dir: Option<PathBuf>,
    pub skip_native: bool,
    pub no_master_fx: bool,
    pub no_watch: bool,
    pub sf2_override: Option<PathBuf>,
}

pub fn parse_args(argv: &[String]) -> Result<Args> {
    let mut it = argv.iter();
    let mut song: Option<PathBuf> = None;
    let mut render = None;
    let mut rate = 44100u32;
    let mut start = 0.0f64;
    let (mut no_cache, mut verify) = (false, false);
    let mut samples_dir: Option<PathBuf> = None;
    let mut skip_native = false;
    let mut no_master_fx = false;
    let mut no_watch = false;
    let mut sf2_override: Option<PathBuf> = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--render" => {
                render = Some(PathBuf::from(
                    it.next().ok_or_else(|| anyhow!("--render needs a path"))?,
                ))
            }
            "--rate" => {
                rate = it
                .next()
                .ok_or_else(|| anyhow!("--rate needs a value"))?
                .parse()?
            }
            "--start" => {
                start = it
                .next()
                .ok_or_else(|| anyhow!("--start needs seconds"))?
                .parse()?
            }
            "--samples-dir" => {
                samples_dir = Some(PathBuf::from(
                    it.next().ok_or_else(|| anyhow!("--samples-dir needs a path"))?,
                ))
            }
            "--sf2" => {
                sf2_override = Some(PathBuf::from(
                    it.next().ok_or_else(|| anyhow!("--sf2 needs a path"))?,
                ))
            }
            "--skip-native" => skip_native = true,
            "--no-cache" => no_cache = true,
            "--verify-cache" => verify = true,
            "--no-master-fx" => no_master_fx = true,
            "--no-watch" => no_watch = true,
            _ if song.is_none() => song = Some(PathBuf::from(a)),
            _ => bail!("unexpected argument {a}"),
        }
    }
    Ok(Args {
        song,
       render,
       rate,
       start,
       no_cache,
       verify,
       samples_dir,
       skip_native,
       no_master_fx,
       no_watch,
       sf2_override,
    })
}
