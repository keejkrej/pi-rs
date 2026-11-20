//! Port of packages/ai/test/openai-chatgpt-oauth.test.ts

use indexmap::IndexMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pi_ai::auth::oauth::openai_chatgpt::openai_chatgpt_oauth;
use pi_ai::auth::types::{
    AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind, LoginOptions, OAuthCredential, ProviderAuthInteraction,
};
use pi_js::abort::AbortController;
use pi_js::fetch::testing::{mock_fetch, response};
use pi_js::{Error, Result, time};
use serde_json::{Value, json};
use serial_test::serial;

const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
const REQUIRED_SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const DEVICE_ID: &str = "e61bbe28-07ef-466d-8e5d-a344f94ab305";

fn json_resp(body: &Value) -> pi_js::fetch::Response {
    response(
        200,
        &[("content-type", "application/json")],
        pi_js::json::stringify(body),
    )
}

fn token_response(scope: &str) -> Value {
    json!({
        "access_token": "access-token",
        "refresh_token": "refresh-token",
        "expires_in": 3600,
        "id_token": "id-token",
        "scope": scope
    })
}

fn parse_form(body: &str) -> IndexMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes()).into_owned().collect()
}

fn query_param(u: &url::Url, key: &str) -> Option<String> {
    u.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned())
}

struct LoginTestInteraction {
    callback_client_id: Option<String>,
    authorize_url: Arc<Mutex<Option<url::Url>>>,
}

#[async_trait::async_trait]
impl AuthInteraction for LoginTestInteraction {
    async fn prompt(&self, prompt: AuthPrompt) -> Result<String> {
        let AuthPromptKind::ManualCode { .. } = prompt.kind else {
            return Err(Error::msg("Unexpected prompt"));
        };
        let url = self
            .authorize_url
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| Error::msg("Authorization URL was not emitted before the callback prompt"))?;
        let redirect_uri = query_param(&url, "redirect_uri").unwrap_or_default();
        let mut cb = url::Url::parse(&redirect_uri).unwrap();
        {
            let mut pairs = cb.query_pairs_mut();
            pairs.append_pair("code", "authorization-code");
            pairs.append_pair("state", &query_param(&url, "state").unwrap_or_default());
            if let Some(cid) = &self.callback_client_id {
                pairs.append_pair("client_id", cid);
            }
        }
        Ok(cb.to_string())
    }

    fn notify(&self, event: AuthEvent) {
        if let AuthEvent::AuthUrl { url, .. } = event {
            *self.authorize_url.lock().unwrap() = Some(url::Url::parse(&url).unwrap());
        }
    }
}

fn connected_credential() -> OAuthCredential {
    let mut cred = OAuthCredential::new("old-refresh", "old-access", 0);
    cred.set_extra("clientId", json!("oaiapp_existing"));
    cred.set_extra("scopes", json!(REQUIRED_SCOPE.split_whitespace().collect::<Vec<_>>()));
    cred
}

