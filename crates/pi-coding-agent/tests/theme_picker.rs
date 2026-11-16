//! Port of packages/coding-agent/test/theme-picker.test.ts

use pi_coding_agent::modes::interactive::theme::theme::{
    ThemeInfo, get_available_themes, get_available_themes_with_paths, set_registered_themes,
};
use pi_js::env::testing::EnvGuard;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use serial_test::serial;

struct PickerFixture {
    temp_root: String,
    agent_dir: String,
    _env: EnvGuard,
}

impl PickerFixture {
    fn new() -> Self {
        let temp_root = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-theme-picker-"])).unwrap();
        let agent_dir = pi_js::path::join(&[&temp_root, "agent"]);
        let env = EnvGuard::set("PI_CODING_AGENT_DIR", Some(&agent_dir));
        pi_js::fs::mkdir(&pi_js::path::join(&[&agent_dir, "themes"]), true, None).unwrap();
        set_registered_themes(Vec::new()).unwrap();
        Self {
            temp_root,
            agent_dir,
            _env: env,
        }
    }
}

impl Drop for PickerFixture {
    fn drop(&mut self) {
        let _ = set_registered_themes(Vec::new());
        let _ = pi_js::fs::rm(&self.temp_root, true, true);
    }
}

#[test]
#[serial]
fn uses_custom_theme_content_names_instead_of_file_names() {
    let fixture = PickerFixture::new();
    let mut custom: Value = serde_json::from_str(include_str!("../src/modes/interactive/theme/dark.json")).unwrap();
    custom["name"] = json!("bar");

    let theme_path = pi_js::path::join(&[&fixture.agent_dir, "themes", "foo.json"]);
    pi_js::fs::write_file(&theme_path, pi_js::json::stringify(&custom).as_bytes(), None).unwrap();

    let available = get_available_themes();
    assert!(available.contains(&"bar".to_string()));
    assert!(!available.contains(&"foo".to_string()));

    let with_paths = get_available_themes_with_paths();
    assert!(with_paths.contains(&ThemeInfo {
        name: "bar".to_string(),
        path: Some(theme_path),
    }));
    assert!(!with_paths.iter().any(|t| t.name == "foo"));
    assert_eq!(with_paths.first().map(|t| t.name.as_str()), Some("system"));
}
