//! Plugin discovery.  `plugins` subcommand prints what was found; the
//! converter and player resolve names through the process-wide
//! `PluginTable`.
//!
//! `PluginTable::get()` scans once and hands out a shared, immutable
//! snapshot.  Nothing else in the binary is allowed to walk the search
//! paths: reopening the filesystem mid-song makes two stages disagree
//! about what is installed, and the resulting bug looks like an audio
//! bug.

use anyhow::Result;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

pub struct PluginTable {
    pub vst3: Vec<PathBuf>,
    pub lv2:  Vec<PathBuf>,
    pub clap: Vec<PathBuf>,
    pub vst2: Vec<PathBuf>,
    /// Canonical search roots.  A recorded VST path is only ever a
    /// path relative to one of these, or to a packaged `plugins/`
    /// entry inside the song archive.
    pub roots: Vec<PathBuf>,
}

/// Process-wide snapshot.  Plugin directories do not change while a
/// song is loaded, and a stable table is what makes resolution
/// deterministic across the converter, loader and renderer.
static INSTANCE: OnceLock<PluginTable> = OnceLock::new();

impl PluginTable {
    /// The one table every caller shares.
    pub fn get() -> &'static PluginTable {
        INSTANCE.get_or_init(Self::scan)
    }

    /// Fresh filesystem scan.  Only `get()` and tests should call this;
    /// application code uses `get()` so the whole run sees one snapshot.
    pub fn scan() -> Self {
        let (mut vst3, mut lv2, mut clap, mut vst2) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let roots: Vec<PathBuf> = search_paths()
        .into_iter()
        .map(|p| p.canonicalize().unwrap_or(p))
        .collect();
        for root in &roots {
            scan(root, &mut vst3, &mut lv2, &mut clap, &mut vst2);
        }

        for v in [&mut vst3, &mut lv2, &mut clap, &mut vst2] {
            for p in v.iter_mut() {
                if let Ok(c) = p.canonicalize() {
                    *p = c;
                }
            }
            v.sort();
            v.dedup();
        }

        PluginTable { vst3, lv2, clap, vst2, roots }
    }

    pub fn find_any(&self, name: &str) -> Option<&PathBuf> {
        find_in(&self.vst3, name)
        .or_else(|| find_in(&self.vst2, name))
        .or_else(|| find_in(&self.lv2, name))
        .or_else(|| find_in(&self.clap, name))
    }

    pub fn find_vst3(&self, name: &str) -> Option<&PathBuf> {
        find_in(&self.vst3, name)
    }

    /// Resolve a recorded *relative* VST path against the same roots the
    /// scanner uses.  Absolute paths and `..` are rejected before any
    /// filesystem access: a sidecar must not be able to point outside the
    /// Circolate plugin tree or leak a build machine's layout.
    pub fn find_relative(&self, rel: &Path) -> Option<PathBuf> {
        let rel = clean_relative(rel)?;
        for root in &self.roots {
            let candidate = root.join(&rel);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        None
    }

    /// If `path` came from the plugin scan, return the portable spelling
    /// to store in a sidecar.  Anything outside a known root is dropped
    /// rather than exported as an absolute path.
    pub fn relative_path(&self, path: &Path) -> Option<PathBuf> {
        for root in &self.roots {
            if let Ok(rel) = path.strip_prefix(root) {
                if !rel.as_os_str().is_empty() {
                    return Some(rel.to_path_buf());
                }
            }
        }
        None
    }

    /// Resolve a `Plugin:` field.  It is either a canonical plugin name
    /// (`"Surge XT"`) or a portable relative bundle path
    /// (`"plugins/Surge XT.vst3"`).  Both spellings are looked up under
    /// the same Circolate search roots; an absolute path is never
    /// accepted.
    ///
    /// Callers that have packaged plugins should check their embedded
    /// map first, then call this for the installed copy.
    pub fn resolve_reference(&self, reference: &str) -> Option<PathBuf> {
        if reference.is_empty() {
            return None;
        }
        if looks_like_bundle_path(reference) {
            let rel = Path::new(reference);
            if let Some(p) = self.find_relative(rel) {
                return Some(p);
            }
            // The bundled layout may differ from the installed one; a
            // relative path still carries a plugin name in its stem.
            let stem = rel.file_stem().and_then(|s| s.to_str()).unwrap_or(reference);
            return self.find_vst3(stem).cloned();
        }
        self.find_vst3(reference).cloned()
    }

    pub fn total(&self) -> usize {
        self.vst3.len() + self.lv2.len() + self.clap.len() + self.vst2.len()
    }
}

#[derive(Clone, Copy)]
enum MatchKind { Exact, Normalized }

