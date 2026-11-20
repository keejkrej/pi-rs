mod support;

use std::sync::Arc;
use std::time::Duration;

use pi_js::abort::AbortController;
use pi_js::fetch::Request;
use pi_mcp::oauth::{
    ListenForLoopbackSignInOptions, McpClientMetadataDocuments, McpLoopbackCallback, McpSignInCancelledError,
    McpSignInOptions, McpSignInRedirect, McpSignInStart, WaitForLoopbackSignInOptions, complete_mcp_sign_in,
    listen_for_loopback_sign_in, response_from_redirect_url, start_mcp_sign_in, wait_for_loopback_sign_in,
};
use pretty_assertions::assert_eq;
use support::auth_server::{AuthorizationServer, MemoryServerStore, authorize_in_browser};
use url::Url;

struct Docs;

impl McpClientMetadataDocuments for Docs {
    fn base_url(&self) -> &str {
        "https://app.example/oauth"
    }

    fn lists_redirect_url(&self, _redirect_url: &str) -> bool {
        true
    }
}

async fn listen_at(server_url: &str, documents: bool) -> McpLoopbackCallback {
    listen_for_loopback_sign_in(ListenForLoopbackSignInOptions {
        server_url: server_url.to_string(),
        store: Arc::new(MemoryServerStore::new()),
        settings: Default::default(),
        client_metadata_documents: documents.then(|| Arc::new(Docs) as Arc<dyn McpClientMetadataDocuments>),
        render_page: None,
    })
    .await
    .expect("listen_for_loopback_sign_in")
}

fn pending(redirect_url: &str) -> McpSignInRedirect {
    McpSignInRedirect {
        authorization_url: "https://auth.example/authorize".to_string(),
        redirect_url: redirect_url.to_string(),
        state: "state-1".to_string(),
        scope: None,
    }
}

#[tokio::test]
async fn completes_through_the_loopback_callback_server() {
    let mut server = AuthorizationServer::start(Default::default()).await;
    let store = Arc::new(MemoryServerStore::new());
    let loopback = listen_for_loopback_sign_in(ListenForLoopbackSignInOptions {
        server_url: server.server_url.clone(),
        store: store.clone(),
        settings: Default::default(),
        client_metadata_documents: None,
        render_page: None,
    })
    .await
    .unwrap();
    let parsed_redirect = Url::parse(&loopback.redirect_url).unwrap();
    assert_eq!(parsed_redirect.host_str(), Some("127.0.0.1"));
    assert_eq!(parsed_redirect.path(), "/callback");
    assert!(parsed_redirect.port().is_some());

    let options = McpSignInOptions {
        server_url: server.server_url.clone(),
        store: store.clone(),
        settings: Default::default(),
        client_name: "app".to_string(),
        client_metadata_documents: None,
        redirect_url: loopback.redirect_url.clone(),
        challenge: None,
        signal: None,
    };
    let authorization = match start_mcp_sign_in(options.clone()).await.unwrap() {
        McpSignInStart::Redirect(r) => r,
        McpSignInStart::Authorized => panic!("expected browser step"),
    };

    let wait_opts = WaitForLoopbackSignInOptions {
        callback: loopback.callback.clone(),
        authorization: authorization.clone(),
        prompt_for_redirect_url: None,
        signal: None,
    };
    let (response_res, _) = tokio::join!(wait_for_loopback_sign_in(wait_opts), async {
        let redirected = authorize_in_browser(&authorization.authorization_url).await;
        let _ = pi_js::fetch::fetch(Request::new("GET", redirected)).await.unwrap();
    });
    complete_mcp_sign_in(options, authorization, response_res.unwrap())
        .await
        .unwrap();

    assert_eq!(
        pi_mcp::oauth::McpOAuthStateStore::load(store.as_ref())
            .await
            .unwrap()
            .unwrap()
            .tokens
            .unwrap()
            .access_token,
        "access-1"
    );
    let regs = server.registrations();
    assert_eq!(regs.len(), 1);
    assert_eq!(regs[0]["redirect_uris"][0], loopback.redirect_url);
    assert_eq!(regs[0]["application_type"], "native");

    loopback.callback.close().await.unwrap();
    server.close().await;
}