#[tokio::test]
#[serial]
async fn registers_a_user_owned_client_and_stores_its_issued_id_and_granted_scopes() {
    let exchange_body = Arc::new(Mutex::new(None));
    let eb_c = exchange_body.clone();
    let _guard = mock_fetch(move |req| {
        let eb = eb_c.clone();
        Box::pin(async move {
            assert_eq!(req.url, TOKEN_URL);
            let form = parse_form(&req.body_text().unwrap());
            *eb.lock().unwrap() = Some(form);
            Ok(json_resp(&token_response(REQUIRED_SCOPE)))
        })
    });

    let authorize_url = Arc::new(Mutex::new(None));
    let interaction = ProviderAuthInteraction::new(
        Arc::new(LoginTestInteraction {
            callback_client_id: Some("oaiapp_issued".to_owned()),
            authorize_url: authorize_url.clone(),
        }),
        AbortController::new().signal(),
    );

    let credential = openai_chatgpt_oauth()
        .login(
            interaction,
            Some(LoginOptions {
                get_device_id: Some(Arc::new(|| DEVICE_ID.to_owned())),
                agent_name: None,
            }),
        )
        .await
        .unwrap();

    let url = authorize_url.lock().unwrap().clone().unwrap();
    assert_eq!(query_param(&url, "client_id").as_deref(), Some("dynamic_agent_client"));
    assert_eq!(query_param(&url, "agent_name_hint").as_deref(), Some("Pi"));
    assert_eq!(
        query_param(&url, "ext_agent_host_id").as_deref(),
        Some(format!("urn:uuid:{DEVICE_ID}").as_str())
    );
    assert_eq!(query_param(&url, "scope").as_deref(), Some(REQUIRED_SCOPE));
    assert_eq!(
        query_param(&url, "redirect_uri").as_deref(),
        Some("http://127.0.0.1:1455/auth/callback")
    );
    assert_eq!(
        query_param(&url, "resource").as_deref(),
        Some("https://api.openai.com/v1")
    );
    assert_eq!(query_param(&url, "code_challenge_method").as_deref(), Some("S256"));

    let form = exchange_body.lock().unwrap().clone().unwrap();
    assert_eq!(form.get("client_id").map(String::as_str), Some("oaiapp_issued"));
    assert_eq!(form.get("code").map(String::as_str), Some("authorization-code"));
    assert_eq!(
        form.get("resource").map(String::as_str),
        Some("https://api.openai.com/v1")
    );
    assert!(form.get("code_verifier").is_some_and(|v| !v.is_empty()));

    assert_eq!(credential.access, "access-token");
    assert_eq!(credential.refresh, "refresh-token");
    assert_eq!(credential.get_extra_str("clientId"), Some("oaiapp_issued"));
    assert_eq!(
        credential.extra.get("scopes"),
        Some(&json!(REQUIRED_SCOPE.split_whitespace().collect::<Vec<_>>()))
    );
}

#[tokio::test]
#[serial]
async fn rejects_registration_without_an_issued_client_id() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let _guard = mock_fetch(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(json_resp(&token_response(REQUIRED_SCOPE))) })
    });

    let interaction = ProviderAuthInteraction::new(
        Arc::new(LoginTestInteraction {
            callback_client_id: None,
            authorize_url: Arc::new(Mutex::new(None)),
        }),
        AbortController::new().signal(),
    );
    let err = openai_chatgpt_oauth()
        .login(
            interaction,
            Some(LoginOptions {
                get_device_id: Some(Arc::new(|| DEVICE_ID.to_owned())),
                agent_name: None,
            }),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("registration callback did not contain an issued client ID")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
#[serial]
async fn rejects_a_token_response_that_did_not_grant_direct_token_use() {
    let _guard = mock_fetch(|_| {
        Box::pin(async {
            Ok(json_resp(&token_response(
                "openid profile email offline_access resource.invoke",
            )))
        })
    });

    let interaction = ProviderAuthInteraction::new(
        Arc::new(LoginTestInteraction {
            callback_client_id: Some("oaiapp_issued".to_owned()),
            authorize_url: Arc::new(Mutex::new(None)),
        }),
        AbortController::new().signal(),
    );
    let err = openai_chatgpt_oauth()
        .login(
            interaction,
            Some(LoginOptions {
                get_device_id: Some(Arc::new(|| DEVICE_ID.to_owned())),
                agent_name: None,
            }),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("grant did not include chatgpt.tokens.use.direct")
    );
}

#[tokio::test]
#[serial]
async fn requires_a_device_id_before_starting_authorization() {
    let authorize_url = Arc::new(Mutex::new(None));
    let interaction1 = ProviderAuthInteraction::new(
        Arc::new(LoginTestInteraction {
            callback_client_id: None,
            authorize_url: authorize_url.clone(),
        }),
        AbortController::new().signal(),
    );
    let err1 = openai_chatgpt_oauth().login(interaction1, None).await.unwrap_err();
    assert!(err1.to_string().contains("requires a device ID"));

    let interaction2 = ProviderAuthInteraction::new(
        Arc::new(LoginTestInteraction {
            callback_client_id: None,
            authorize_url: authorize_url.clone(),
        }),
        AbortController::new().signal(),
    );
    let err2 = openai_chatgpt_oauth()
        .login(
            interaction2,
            Some(LoginOptions {
                get_device_id: Some(Arc::new(|| "not-a-uuid".to_owned())),
                agent_name: None,
            }),
        )
        .await
        .unwrap_err();
    assert!(err2.to_string().contains("requires a device ID"));
    assert!(authorize_url.lock().unwrap().is_none());
}

#[tokio::test]
#[serial]
async fn requires_refresh_responses_to_rotate_the_refresh_token() {
    let mut without_refresh = token_response(REQUIRED_SCOPE);
    without_refresh.as_object_mut().unwrap().remove("refresh_token");
    let _guard = mock_fetch(move |_| {
        let body = without_refresh.clone();
        Box::pin(async move { Ok(json_resp(&body)) })
    });

    let err = openai_chatgpt_oauth()
        .refresh(connected_credential(), AbortController::new().signal())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("token response has invalid refresh_token"));
}

