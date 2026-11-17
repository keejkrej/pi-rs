//! Port of packages/coding-agent/test/paths.test.ts

use pi_coding_agent::utils::paths::{
    PathInputOptions, canonicalize_path, get_cwd_relative_path, is_local_path, normalize_path,
    normalize_windows_shell_path, resolve_path,
};
use pi_js::env::testing::PlatformGuard;
use tempfile::TempDir;

fn to_file_url(p: &str) -> String {
    url::Url::from_file_path(p)
        .expect("absolute path to file URL")
        .to_string()
}

mod canonicalize_path_tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn returns_the_real_path_for_a_regular_file() {
        let dir = TempDir::new().unwrap();
        let dir_path = dir.path().to_string_lossy().into_owned();
        let file = pi_js::path::join(&[&dir_path, "file.txt"]);
        pi_js::fs::write_file(&file, "hello", None).unwrap();
        assert_eq!(canonicalize_path(&file), pi_js::fs::realpath(&file).unwrap());
    }

    #[test]
    fn resolves_symlinks_to_their_targets() {
        let dir = TempDir::new().unwrap();
        let dir_path = dir.path().to_string_lossy().into_owned();
        let target = pi_js::path::join(&[&dir_path, "target.txt"]);
        let link = pi_js::path::join(&[&dir_path, "link.txt"]);
        pi_js::fs::write_file(&target, "hello", None).unwrap();
        pi_js::fs::symlink(&target, &link).unwrap();
        assert_eq!(canonicalize_path(&link), pi_js::fs::realpath(&target).unwrap());
    }

    #[test]
    fn resolves_directory_symlinks() {
        let dir = TempDir::new().unwrap();
        let dir_path = dir.path().to_string_lossy().into_owned();
        let target_dir = pi_js::path::join(&[&dir_path, "target-dir"]);
        let link_dir = pi_js::path::join(&[&dir_path, "link-dir"]);
        pi_js::fs::mkdir(&target_dir, true, None).unwrap();
        pi_js::fs::symlink(&target_dir, &link_dir).unwrap();
        assert_eq!(canonicalize_path(&link_dir), pi_js::fs::realpath(&target_dir).unwrap());
    }

    #[test]
    fn falls_back_to_the_raw_path_when_the_target_does_not_exist() {
        let dir = TempDir::new().unwrap();
        let dir_path = dir.path().to_string_lossy().into_owned();
        let nonexistent = pi_js::path::join(&[&dir_path, "no-such-file"]);
        assert_eq!(canonicalize_path(&nonexistent), nonexistent);
    }

    #[test]
    fn falls_back_to_the_raw_path_for_a_dangling_symlink() {
        let dir = TempDir::new().unwrap();
        let dir_path = dir.path().to_string_lossy().into_owned();
        let target = pi_js::path::join(&[&dir_path, "target.txt"]);
        let link = pi_js::path::join(&[&dir_path, "link.txt"]);
        pi_js::fs::symlink(&target, &link).unwrap();
        assert_eq!(canonicalize_path(&link), link);
    }
}

mod get_cwd_relative_path_tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn keeps_cwd_relative_names_that_start_with_dots() {
        let cwd = pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-paths-cwd"]);
        assert_eq!(
            get_cwd_relative_path(&pi_js::path::join(&[&cwd, "..config", "AGENTS.md"]), &cwd),
            Some(pi_js::path::join(&["..config", "AGENTS.md"]))
        );
    }

    #[test]
    fn rejects_parent_directory_traversals() {
        let cwd = pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-paths-cwd"]);
        assert_eq!(
            get_cwd_relative_path(&pi_js::path::join(&[&cwd, "..", "AGENTS.md"]), &cwd),
            None
        );
    }
}

