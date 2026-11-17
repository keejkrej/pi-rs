//! Port of packages/coding-agent/test/suite/regressions/7193-event-bus-lifecycle.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use pi_coding_agent::core::diagnostics::ResourceDiagnostic;
use pi_coding_agent::core::event_bus::{EventBus, create_event_bus};
use pi_coding_agent::core::extensions::loader::{create_extension_runtime, load_extension_from_factory};
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, InlineExtension, LoadExtensionErrorEntry, LoadExtensionsResult as CoreLoadExtensionsResult,
};
use pi_coding_agent::core::prompt_templates::PromptTemplate;
use pi_coding_agent::core::resource_loader::{
    ContextFile, LoadExtensionsResult, PromptSource, ResourceExtensionPaths, ResourceLoader,
    ResourceLoaderReloadOptions, Theme as LoaderTheme,
};
use pi_coding_agent::core::skills::Skill;
use pi_js::BoxFuture;
use serde_json::Value;
use support::suite::harness::{HarnessOptions, create_harness};

struct ReloadableExtensionLoader {
    cwd: String,
    event_bus: Arc<dyn EventBus>,
    factory: InlineExtension,
    current: Arc<Mutex<CoreLoadExtensionsResult>>,
}

impl ReloadableExtensionLoader {
    async fn load_once(cwd: &str, event_bus: Arc<dyn EventBus>, factory: &InlineExtension) -> CoreLoadExtensionsResult {
        let runtime = create_extension_runtime();
        let (path, ext_factory) = match factory {
            InlineExtension::Factory(f) => ("<inline:1>".to_string(), Arc::clone(f)),
            InlineExtension::Named(n) => (format!("<inline:{}>", n.name), Arc::clone(&n.factory)),
        };
        let mut extensions = Vec::new();
        let mut errors = Vec::new();
        match load_extension_from_factory(ext_factory, cwd, event_bus, runtime.clone(), Some(&path)).await {
            Ok(ext) => extensions.push(ext),
            Err(err) => errors.push(LoadExtensionErrorEntry {
                path,
                error: err.to_string(),
            }),
        }
        CoreLoadExtensionsResult {
            extensions,
            errors,
            warnings: Vec::new(),
            runtime,
        }
    }
}

impl ResourceLoader for ReloadableExtensionLoader {
    fn get_extensions(&self) -> LoadExtensionsResult {
        LoadExtensionsResult::default()
    }

    fn get_core_extensions(&self) -> CoreLoadExtensionsResult {
        self.current.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn get_skills(&self) -> (Vec<Skill>, Vec<ResourceDiagnostic>) {
        (Vec::new(), Vec::new())
    }

    fn get_prompts(&self) -> (Vec<PromptTemplate>, Vec<ResourceDiagnostic>) {
        (Vec::new(), Vec::new())
    }

    fn get_themes(&self) -> (Vec<LoaderTheme>, Vec<ResourceDiagnostic>) {
        (Vec::new(), Vec::new())
    }

    fn get_agents_files(&self) -> Vec<ContextFile> {
        Vec::new()
    }

    fn get_system_prompt(&self) -> Option<String> {
        None
    }

    fn get_system_prompt_source(&self) -> Option<PromptSource> {
        None
    }

    fn get_append_system_prompt(&self) -> Vec<String> {
        Vec::new()
    }

    fn get_append_system_prompt_sources(&self) -> Vec<PromptSource> {
        Vec::new()
    }

    fn extend_resources(&self, _paths: ResourceExtensionPaths) {}

    fn reload(&self, _options: Option<ResourceLoaderReloadOptions>) -> BoxFuture<pi_coding_agent::Result<()>> {
        let cwd = self.cwd.clone();
        let bus = Arc::clone(&self.event_bus);
        let factory = self.factory.clone();
        let current = Arc::clone(&self.current);
        Box::pin(async move {
            let next = Self::load_once(&cwd, bus, &factory).await;
            *current.lock().unwrap_or_else(PoisonError::into_inner) = next;
            Ok(())
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn removes_extension_owned_event_bus_listeners_on_reload_and_dispose() {
    let event_bus: Arc<dyn EventBus> = create_event_bus();
    let extension_calls = Arc::new(AtomicUsize::new(0));
    let host_calls = Arc::new(AtomicUsize::new(0));

    let host_counter = Arc::clone(&host_calls);
    let _host_unsub = event_bus.on_sync(
        "reload:test",
        Arc::new(move |_| {
            host_counter.fetch_add(1, Ordering::SeqCst);
        }),
    );

    let first_api: Arc<Mutex<Option<ExtensionApi>>> = Arc::new(Mutex::new(None));
    let first_api_c = Arc::clone(&first_api);
    let ext_counter = Arc::clone(&extension_calls);
    let factory = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let first_api_c = Arc::clone(&first_api_c);
        let ext_counter = Arc::clone(&ext_counter);
        Box::pin(async move {
            let mut guard = first_api_c.lock().unwrap_or_else(PoisonError::into_inner);
            if guard.is_none() {
                *guard = Some(pi.clone());
            }
            drop(guard);
            let _unsub = pi.events().on_sync(
                "reload:test",
                Arc::new(move |_| {
                    ext_counter.fetch_add(1, Ordering::SeqCst);
                }),
            );
            Ok(())
        })
    }));

    let cwd = std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .to_string_lossy()
        .into_owned();
    let initial = ReloadableExtensionLoader::load_once(&cwd, Arc::clone(&event_bus), &factory).await;
    let loader = Arc::new(ReloadableExtensionLoader {
        cwd,
        event_bus: Arc::clone(&event_bus),
        factory,
        current: Arc::new(Mutex::new(initial)),
    });

    let harness = create_harness(HarnessOptions {
        resource_loader: Some(loader),
        ..Default::default()
    })
    .await;
    harness
        .session
        .bind_extensions(Default::default())
        .await
        .expect("bind extensions");

    let emit = || async {
        let ext_before = extension_calls.load(Ordering::SeqCst);
        let host_before = host_calls.load(Ordering::SeqCst);
        event_bus.emit("reload:test", Value::Null);
        tokio::task::yield_now().await;
        (
            extension_calls.load(Ordering::SeqCst) - ext_before,
            host_calls.load(Ordering::SeqCst) - host_before,
        )
    };

    assert_eq!(emit().await, (1, 1));
    harness.session.reload(None).await.expect("first reload");
    let captured_first = first_api
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .expect("captured first api");
    let stale_err = captured_first
        .get_commands()
        .expect_err("firstApi.get_commands should error after reload")
        .to_string();
    assert!(
        stale_err.contains("stale after session replacement or reload"),
        "unexpected error: {stale_err}"
    );
    assert_eq!(emit().await, (1, 1));

    harness.session.reload(None).await.expect("second reload");
    assert_eq!(emit().await, (1, 1));

    harness.session.dispose();
    assert_eq!(emit().await, (0, 1));

    harness.cleanup();
}
