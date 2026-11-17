// PORT: QuickJS plugin runtime enabled per user requirement (overrides §10.6)
//! Port of packages/coding-agent/test/suite/regressions/extension-factory-cache.test.ts

use std::fs;
use std::path::PathBuf;

use pi_coding_agent::core::extensions::{clear_extension_cache, load_extensions, load_extensions_cached};
use pi_coding_agent::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions};

fn write_counting_extension(file_path: &std::path::Path, log_path: &std::path::Path) {
    let log_str = log_path.to_string_lossy().replace('\\', "\\\\");
    fs::write(
        file_path,
        format!(
            r#"
import * as fs from "node:fs";
fs.appendFileSync("{log_str}", "load\n");

export default function () {{
    fs.appendFileSync("{log_str}", "factory\n");
}}
"#
        ),
    )
    .unwrap();
}

fn count_lines(log_path: &std::path::Path) -> (usize, usize) {
    let text = fs::read_to_string(log_path).unwrap_or_default();
    let loads = text.lines().filter(|l| *l == "load").count();
    let runs = text.lines().filter(|l| *l == "factory").count();
    (loads, runs)
}

fn fixture(name: &str) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cwd = root.join(format!("{name}-cwd"));
    let log = root.join("counts.log");
    fs::create_dir_all(&cwd).unwrap();
    (dir, root, cwd, log)
}

#[tokio::test]
#[serial_test::serial]
async fn caches_extension_modules_for_same_cwd_but_reruns_factories() {
    clear_extension_cache();
    let (_guard, root, cwd, log) = fixture("same-cwd");
    let ext_path = root.join("counting.ts");
    write_counting_extension(&ext_path, &log);
    let paths = vec![ext_path.to_string_lossy().into_owned()];
    let cwd_str = cwd.to_str().unwrap();

    let first = load_extensions_cached(&paths, cwd_str, None, None).await;
    let second = load_extensions_cached(&paths, cwd_str, None, None).await;
    assert_eq!(first.errors, vec![]);
    assert_eq!(second.errors, vec![]);

    let (loads, runs) = count_lines(&log);
    assert_eq!(loads, 1);
    assert_eq!(runs, 2);
    assert!(!std::sync::Arc::ptr_eq(&first.runtime.inner, &second.runtime.inner));
    clear_extension_cache();
}

#[tokio::test]
#[serial_test::serial]
async fn does_not_cache_direct_load_extensions_calls() {
    clear_extension_cache();
    let (_guard, root, cwd, log) = fixture("direct");
    let ext_path = root.join("counting.ts");
    write_counting_extension(&ext_path, &log);
    let paths = vec![ext_path.to_string_lossy().into_owned()];
    let cwd_str = cwd.to_str().unwrap();

    let first = load_extensions(&paths, cwd_str, None, None).await;
    let second = load_extensions(&paths, cwd_str, None, None).await;
    assert_eq!(first.errors, vec![]);
    assert_eq!(second.errors, vec![]);

    let (loads, runs) = count_lines(&log);
    assert_eq!(loads, 2);
    assert_eq!(runs, 2);
    clear_extension_cache();
}

#[tokio::test]
#[serial_test::serial]
async fn keeps_the_cache_scoped_to_one_cwd() {
    clear_extension_cache();
    let (_guard, root, _cwd, log) = fixture("cross-cwd");
    let first_cwd = root.join("first");
    let second_cwd = root.join("second");
    fs::create_dir_all(&first_cwd).unwrap();
    fs::create_dir_all(&second_cwd).unwrap();
    let ext_path = root.join("counting.ts");
    write_counting_extension(&ext_path, &log);
    let paths = vec![ext_path.to_string_lossy().into_owned()];

    load_extensions_cached(&paths, first_cwd.to_str().unwrap(), None, None).await;
    load_extensions_cached(&paths, second_cwd.to_str().unwrap(), None, None).await;
    load_extensions_cached(&paths, second_cwd.to_str().unwrap(), None, None).await;

    let (loads, runs) = count_lines(&log);
    assert_eq!(loads, 2);
    assert_eq!(runs, 3);
    clear_extension_cache();
}

#[tokio::test]
#[serial_test::serial]
async fn clears_cached_extension_modules_across_default_resource_loader_reload() {
    clear_extension_cache();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let agent_dir = root.join("agent");
    let ext_dir = agent_dir.join("extensions");
    let cwd = root.join("cwd");
    let log = root.join("counts.log");
    fs::create_dir_all(&ext_dir).unwrap();
    fs::create_dir_all(&cwd).unwrap();

    let ext_path = ext_dir.join("counting.ts");
    write_counting_extension(&ext_path, &log);

    let loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: cwd.to_string_lossy().into_owned(),
        agent_dir: agent_dir.to_string_lossy().into_owned(),
        no_skills: true,
        no_prompt_templates: true,
        no_themes: true,
        no_context_files: true,
        ..Default::default()
    });

    loader.reload(None).await.expect("initial reload");
    loader.reload(None).await.expect("second reload");
    let (loads, runs) = count_lines(&log);
    assert_eq!(loads, 2);
    assert_eq!(runs, 2);
    clear_extension_cache();
}
