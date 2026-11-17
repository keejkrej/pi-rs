//! Port of packages/coding-agent/test/theme-export.test.ts

use pi_coding_agent::modes::interactive::theme::theme::{ThemeExportColors, get_theme_export_colors};
use pi_js::env::testing::EnvGuard;
use pi_tui::{Color, color_to_hex, okhsl_color};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use serial_test::serial;

struct ExportFixture {
    temp_root: String,
    _env: EnvGuard,
}

impl ExportFixture {
    fn new() -> Self {
        let temp_root = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-theme-export-"])).unwrap();
        let agent_dir = pi_js::path::join(&[&temp_root, "agent"]);
        let env = EnvGuard::set("PI_CODING_AGENT_DIR", Some(&agent_dir));
        pi_js::fs::mkdir(&pi_js::path::join(&[&agent_dir, "themes"]), true, None).unwrap();
        Self { temp_root, _env: env }
    }

    fn write_theme(&self, file_name: &str, value: &Value) {
        let agent_dir = pi_js::env::var("PI_CODING_AGENT_DIR").unwrap();
        let path = pi_js::path::join(&[&agent_dir, "themes", file_name]);
        pi_js::fs::write_file(&path, pi_js::json::stringify(value).as_bytes(), None).unwrap();
    }
}

impl Drop for ExportFixture {
    fn drop(&mut self) {
        let _ = pi_js::fs::rm(&self.temp_root, true, true);
    }
}

fn dark_theme_json() -> Value {
    serde_json::from_str(include_str!("../src/modes/interactive/theme/dark.json")).unwrap()
}

#[test]
#[serial]
fn resolves_export_variable_references_using_the_same_syntax_as_colors() {
    let fixture = ExportFixture::new();
    let mut custom = dark_theme_json();
    custom["name"] = json!("custom-export-vars");
    let vars = custom["vars"].as_object_mut().unwrap();
    vars.insert("pageBgVar".to_string(), json!("#112233"));
    vars.insert("pageBgAlias".to_string(), json!("pageBgVar"));
    vars.insert("infoBgVar".to_string(), json!("#445566"));
    vars.insert("cardBgVar".to_string(), json!("#223344"));
    custom["export"] = json!({
        "pageBg": "pageBgAlias",
        "cardBg": "cardBgVar",
        "infoBg": "infoBgVar",
    });

    fixture.write_theme("custom-export-vars.json", &custom);

    assert_eq!(
        get_theme_export_colors(Some("custom-export-vars")),
        ThemeExportColors {
            page_bg: Some("#112233".to_string()),
            card_bg: Some("#223344".to_string()),
            info_bg: Some("#445566".to_string()),
        }
    );
}

#[test]
#[serial]
fn converts_okhsl_export_colors_to_hex_because_css_does_not_support_them() {
    let fixture = ExportFixture::new();
    let mut custom = dark_theme_json();
    custom["name"] = json!("custom-export-okhsl");
    custom["vars"] = json!({ "card": "okhsl(250 20% 20%)" });
    custom["export"] = json!({
        "pageBg": "okhsl(250 20% 15%)",
        "cardBg": "card",
        "infoBg": "oklch(30% 0.05 80)",
    });

    fixture.write_theme("custom-export-okhsl.json", &custom);

    assert_eq!(
        get_theme_export_colors(Some("custom-export-okhsl")),
        ThemeExportColors {
            page_bg: Some(color_to_hex(Color::Rgb(okhsl_color(250.0, 0.2, 0.15).unwrap()))),
            card_bg: Some(color_to_hex(Color::Rgb(okhsl_color(250.0, 0.2, 0.2).unwrap()))),
            info_bg: Some("oklch(30% 0.05 80)".to_string()),
        }
    );
}

#[test]
#[serial]
fn resolves_recursive_vars_and_converts_256_color_export_values_to_hex() {
    let fixture = ExportFixture::new();
    let mut custom = dark_theme_json();
    custom["name"] = json!("custom-export-recursive");
    let vars = custom["vars"].as_object_mut().unwrap();
    vars.insert("deepPageBg".to_string(), json!("#abcdef"));
    vars.insert("pageBgAlias".to_string(), json!("deepPageBg"));
    vars.insert("cardBgAnsi".to_string(), json!(24));
    custom["export"] = json!({
        "pageBg": "pageBgAlias",
        "cardBg": "cardBgAnsi",
        "infoBg": "",
    });

    fixture.write_theme("custom-export-recursive.json", &custom);

    assert_eq!(
        get_theme_export_colors(Some("custom-export-recursive")),
        ThemeExportColors {
            page_bg: Some("#abcdef".to_string()),
            card_bg: Some("#005f87".to_string()),
            info_bg: None,
        }
    );
}
