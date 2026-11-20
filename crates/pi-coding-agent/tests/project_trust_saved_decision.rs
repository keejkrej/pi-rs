//! Port of packages/coding-agent/test/project-trust-saved-decision.test.ts

use std::sync::{Arc, Mutex};

use pi_coding_agent::cli::project_trust::{CreateProjectTrustContextOptions, create_project_trust_runtime_context};
use pi_coding_agent::core::agent_session::ExtensionBindings;
use pi_coding_agent::core::extensions::types::{ExtensionApi, InlineExtension, SessionStartEvent};
use pi_coding_agent::core::project_trust::{AppMode, ResolveProjectTrustedOptions, resolve_project_trusted};
use pi_coding_agent::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::{SettingsManager, SettingsManagerCreateOptions};
use pi_coding_agent::core::trust_manager::ProjectTrustStore;
use pi_js::env::testing::EnvGuard;
use serial_test::serial;

async fn probe_project_trust(agent_dir: &str, project_dir: &str) -> bool {
    let trust_store = ProjectTrustStore::new(agent_dir);
    let trust_ctx = create_project_trust_runtime_context(CreateProjectTrustContextOptions {
        cwd: project_dir.to_string(),
        mode: AppMode::Print,
        settings_manager: SettingsManager::in_memory(None, None),
        has_ui: false,
    });
    let trusted = resolve_project_trusted(ResolveProjectTrustedOptions {
        cwd: project_dir,
        trust_store: &trust_store,
        trust_override: None,
        default_project_trust: None,
        extension_host: None,
        project_trust_context: &*trust_ctx,
        on_extension_error: None,
    })
    .await;

    let settings_manager = SettingsManager::create(
        project_dir,
        Some(agent_dir),
        Some(SettingsManagerCreateOptions {
            project_trusted: Some(trusted),
        }),
    );

    let seen = Arc::new(Mutex::new(None::<bool>));
    let seen_c = Arc::clone(&seen);
    let inline = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let seen_c = Arc::clone(&seen_c);
        Box::pin(async move {
            pi.on::<SessionStartEvent>(move |_ev, ctx| {
                let seen_c = Arc::clone(&seen_c);
                Box::pin(async move {
                    *seen_c.lock().unwrap_or_else(|e| e.into_inner()) = ctx.is_project_trusted().ok();
                    Ok(None)
                })
            });
            Ok(())
        })
    }));

    let loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: project_dir.to_string(),
        agent_dir: agent_dir.to_string(),
        settings_manager: Some(settings_manager.clone()),
        extension_factories: vec![inline.into()],
        no_extensions: true,
        no_skills: true,
        no_prompt_templates: true,
        no_themes: true,
        ..Default::default()
    }));
    loader.reload(None).await.expect("reload");

    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(project_dir.to_string()),
        agent_dir: Some(agent_dir.to_string()),
        settings_manager: Some(settings_manager),
        session_manager: Some(SessionManager::in_memory(Some(project_dir), None, None).expect("session manager")),
        resource_loader: Some(loader),
        ..Default::default()
    }))
    .await
    .expect("create agent session");

    created
        .session
        .bind_extensions(ExtensionBindings::default())
        .await
        .expect("bind extensions");
    created.session.dispose();
    seen.lock()
        .unwrap_or_else(|e| e.into_inner())
        .expect("session_start ran")
}

// #10947
#[tokio::test]
#[serial]
async fn reports_saved_do_not_trust_decision_to_extensions() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(temp.path()).expect("canonical root");
    let agent_dir = root.join("agent");
    let project_dir = root.join("project");
    std::fs::create_dir_all(&agent_dir).expect("mkdir agent");
    std::fs::create_dir_all(&project_dir).expect("mkdir project");
    std::fs::write(project_dir.join("AGENTS.md"), "# untrusted\n").expect("write AGENTS.md");

    let agent_str = agent_dir.to_str().expect("utf8");
    let project_str = project_dir.to_str().expect("utf8");
    let _agent_env = EnvGuard::set("PI_CODING_AGENT_DIR", Some(agent_str));
    let _offline_env = EnvGuard::set("PI_OFFLINE", Some("1"));

    ProjectTrustStore::new(agent_str)
        .set(project_str, Some(false))
        .expect("save untrusted decision");
    assert!(!probe_project_trust(agent_str, project_str).await);
}

#[tokio::test]
#[serial]
async fn auto_trusts_when_no_decision_is_saved() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(temp.path()).expect("canonical root");
    let agent_dir = root.join("agent");
    let project_dir = root.join("project");
    std::fs::create_dir_all(&agent_dir).expect("mkdir agent");
    std::fs::create_dir_all(&project_dir).expect("mkdir project");
    std::fs::write(project_dir.join("AGENTS.md"), "# untrusted\n").expect("write AGENTS.md");

    let agent_str = agent_dir.to_str().expect("utf8");
    let project_str = project_dir.to_str().expect("utf8");
    let _agent_env = EnvGuard::set("PI_CODING_AGENT_DIR", Some(agent_str));
    let _offline_env = EnvGuard::set("PI_OFFLINE", Some("1"));

    assert!(probe_project_trust(agent_str, project_str).await);
}

#[tokio::test]
async fn resolve_project_trusted_honors_saved_decision_before_auto_trust() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(temp.path()).expect("canonical root");
    let agent_dir = root.join("agent");
    let project_dir = root.join("project");
    std::fs::create_dir_all(&agent_dir).expect("mkdir agent");
    std::fs::create_dir_all(&project_dir).expect("mkdir project");
    std::fs::write(project_dir.join("AGENTS.md"), "# untrusted\n").expect("write AGENTS.md");

    let agent_str = agent_dir.to_str().expect("utf8");
    let project_str = project_dir.to_str().expect("utf8");
    let trust_store = ProjectTrustStore::new(agent_str);
    let trust_ctx = create_project_trust_runtime_context(CreateProjectTrustContextOptions {
        cwd: project_str.to_string(),
        mode: AppMode::Print,
        settings_manager: SettingsManager::in_memory(None, None),
        has_ui: false,
    });

    let resolve = || {
        resolve_project_trusted(ResolveProjectTrustedOptions {
            cwd: project_str,
            trust_store: &trust_store,
            trust_override: None,
            default_project_trust: None,
            extension_host: None,
            project_trust_context: &*trust_ctx,
            on_extension_error: None,
        })
    };

    assert!(resolve().await);
    trust_store.set(project_str, Some(false)).expect("set false");
    assert!(!resolve().await);
    trust_store.set(project_str, Some(true)).expect("set true");
    assert!(resolve().await);
}
