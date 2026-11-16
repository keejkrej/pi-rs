//! Port of packages/durable/test/chat-support.ts
#![allow(dead_code)]

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_ai::types::{AssistantContentBlock, Message, UserContentBlock, UserMessageContent};
use pi_ai::{FauxProviderHandle, FauxResponseStep, Models, RegisterFauxProviderOptions, create_models, faux_provider};
use pi_chord::Context;

use pi_durable::env::ExecutionEnv;
use pi_durable::harness::harness::{Conversation, Harness, RootConversationOptions};
use pi_durable::harness::registry::{RegistryImpl, create_registry};
use pi_durable::harness::types::{
    AgentChange, EnvFactory, FieldChange, HarnessOptions, HarnessSettings, HarnessSettingsSnapshot, ModelRef,
    RegistryReader, ToolRegistration,
};
use pi_durable::types::{EntryRecord, Storage};

use super::session_support::context;
use super::task_support::{Deferred, aborted, deferred};

/// Mutable Harness settings wrapper matching TS object property assignment between decisions.
#[derive(Default)]
pub struct TestHarnessSettings {
    snapshot: Mutex<HarnessSettingsSnapshot>,
}

impl TestHarnessSettings {
    pub fn update<F: FnOnce(&mut HarnessSettingsSnapshot)>(&self, f: F) {
        let mut guard = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard);
    }

    pub fn set(&self, snapshot: HarnessSettingsSnapshot) {
        *self.snapshot.lock().unwrap_or_else(|e| e.into_inner()) = snapshot;
    }
}