#[tokio::test]
async fn rejects_a_response_on_another_redirect_path() {
    let server_url = "https://mcp.example/mcp";
    let loopback = listen_at(server_url, true).await;
    let id = pi_js::b64::encode_url(&pi_js::crypto::sha256(server_url.as_bytes())[..9]);
    let wait_opts = WaitForLoopbackSignInOptions {
        callback: loopback.callback.clone(),
        authorization: pending(&format!("{}/{id}", loopback.redirect_url)),
        prompt_for_redirect_url: None,
        signal: None,
    };
    let waiting = tokio::spawn(async move { wait_for_loopback_sign_in(wait_opts).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let resp = pi_js::fetch::fetch(Request::new(
        "GET",
        format!("{}?code=c&state=state-1", loopback.redirect_url),
    ))
    .await
    .unwrap();
    assert_eq!(resp.status(), 400);
    let err = waiting.await.unwrap().unwrap_err();
    assert!(
        err.to_string().contains("arrived on another redirect URI"),
        "err: {err}"
    );
    loopback.callback.close().await.unwrap();
}

#[tokio::test]
async fn takes_a_pasted_redirect_url_while_waiting_for_the_callback() {
    let loopback = listen_at("https://mcp.example/mcp", false).await;
    let pasted = format!("{}?code=pasted&state=state-1", loopback.redirect_url);
    let response = wait_for_loopback_sign_in(WaitForLoopbackSignInOptions {
        callback: loopback.callback.clone(),
        authorization: pending(&loopback.redirect_url),
        prompt_for_redirect_url: Some(Arc::new(move |_| {
            let p = pasted.clone();
            Box::pin(async move { Ok(Some(p)) })
        })),
        signal: None,
    })
    .await
    .unwrap();
    assert_eq!(
        response,
        pi_mcp::oauth::McpAuthorizationResponse {
            code: "pasted".to_string(),
            state: "state-1".to_string(),
            iss: None,
        }
    );
    loopback.callback.close().await.unwrap();
}

#[tokio::test]
async fn cancels_the_wait_and_an_empty_paste_cancels_too() {
    let loopback = listen_at("https://mcp.example/mcp", false).await;
    let controller = AbortController::new();
    let waiting = tokio::spawn({
        let opts = WaitForLoopbackSignInOptions {
            callback: loopback.callback.clone(),
            authorization: pending(&loopback.redirect_url),
            prompt_for_redirect_url: None,
            signal: Some(controller.signal()),
        };
        async move { wait_for_loopback_sign_in(opts).await }
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    controller.abort(None);
    let err1 = waiting.await.unwrap().unwrap_err();
    assert!(err1.downcast_ref::<McpSignInCancelledError>().is_some());

    let mut auth2 = pending(&loopback.redirect_url);
    auth2.state = "state-2".to_string();
    let err2 = wait_for_loopback_sign_in(WaitForLoopbackSignInOptions {
        callback: loopback.callback.clone(),
        authorization: auth2,
        prompt_for_redirect_url: Some(Arc::new(|_| Box::pin(async { Ok(Some(String::new())) }))),
        signal: None,
    })
    .await
    .unwrap_err();
    assert!(err2.downcast_ref::<McpSignInCancelledError>().is_some());
    loopback.callback.close().await.unwrap();
}

#[test]
fn parses_and_validates_pasted_redirect_urls() {
    let authorization = pending("http://127.0.0.1:1234/callback/server-id");
    let redirect_url = &authorization.redirect_url;

    assert_eq!(
        response_from_redirect_url(&format!(" {redirect_url}?code=c&state=state-1&iss=x "), &authorization).unwrap(),
        pi_mcp::oauth::McpAuthorizationResponse {
            code: "c".to_string(),
            state: "state-1".to_string(),
            iss: Some("x".to_string()),
        }
    );

    assert!(
        response_from_redirect_url(&format!("{redirect_url}?code=c&state=other"), &authorization)
            .unwrap_err()
            .to_string()
            .contains("belongs to a different sign-in")
    );
    assert!(
        response_from_redirect_url("http://127.0.0.1:1234/callback?code=c&state=state-1", &authorization)
            .unwrap_err()
            .to_string()
            .contains("does not match this sign-in's redirect URI")
    );
    assert!(
        response_from_redirect_url(&format!("{redirect_url}?error=access_denied"), &authorization)
            .unwrap_err()
            .to_string()
            .contains("access_denied")
    );
    assert!(
        response_from_redirect_url("code=c", &authorization)
            .unwrap_err()
            .to_string()
            .contains("Expected the full redirect URL")
    );
}
