//! Port of packages/coding-agent/test/auth-login.test.ts

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthEvent, AuthPrompt, AuthResult, AuthSelectOption, Credential,
    CredentialStore, LoginOptions, OAuthAuth, OAuthCredential, ProviderAuth, ProviderAuthInteraction,
};
use pi_ai::models::{Provider, RefreshModelsContext};
use pi_ai::models_store::InMemoryModelsStore;
use pi_ai::types::{ApiStreamOptions, Model, ModelCost, ModelInput, SimpleStreamOptions, TranscriptContext};
use pi_ai::{AssistantMessageEventStream, Error, ModelAuth, Result};
use pi_coding_agent::cli::auth_login::{AuthLoginOptions, run_auth_login};
use pi_coding_agent::cli::auth_terminal::{AuthOutput, LoginTerminal};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_js::BoxFuture;
use pi_js::abort::{AbortController, AbortSignal};
use pi_js::time::now_ms;
use serde_json::{Value, json};
use tempfile::TempDir;

const PROVIDER_ID: &str = "login-test";
const USAGE: &str = "Usage: pi auth login <provider> [--api-key] [--no-browser]";

struct ScriptedTerminal {
    interactive: bool,
    answers: Mutex<VecDeque<String>>,
    lines: Mutex<Vec<String>>,
    errors: Mutex<Vec<String>>,
    questions: Mutex<Vec<String>>,
    opened: Mutex<Vec<String>>,
    wait_for_abort_instead: bool,
}

impl ScriptedTerminal {
    fn new(answers: &[&str], interactive: bool) -> Arc<Self> {
        Arc::new(Self {
            interactive,
            answers: Mutex::new(answers.iter().map(|s| (*s).to_owned()).collect()),
            lines: Mutex::new(Vec::new()),
            errors: Mutex::new(Vec::new()),
            questions: Mutex::new(Vec::new()),
            opened: Mutex::new(Vec::new()),
            wait_for_abort_instead: false,
        })
    }

    fn abort_waiter() -> Arc<Self> {
        Arc::new(Self {
            interactive: true,
            answers: Mutex::new(VecDeque::new()),
            lines: Mutex::new(Vec::new()),
            errors: Mutex::new(Vec::new()),
            questions: Mutex::new(Vec::new()),
            opened: Mutex::new(Vec::new()),
            wait_for_abort_instead: true,
        })
    }

    fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    fn errors(&self) -> Vec<String> {
        self.errors.lock().unwrap().clone()
    }

    fn questions(&self) -> Vec<String> {
        self.questions.lock().unwrap().clone()
    }

    fn opened(&self) -> Vec<String> {
        self.opened.lock().unwrap().clone()
    }
}

impl AuthOutput for ScriptedTerminal {
    fn write(&self, line: &str) {
        self.lines.lock().unwrap().push(line.to_owned());
    }

    fn error(&self, line: &str) {
        self.errors.lock().unwrap().push(line.to_owned());
    }
}

#[async_trait]
impl LoginTerminal for ScriptedTerminal {
    fn interactive(&self) -> bool {
        self.interactive
    }

    async fn question(&self, message: &str, signal: &AbortSignal) -> Result<String> {
        self.questions.lock().unwrap().push(message.to_owned());
        if self.wait_for_abort_instead {
            while !signal.aborted() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            return Err(Error::msg("prompt aborted"));
        }
        if signal.aborted() {
            return Err(Error::msg("Login cancelled"));
        }
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| Error::msg(format!("Unexpected prompt: {message}")))
    }

    async fn secret(&self, message: &str, signal: &AbortSignal) -> Result<String> {
        self.question(message, signal).await
    }

    fn open_url(&self, url: &str) {
        self.opened.lock().unwrap().push(url.to_owned());
    }
}

