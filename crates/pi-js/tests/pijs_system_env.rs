//! pi_js::env: overlay layers, test guards and the `os.*` / `process.*` helpers.
//! Expected values for `os.tmpdir()`, `os.homedir()` and `process.chdir` errors were
//! checked against node v24.21.0.

use pi_js::env::{self, testing::CwdGuard, testing::EnvGuard, testing::PlatformGuard};
use pi_js::error::Error;

const KEY: &str = "PI_JS_SYSTEM_ENV_TEST_KEY";

#[test]
fn env_guard_overrides_and_restores() {
    assert_eq!(env::var(KEY), None);
    {
        let _g = EnvGuard::set(KEY, Some("one"));
        assert_eq!(env::var(KEY).as_deref(), Some("one"));
        assert_eq!(env::vars().get(KEY).map(String::as_str), Some("one"));
        {
            let _inner = EnvGuard::set(KEY, None);
            assert_eq!(env::var(KEY), None);
            assert!(!env::vars().contains_key(KEY));
        }
        assert_eq!(env::var(KEY).as_deref(), Some("one"));
    }
    assert_eq!(env::var(KEY), None);
    assert!(!env::vars().contains_key(KEY));
}

#[test]
fn env_guard_can_hide_real_variables() {
    // A variable from the real OS environment (not one another test put in the overlay).
    let (key, value) = std::env::vars()
        .find(|(k, _)| !k.starts_with('='))
        .expect("some environment variable");
    let _g = EnvGuard::set(&key, None);
    assert_eq!(env::var(&key), None);
    assert!(!env::vars().contains_key(&key));
    drop(_g);
    assert_eq!(env::var(&key), Some(value));
}

#[test]
fn set_var_under_a_guard_stays_thread_local() {
    let key = "PI_JS_SYSTEM_ENV_TL_WRITE";
    {
        let mut g = EnvGuard::empty();
        g.stub("PI_JS_SYSTEM_ENV_STUBBED", Some("s"));
        env::set_var(key, "v");
        assert_eq!(env::var(key).as_deref(), Some("v"));
        assert_eq!(env::var("PI_JS_SYSTEM_ENV_STUBBED").as_deref(), Some("s"));
        // Other threads see neither the stub nor the write.
        let seen = std::thread::spawn(move || (env::var(key), env::var("PI_JS_SYSTEM_ENV_STUBBED")))
            .join()
            .unwrap();
        assert_eq!(seen, (None, None));
        env::remove_var(key);
        assert_eq!(env::var(key), None);
        env::set_var(key, "again");
    }
    // Writes made while a guard was alive are discarded with the last guard.
    assert_eq!(env::var(key), None);
}

#[test]
fn set_many_installs_several_overrides() {
    let _g = EnvGuard::set_many(&[("PI_JS_SYSTEM_A", Some("a")), ("PI_JS_SYSTEM_B", Some("b"))]);
    assert_eq!(env::var("PI_JS_SYSTEM_A").as_deref(), Some("a"));
    assert_eq!(env::var("PI_JS_SYSTEM_B").as_deref(), Some("b"));
    let vars = env::vars();
    let pos_a = vars.get_index_of("PI_JS_SYSTEM_A").unwrap();
    let pos_b = vars.get_index_of("PI_JS_SYSTEM_B").unwrap();
    assert!(pos_a < pos_b, "new keys are appended in insertion order");
}

#[test]
#[serial_test::serial]
fn set_var_without_a_guard_is_process_global() {
    let key = format!("PI_JS_SYSTEM_ENV_GLOBAL_{}", env::pid());
    env::set_var(&key, "global");
    let k = key.clone();
    let seen = std::thread::spawn(move || env::var(&k)).join().unwrap();
    assert_eq!(seen.as_deref(), Some("global"));
    assert_eq!(env::vars().get(&key).map(String::as_str), Some("global"));
    env::remove_var(&key);
    assert_eq!(env::var(&key), None);
    assert!(!env::vars().contains_key(&key));
}

type Vars = &'static [(&'static str, Option<&'static str>)];

#[cfg(not(windows))]
#[test]
fn tmp_dir_follows_node_rules() {
    let cases: &[(Vars, &str)] = &[
        (&[("TMPDIR", Some("/x/y/")), ("TMP", None), ("TEMP", None)], "/x/y"),
        (&[("TMPDIR", Some("/")), ("TMP", None), ("TEMP", None)], "/"),
        (&[("TMPDIR", Some("//")), ("TMP", None), ("TEMP", None)], "/"),
        (&[("TMPDIR", Some("")), ("TMP", Some("/t/")), ("TEMP", None)], "/t"),
        (&[("TMPDIR", None), ("TMP", None), ("TEMP", Some("/e"))], "/e"),
        (&[("TMPDIR", Some("")), ("TMP", Some("")), ("TEMP", Some(""))], "/tmp"),
        (&[("TMPDIR", None), ("TMP", None), ("TEMP", None)], "/tmp"),
    ];
    for (vars, want) in cases {
        let _g = EnvGuard::set_many(vars);
        assert_eq!(env::tmp_dir(), *want, "{vars:?}");
    }
}