/// One matcher, used by both lookup and `plugins --explain`.  A second
/// copy of these rules is how two callers end up disagreeing about the
/// same plugin name.
fn match_in<'a>(list: &'a [PathBuf], name: &str) -> Option<(&'a PathBuf, MatchKind)> {
    if name.is_empty() {
        return None;
    }

    if let Some(p) = list.iter().find(|p| {
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        stem.eq_ignore_ascii_case(name)
    }) {
        return Some((p, MatchKind::Exact));
    }

    let want = normalize_plugin_key(name);
    if want.len() < 2 {
        return None;
    }
    list.iter().find_map(|p| {
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        (normalize_plugin_key(stem) == want).then_some((p, MatchKind::Normalized))
    })
}

fn find_in<'a>(list: &'a [PathBuf], name: &str) -> Option<&'a PathBuf> {
    match_in(list, name).map(|(p, _)| p)
}

/// True when a `Plugin:` value is a path rather than a plain name.
/// Bundles carry a known extension or a directory separator.
pub fn looks_like_bundle_path(reference: &str) -> bool {
    if reference.contains('/') || reference.contains('\\') {
        return true;
    }
    let lower = reference.to_ascii_lowercase();
    [".vst3", ".vst", ".so", ".dll", ".component"]
        .iter().any(|ext| lower.ends_with(ext))
}

/// A portable VST path is relative, has no `..`, and has no drive or
/// UNC prefix.  `./plugins/Foo.vst3` is accepted and normalized to
/// `plugins/Foo.vst3`; `/home/me/.vst3/Foo.vst3` is not.
pub fn is_relative_bundle_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
    && !path.is_absolute()
    && !path.to_string_lossy().contains(':')
    && path.components().all(|c| {
        matches!(c, Component::Normal(_) | Component::CurDir)
    })
}

pub fn clean_relative(path: &Path) -> Option<PathBuf> {
    if !is_relative_bundle_path(path) {
        return None;
    }
    let out: PathBuf = path.components()
    .filter(|c| !matches!(c, Component::CurDir))
    .collect();
    (!out.as_os_str().is_empty()).then_some(out)
}

fn normalize_plugin_key(s: &str) -> String {
    let base = if let Some(idx) = s.rfind(" #") {
        let tail = &s[idx + 2..];
        if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
            &s[..idx]
        } else {
            s
        }
    } else {
        s
    };
    base.chars()
    .filter(|c| c.is_ascii_alphanumeric())
    .map(|c| c.to_ascii_lowercase())
    .collect()
}

pub fn run(argv: &[String]) -> Result<()> {
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        println!("usage: circolate plugins [--paths] [--explain NAME]");
        println!();
        println!("  --paths, -v       print the search roots before the listing");
        println!("  --explain, -e N   show how N resolves (same matcher as playback)");
        return Ok(());
    }
    if let Some(i) = argv.iter().position(|a| a == "--explain" || a == "-e") {
        return explain(argv.get(i + 1).map(String::as_str).unwrap_or(""));
    }

    let verbose = argv.iter().any(|a| a == "--paths" || a == "-v");

    if verbose {
        eprintln!("search paths:");
        for p in &search_paths() {
            eprintln!("  {}", p.display());
        }
    }

    let idx = PluginTable::get();

    print_group("VST3", &idx.vst3);
    print_group("VST2", &idx.vst2);
    print_group("LV2",  &idx.lv2);
    print_group("CLAP", &idx.clap);

    println!("\n{} plugin(s) found", idx.total());
    Ok(())
}

/// Show one resolution decision end to end.  This is the cheapest way
/// to answer "why is this VST silent?" without loading a song.
fn explain(name: &str) -> Result<()> {
    let roots = search_paths();
    let table = PluginTable::get();

    println!("explain {name:?}");
    println!("  key: {}", normalize_plugin_key(name));
    println!("  searched {} root(s):", roots.len());
    for root in &roots {
        println!("    {}", root.display());
    }

    for (label, list) in [
        ("VST3", &table.vst3),
        ("VST2", &table.vst2),
        ("LV2", &table.lv2),
        ("CLAP", &table.clap),
    ] {
        if let Some((path, kind)) = match_in(list, name) {
            let how = match kind {
                MatchKind::Exact => "exact stem",
                MatchKind::Normalized => "normalized name",
            };
            println!("  result: {label} {} ({how})", path.display());
            return Ok(());
        }
    }

    println!("  result: Missing::NotInstalled");
    Ok(())
}

fn print_group(label: &str, items: &[PathBuf]) {
    if items.is_empty() {
        return;
    }
    println!("{label} ({}):", items.len());
    for p in items {
        println!("  {}", p.display());
    }
}

