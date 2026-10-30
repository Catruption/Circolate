// src/log/mod.rs
//! Central log bus.

pub mod spec;
pub mod pipe;

pub use spec::{Category, Level, LogEvent, CATEGORIES, CATEGORY_COUNT};

use std::fs::File;
use std::io::Write as _;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::SystemTime;

pub struct Record {
    pub level:    Level,
    pub category: Category,
    pub key:      &'static str,
    pub message:  String,
    pub at:       SystemTime,
}

pub trait Sink: Send + Sync {
    fn event(&self, rec: &Record);
}

pub struct StderrSink;

impl Sink for StderrSink {
    fn event(&self, rec: &Record) {
        let stderr = std::io::stderr();
        let mut lock = stderr.lock();
        let _ = writeln!(
            lock,
            "[{} {}] {}",
            rec.level.label(),
                         rec.category.slug(),
                         rec.message,
        );
    }
}

pub struct FileSink {
    file: Mutex<File>,
}

impl FileSink {
    pub fn new(path: &std::path::Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
        Ok(Self { file: Mutex::new(file) })
    }

    pub fn from_env() -> Option<Self> {
        let path = std::env::var_os("CIRCOLATE_LOG_FILE")?;
        Self::new(std::path::Path::new(&path)).ok()
    }
}

impl Sink for FileSink {
    fn event(&self, rec: &Record) {
        let Ok(mut f) = self.file.lock() else { return };
        let secs = rec
        .at
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
        let msg = rec.message.replace('\n', "\\n").replace('\r', "\\r");
        let _ = writeln!(
            f,
            "{secs}\t{}\t{}\t{}\t{}",
            rec.level.slug(),
                         rec.category.slug(),
                         rec.key,
                         msg,
        );
    }
}

pub struct RingSink {
    inner: Mutex<std::collections::VecDeque<Record>>,
    cap:   usize,
}

impl RingSink {
    pub fn new(cap: usize) -> Self {
        Self {
            inner: Mutex::new(std::collections::VecDeque::with_capacity(cap.min(4096))),
            cap,
        }
    }

    pub fn snapshot(&self) -> Vec<Record> {
        let Ok(guard) = self.inner.lock() else { return Vec::new() };
        guard.iter().map(|r| Record {
            level:    r.level,
            category: r.category,
            key:      r.key,
            message:  r.message.clone(),
                         at:       r.at,
        }).collect()
    }
}

impl Sink for RingSink {
    fn event(&self, rec: &Record) {
        let Ok(mut guard) = self.inner.lock() else { return };
        if guard.len() == self.cap { guard.pop_front(); }
        guard.push_back(Record {
            level:    rec.level,
            category: rec.category,
            key:      rec.key,
            message:  rec.message.clone(),
                        at:       rec.at,
        });
    }
}

#[derive(Clone)]
pub struct LogFilter {
    per_category: [Level; CATEGORY_COUNT],
}

impl LogFilter {
    pub fn default_warn() -> Self {
        Self { per_category: [Level::Warn; CATEGORY_COUNT] }
    }

    pub fn all(level: Level) -> Self {
        Self { per_category: [level; CATEGORY_COUNT] }
    }

    pub fn get(&self, cat: Category) -> Level {
        self.per_category[cat.index()]
    }

    pub fn set(&mut self, cat: Category, level: Level) {
        self.per_category[cat.index()] = level;
    }

    pub fn allows(&self, spec: &LogEvent) -> bool {
        spec.level <= self.per_category[spec.category.index()]
    }

