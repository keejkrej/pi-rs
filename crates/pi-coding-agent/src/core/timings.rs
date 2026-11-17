//! Port of packages/coding-agent/src/core/timings.ts

use std::sync::{LazyLock, Mutex};

use indexmap::IndexMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimingNamespaceKind {
    Main,
    Extensions,
}

impl TimingNamespaceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Extensions => "extensions",
        }
    }
}

struct TimingEntry {
    label: String,
    ms: i64,
}

struct TimingNamespace {
    timings: Vec<TimingEntry>,
    last_time: i64,
}

static ENABLED: LazyLock<bool> = LazyLock::new(|| pi_js::env::var("PI_TIMING").as_deref() == Some("1"));
static TIMING_NAMESPACES: LazyLock<Mutex<IndexMap<TimingNamespaceKind, TimingNamespace>>> =
    LazyLock::new(|| Mutex::new(IndexMap::new()));

pub fn reset_timings(namespace: TimingNamespaceKind) {
    if !*ENABLED {
        return;
    }
    if let Ok(mut guard) = TIMING_NAMESPACES.lock() {
        guard.insert(
            namespace,
            TimingNamespace {
                timings: Vec::new(),
                last_time: pi_js::time::now_ms(),
            },
        );
    }
}

pub fn time(label: &str, namespace: TimingNamespaceKind) {
    if !*ENABLED {
        return;
    }
    let now = pi_js::time::now_ms();
    if let Ok(mut guard) = TIMING_NAMESPACES.lock() {
        let ns = guard.entry(namespace).or_insert_with(|| TimingNamespace {
            timings: Vec::new(),
            last_time: now,
        });
        let delta = now - ns.last_time;
        ns.timings.push(TimingEntry {
            label: label.to_owned(),
            ms: delta,
        });
        ns.last_time = now;
    }
}

fn print_timing_group(title: &str, timings: &[TimingEntry]) {
    let printable: Vec<&TimingEntry> = timings.iter().filter(|t| t.ms >= 0).collect();
    if printable.is_empty() {
        return;
    }
    pi_js::console::error(&format!("\n--- {title} ---"));
    let mut total = 0i64;
    for t in &printable {
        total += t.ms;
        pi_js::console::error(&format!("  {}: {}ms", t.label, t.ms));
    }
    pi_js::console::error(&format!("  TOTAL: {total}ms"));
    pi_js::console::error(&format!("{}\n", "-".repeat(title.len() + 8)));
}

pub fn print_timings() {
    if !*ENABLED {
        return;
    }
    if let Ok(guard) = TIMING_NAMESPACES.lock() {
        for (ns_kind, ns) in guard.iter() {
            print_timing_group(&format!("Startup Timings: {}", ns_kind.as_str()), &ns.timings);
        }
    }
}