#[tokio::test]
#[serial]
async fn refreshes_with_the_credentials_issued_client_id_and_stores_replacement_scopes() {
    let refresh_body = Arc::new(Mutex::new(None));
    let rb_c = refresh_body.clone();
    let _guard = mock_fetch(move |req| {
        let rb = rb_c.clone();
        Box::pin(async move {
            assert_eq!(req.url, TOKEN_URL);
            *rb.lock().unwrap() = Some(parse_form(&req.body_text().unwrap()));
            let mut resp = token_response(REQUIRED_SCOPE);
            resp["access_token"] = json!("new-access");
            resp["refresh_token"] = json!("new-refresh");
            Ok(json_resp(&resp))
        })
    });

    let before = time::now_ms();
    let credential = openai_chatgpt_oauth()
        .refresh(connected_credential(), AbortController::new().signal())
        .await
        .unwrap();

    assert!(credential.expires >= before + (3600 - 180) * 1000);
    assert!(credential.expires <= time::now_ms() + (3600 - 180) * 1000);

    let form = refresh_body.lock().unwrap().clone().unwrap();
    assert_eq!(form.get("grant_type").map(String::as_str), Some("refresh_token"));
    assert_eq!(form.get("client_id").map(String::as_str), Some("oaiapp_existing"));
    assert_eq!(form.get("refresh_token").map(String::as_str), Some("old-refresh"));
    assert_eq!(
        form.get("resource").map(String::as_str),
        Some("https://api.openai.com/v1")
    );
    assert!(!form.contains_key("scope"));

    assert_eq!(credential.access, "new-access");
    assert_eq!(credential.refresh, "new-refresh");
    assert_eq!(credential.get_extra_str("clientId"), Some("oaiapp_existing"));
    assert_eq!(
        credential.extra.get("scopes"),
        Some(&json!(REQUIRED_SCOPE.split_whitespace().collect::<Vec<_>>()))
    );
}

#[tokio::test]
#[serial]
async fn uses_the_apps_agent_name_as_the_name_hint() {
    let authorize_url = Arc::new(Mutex::new(None::<url::Url>));
    let _guard = mock_fetch(move |req| {
        assert_eq!(req.url, TOKEN_URL);
        Box::pin(async move { Ok(json_resp(&token_response(REQUIRED_SCOPE))) })
    });
    let interaction = ProviderAuthInteraction::new(
        Arc::new(LoginTestInteraction {
            callback_client_id: Some("oaiapp_issued".to_owned()),
            authorize_url: authorize_url.clone(),
        }),
        AbortController::new().signal(),
    );
    openai_chatgpt_oauth()
        .login(
            interaction,
            Some(LoginOptions {
                get_device_id: Some(Arc::new(|| DEVICE_ID.to_owned())),
                agent_name: Some("my-app".to_owned()),
            }),
        )
        .await
        .unwrap();

    let url = authorize_url.lock().unwrap().clone().unwrap();
    assert_eq!(query_param(&url, "agent_name_hint").as_deref(), Some("my-app"));
}