    pub fn from_env() -> Self {
        let mut f = Self::default_warn();
        if let Some(g) = std::env::var_os("CIRCOLATE_LOG")
            .and_then(|s| s.to_str().and_then(Level::parse))
            {
                for cat in CATEGORIES { f.set(*cat, g); }
            }
            if let Some(spec) = std::env::var_os("CIRCOLATE_DEBUG") {
                if let Some(spec) = spec.to_str() {
                    for part in spec.split(',') {
                        let part = part.trim();
                        if part.is_empty() { continue; }
                        let (name, level) = match part.split_once(':') {
                            Some((n, l)) => (n, Level::parse(l).unwrap_or(Level::Debug)),
                            None => (part, Level::Debug),
                        };
                        if let Some(cat) = Category::parse(name) {
                            f.set(cat, level);
                        }
                    }
                }
            }
            // Legacy env vars → per-category Debug
            if std::env::var_os("SLICEX_DEBUG").is_some() {
                f.set(Category::FlpSlicex, Level::Debug);
            }
            if std::env::var_os("FLP_DEBUG").is_some() {
                f.set(Category::Flp, Level::Debug);
                f.set(Category::FlpSlicex, Level::Debug);
                f.set(Category::Player, Level::Debug);
            }
            if std::env::var_os("SHIM_DEBUG").is_some()
                || std::env::var_os("SHIMS_DEBUG").is_some()
            {
                f.set(Category::Player, Level::Debug);
            }
            if std::env::var_os("TUI_DEBUG").is_some() {
                f.set(Category::Tui, Level::Debug);
            }
            if std::env::var_os("CIRCOLATE_QUIET").is_some() {
                for cat in CATEGORIES { f.set(*cat, Level::Error); }
            }
            f
    }
}

struct State {
    sinks:  Vec<Box<dyn Sink>>,
    ring:   Option<Arc<RingSink>>,
}

static STATE:  OnceLock<State> = OnceLock::new();
static FILTER: OnceLock<RwLock<Arc<LogFilter>>> = OnceLock::new();

fn filter_handle() -> &'static RwLock<Arc<LogFilter>> {
    FILTER.get_or_init(|| RwLock::new(Arc::new(LogFilter::from_env())))
}

pub fn filter() -> Arc<LogFilter> {
    filter_handle().read().unwrap().clone()
}

pub fn set_filter(f: LogFilter) {
    let h = filter_handle();
    *h.write().unwrap() = Arc::new(f);
}

pub fn init_with_ring(
    sinks: Vec<Box<dyn Sink>>,
    ring:  Option<Arc<RingSink>>,
    filter: Option<LogFilter>,
) {
    if let Some(f) = filter { set_filter(f); }
    let _ = STATE.set(State { sinks, ring });
}

pub fn ring() -> Option<Arc<RingSink>> {
    STATE.get().and_then(|s| s.ring.clone())
}

pub fn emit(key: &'static str, values: &[(&'static str, String)]) {
    let Some(spec) = spec::lookup(key) else {
        debug_assert!(false, "emit: unknown log key {:?}", key);
        return;
    };
    if !filter().allows(spec) { return; }
    let rec = Record {
        level:    spec.level,
        category: spec.category,
        key:      spec.key,
        message:  spec::render(spec, values),
        at:       SystemTime::now(),
    };
    let Some(state) = STATE.get() else { return };
    for sink in &state.sinks { sink.event(&rec); }
    if let Some(ring) = &state.ring { ring.event(&rec); }
}

#[macro_export]
macro_rules! log_event {
    ($key:literal $(, $field:ident : $val:expr)* $(,)?) => {{
        #[cfg(debug_assertions)]
        {
            let spec = $crate::log::spec::lookup($key)
            .unwrap_or_else(|| panic!("log_event!: unknown key {:?}", $key));
            let given: &[&str] = &[$( stringify!($field) ),*];
            assert_eq!(
                given.len(), spec.fields.len(),
                       "log_event!({}): expected {} field(s), got {}",
                       $key, spec.fields.len(), given.len(),
            );
            for f in given {
                assert!(
                    spec.fields.contains(f),
                        "log_event!({}): unknown field {:?} — check src/log/spec.rs",
                        $key, f,
                );
            }
        }
        $crate::log::emit(
            $key,
            &[$( ( stringify!($field), format!("{}", $val) ) ),*],
        )
    }};
}