fn test_model(provider_id: &str) -> Model {
    Model {
        r#type: None,
        id: "test-model".to_owned(),
        name: "test-model".to_owned(),
        api: "openai-completions".to_owned(),
        provider: provider_id.to_owned(),
        base_url: "https://example.test/v1".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 1000,
        max_tokens: 100,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

type OAuthLoginFn =
    Arc<dyn Fn(ProviderAuthInteraction, Option<LoginOptions>) -> BoxFuture<Result<OAuthCredential>> + Send + Sync>;
type ApiKeyLoginFn = Arc<dyn Fn(ProviderAuthInteraction) -> BoxFuture<Result<ApiKeyCredential>> + Send + Sync>;
type RefreshHook = Arc<dyn Fn(RefreshModelsContext) -> BoxFuture<Result<()>> + Send + Sync>;

struct CustomOAuth {
    login_fn: OAuthLoginFn,
}

#[async_trait]
impl OAuthAuth for CustomOAuth {
    fn name(&self) -> &str {
        "Login Test account"
    }

    async fn login(
        &self,
        interaction: ProviderAuthInteraction,
        options: Option<LoginOptions>,
    ) -> Result<OAuthCredential> {
        (self.login_fn)(interaction, options).await
    }

    async fn refresh(&self, cred: OAuthCredential, _sig: AbortSignal) -> Result<OAuthCredential> {
        Ok(cred)
    }

    async fn to_auth(&self, cred: OAuthCredential) -> Result<ModelAuth> {
        Ok(ModelAuth {
            api_key: Some(cred.access),
            headers: None,
            base_url: None,
        })
    }
}

struct CustomApiKey {
    name: String,
    supports_login: bool,
    login_fn: Option<ApiKeyLoginFn>,
}

#[async_trait]
impl ApiKeyAuth for CustomApiKey {
    fn name(&self) -> &str {
        &self.name
    }

    fn supports_login(&self) -> bool {
        self.supports_login
    }

    async fn login(&self, interaction: ProviderAuthInteraction) -> Result<ApiKeyCredential> {
        if let Some(ref f) = self.login_fn {
            f(interaction).await
        } else {
            Ok(ApiKeyCredential {
                key: Some(
                    interaction
                        .prompt(AuthPrompt::secret("Enter Login Test API key", None))
                        .await?,
                ),
                env: None,
            })
        }
    }

    async fn resolve(&self, input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        Ok(input.credential.and_then(|c| c.key).map(|k| AuthResult {
            auth: ModelAuth {
                api_key: Some(k),
                headers: None,
                base_url: None,
            },
            env: None,
            source: None,
        }))
    }
}

struct TestProvider {
    id: String,
    name: String,
    auth: ProviderAuth,
    refresh_hook: Option<RefreshHook>,
}

impl Provider for TestProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        vec![test_model(&self.id)]
    }

    fn refresh_models(&self, context: RefreshModelsContext) -> Option<BoxFuture<Result<()>>> {
        self.refresh_hook.as_ref().map(|hook| hook(context))
    }

    fn stream(&self, _m: &Model, _c: &TranscriptContext, _o: Option<ApiStreamOptions>) -> AssistantMessageEventStream {
        panic!("unused")
    }

    fn stream_simple(
        &self,
        _m: &Model,
        _c: &TranscriptContext,
        _o: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        panic!("unused")
    }
}

fn default_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(CustomOAuth {
        login_fn: Arc::new(|interaction, _| {
            Box::pin(async move {
                let method = interaction
                    .prompt(AuthPrompt::select(
                        "Sign in to Login Test:",
                        vec![
                            AuthSelectOption {
                                id: "browser".to_owned(),
                                label: "Sign in with browser".to_owned(),
                                description: None,
                            },
                            AuthSelectOption {
                                id: "device".to_owned(),
                                label: "Sign in with device code".to_owned(),
                                description: None,
                            },
                        ],
                    ))
                    .await?;
                if method == "device" {
                    interaction.notify(AuthEvent::DeviceCode {
                        user_code: "ABCD-1234".to_owned(),
                        verification_uri: "https://example.test/device".to_owned(),
                        open_browser_on_confirm: None,
                        copy_code_on_confirm: None,
                        interval_seconds: None,
                        expires_in_seconds: None,
                    });
                    return Ok(OAuthCredential::new("refresh", "access-device", now_ms() + 3_600_000));
                }
                interaction.notify(AuthEvent::AuthUrl {
                    url: "https://example.test/authorize".to_owned(),
                    instructions: Some("Finish in the browser".to_owned()),
                });
                let code = interaction
                    .prompt(AuthPrompt::manual_code("Paste the code", None))
                    .await?;
                Ok(OAuthCredential::new(
                    "refresh",
                    format!("access-{code}"),
                    now_ms() + 3_600_000,
                ))
            })
        }),
    })
}

fn default_api_key() -> Arc<dyn ApiKeyAuth> {
    Arc::new(CustomApiKey {
        name: "Login Test API key".to_owned(),
        supports_login: true,
        login_fn: None,
    })
}

