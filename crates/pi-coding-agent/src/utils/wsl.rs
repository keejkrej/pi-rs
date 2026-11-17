//! Port of packages/coding-agent/src/utils/wsl.ts

use indexmap::IndexMap;

/// Windows Subsystem for Linux, where Windows executables are reachable through interop.
pub fn is_wsl(env: Option<&IndexMap<String, String>>) -> bool {
    let has_var = |key: &str| -> bool {
        match env {
            Some(map) => map.get(key).is_some_and(|v| !v.is_empty()),
            None => pi_js::env::var(key).is_some_and(|v| !v.is_empty()),
        }
    };

    if has_var("WSL_DISTRO_NAME") || has_var("WSLENV") {
        return true;
    }

    match pi_js::fs::read_to_string("/proc/version") {
        Ok(release) => {
            let lower = release.to_ascii_lowercase();
            lower.contains("microsoft") || lower.contains("wsl")
        }
        Err(_) => false,
    }
}