impl HarnessSettings for TestHarnessSettings {
    fn get(&self) -> HarnessSettingsSnapshot {
        self.snapshot.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Mutable clock matching `now: () => setup.now()` in TS.
pub struct TestClock {
    func: Mutex<Arc<dyn Fn() -> i64 + Send + Sync>>,
}

impl TestClock {
    pub fn system() -> Self {
        Self {
            func: Mutex::new(Arc::new(pi_js::time::now_ms)),
        }
    }

    pub fn now(&self) -> i64 {
        let func = self.func.lock().unwrap_or_else(|e| e.into_inner()).clone();
        func()
    }

    pub fn set<F: Fn() -> i64 + Send + Sync + 'static>(&self, func: F) {
        *self.func.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(func);
    }

    pub fn set_fixed(&self, ms: i64) {
        self.set(move || ms);
    }
}

/// Models and registry that survive a close/reopen, like a host process's own objects.
pub struct ChatSetup {
    pub faux: FauxProviderHandle,
    pub models: Models,
    pub registry: Arc<RegistryImpl>,
    pub reports: Arc<Mutex<Vec<pi_js::Error>>>,
    pub settings: Arc<TestHarnessSettings>,
    pub now: Arc<TestClock>,
}

pub fn chat_setup(options: Option<RegisterFauxProviderOptions>) -> ChatSetup {
    let faux = faux_provider(options);
    let models = create_models(None);
    models.set_provider(faux.provider());
    ChatSetup {
        faux,
        models,
        registry: create_registry(),
        reports: Arc::new(Mutex::new(Vec::new())),
        settings: Arc::new(TestHarnessSettings::default()),
        now: Arc::new(TestClock::system()),
    }
}

#[derive(Clone, Default)]
pub enum OpenChatEnv {
    #[default]
    None,
    One(Arc<dyn ExecutionEnv>),
    Factory(EnvFactory),
}

pub struct OpenedChat {
    pub harness: Harness,
    pub root: Conversation,
}

/// Open a Harness over `storage` and return its root, configured with the faux model on first creation.
pub async fn open_chat(storage: Arc<dyn Storage>, setup: &ChatSetup, env: OpenChatEnv) -> pi_js::Result<OpenedChat> {
    let env_factory: Option<EnvFactory> = match env {
        OpenChatEnv::None => None,
        OpenChatEnv::One(env_arc) => Some(Arc::new(move |_, _| {
            let env_cloned = Arc::clone(&env_arc);
            Box::pin(async move { Ok(Some(env_cloned)) })
        })),
        OpenChatEnv::Factory(factory) => Some(factory),
    };
    let reports_cb = Arc::clone(&setup.reports);
    let now_clock = Arc::clone(&setup.now);
    let harness = Harness::open(
        storage,
        HarnessOptions {
            models: setup.models.clone(),
            registry: Arc::clone(&setup.registry) as Arc<dyn RegistryReader>,
            settings: Some(Arc::clone(&setup.settings) as Arc<dyn HarnessSettings>),
            env: env_factory,
            conversation_created: None,
            now: Some(Arc::new(move || now_clock.now())),
            on_report: Some(Arc::new(move |err| {
                reports_cb.lock().unwrap_or_else(|e| e.into_inner()).push(err);
            })),
        },
        context(),
    )
    .await?;
    let root = harness
        .root(
            context(),
            Some(RootConversationOptions {
                agent: Some(AgentChange {
                    model: FieldChange::Set(ModelRef {
                        provider: "faux".to_owned(),
                        model_id: "faux-1".to_owned(),
                    }),
                    ..Default::default()
                }),
                init: None,
            }),
        )
        .await?;
    Ok(OpenedChat { harness, root })
}

/// Raw entries of a conversation, oldest first.
pub async fn all_entries(
    conversation: &Conversation,
    call_context: Option<Context>,
) -> pi_js::Result<Vec<EntryRecord>> {
    let mut page = conversation
        .entries(Default::default(), 1000, None, call_context.unwrap_or_else(context))
        .await?;
    page.items.reverse();
    Ok(page.items)
}

/// Text of the first text content of a message.
pub fn text_of(message: &Message) -> Option<&str> {
    match message {
        Message::System(_) => None,
        Message::User(u) => match &u.content {
            UserMessageContent::Text(s) => Some(s.as_str()),
            UserMessageContent::Blocks(blocks) => blocks.iter().find_map(|c| match c {
                UserContentBlock::Text(t) => Some(t.text.as_str()),
                UserContentBlock::Image(_) => None,
            }),
        },
        Message::Assistant(a) => a.content.iter().find_map(|c| match c {
            AssistantContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        }),
        Message::ToolResult(r) => r.content.iter().find_map(|c| match c {
            UserContentBlock::Text(t) => Some(t.text.as_str()),
            UserContentBlock::Image(_) => None,
        }),
    }
}

/// Poll `check` until it holds; works in both real-time and paused-clock tests.
pub async fn wait_for<F, Fut>(mut check: F, timeout_ms: u64)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    while !check().await {
        if tokio::time::Instant::now() > deadline {
            panic!("Condition was not reached");
        }
        pi_js::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Synchronous predicate variant of [`wait_for`].
pub async fn wait_for_sync<F: FnMut() -> bool>(mut check: F, timeout_ms: u64) {
    wait_for(
        || {
            let ok = check();
            std::future::ready(ok)
        },
        timeout_ms,
    )
    .await;
}

pub struct Unanswered {
    pub step: FauxResponseStep,
    pub reached: Deferred<()>,
}

/// Faux response that never answers; the run stays busy until its generation is cancelled.
/// `reached` settles once the request was sent, after the generation's preparation and request commits.
pub fn unanswered() -> Unanswered {
    let reached = deferred::<()>();
    let reached_cb = reached.clone();
    let step = FauxResponseStep::Factory(Arc::new(move |_context, options, _state, _model| {
        reached_cb.resolve(());
        Box::pin(async move {
            let signal = options
                .and_then(|o| o.base.signal)
                .expect("unanswered requires abort signal");
            aborted(&signal).await
        })
    }));
    Unanswered { step, reached }
}

/// The installed tools with these names, as `configure()` takes them.
pub fn tools_named(setup: &ChatSetup, names: &[&str]) -> Vec<ToolRegistration> {
    let installed = setup.registry.snapshot().tools();
    names
        .iter()
        .map(|name| {
            installed
                .iter()
                .find(|entry| entry.tool.name == *name)
                .map(|entry| entry.tool.clone())
                .unwrap_or_else(|| panic!("Tool {name} is not installed"))
        })
        .collect()
}