fn both_methods_auth() -> ProviderAuth {
    ProviderAuth {
        oauth: Some(default_oauth()),
        api_key: Some(default_api_key()),
    }
}

async fn setup_with_network(
    auth: ProviderAuth,
    provider_id: &str,
    allow_model_network: bool,
    refresh_hook: Option<RefreshHook>,
) -> (Arc<AuthStorage>, ModelRuntime) {
    let credentials = Arc::new(AuthStorage::in_memory(None));
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(Arc::clone(&credentials) as Arc<dyn CredentialStore + Send + Sync>),
        models_path: Some(None),
        models_store: Some(Arc::new(InMemoryModelsStore::new())),
        allow_model_network: Some(allow_model_network),
        ..Default::default()
    })
    .await
    .unwrap();
    runtime
        .register_native_provider(Arc::new(TestProvider {
            id: provider_id.to_owned(),
            name: "Login Test".to_owned(),
            auth,
            refresh_hook,
        }))
        .unwrap();
    (credentials, runtime)
}

async fn setup(
    auth: ProviderAuth,
    provider_id: &str,
    refresh_hook: Option<RefreshHook>,
) -> (Arc<AuthStorage>, ModelRuntime) {
    setup_with_network(auth, provider_id, false, refresh_hook).await
}

async fn run_cmd(
    args: &[&str],
    runtime: &ModelRuntime,
    terminal: Arc<dyn LoginTerminal>,
    signal: AbortSignal,
    agent_dir: Option<&str>,
) -> i32 {
    run_auth_login(
        args,
        AuthLoginOptions {
            model_runtime: runtime,
            terminal,
            agent_dir: agent_dir.unwrap_or("/nonexistent-agent-dir"),
            cwd: "/nonexistent-cwd",
            get_device_id: Arc::new(|| "device".to_owned()),
            signal,
        },
    )
    .await
}

#[tokio::test]
async fn requires_exact_provider_and_known_options() {
    let (_creds, runtime) = setup(both_methods_auth(), PROVIDER_ID, None).await;
    for args in [
        vec![],
        vec!["--api-key"],
        vec!["--provider", PROVIDER_ID],
        vec![PROVIDER_ID, "extra"],
    ] {
        let term = ScriptedTerminal::new(&[], true);
        let code = run_cmd(
            &args,
            &runtime,
            Arc::clone(&term) as Arc<dyn LoginTerminal>,
            AbortController::new().signal(),
            None,
        )
        .await;
        assert_eq!(code, 1);
        assert_eq!(term.errors(), vec![USAGE]);
    }
}

#[tokio::test]
async fn rejects_no_browser_with_api_key() {
    let (_creds, runtime) = setup(both_methods_auth(), PROVIDER_ID, None).await;
    let term = ScriptedTerminal::new(&[], true);
    let code = run_cmd(
        &[PROVIDER_ID, "--api-key", "--no-browser"],
        &runtime,
        Arc::clone(&term) as Arc<dyn LoginTerminal>,
        AbortController::new().signal(),
        None,
    )
    .await;
    assert_eq!(code, 1);
    assert_eq!(
        term.errors(),
        vec!["--no-browser only applies to account sign-in; omit it with --api-key."]
    );
}

#[tokio::test]
async fn rejects_unknown_provider_and_lists_login_providers() {
    let (_creds, runtime) = setup(both_methods_auth(), PROVIDER_ID, None).await;
    let term = ScriptedTerminal::new(&[], true);
    let code = run_cmd(
        &["nope"],
        &runtime,
        Arc::clone(&term) as Arc<dyn LoginTerminal>,
        AbortController::new().signal(),
        None,
    )
    .await;
    assert_eq!(code, 1);
    let errors = term.errors();
    assert_eq!(errors[0], "Unknown provider \"nope\".");
    assert!(errors[1].contains(PROVIDER_ID));
}

#[tokio::test]
async fn requires_interactive_terminal_after_arg_checks() {
    let (_creds, runtime) = setup(both_methods_auth(), PROVIDER_ID, None).await;
    let term = ScriptedTerminal::new(&[], false);
    let code = run_cmd(
        &[PROVIDER_ID],
        &runtime,
        Arc::clone(&term) as Arc<dyn LoginTerminal>,
        AbortController::new().signal(),
        None,
    )
    .await;
    assert_eq!(code, 1);
    assert_eq!(
        term.errors(),
        vec!["\"pi auth login <provider> [--api-key] [--no-browser]\" requires an interactive terminal"]
    );
}