mod resolve_path_tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn expands_only_home_tilde_shortcuts() {
        let cwd = pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-paths-cwd"]);
        assert_eq!(normalize_path("~", PathInputOptions::default()), pi_js::env::home_dir());
        assert_eq!(
            normalize_path("~/file.txt", PathInputOptions::default()),
            pi_js::path::join(&[&pi_js::env::home_dir(), "file.txt"])
        );
        assert_eq!(
            resolve_path("~draft.md", Some(&cwd), PathInputOptions::default()),
            pi_js::path::resolve(&[&cwd, "~draft.md"])
        );
        assert_eq!(normalize_path("~draft.md", PathInputOptions::default()), "~draft.md");
    }

    #[test]
    fn resolves_relative_paths_against_the_base_directory() {
        let cwd = pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-paths-cwd"]);
        assert_eq!(
            resolve_path("subdir/file.txt", Some(&cwd), PathInputOptions::default()),
            pi_js::path::resolve(&[&cwd, "subdir/file.txt"])
        );
        assert_eq!(
            resolve_path("subdir/file.txt", Some(&to_file_url(&cwd)), PathInputOptions::default()),
            pi_js::path::resolve(&[&cwd, "subdir/file.txt"])
        );
    }

    #[test]
    fn accepts_file_urls() {
        let dir = TempDir::new().unwrap();
        let dir_path = dir.path().to_string_lossy().into_owned();
        let file_path = pi_js::path::join(&[&dir_path, "file with spaces.txt"]);
        let base = pi_js::path::join(&[&dir_path, "base"]);
        assert_eq!(
            resolve_path(&to_file_url(&file_path), Some(&base), PathInputOptions::default()),
            pi_js::path::resolve(&[&file_path])
        );
    }

    #[test]
    fn preserves_posix_absolute_paths_with_literal_percent_sequences() {
        if pi_js::env::platform() == "win32" {
            return;
        }
        let dir = TempDir::new().unwrap();
        let dir_path = dir.path().to_string_lossy().into_owned();
        let base = pi_js::path::join(&[&dir_path, "base"]);
        for file_path in [
            pi_js::path::join(&[&dir_path, "report%2026.md"]),
            pi_js::path::join(&[&dir_path, "foo%2Fbar"]),
            pi_js::path::join(&[&dir_path, "malformed%A.md"]),
        ] {
            assert_eq!(
                resolve_path(&file_path, Some(&base), PathInputOptions::default()),
                pi_js::path::resolve(&[&file_path])
            );
        }
    }

    #[test]
    fn invalid_file_url_does_not_panic() {
        // PORT: `utils/paths.rs::resolve_path` returns `String` (not `Result<String>`), so invalid UTF-8
        // percent sequences in `file://` URLs do not throw.
        let resolved = resolve_path("file:///%E0%A4%A", Some("/tmp"), PathInputOptions::default());
        assert!(!resolved.is_empty());
    }
}

mod normalize_windows_shell_path_tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn converts_git_bash_msys_cygwin_and_wsl_drive_paths() {
        assert_eq!(
            normalize_windows_shell_path("/c/Users/example/project"),
            "C:\\Users\\example\\project"
        );
        assert_eq!(normalize_windows_shell_path("/cygdrive/d/work"), "D:\\work");
        assert_eq!(normalize_windows_shell_path("/mnt/e/source"), "E:\\source");
        assert_eq!(normalize_windows_shell_path("/c"), "C:\\");
    }

    #[test]
    fn leaves_other_path_forms_unchanged() {
        for path in [
            "C:/Users/example",
            "C:\\Users\\example",
            "//server/share/file",
            "/c/Users\\example",
            "relative/file",
            "/tmp/file",
        ] {
            assert_eq!(normalize_windows_shell_path(path), path);
        }
    }

    #[test]
    fn is_applied_by_normal_path_handling_on_windows() {
        let _guard = PlatformGuard::set("win32");
        assert_eq!(
            normalize_path("/c/Users/example", PathInputOptions::default()),
            "C:\\Users\\example"
        );
    }
}

mod is_local_path_tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn returns_true_for_bare_names() {
        assert_eq!(is_local_path("my-package"), true);
    }

    #[test]
    fn returns_true_for_relative_paths() {
        assert_eq!(is_local_path("./foo"), true);
    }

    #[test]
    fn returns_true_for_file_urls() {
        assert_eq!(is_local_path("file:///tmp/foo"), true);
    }

    #[test]
    fn returns_false_for_npm_protocol() {
        assert_eq!(is_local_path("npm:package"), false);
    }

    #[test]
    fn returns_false_for_git_protocol() {
        assert_eq!(is_local_path("git://repo"), false);
    }

    #[test]
    fn returns_false_for_https_protocol() {
        assert_eq!(is_local_path("https://example.com"), false);
    }
}
