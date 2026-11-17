//! Port of packages/coding-agent/test/clipboard-image.test.ts

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use indexmap::IndexMap;
use pi_coding_agent::utils::clipboard_image::{ClipboardImage, ReadClipboardImageOptions, read_clipboard_image};
use pi_js::Error;
use pi_js::env::testing::{EnvGuard, PlatformGuard};
use pi_tui::native_platform::testing::override_get_image;
use serial_test::serial;
use tempfile::TempDir;

const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
];

fn write_exec(dir: &Path, name: &str, script: &str) {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write shim");
    let mut perms = fs::metadata(&path).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).expect("chmod");
}

fn read_calls(log_path: &Path) -> Vec<String> {
    fs::read_to_string(log_path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

fn make_shim_env(dir: &Path) -> EnvGuard {
    let cur = pi_js::env::var("PATH").unwrap_or_default();
    let prefixed = format!("{}:{cur}", dir.display());
    EnvGuard::set_many(&[
        ("PATH", Some(prefixed.as_str())),
        ("DISPLAY", Some(":0")),
        ("TERMUX_VERSION", None),
        ("WSL_DISTRO_NAME", None),
        ("WSLENV", None),
        ("WAYLAND_DISPLAY", None),
    ])
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn wayland_and_x11_command_results_stop_fallback() {
    for (backend, command, env_pairs) in [
        ("wayland", "wl-paste", vec![("WAYLAND_DISPLAY", "1"), ("DISPLAY", ":0")]),
        ("x11", "xclip", vec![("DISPLAY", ":0")]),
    ] {
        for present in [true, false] {
            let dir = TempDir::new().expect("tempdir");
            let log = dir.path().join("calls.log");
            let png_path = dir.path().join("img.png");
            fs::write(&png_path, TINY_PNG).expect("write png");

            let list_out = if present {
                "text/plain\nimage/png\n"
            } else {
                "text/plain\n"
            };
            write_exec(
                dir.path(),
                command,
                &format!(
                    "printf '%s\\n' \"{command} $*\" >> '{}'\ncase \"$*\" in\n  *--list-types*|*TARGETS*) printf '{list_out}' ;;\n  *image/png*) cat '{}' ;;\n  *) exit 1 ;;\nesac",
                    log.display(),
                    png_path.display()
                ),
            );
            // Provide a non-target sibling binary that fails if mistakenly invoked
            let other = if command == "wl-paste" { "xclip" } else { "wl-paste" };
            write_exec(
                dir.path(),
                other,
                &format!("printf '%s\\n' \"{other} $*\" >> '{}'\nexit 1", log.display()),
            );

            let _env_guard = make_shim_env(dir.path());
            let _plat = PlatformGuard::set("linux");
            let native_calls = Arc::new(AtomicUsize::new(0));
            let nc = Arc::clone(&native_calls);
            let _native = override_get_image(move |_| {
                nc.fetch_add(1, Ordering::SeqCst);
                Ok(Some(Some(TINY_PNG.to_vec())))
            });

            let mut env = IndexMap::new();
            for (k, v) in &env_pairs {
                env.insert((*k).to_owned(), (*v).to_owned());
            }

            let res = read_clipboard_image(Some(ReadClipboardImageOptions {
                env: Some(env),
                platform: Some("linux".to_owned()),
            }))
            .await;

            if present {
                assert_eq!(
                    res,
                    Some(ClipboardImage {
                        bytes: TINY_PNG.to_vec(),
                        mime_type: "image/png".to_owned(),
                    }),
                    "{backend} present={present}"
                );
            } else {
                assert_eq!(res, None, "{backend} present={present}");
            }
            let calls = read_calls(&log);
            assert_eq!(calls.len(), if present { 2 } else { 1 }, "{backend}: {calls:?}");
            assert!(calls.iter().all(|c| c.starts_with(command)), "{calls:?}");
            assert_eq!(native_calls.load(Ordering::SeqCst), 0);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn x11_does_not_probe_image_types_when_targets_fails() {
    let dir = TempDir::new().expect("tempdir");
    let log = dir.path().join("calls.log");
    write_exec(
        dir.path(),
        "xclip",
        &format!("printf '%s\\n' \"$*\" >> '{}'\nexit 1", log.display()),
    );
    let _env_guard = make_shim_env(dir.path());
    let _plat = PlatformGuard::set("linux");
    let native_calls = Arc::new(AtomicUsize::new(0));
    let nc = Arc::clone(&native_calls);
    let _native = override_get_image(move |_| {
        nc.fetch_add(1, Ordering::SeqCst);
        Ok(Some(None))
    });

    let mut env = IndexMap::new();
    env.insert("DISPLAY".to_owned(), ":0".to_owned());

    let res = read_clipboard_image(Some(ReadClipboardImageOptions {
        env: Some(env),
        platform: Some("linux".to_owned()),
    }))
    .await;

    assert_eq!(res, None);
    assert_eq!(read_calls(&log), vec!["-selection clipboard -t TARGETS -o"]);
    assert_eq!(native_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn x11_does_not_probe_unadvertised_image_types() {
    let dir = TempDir::new().expect("tempdir");
    let log = dir.path().join("calls.log");
    write_exec(
        dir.path(),
        "xclip",
        &format!(
            "printf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n  *TARGETS*) printf 'image/png\\n' ;;\n  *image/png*) exit 1 ;;\n  *) printf 'hello' ;;\nesac",
            log.display()
        ),
    );
    let _env_guard = make_shim_env(dir.path());
    let _plat = PlatformGuard::set("linux");
    let native_calls = Arc::new(AtomicUsize::new(0));
    let nc = Arc::clone(&native_calls);
    let _native = override_get_image(move |_| {
        nc.fetch_add(1, Ordering::SeqCst);
        Ok(Some(None))
    });

    let mut env = IndexMap::new();
    env.insert("DISPLAY".to_owned(), ":0".to_owned());

    let res = read_clipboard_image(Some(ReadClipboardImageOptions {
        env: Some(env),
        platform: Some("linux".to_owned()),
    }))
    .await;

    assert_eq!(res, None);
    assert_eq!(
        read_calls(&log),
        vec![
            "-selection clipboard -t TARGETS -o",
            "-selection clipboard -t image/png -o",
        ]
    );
    assert_eq!(native_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn native_x11_result_stops_fallback() {
    for case in [Some(TINY_PNG.to_vec()), Some(Vec::new()), None] {
        let dir = TempDir::new().expect("tempdir");
        let log = dir.path().join("calls.log");
        write_exec(
            dir.path(),
            "xclip",
            &format!("printf 'xclip\\n' >> '{}'\nexit 1", log.display()),
        );
        let _env_guard = make_shim_env(dir.path());
        let _plat = PlatformGuard::set("linux");
        let ret = case.clone();
        let native_calls = Arc::new(AtomicUsize::new(0));
        let nc = Arc::clone(&native_calls);
        let _native = override_get_image(move |_| {
            nc.fetch_add(1, Ordering::SeqCst);
            Ok(Some(ret.clone()))
        });

        let mut env = IndexMap::new();
        env.insert("DISPLAY".to_owned(), ":0".to_owned());

        let res = read_clipboard_image(Some(ReadClipboardImageOptions {
            env: Some(env),
            platform: Some("linux".to_owned()),
        }))
        .await;

        let expected = case.and_then(|b| {
            if b.is_empty() {
                None
            } else {
                Some(ClipboardImage {
                    bytes: b,
                    mime_type: "image/png".to_owned(),
                })
            }
        });
        assert_eq!(res, expected);
        assert_eq!(native_calls.load(Ordering::SeqCst), 1);
        assert_eq!(read_calls(&log), vec!["xclip"]);
    }
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn wayland_falls_back_to_x11_after_failure() {
    let dir = TempDir::new().expect("tempdir");
    let log = dir.path().join("calls.log");
    let png_path = dir.path().join("img.png");
    fs::write(&png_path, TINY_PNG).expect("write png");

    write_exec(
        dir.path(),
        "wl-paste",
        &format!("printf 'wl-paste\\n' >> '{}'\nexit 1", log.display()),
    );
    write_exec(
        dir.path(),
        "xclip",
        &format!(
            "printf 'xclip\\n' >> '{}'\ncase \"$*\" in\n  *TARGETS*) printf 'image/png\\n' ;;\n  *) cat '{}' ;;\nesac",
            log.display(),
            png_path.display()
        ),
    );
    let _env_guard = make_shim_env(dir.path());
    let _plat = PlatformGuard::set("linux");
    let native_calls = Arc::new(AtomicUsize::new(0));
    let nc = Arc::clone(&native_calls);
    let _native = override_get_image(move |_| {
        nc.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Some(TINY_PNG.to_vec())))
    });

    let mut env = IndexMap::new();
    env.insert("WAYLAND_DISPLAY".to_owned(), "1".to_owned());

    let res = read_clipboard_image(Some(ReadClipboardImageOptions {
        env: Some(env),
        platform: Some("linux".to_owned()),
    }))
    .await;

    assert_eq!(
        res,
        Some(ClipboardImage {
            bytes: TINY_PNG.to_vec(),
            mime_type: "image/png".to_owned(),
        })
    );
    assert_eq!(native_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn wsl_tries_powershell_before_broken_native_x11_bridge() {
    let dir = TempDir::new().expect("tempdir");
    let tmp_record = dir.path().join("tmpfile.txt");
    let ps_args_log = dir.path().join("ps.args");
    let png_path = dir.path().join("img.png");
    fs::write(&png_path, TINY_PNG).expect("write png");

    write_exec(dir.path(), "wl-paste", "exit 1");
    write_exec(dir.path(), "xclip", "exit 1");
    write_exec(
        dir.path(),
        "wslpath",
        &format!(
            "printf '%s' \"$2\" > '{}'\nprintf 'C:\\\\Users\\\\O'\\''Hare\\\\clip.png\\n'",
            tmp_record.display()
        ),
    );
    write_exec(
        dir.path(),
        "powershell.exe",
        &format!(
            "printf '%s\\n' \"$3\" > '{}'\n[ -z \"$PI_WSL_CLIPBOARD_IMAGE_PATH\" ] || exit 2\nTARGET=\"$(cat '{}')\"\ncp '{}' \"$TARGET\"\nprintf 'ok\\n'",
            ps_args_log.display(),
            tmp_record.display(),
            png_path.display()
        ),
    );

    let _env_guard = make_shim_env(dir.path());
    let _plat = PlatformGuard::set("linux");
    let native_calls = Arc::new(AtomicUsize::new(0));
    let nc = Arc::clone(&native_calls);
    let _native = override_get_image(move |_| {
        nc.fetch_add(1, Ordering::SeqCst);
        Err(Error::msg("Broken X11 bridge"))
    });

    let mut env = IndexMap::new();
    env.insert("WSL_DISTRO_NAME".to_owned(), "Ubuntu".to_owned());

    let res = read_clipboard_image(Some(ReadClipboardImageOptions {
        env: Some(env),
        platform: Some("linux".to_owned()),
    }))
    .await;

    assert_eq!(
        res,
        Some(ClipboardImage {
            bytes: TINY_PNG.to_vec(),
            mime_type: "image/png".to_owned(),
        })
    );
    let script = fs::read_to_string(&ps_args_log).expect("read ps script");
    assert!(script.contains("$path = 'C:\\Users\\O''Hare\\clip.png'"), "{script}");
    assert_eq!(native_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn darwin_and_win32_read_native_image_once() {
    for plat in ["darwin", "win32"] {
        for case in [Some(TINY_PNG.to_vec()), Some(Vec::new()), None] {
            let dir = TempDir::new().expect("tempdir");
            let log = dir.path().join("calls.log");
            write_exec(
                dir.path(),
                "wl-paste",
                &format!("printf 'wl-paste\\n' >> '{}'", log.display()),
            );
            let _env_guard = make_shim_env(dir.path());
            let _plat = PlatformGuard::set(plat);
            let ret = case.clone();
            let native_calls = Arc::new(AtomicUsize::new(0));
            let nc = Arc::clone(&native_calls);
            let _native = override_get_image(move |_| {
                nc.fetch_add(1, Ordering::SeqCst);
                Ok(Some(ret.clone()))
            });

            let res = read_clipboard_image(Some(ReadClipboardImageOptions {
                env: Some(IndexMap::new()),
                platform: Some(plat.to_owned()),
            }))
            .await;

            let expected = case.and_then(|b| {
                if b.is_empty() {
                    None
                } else {
                    Some(ClipboardImage {
                        bytes: b,
                        mime_type: "image/png".to_owned(),
                    })
                }
            });
            assert_eq!(res, expected, "platform={plat}");
            assert_eq!(native_calls.load(Ordering::SeqCst), 1);
            assert!(read_calls(&log).is_empty());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn termux_does_not_read_image_clipboards() {
    let dir = TempDir::new().expect("tempdir");
    let log = dir.path().join("calls.log");
    write_exec(
        dir.path(),
        "wl-paste",
        &format!("printf 'wl-paste\\n' >> '{}'", log.display()),
    );
    let _env_guard = make_shim_env(dir.path());
    let _plat = PlatformGuard::set("linux");
    let native_calls = Arc::new(AtomicUsize::new(0));
    let nc = Arc::clone(&native_calls);
    let _native = override_get_image(move |_| {
        nc.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Some(TINY_PNG.to_vec())))
    });

    let mut env = IndexMap::new();
    env.insert("TERMUX_VERSION".to_owned(), "0.119".to_owned());

    let res = read_clipboard_image(Some(ReadClipboardImageOptions {
        env: Some(env),
        platform: Some("linux".to_owned()),
    }))
    .await;

    assert_eq!(res, None);
    assert_eq!(native_calls.load(Ordering::SeqCst), 0);
    assert!(read_calls(&log).is_empty());
}