fn search_paths() -> Vec<PathBuf> {
    // One resolver, in `deps::registry::paths`.  Keeping a second list
    // here is how the converter and player drifted apart in the first
    // place.
    crate::deps::registry::paths::effective()
}

fn scan(
    root: &Path,
    vst3: &mut Vec<PathBuf>,
    lv2:  &mut Vec<PathBuf>,
    clap: &mut Vec<PathBuf>,
    vst2: &mut Vec<PathBuf>,
) {
    scan_depth(root, 0, 4, vst3, lv2, clap, vst2);
}

fn in_vst_tree(dir: &Path) -> bool {
    dir.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s.eq_ignore_ascii_case("vst") || s.eq_ignore_ascii_case(".vst")
    })
}

fn scan_depth(
    dir: &Path,
    depth: usize,
    max: usize,
    vst3: &mut Vec<PathBuf>,
    lv2:  &mut Vec<PathBuf>,
    clap: &mut Vec<PathBuf>,
    vst2: &mut Vec<PathBuf>,
) {
    if depth > max {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };

    for e in entries.flatten() {
        let p = e.path();
        let is_dir = p.is_dir();
        let ext = p
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

        match (ext.as_str(), is_dir) {
            ("vst3", true)  => { vst3.push(p); continue; }
            ("vst3", false) => { vst3.push(p); continue; }
            ("lv2", true)   => { lv2.push(p);  continue; }
            ("clap", false) => { clap.push(p); continue; }
            ("so", false) | ("dll", false) => {
                if in_vst_tree(dir) {
                    vst2.push(p);
                }
                continue;
            }
            _ => {}
        }

        if is_dir {
            scan_depth(&p, depth + 1, max, vst3, lv2, clap, vst2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_table_is_one_snapshot() {
        assert!(std::ptr::eq(PluginTable::get(), PluginTable::get()));
    }

    #[test]
    fn lookup_normalizes_case_and_number_suffix() {
        let table = vec![PathBuf::from("/plugins/Surge XT.vst3")];
        assert!(find_in(&table, "surge xt").is_some());
        assert!(find_in(&table, "Surge XT #2").is_some());
        assert!(find_in(&table, "SurgeXT").is_some());
        assert!(find_in(&table, "missing").is_none());
    }

    #[test]
    fn relative_paths_reject_absolute_and_parent_traversal() {
        assert!(is_relative_bundle_path(Path::new("plugins/Foo.vst3")));
        assert!(is_relative_bundle_path(Path::new("./plugins/Foo.vst3")));
        assert!(!is_relative_bundle_path(Path::new("/plugins/Foo.vst3")));
        assert!(!is_relative_bundle_path(Path::new("../Foo.vst3")));
        assert!(!is_relative_bundle_path(Path::new("C:\\Foo.vst3")));
        assert_eq!(
            clean_relative(Path::new("./plugins/Foo.vst3")),
            Some(PathBuf::from("plugins/Foo.vst3"))
        );
    }

    #[test]
    fn relative_path_is_stripped_from_a_known_root() {
        let table = PluginTable {
            vst3: Vec::new(), lv2: Vec::new(), clap: Vec::new(), vst2: Vec::new(),
            roots: vec![PathBuf::from("/share/circolate/plugins")],
        };
        assert_eq!(
            table.relative_path(Path::new("/share/circolate/plugins/Surge XT.vst3")),
            Some(PathBuf::from("Surge XT.vst3"))
        );
        assert_eq!(
            table.relative_path(Path::new("/home/me/.vst3/Surge XT.vst3")),
            None
        );
    }

    #[test]
    fn resolve_reference_accepts_names_and_portable_paths() {
        let bundle = PathBuf::from("/plugins/Foo.vst3");
        let table = PluginTable {
            vst3: vec![bundle.clone()],
            lv2: Vec::new(), clap: Vec::new(), vst2: Vec::new(),
            roots: vec![PathBuf::from("/does/not/exist")],
        };
        assert_eq!(table.resolve_reference("Foo"), Some(bundle.clone()));
        assert_eq!(table.resolve_reference("plugins/Foo.vst3"), Some(bundle.clone()));
        // An absolute path is rejected, but its stem still finds the
        // installed copy by name.
        assert_eq!(table.resolve_reference("/home/me/Foo.vst3"), Some(bundle.clone()));
        assert_eq!(table.resolve_reference("missing"), None);
    }

    #[test]
    fn bundle_paths_are_detected() {
        assert!(looks_like_bundle_path("plugins/Surge XT.vst3"));
        assert!(looks_like_bundle_path("Surge XT.vst3"));
        assert!(looks_like_bundle_path("plugins\\Surge XT.vst3"));
        assert!(!looks_like_bundle_path("Surge XT"));
        assert!(!looks_like_bundle_path("SurgeXT"));
    }
}