#[cfg(windows)]
#[test]
fn tmp_dir_follows_node_rules() {
    let cases: &[(Vars, &str)] = &[
        (&[("TEMP", Some("C:\\t\\")), ("TMP", None)], "C:\\t"),
        (&[("TEMP", Some("C:\\")), ("TMP", None)], "C:\\"),
        (&[("TEMP", None), ("TMP", Some("D:\\x"))], "D:\\x"),
        (
            &[("TEMP", None), ("TMP", None), ("SystemRoot", Some("C:\\Windows"))],
            "C:\\Windows\\temp",
        ),
    ];
    for (vars, want) in cases {
        let _g = EnvGuard::set_many(vars);
        assert_eq!(env::tmp_dir(), *want, "{vars:?}");
    }
}

#[test]
fn home_dir_prefers_the_environment() {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    {
        let _g = EnvGuard::set(key, Some("/somewhere/home"));
        assert_eq!(env::home_dir(), "/somewhere/home");
    }
    {
        // libuv returns an empty HOME as-is (`HOME= node -p 'os.homedir()'` prints "").
        let _g = EnvGuard::set(key, Some(""));
        assert_eq!(env::home_dir(), "");
    }
    #[cfg(unix)]
    {
        // Without HOME, the password database is used.
        let _g = EnvGuard::set(key, None);
        assert!(!env::home_dir().is_empty());
    }
}

#[test]
fn platform_guard_overrides_and_restores() {
    let native = env::platform();
    assert!(
        [
            "darwin", "linux", "win32", "freebsd", "openbsd", "sunos", "aix", "android"
        ]
        .contains(&native)
    );
    {
        let _g = PlatformGuard::set("win32");
        assert_eq!(env::platform(), "win32");
        {
            let _inner = PlatformGuard::set("linux");
            assert_eq!(env::platform(), "linux");
        }
        assert_eq!(env::platform(), "win32");
    }
    assert_eq!(env::platform(), native);
    if cfg!(target_os = "macos") {
        assert_eq!(native, "darwin");
    }
}

#[test]
fn arch_and_os_info() {
    let arch = env::arch();
    if cfg!(target_arch = "x86_64") {
        assert_eq!(arch, "x64");
    } else if cfg!(target_arch = "aarch64") {
        assert_eq!(arch, "arm64");
    }
    assert!(!env::os_release().is_empty());
    assert_eq!(env::pid(), std::process::id());
}

#[test]
fn cwd_guard_overrides_cwd_and_chdir() {
    let dir = tempfile::TempDir::new().unwrap();
    let canonical = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("file"), "x").unwrap();
    let real = env::cwd();
    {
        let _g = CwdGuard::set(canonical.clone());
        assert_eq!(env::cwd(), canonical);
        env::chdir("sub").unwrap();
        // `uv_cwd` drops the `\\?\` prefix that `canonicalize` and `path.join` keep.
        let sub = pi_js::path::join(&[&canonical, "sub"]);
        let sub = sub.strip_prefix(r"\\?\").unwrap_or(&sub);
        assert_eq!(env::cwd(), sub);
        env::chdir("..").unwrap();
        let canonical_cwd = canonical.strip_prefix(r"\\?\").unwrap_or(&canonical);
        assert_eq!(env::cwd(), canonical_cwd);

        let err = env::chdir("missing").unwrap_err();
        let Error::Node(e) = err else {
            panic!("expected NodeError")
        };
        assert_eq!(e.code, "ENOENT");
        assert_eq!(e.syscall, "chdir");
        assert_eq!(
            e.message,
            format!("ENOENT: no such file or directory, chdir '{canonical_cwd}' -> 'missing'")
        );
        assert_eq!(e.path.as_deref(), Some(canonical_cwd));
        assert_eq!(e.dest.as_deref(), Some("missing"));

        let err = env::chdir("file").unwrap_err();
        // Unix `chdir` of a file is ENOTDIR. Windows `SetCurrentDirectoryW` returns
        // ERROR_DIRECTORY, which libuv reports as ENOENT.
        let file_code = if cfg!(windows) {
            "ENOENT: no such file or directory"
        } else {
            "ENOTDIR: not a directory"
        };
        assert_eq!(
            err.to_string(),
            format!("{file_code}, chdir '{canonical_cwd}' -> 'file'")
        );
        let err = env::chdir("").unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("ENOENT: no such file or directory, chdir '{canonical_cwd}' -> ''")
        );
        // The real process cwd is untouched.
        assert!(env::testing::cwd_overridden());
    }
    assert!(!env::testing::cwd_overridden());
    assert_eq!(env::cwd(), real);
}

#[cfg(unix)]
#[test]
fn chdir_errors_without_a_guard_use_the_real_cwd() {
    let real = env::cwd();
    let err = env::chdir("/nonexistent/pi-js-system").unwrap_err();
    let Error::Node(e) = err else {
        panic!("expected NodeError")
    };
    assert_eq!(e.errno, -libc::ENOENT);
    assert_eq!(
        e.message,
        format!("ENOENT: no such file or directory, chdir '{real}' -> '/nonexistent/pi-js-system'")
    );
    assert_eq!(env::cwd(), real);
}

#[cfg(windows)]
#[test]
fn windows_keys_are_case_insensitive() {
    let _g = EnvGuard::set("Pi_Js_Mixed", Some("v"));
    assert_eq!(env::var("PI_JS_MIXED").as_deref(), Some("v"));
    assert_eq!(env::var("pi_js_mixed").as_deref(), Some("v"));
}
