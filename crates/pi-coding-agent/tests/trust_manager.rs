//! Port of packages/coding-agent/test/trust-manager.test.ts

use indexmap::IndexMap;
use pi_coding_agent::core::trust_manager::{ProjectTrustStore, has_trust_requiring_project_resources};
use pi_js::env::testing::{EnvGuard, PlatformGuard};
use tempfile::TempDir;

#[test]
fn stores_decisions_and_inherits_from_parent_directories() {
    let temp = TempDir::new().expect("temp dir");
    let agent_dir = temp.path().join("agent");
    let parent_dir = temp.path().join("trusted-parent");
    let child_dir = parent_dir.join("project");
    std::fs::create_dir_all(&agent_dir).expect("mkdir agent");
    std::fs::create_dir_all(&child_dir).expect("mkdir child");

    let store = ProjectTrustStore::new(agent_dir.to_str().expect("utf8"));
    let parent_str = parent_dir.to_str().expect("utf8");
    let child_str = child_dir.to_str().expect("utf8");

    assert_eq!(store.get(child_str).expect("get initial"), None);
    store.set(parent_str, Some(true)).expect("set parent true");
    assert_eq!(store.get(child_str).expect("inherit parent true"), Some(true));
    store.set(child_str, Some(false)).expect("set child false");
    assert_eq!(store.get(child_str).expect("override child false"), Some(false));
    store.set(child_str, None).expect("clear child decision");
    assert_eq!(store.get(child_str).expect("inherit parent true again"), Some(true));
}

#[test]
fn detects_trust_requiring_project_resources() {
    let temp = TempDir::new().expect("temp dir");
    let temp_str = temp.path().to_str().expect("utf8");
    let cwd = temp.path().join("project");
    std::fs::create_dir_all(temp.path().join(".pi").join("agent")).expect("mkdir .pi/agent");
    std::fs::create_dir_all(temp.path().join(".agents").join("skills")).expect("mkdir .agents/skills");
    std::fs::create_dir_all(&cwd).expect("mkdir cwd");

    let _home = EnvGuard::set("HOME", Some(temp_str));
    let cwd_str = cwd.to_str().expect("utf8");

    assert!(!has_trust_requiring_project_resources(temp_str));
    assert!(!has_trust_requiring_project_resources(cwd_str));

    std::fs::write(temp.path().join(".pi").join("settings.json"), "{}").expect("write settings.json");
    assert!(has_trust_requiring_project_resources(temp_str));
    std::fs::remove_file(temp.path().join(".pi").join("settings.json")).expect("rm settings.json");

    std::fs::create_dir_all(cwd.join(".pi")).expect("mkdir cwd/.pi");
    std::fs::write(cwd.join(".pi").join("settings.json"), "{}").expect("write cwd settings");
    assert!(has_trust_requiring_project_resources(cwd_str));

    std::fs::remove_dir_all(cwd.join(".pi")).expect("rm cwd/.pi");
    std::fs::create_dir_all(cwd.join(".agents").join("skills")).expect("mkdir cwd/.agents/skills");
    assert!(has_trust_requiring_project_resources(cwd_str));
}

#[test]
fn ignores_user_agents_skills_dir_when_home_differs_only_by_case_on_windows() {
    let temp = TempDir::new().expect("temp dir");
    let mixed_case_home = temp.path().join("Home");
    let project_dir = mixed_case_home.join("project");
    std::fs::create_dir_all(mixed_case_home.join(".agents").join("skills")).expect("mkdir skills");
    std::fs::create_dir_all(&project_dir).expect("mkdir project");

    let lower_home = mixed_case_home.to_str().expect("utf8").to_lowercase();
    let _home = EnvGuard::set("HOME", Some(&lower_home));
    let _plat = PlatformGuard::set("win32");

    assert!(!has_trust_requiring_project_resources(
        project_dir.to_str().expect("utf8")
    ));
}

#[test]
fn matches_saved_decisions_regardless_of_casing_on_windows() {
    let temp = TempDir::new().expect("temp dir");
    let agent_dir = temp.path().join("agent");
    let project_dir = temp.path().join("Project-Mixed");
    std::fs::create_dir_all(&agent_dir).expect("mkdir agent");
    std::fs::create_dir_all(&project_dir).expect("mkdir project");

    let store = ProjectTrustStore::new(agent_dir.to_str().expect("utf8"));
    let project_str = project_dir.to_str().expect("utf8");
    let project_lower = project_str.to_lowercase();

    store.set(project_str, Some(true)).expect("set true");
    let _plat = PlatformGuard::set("win32");

    assert_eq!(store.get(&project_lower).expect("get lowercase"), Some(true));
    store.set(&project_lower, Some(false)).expect("set lowercase false");

    let saved: IndexMap<String, bool> =
        pi_js::json::parse_as(&std::fs::read_to_string(agent_dir.join("trust.json")).expect("read trust.json"))
            .expect("parse trust.json");
    assert_eq!(saved.values().copied().collect::<Vec<_>>(), vec![false]);
    assert_eq!(store.get(project_str).expect("get original case"), Some(false));
}
