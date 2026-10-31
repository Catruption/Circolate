//! Asset extraction and caching for project loading.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use crate::asset::AssetSource;

pub fn cover_cache_dir() -> PathBuf {
    crate::paths::user_cache().join("covers")
}

pub fn find_cover_path(source: &dyn AssetSource) -> Option<PathBuf> {
    const PREFERRED: &[&str] = &["cover.jpg", "cover.jpeg", "cover.png", "cover.webp", "cover.gif"];

    let (name, bytes) = 'find: {
        for name in PREFERRED {
            if let Ok(Some(b)) = source.read_bytes(name) {
                break 'find (name.to_string(), b);
            }
        }
        for entry in source.list("", None) {
            if entry.contains('/') { continue; }
            let lower = entry.to_ascii_lowercase();
            let stem = lower.rsplit_once('.').map(|(s, _)| s).unwrap_or(&lower);
            if stem != "cover" { continue; }
            if let Ok(Some(b)) = source.read_bytes(&entry) {
                break 'find (entry, b);
            }
        }
        return None;
    };

    let ext = name.rsplit('.').next().unwrap_or("bin");
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        h.finish()
    };
    let dir = cover_cache_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{:016x}.{}", hash, ext));
    if !path.exists() {
        std::fs::write(&path, &bytes).ok()?;
    }
    Some(path)
}

pub fn fallback_cover_dir() -> Option<PathBuf> {
    let dir = crate::paths::system_data().join("cover");
    if dir.is_dir() { Some(dir) } else { None }
}

pub fn fallback_cover_path(source: &dyn AssetSource) -> Option<PathBuf> {
    use std::sync::{Mutex, OnceLock};
    static PICKS: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();

    let key = source.cache_key().unwrap_or_else(|| source.describe().to_string());
    let mut picks = PICKS.get_or_init(Default::default).lock().ok()?;
    if let Some(prev) = picks.get(&key) {
        if prev.as_ref().map_or(true, |p| p.is_file()) {
            return prev.clone();
        }
    }
    let pick = fallback_cover_dir().and_then(|d| crate::tui::widgets::cover::random_image_in(&d));
    picks.insert(key, pick.clone());
    pick
}

pub fn plugins_cache_dir() -> PathBuf {
    crate::paths::user_cache().join("plugins")
}

pub fn extract_embedded_plugins(source: &dyn AssetSource) -> Result<HashMap<String, PathBuf>> {
    let entries = source.list_recursive("plugins");
    if entries.is_empty() {
        return Ok(HashMap::new());
    }

    let key = source.cache_key().unwrap_or_else(|| "anonymous".into());
    let cache = plugins_cache_dir().join(&key);

    if !cache.exists() {
        std::fs::create_dir_all(&cache)?;
        for rel in &entries {
            let Some(bytes) = source.read_bytes(rel)? else { continue };
            let dest = cache.join(rel);
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::write(&dest, &bytes).with_context(|| format!("writing {}", dest.display()))?;
        }
    }

    let mut map = HashMap::new();
    let plugins_root = cache.join("plugins");
    if let Ok(rd) = std::fs::read_dir(&plugins_root) {
        for e in rd.flatten() {
            let p = e.path();
            if !p.exists() { continue; }
            let Some(name) = p.file_name().and_then(|s| s.to_str()) else { continue };
            let stem = name.strip_suffix(".vst3").unwrap_or(name).to_string();
            map.insert(stem, p);
        }
    }
    Ok(map)
}

pub fn sf2_cache_dir() -> PathBuf {
    crate::paths::user_cache().join("sf2")
}

pub fn materialize_sf2_file(source: &dyn AssetSource, rel: &str) -> Result<Option<PathBuf>> {
    let Some(bytes) = source.read_bytes(rel)? else { return Ok(None); };
    let key = source.cache_key().unwrap_or_else(|| "anonymous".into());
    let dest = sf2_cache_dir().join(&key).join(rel);

    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(&dest, &bytes).with_context(|| format!("writing {}", dest.display()))?;
    Ok(Some(dest))
}

pub fn materialize_sf2_reference(source: &dyn AssetSource, state_bytes: &mut Vec<u8>) -> Result<()> {
    let mut v: serde_json::Value = match serde_json::from_slice(state_bytes.as_slice()) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };

    let Some(fields) = v.get_mut("fields").and_then(|f| f.as_object_mut()) else { return Ok(()); };
    let Some(file_val) = fields.get_mut("file") else { return Ok(()) };

    let outer_owned: String = match file_val.as_str() {
        Some(s) => s.to_string(),
        None => return Ok(()),
    };
    let inner: String = match serde_json::from_str(&outer_owned) {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };

    if Path::new(&inner).exists() { return Ok(()); }

    let rel = inner.trim_start_matches("./");
    let Some(bytes) = source.read_bytes(rel)? else { return Ok(()); };

    let key = source.cache_key().unwrap_or_else(|| "anonymous".into());
    let dest = sf2_cache_dir().join(&key).join(rel);
    if !dest.exists() {
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&dest, &bytes).with_context(|| format!("writing {}", dest.display()))?;
    }

    let abs = dest.canonicalize().unwrap_or(dest);
    let new_inner = abs.to_string_lossy().into_owned();

    *file_val = serde_json::Value::String(serde_json::to_string(&new_inner)?);
    *state_bytes = serde_json::to_vec(&v)?;
    Ok(())
}