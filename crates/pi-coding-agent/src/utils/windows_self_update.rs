//! Port of packages/coding-agent/src/utils/windows-self-update.ts

use indexmap::IndexSet;

use crate::utils::paths::get_cwd_relative_path;

const QUARANTINE_DIR_NAME: &str = ".pi-native-quarantine";

fn normalize_path(path: &str) -> String {
    pi_js::path::to_namespaced_path(&pi_js::path::resolve(&[path]))
}

fn get_quarantine_root(package_dir: &str) -> Option<String> {
    let mut current = pi_js::path::resolve(&[package_dir]);
    loop {
        if pi_js::path::basename(&current, None).eq_ignore_ascii_case("node_modules") {
            return Some(pi_js::path::join(&[&current, QUARANTINE_DIR_NAME]));
        }
        let parent = pi_js::path::dirname(&current);
        if parent == current {
            return None;
        }
        current = parent;
    }
}

fn get_loaded_shared_objects_in_package_dir(package_dir: &str) -> Vec<String> {
    let _ = (
        package_dir,
        normalize_path,
        get_cwd_relative_path,
        IndexSet::<String>::new(),
    );
    // PORT: Node's `process.report.getReport().sharedObjects` does not exist in Rust standalone binaries.
    Vec::new()
}

pub fn cleanup_windows_self_update_quarantine(package_dir: &str) {
    let Some(quarantine_root) = get_quarantine_root(package_dir) else {
        return;
    };
    let _ = pi_js::fs::rm(&quarantine_root, true, true);
}

pub fn quarantine_windows_native_dependencies(package_dir: &str) {
    let resolved_package_dir = normalize_path(package_dir);
    let Some(quarantine_root) = get_quarantine_root(&resolved_package_dir) else {
        return;
    };

    let loaded_files = get_loaded_shared_objects_in_package_dir(&resolved_package_dir);
    if loaded_files.is_empty() {
        return;
    }

    let quarantine_run_dir = pi_js::path::join(&[
        &quarantine_root,
        &format!(
            "{}-{}-{}",
            pi_js::time::now_ms(),
            pi_js::env::pid(),
            pi_js::crypto::random_uuid()
        ),
    ]);

    for loaded_file in loaded_files {
        if !pi_js::fs::exists(&loaded_file) {
            continue;
        }
        let rel = pi_js::path::relative(&resolved_package_dir, &loaded_file);
        let quarantine_path = pi_js::path::join(&[&quarantine_run_dir, &rel]);
        let parent = pi_js::path::dirname(&quarantine_path);
        let _ = pi_js::fs::mkdir(&parent, true, None);
        if pi_js::fs::rename(&loaded_file, &quarantine_path).is_ok() {
            let _ = pi_js::fs::copy_file(&quarantine_path, &loaded_file);
        }
    }
}