#[tokio::test]
async fn signs_in_with_account_in_browser_without_asking_for_method() {
    let (creds, runtime) = setup(both_methods_auth(), PROVIDER_ID, None).await;
    let term = ScriptedTerminal::new(&["the-code"], true);
    let code = run_cmd(
        &["login test"],
        &runtime,
        Arc::clone(&term) as Arc<dyn LoginTerminal>,
        AbortController::new().signal(),
        None,
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(term.questions(), vec!["Paste the code: "]);
    let lines = term.lines();
    assert!(lines.contains(&"Open this URL to sign in:\n  https://example.test/authorize".to_owned()));
    assert!(lines.contains(&"Finish in the browser".to_owned()));
    assert_eq!(term.opened(), vec!["https://example.test/authorize"]);
    assert!(lines.contains(
        &"To use \"Sign in with device code\" instead, run \"pi auth login login-test --no-browser\"".to_owned()
    ));
    assert!(matches!(
        creds.read(PROVIDER_ID, None).await.unwrap(),
        Some(Credential::OAuth(ref o)) if o.access == "access-the-code"
    ));
    assert!(lines.iter().any(|l| l.starts_with("Logged in to Login Test.")));
}

#[tokio::test]
async fn lists_several_other_sign_in_methods() {
    let auth = ProviderAuth {
        oauth: Some(Arc::new(CustomOAuth {
            login_fn: Arc::new(|interaction, _| {
                Box::pin(async move {
                    let _ = interaction
                        .prompt(AuthPrompt::select(
                            "Sign in to Login Test:",
                            vec![
                                AuthSelectOption {
                                    id: "browser".to_owned(),
                                    label: "Browser".to_owned(),
                                    description: None,
                                },
                                AuthSelectOption {
                                    id: "device".to_owned(),
                                    label: "Device code".to_owned(),
                                    description: None,
                                },
                                AuthSelectOption {
                                    id: "copy".to_owned(),
                                    label: "Copy code".to_owned(),
                                    description: None,
                                },
                            ],
                        ))
                        .await?;
                    interaction.notify(AuthEvent::AuthUrl {
                        url: "https://example.test/authorize".to_owned(),
                        instructions: None,
                    });
                    Ok(OAuthCredential::new("refresh", "access", now_ms() + 3_600_000))
                })
            }),
        })),
        api_key: Some(default_api_key()),
    };
    let (_creds, runtime) = setup(auth, PROVIDER_ID, None).await;
    let term = ScriptedTerminal::new(&[], true);
    let code = run_cmd(
        &[PROVIDER_ID],
        &runtime,
        Arc::clone(&term) as Arc<dyn LoginTerminal>,
        AbortController::new().signal(),
        None,
    )
    .await;
    assert_eq!(code, 0);
    assert!(
        term.lines().contains(
            &"Other sign-in methods: \"Device code\", \"Copy code\". Run \"pi auth login login-test --no-browser\" to choose one.".to_owned()
        )
    );
}

#[tokio::test]
async fn offers_no_other_methods_when_provider_has_no_method_choice() {
    let auth = ProviderAuth {
        oauth: Some(Arc::new(CustomOAuth {
            login_fn: Arc::new(|interaction, _| {
                Box::pin(async move {
                    interaction.notify(AuthEvent::AuthUrl {
                        url: "https://example.test/authorize".to_owned(),
                        instructions: None,
                    });
                    let code = interaction
                        .prompt(AuthPrompt::manual_code("Paste the code", None))
                        .await?;
                    Ok(OAuthCredential::new(
                        "refresh",
                        format!("access-{code}"),
                        now_ms() + 3_600_000,
                    ))
                })
            }),
        })),
        api_key: Some(default_api_key()),
    };
    let (creds, runtime) = setup(auth, PROVIDER_ID, None).await;
    let term = ScriptedTerminal::new(&["the-code"], true);
    let code = run_cmd(
        &[PROVIDER_ID],
        &runtime,
        Arc::clone(&term) as Arc<dyn LoginTerminal>,
        AbortController::new().signal(),
        None,
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(term.opened(), vec!["https://example.test/authorize"]);
    assert!(!term.lines().iter().any(|l| l.contains("--no-browser")));
    assert!(matches!(
        creds.read(PROVIDER_ID, None).await.unwrap(),
        Some(Credential::OAuth(ref o)) if o.access == "access-the-code"
    ));
}

#[tokio::test]
async fn no_browser_takes_only_other_method_and_opens_nothing() {
    let (creds, runtime) = setup(both_methods_auth(), PROVIDER_ID, None).await;
    let term = ScriptedTerminal::new(&[], true);
    let code = run_cmd(
        &[PROVIDER_ID, "--no-browser"],
        &runtime,
        Arc::clone(&term) as Arc<dyn LoginTerminal>,
        AbortController::new().signal(),
        None,
    )
    .await;
    assert_eq!(code, 0);
    assert!(term.questions().is_empty());
    let lines = term.lines();
    assert_eq!(lines[0], "Sign in to Login Test: Sign in with device code");
    assert!(lines.contains(&"Open https://example.test/device and enter code: ABCD-1234".to_owned()));
    assert!(term.opened().is_empty());
    assert!(!lines.iter().any(|l| l.contains("--no-browser")));
    assert!(matches!(
        creds.read(PROVIDER_ID, None).await.unwrap(),
        Some(Credential::OAuth(ref o)) if o.access == "access-device"
    ));
}

#[tokio::test]
async fn no_browser_asks_among_several_other_methods_and_reasks_on_invalid_selection() {
    let auth = ProviderAuth {
        oauth: Some(Arc::new(CustomOAuth {
            login_fn: Arc::new(|interaction, _| {
                Box::pin(async move {
                    let method = interaction
                        .prompt(AuthPrompt::select(
                            "Sign in to Login Test:",
                            vec![
                                AuthSelectOption {
                                    id: "browser".to_owned(),
                                    label: "Browser".to_owned(),
                                    description: None,
                                },
                                AuthSelectOption {
                                    id: "device".to_owned(),
                                    label: "Device code".to_owned(),
                                    description: None,
                                },
                                AuthSelectOption {
                                    id: "copy".to_owned(),
                                    label: "Copy code".to_owned(),
                                    description: None,
                                },
                            ],
                        ))
                        .await?;
                    Ok(OAuthCredential::new(
                        "refresh",
                        format!("access-{method}"),
                        now_ms() + 3_600_000,
                    ))
                })
            }),
        })),
        api_key: Some(default_api_key()),
    };
    let (creds, runtime) = setup(auth, PROVIDER_ID, None).await;
    let term = ScriptedTerminal::new(&["7", "2"], true);
    let code = run_cmd(
        &["--no-browser", PROVIDER_ID],
        &runtime,
        Arc::clone(&term) as Arc<dyn LoginTerminal>,
        AbortController::new().signal(),
        None,
    )
    .await;
    assert_eq!(code, 0);
    let lines = term.lines();
    assert_eq!(
        &lines[..3],
        ["Sign in to Login Test:", "  1. Device code", "  2. Copy code"]
    );
    assert!(lines.contains(&"Enter a number from 1 to 2.".to_owned()));
    assert!(matches!(
        creds.read(PROVIDER_ID, None).await.unwrap(),
        Some(Credential::OAuth(ref o)) if o.access == "access-copy"
    ));
}

#[tokio::test]
async fn no_browser_keeps_single_method_without_opening_browser() {
    let auth = ProviderAuth {
        oauth: Some(Arc::new(CustomOAuth {
            login_fn: Arc::new(|interaction, _| {
                Box::pin(async move {
                    let method = interaction
                        .prompt(AuthPrompt::select(
                            "Sign in to Login Test:",
                            vec![AuthSelectOption {
                                id: "browser".to_owned(),
                                label: "Browser".to_owned(),
                                description: None,
                            }],
                        ))
                        .await?;
                    interaction.notify(AuthEvent::AuthUrl {
                        url: "https://example.test/authorize".to_owned(),
                        instructions: None,
                    });
                    Ok(OAuthCredential::new(
                        "refresh",
                        format!("access-{method}"),
                        now_ms() + 3_600_000,
                    ))
                })
            }),
        })),
        api_key: Some(default_api_key()),
    };
    let (creds, runtime) = setup(auth, PROVIDER_ID, None).await;
    let term = ScriptedTerminal::new(&[], true);
    let code = run_cmd(
        &[PROVIDER_ID, "--no-browser"],
        &runtime,
        Arc::clone(&term) as Arc<dyn LoginTerminal>,
        AbortController::new().signal(),
        None,
    )
    .await;
    assert_eq!(code, 0);
    assert!(term.questions().is_empty());
    assert!(term.opened().is_empty());
    assert!(matches!(
        creds.read(PROVIDER_ID, None).await.unwrap(),
        Some(Credential::OAuth(ref o)) if o.access == "access-browser"
    ));
}

#[tokio::test]
async fn asks_for_selections_during_api_key_setup() {
    let auth = ProviderAuth {
        oauth: None,
        api_key: Some(Arc::new(CustomApiKey {
            name: "Login Test API key".to_owned(),
            supports_login: true,
            login_fn: Some(Arc::new(|interaction| {
                Box::pin(async move {
                    let key = interaction
                        .prompt(AuthPrompt::select(
                            "Choose credentials:",
                            vec![
                                AuthSelectOption {
                                    id: "first".to_owned(),
                                    label: "First".to_owned(),
                                    description: None,
                                },
                                AuthSelectOption {
                                    id: "second".to_owned(),
                                    label: "Second".to_owned(),
                                    description: Some("Other credentials".to_owned()),
                                },
                            ],
                        ))
                        .await?;
                    Ok(ApiKeyCredential {
                        key: Some(key),
                        env: None,
                    })
                })
            })),
        })),
    };
    let (creds, runtime) = setup(auth, PROVIDER_ID, None).await;
    let term = ScriptedTerminal::new(&["2"], true);
    let code = run_cmd(
        &[PROVIDER_ID, "--api-key"],
        &runtime,
        Arc::clone(&term) as Arc<dyn LoginTerminal>,
        AbortController::new().signal(),
        None,
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(term.questions(), vec!["Select 1-2: "]);
    assert!(term.lines().contains(&"  2. Second - Other credentials".to_owned()));
    assert!(matches!(
        creds.read(PROVIDER_ID, None).await.unwrap(),
        Some(Credential::ApiKey(ref a)) if a.key.as_deref() == Some("second")
    ));
}

#[tokio::test]
async fn cancels_pending_prompt_without_cancelling_login_when_browser_callback_wins() {
    let prompt_ctrl = Arc::new(AbortController::new());
    let p_ctrl = Arc::clone(&prompt_ctrl);
    let auth = ProviderAuth {
        oauth: Some(Arc::new(CustomOAuth {
            login_fn: Arc::new(move |interaction, _| {
                let p_ctrl = Arc::clone(&p_ctrl);
                Box::pin(async move {
                    let prompt = AuthPrompt::manual_code("Paste the code", Some("redirect URL".to_owned()))
                        .with_signal(p_ctrl.signal());
                    p_ctrl.abort(None);
                    let err = interaction.prompt(prompt).await.unwrap_err();
                    assert!(!err.to_string().is_empty());
                    assert!(!interaction.signal.aborted());
                    Ok(OAuthCredential::new("refresh", "callback", now_ms() + 3_600_000))
                })
            }),
        })),
        api_key: None,
    };
    let (creds, runtime) = setup(auth, PROVIDER_ID, None).await;
    let term = ScriptedTerminal::abort_waiter();
    let code = run_cmd(
        &[PROVIDER_ID],
        &runtime,
        Arc::clone(&term) as Arc<dyn LoginTerminal>,
        AbortController::new().signal(),
        None,
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(term.questions(), vec!["Paste the code (redirect URL): "]);
    assert!(matches!(
        creds.read(PROVIDER_ID, None).await.unwrap(),
        Some(Credential::OAuth(ref o)) if o.access == "callback"
    ));
}

#[tokio::test]
async fn reports_committed_login_separately_from_sync_failure() {
    let fail_sync = Arc::new(AtomicBool::new(false));
    let fs = Arc::clone(&fail_sync);
    let (_creds, runtime) = setup(
        ProviderAuth {
            oauth: None,
            api_key: Some(default_api_key()),
        },
        PROVIDER_ID,
        Some(Arc::new(move |ctx| {
            let fs = Arc::clone(&fs);
            Box::pin(async move {
                if !ctx.allow_network && fs.load(Ordering::SeqCst) {
                    return Err(Error::msg("snapshot failed"));
                }
                Ok(())
            })
        })),
    )
    .await;
    fail_sync.store(true, Ordering::SeqCst);

    let term = ScriptedTerminal::new(&["saved"], true);
    let code = run_cmd(
        &[PROVIDER_ID],
        &runtime,
        Arc::clone(&term) as Arc<dyn LoginTerminal>,
        AbortController::new().signal(),
        None,
    )
    .await;
    assert_eq!(code, 1);
    assert!(
        term.errors()
            .first()
            .is_some_and(|e| e.contains("Logged in to Login Test, but local model state could not be synchronized:"))
    );
}

#[tokio::test]
async fn api_key_saves_key_and_handles_providers_without_api_key_or_outside_pi() {
    let (creds, runtime) = setup(both_methods_auth(), PROVIDER_ID, None).await;
    let term = ScriptedTerminal::new(&["sk-test"], true);
    let code = run_cmd(
        &[PROVIDER_ID, "--api-key"],
        &runtime,
        Arc::clone(&term) as Arc<dyn LoginTerminal>,
        AbortController::new().signal(),
        None,
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(term.questions(), vec!["Enter Login Test API key: "]);
    assert!(matches!(
        creds.read(PROVIDER_ID, None).await.unwrap(),
        Some(Credential::ApiKey(ref a)) if a.key.as_deref() == Some("sk-test")
    ));
    assert!(
        term.lines()
            .iter()
            .any(|l| l.starts_with("Saved API key for Login Test."))
    );

    let (_, api_only_rt) = setup(
        ProviderAuth {
            oauth: None,
            api_key: Some(default_api_key()),
        },
        PROVIDER_ID,
        None,
    )
    .await;
    let t2 = ScriptedTerminal::new(&["sk-test"], true);
    assert_eq!(
        run_cmd(
            &[PROVIDER_ID],
            &api_only_rt,
            Arc::clone(&t2) as Arc<dyn LoginTerminal>,
            AbortController::new().signal(),
            None
        )
        .await,
        0
    );
    assert_eq!(t2.questions(), vec!["Enter Login Test API key: "]);

    let (_, oauth_only_rt) = setup(
        ProviderAuth {
            oauth: Some(default_oauth()),
            api_key: None,
        },
        PROVIDER_ID,
        None,
    )
    .await;
    let t3 = ScriptedTerminal::new(&[], true);
    assert_eq!(
        run_cmd(
            &[PROVIDER_ID, "--api-key"],
            &oauth_only_rt,
            Arc::clone(&t3) as Arc<dyn LoginTerminal>,
            AbortController::new().signal(),
            None
        )
        .await,
        1
    );
    assert_eq!(
        t3.errors(),
        vec!["Login Test has no API key login. Omit --api-key to sign in."]
    );

    let (_, outside_rt) = setup(
        ProviderAuth {
            oauth: None,
            api_key: Some(Arc::new(CustomApiKey {
                name: "AWS profile".to_owned(),
                supports_login: false,
                login_fn: None,
            })),
        },
        PROVIDER_ID,
        None,
    )
    .await;
    let t4 = ScriptedTerminal::new(&[], true);
    assert_eq!(
        run_cmd(
            &[PROVIDER_ID],
            &outside_rt,
            Arc::clone(&t4) as Arc<dyn LoginTerminal>,
            AbortController::new().signal(),
            None
        )
        .await,
        1
    );
    assert_eq!(t4.errors(), vec!["Login Test is configured outside pi (AWS profile)."]);
}

#[tokio::test]
async fn ctrl_c_during_catalog_refresh_skips_it_and_keeps_login() {
    let controller = Arc::new(AbortController::new());
    let ctrl_for_refresh = Arc::clone(&controller);
    let (creds, runtime) = setup_with_network(
        ProviderAuth {
            oauth: None,
            api_key: Some(default_api_key()),
        },
        PROVIDER_ID,
        true,
        Some(Arc::new(move |ctx| {
            let ctrl = Arc::clone(&ctrl_for_refresh);
            Box::pin(async move {
                if !ctx.allow_network {
                    return Ok(());
                }
                ctrl.abort(None);
                while !ctx.signal.aborted() {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                Ok(())
            })
        })),
    )
    .await;

    let term = ScriptedTerminal::new(&["sk-test"], true);
    let code = run_cmd(
        &[PROVIDER_ID],
        &runtime,
        Arc::clone(&term) as Arc<dyn LoginTerminal>,
        controller.signal(),
        None,
    )
    .await;
    assert_eq!(code, 0);
    assert!(
        term.lines()
            .contains(&"Skipped the Login Test model catalog refresh; pi refreshes it on next start.".to_owned()),
        "lines={:?}, errors={:?}",
        term.lines(),
        term.errors()
    );
    assert!(matches!(
        creds.read(PROVIDER_ID, None).await.unwrap(),
        Some(Credential::ApiKey(ref a)) if a.key.as_deref() == Some("sk-test")
    ));
}

#[tokio::test]
async fn reports_cancellation() {
    let (creds, runtime) = setup(
        ProviderAuth {
            oauth: None,
            api_key: Some(default_api_key()),
        },
        PROVIDER_ID,
        None,
    )
    .await;
    let ctrl = AbortController::new();
    ctrl.abort(None);
    let term = ScriptedTerminal::new(&["sk-test"], true);
    let code = run_cmd(
        &[PROVIDER_ID],
        &runtime,
        Arc::clone(&term) as Arc<dyn LoginTerminal>,
        ctrl.signal(),
        None,
    )
    .await;
    assert_eq!(code, 1);
    assert_eq!(term.errors(), vec!["Login cancelled."]);
    assert_eq!(creds.read(PROVIDER_ID, None).await.unwrap(), None);
}

#[tokio::test]
async fn keeps_unrelated_mcp_servers_when_fallback_names_occupied() {
    for fallback in ["radius-mcp", "radius_mcp"] {
        let dir = TempDir::new().unwrap();
        let agent_dir = dir.path().to_str().unwrap();
        let mcp_path = dir.path().join("mcp.json");
        let servers = json!({
            "radius": { "command": "original-one" },
            fallback: { "command": "original-two" },
            "radius-mcp-2": { "command": "original-three" }
        });
        std::fs::write(&mcp_path, json!({ "mcpServers": servers }).to_string()).unwrap();

        let (_creds, runtime) = setup(
            ProviderAuth {
                oauth: None,
                api_key: Some(default_api_key()),
            },
            "radius",
            None,
        )
        .await;
        let term = ScriptedTerminal::new(&["sk-test"], true);
        let code = run_cmd(
            &["radius"],
            &runtime,
            Arc::clone(&term) as Arc<dyn LoginTerminal>,
            AbortController::new().signal(),
            Some(agent_dir),
        )
        .await;
        assert_eq!(code, 0);

        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&mcp_path).unwrap()).unwrap();
        let expected_url = "https://radius.pi.dev/mcp";
        assert_eq!(parsed["mcpServers"]["radius"], json!({ "command": "original-one" }));
        assert_eq!(parsed["mcpServers"][fallback], json!({ "command": "original-two" }));
        assert_eq!(
            parsed["mcpServers"]["radius-mcp-2"],
            json!({ "command": "original-three" })
        );
        assert_eq!(
            parsed["mcpServers"]["radius-mcp-3"],
            json!({ "url": expected_url, "auth": { "provider": "radius" } })
        );
        assert!(term.lines().contains(&format!(
            "Added MCP server \"radius-mcp-3\" in {} to use the Login Test login.",
            mcp_path.to_string_lossy()
        )));
    }
}

#[tokio::test]
async fn configures_radius_mcp_server_once_without_asking() {
    let dir = TempDir::new().unwrap();
    let agent_dir = dir.path().to_str().unwrap();
    let mcp_path = dir.path().join("mcp.json");

    let (_creds, runtime) = setup(
        ProviderAuth {
            oauth: None,
            api_key: Some(default_api_key()),
        },
        "radius",
        None,
    )
    .await;

    let first = ScriptedTerminal::new(&["sk-test"], true);
    let code = run_cmd(
        &["radius"],
        &runtime,
        Arc::clone(&first) as Arc<dyn LoginTerminal>,
        AbortController::new().signal(),
        Some(agent_dir),
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(first.questions(), vec!["Enter Login Test API key: "]);
    assert!(first.lines().contains(&format!(
        "Added MCP server \"radius\" in {} to use the Login Test login.",
        mcp_path.to_string_lossy()
    )));
    let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&mcp_path).unwrap()).unwrap();
    assert_eq!(parsed["mcpServers"]["radius"]["auth"], json!({ "provider": "radius" }));

    let second = ScriptedTerminal::new(&["sk-test"], true);
    let code2 = run_cmd(
        &["radius"],
        &runtime,
        Arc::clone(&second) as Arc<dyn LoginTerminal>,
        AbortController::new().signal(),
        Some(agent_dir),
    )
    .await;
    assert_eq!(code2, 0);
    assert!(!second.lines().iter().any(|l| l.contains("MCP server")));
}
