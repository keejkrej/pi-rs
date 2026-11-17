//! Port of packages/coding-agent/test/config.test.ts

use pi_coding_agent::config::{
    ENV_AGENT_DIR, InstallMethod, SelfUpdatePackageTarget, detect_install_method, find_node_package_dir, get_agent_dir,
    get_docs_path, get_package_dir, get_self_update_command, get_self_update_unavailable_instruction,
    get_share_viewer_url, get_update_instruction,
};
use pi_js::env::testing::EnvGuard;
use tempfile::TempDir;

#[test]
fn find_node_package_dir_skips_binary_metadata_copied_into_dist() {
    let temp = TempDir::new().expect("temp dir");
    let dist_dir = temp.path().join("dist");
    let bundle_dir = dist_dir.join("bundle");
    std::fs::create_dir_all(&bundle_dir).expect("mkdir bundle");
    std::fs::write(temp.path().join("package.json"), "{}").expect("write root package.json");
    std::fs::write(dist_dir.join("package.json"), "{}").expect("write dist package.json");

    let got = find_node_package_dir(bundle_dir.to_str().expect("utf8"));
    assert_eq!(got, temp.path().to_str().expect("utf8"));
}

#[test]
fn does_not_self_update_unknown_wrapper_installs() {
    assert_eq!(detect_install_method(), InstallMethod::Unknown);
    assert_eq!(
        get_self_update_command("@earendil-works/pi-coding-agent", None, None),
        None
    );
    assert_eq!(
        get_update_instruction("@earendil-works/pi-coding-agent"),
        "Update @earendil-works/pi-coding-agent using the package manager, wrapper, or source checkout that provides this installation."
    );
}

#[test]
fn supports_package_dir_override_and_config_paths() {
    let temp = TempDir::new().expect("temp dir");
    let custom_pkg = temp.path().join("custom-pkg");
    std::fs::create_dir_all(custom_pkg.join("docs")).expect("mkdir docs");

    {
        let _pkg_guard = EnvGuard::set("PI_PACKAGE_DIR", Some(custom_pkg.to_str().expect("utf8")));
        assert_eq!(
            pi_js::path::resolve(&[&get_package_dir()]),
            pi_js::path::resolve(&[custom_pkg.to_str().expect("utf8")])
        );
        assert_eq!(
            pi_js::path::resolve(&[&get_docs_path()]),
            pi_js::path::resolve(&[custom_pkg.join("docs").to_str().expect("utf8")])
        );
    }

    {
        let _agent_guard = EnvGuard::set(&ENV_AGENT_DIR, Some(temp.path().to_str().expect("utf8")));
        assert_eq!(get_agent_dir(), temp.path().to_str().expect("utf8"));
    }

    {
        let _share_guard = EnvGuard::set("PI_SHARE_VIEWER_URL", Some("https://viewer.example/view/"));
        assert_eq!(get_share_viewer_url("abc123"), "https://viewer.example/view/#abc123");
    }

    let renamed = SelfUpdatePackageTarget {
        package_name: "@new-scope/pi".to_owned(),
        install_spec: "@new-scope/pi".to_owned(),
    };
    assert_eq!(
        get_self_update_unavailable_instruction("@earendil-works/pi-coding-agent", None, Some(renamed)),
        "Update @new-scope/pi using the package manager, wrapper, or source checkout that provides this installation."
    );
}

// PORT: The npm/pnpm/yarn/bun self-update tests in `config.test.ts` mutate Node's
// `Object.defineProperty(process, "execPath", ...)` and `process.argv[1]` to simulate
// different global node_modules install trees. In Rust `config::detect_install_method()`
// reads compile-time `env!("CARGO_MANIFEST_DIR")` and `std::env::current_exe()` without
// an overridable test seam, so those process.execPath-mocked permutations are not testable
// from an external integration test binary without modifying `src/config.rs`.
