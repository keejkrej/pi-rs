mod support;

use std::sync::Arc;
use std::time::Duration;

use pi_js::abort::AbortController;
use pi_mcp::oauth::{
    McpAuthorizationResponse, McpClientMetadataDocuments, McpClientRegistration, McpOAuthSettings, McpOAuthStateStore,
    McpRefreshLockBusyError, McpSignInCancelledError, McpSignInOptions, McpSignInRedirect, McpSignInStart,
    OAuthChallenge, complete_mcp_sign_in, start_mcp_sign_in,
};
use pretty_assertions::assert_eq;
use serde_json::json;
use support::auth_server::{
    AuthorizationServer, MemoryServerStore, RecordedSave, RecordingStore, StartAuthServerOptions, authorize_in_browser,
};
use url::Url;

const ROUTE: &str = "https://app.example/mcp/callback";
const OTHER_ROUTE: &str = "https://relay.example/mcp/callback";
const LOOPBACK: &str = "http://127.0.0.1:1/callback";

struct Documents {
    route_only: bool,
    all: bool,
}

impl McpClientMetadataDocuments for Documents {
    fn base_url(&self) -> &str {
        "https://app.example/oauth"
    }

    fn lists_redirect_url(&self, redirect_url: &str) -> bool {
        if self.all {
            return true;
        }
        if self.route_only {
            return redirect_url == ROUTE;
        }
        Url::parse(redirect_url).is_ok_and(|u| u.host_str() == Some("127.0.0.1") && u.path() == "/callback")
    }
}

fn default_docs() -> Arc<dyn McpClientMetadataDocuments> {
    Arc::new(Documents {
        route_only: false,
        all: false,
    })
}

fn route_docs() -> Arc<dyn McpClientMetadataDocuments> {
    Arc::new(Documents {
        route_only: true,
        all: false,
    })
}

fn all_docs() -> Arc<dyn McpClientMetadataDocuments> {
    Arc::new(Documents {
        route_only: false,
        all: true,
    })
}

fn sign_in_options(server: &AuthorizationServer) -> McpSignInOptions {
    McpSignInOptions {
        server_url: server.server_url.clone(),
        store: Arc::new(MemoryServerStore::new()),
        settings: McpOAuthSettings::default(),
        redirect_url: ROUTE.to_string(),
        client_name: "app".to_string(),
        client_metadata_documents: None,
        challenge: None,
        signal: None,
    }
}

async fn start_redirect(options: McpSignInOptions) -> McpSignInRedirect {
    match start_mcp_sign_in(options).await.expect("start_mcp_sign_in") {
        McpSignInStart::Redirect(r) => r,
        McpSignInStart::Authorized => panic!("expected browser step"),
    }
}

fn route_response(location: &str) -> McpAuthorizationResponse {
    let url = Url::parse(location).expect("valid redirect URL");
    let qp = |name: &str| url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned());
    McpAuthorizationResponse {
        code: qp("code").unwrap_or_default(),
        state: qp("state").unwrap_or_default(),
        iss: qp("iss").filter(|s| !s.is_empty()),
    }
}

async fn sign_in(options: McpSignInOptions) -> McpSignInRedirect {
    let authorization = start_redirect(options.clone()).await;
    let location = authorize_in_browser(&authorization.authorization_url).await;
    let response = route_response(&location);
    complete_mcp_sign_in(options, authorization.clone(), response)
        .await
        .expect("complete sign-in");
    authorization
}

fn query_param(url: &str, name: &str) -> Option<String> {
    Url::parse(url)
        .ok()?
        .query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

#[tokio::test]
async fn completes_in_a_second_call_with_same_store_and_persisted_redirect() {
    let mut server = AuthorizationServer::start(Default::default()).await;
    let mut options = sign_in_options(&server);
    options.settings.scope = Some("issues:read".to_string());

    let started = start_redirect(options.clone()).await;
    let json_text = pi_js::json::stringify(&started);
    let authorization: McpSignInRedirect = serde_json::from_value(pi_js::json::parse(&json_text).unwrap()).unwrap();
    assert_eq!(authorization, started);
    assert_eq!(authorization.redirect_url, ROUTE);
    assert_eq!(authorization.scope.as_deref(), Some("issues:read"));
    assert_eq!(
        query_param(&authorization.authorization_url, "state").as_deref(),
        Some(authorization.state.as_str())
    );
    let regs = server.registrations();
    assert_eq!(regs[0]["client_name"], "app");
    assert_eq!(regs[0]["redirect_uris"], json!([ROUTE]));
    assert_eq!(regs[0]["application_type"], "web");

    let response = route_response(&authorize_in_browser(&authorization.authorization_url).await);
    complete_mcp_sign_in(options.clone(), authorization, response)
        .await
        .unwrap();

    let tokens = McpOAuthStateStore::load(options.store.as_ref())
        .await
        .unwrap()
        .unwrap()
        .tokens
        .unwrap();
    assert_eq!(tokens.access_token, "access-1");
    assert_eq!(tokens.scope.as_deref(), Some("issues:read"));
    assert_eq!(
        server.token_requests()[0].get("redirect_uri").map(String::as_str),
        Some(ROUTE)
    );
    assert_eq!(
        server.token_requests()[0].get("client_id").map(String::as_str),
        Some("client-1")
    );
    server.close().await;
}

#[tokio::test]
async fn refreshes_stored_tokens_instead_of_redirecting() {
    let mut server = AuthorizationServer::start(Default::default()).await;
    let options = sign_in_options(&server);
    sign_in(options.clone()).await;

    assert_eq!(
        start_mcp_sign_in(options.clone()).await.unwrap(),
        McpSignInStart::Authorized
    );
    assert_eq!(server.authorizations().len(), 1);
    assert_eq!(
        server.token_requests()[1].get("grant_type").map(String::as_str),
        Some("refresh_token")
    );
    assert_eq!(
        McpOAuthStateStore::load(options.store.as_ref())
            .await
            .unwrap()
            .unwrap()
            .tokens
            .unwrap()
            .access_token,
        "access-2"
    );
    server.close().await;
}

#[tokio::test]
async fn refreshes_under_the_stores_refresh_lock() {
    let mut server = AuthorizationServer::start(Default::default()).await;
    let store = Arc::new(RecordingStore::new(None));
    let mut options = sign_in_options(&server);
    options.store = store.clone();
    sign_in(options.clone()).await;

    assert_eq!(start_mcp_sign_in(options).await.unwrap(), McpSignInStart::Authorized);
    let access_2_saves: Vec<RecordedSave> = store
        .saves()
        .into_iter()
        .filter(|s| s.access_token.as_deref() == Some("access-2"))
        .collect();
    assert_eq!(
        access_2_saves,
        vec![RecordedSave {
            locked: true,
            access_token: Some("access-2".to_string()),
        }]
    );
    server.close().await;
}

#[tokio::test]
async fn goes_to_browser_step_when_lock_is_busy_and_fails_on_other_lock_errors() {
    let mut server = AuthorizationServer::start(Default::default()).await;
    let busy_store = Arc::new(RecordingStore::new(Some(Arc::new(|| {
        pi_js::Error::typed("McpRefreshLockBusyError", McpRefreshLockBusyError::new())
    }))));
    let mut options = sign_in_options(&server);
    options.store = busy_store;
    sign_in(options.clone()).await;

    assert!(matches!(
        start_mcp_sign_in(options).await.unwrap(),
        McpSignInStart::Redirect(_)
    ));
    let grants: Vec<String> = server
        .token_requests()
        .into_iter()
        .filter_map(|p| p.get("grant_type").cloned())
        .collect();
    assert_eq!(grants, vec!["authorization_code".to_string()]);

    let mut server2 = AuthorizationServer::start(Default::default()).await;
    let err_store = Arc::new(RecordingStore::new(Some(Arc::new(|| {
        pi_js::Error::msg("EACCES: permission denied")
    }))));
    let mut opts2 = sign_in_options(&server2);
    opts2.store = err_store;
    let err = start_mcp_sign_in(opts2).await.unwrap_err();
    assert!(err.to_string().contains("EACCES: permission denied"));
    assert!(server2.registrations().is_empty());
    assert!(server2.authorizations().is_empty());
    server.close().await;
    server2.close().await;
}

#[tokio::test]
async fn refuses_to_complete_with_wrong_redirect_uri_superseded_or_wrong_state() {
    let mut server = AuthorizationServer::start(StartAuthServerOptions {
        cimd: true,
        iss_parameter: false,
    })
    .await;
    let mut options = sign_in_options(&server);
    options.redirect_url = LOOPBACK.to_string();
    options.client_metadata_documents = Some(default_docs());

    let authorization = start_redirect(options.clone()).await;
    let response = route_response(&authorize_in_browser(&authorization.authorization_url).await);
    let mut wrong = options.clone();
    wrong.redirect_url = "http://127.0.0.1:2/callback".to_string();
    let err = complete_mcp_sign_in(wrong, authorization.clone(), response)
        .await
        .unwrap_err();
    assert!(err.to_string().contains(&format!(
        "but the authorization request used {}",
        authorization.redirect_url
    )));
    assert!(server.token_requests().is_empty());

    let mut server2 = AuthorizationServer::start(Default::default()).await;
    let opts2 = sign_in_options(&server2);
    let first = start_redirect(opts2.clone()).await;
    let first_resp = route_response(&authorize_in_browser(&first.authorization_url).await);
    let second = start_redirect(opts2.clone()).await;
    assert_ne!(second.state, first.state);

    let err_superseded = complete_mcp_sign_in(opts2.clone(), first, first_resp.clone())
        .await
        .unwrap_err();
    assert!(
        err_superseded
            .to_string()
            .contains("Another sign-in to this MCP server started since")
    );
    assert!(server2.token_requests().is_empty());

    let err_state = complete_mcp_sign_in(
        opts2,
        second,
        McpAuthorizationResponse {
            state: "other".to_string(),
            ..first_resp
        },
    )
    .await
    .unwrap_err();
    assert!(err_state.to_string().contains("belongs to a different sign-in"));
    assert!(server2.token_requests().is_empty());

    server.close().await;
    server2.close().await;
}

#[tokio::test]
async fn manages_stored_client_across_redirects_cimd_and_step_up() {
    let mut server = AuthorizationServer::start(Default::default()).await;
    let options = sign_in_options(&server);
    sign_in(options.clone()).await;

    let mut other_opts = options.clone();
    other_opts.redirect_url = OTHER_ROUTE.to_string();
    assert_eq!(
        start_mcp_sign_in(other_opts.clone()).await.unwrap(),
        McpSignInStart::Authorized
    );
    assert_eq!(server.registrations().len(), 1);
    assert_eq!(
        server.token_requests()[1].get("client_id").map(String::as_str),
        Some("client-1")
    );

    server.revoke_refresh_tokens();
    let kept = start_redirect(options.clone()).await;
    assert_eq!(
        query_param(&kept.authorization_url, "client_id").as_deref(),
        Some("client-1")
    );
    assert_eq!(server.registrations().len(), 1);

    let dropped = start_redirect(other_opts).await;
    assert_eq!(
        query_param(&dropped.authorization_url, "client_id").as_deref(),
        Some("client-2")
    );
    assert_eq!(server.registrations()[1]["redirect_uris"], json!([OTHER_ROUTE]));
    let state = McpOAuthStateStore::load(options.store.as_ref()).await.unwrap().unwrap();
    assert_eq!(state.client_information.unwrap().client_id, "client-2");
    assert!(state.tokens.is_none());
    server.close().await;

    // Does not refresh with a registered client that cimd replaces.
    let mut cimd_srv = AuthorizationServer::start(StartAuthServerOptions {
        cimd: true,
        iss_parameter: true,
    })
    .await;
    let mut base = sign_in_options(&cimd_srv);
    base.redirect_url = LOOPBACK.to_string();
    base.client_metadata_documents = Some(default_docs());

    let mut dcr = base.clone();
    dcr.settings.client_registration = Some(McpClientRegistration::Dcr);
    sign_in(dcr).await;
    assert_eq!(
        McpOAuthStateStore::load(base.store.as_ref())
            .await
            .unwrap()
            .unwrap()
            .client_information
            .unwrap()
            .client_id,
        "client-1"
    );

    let mut cimd = base.clone();
    cimd.settings.client_registration = Some(McpClientRegistration::Cimd);
    let auth = start_redirect(cimd).await;
    assert_eq!(
        query_param(&auth.authorization_url, "client_id").as_deref(),
        Some("https://app.example/oauth/client.json")
    );
    let grants: Vec<String> = cimd_srv
        .token_requests()
        .into_iter()
        .filter_map(|p| p.get("grant_type").cloned())
        .collect();
    assert_eq!(grants, vec!["authorization_code".to_string()]);
    let st = McpOAuthStateStore::load(base.store.as_ref()).await.unwrap().unwrap();
    assert!(st.client_information.is_none());
    assert!(st.tokens.is_none());
    cimd_srv.close().await;

    // Asks for granted scope + challenged scope on step-up without refreshing.
    let mut scope_srv = AuthorizationServer::start(Default::default()).await;
    let mut s_opts = sign_in_options(&scope_srv);
    s_opts.challenge = Some(OAuthChallenge {
        resource_metadata_url: None,
        scope: Some("issues:read".to_string()),
        error: None,
        error_description: None,
    });
    sign_in(s_opts.clone()).await;
    let mut step_up = s_opts.clone();
    step_up.challenge = Some(OAuthChallenge {
        resource_metadata_url: None,
        scope: Some("issues:write".to_string()),
        error: Some("insufficient_scope".to_string()),
        error_description: None,
    });
    let auth2 = sign_in(step_up).await;
    assert_eq!(auth2.scope.as_deref(), Some("issues:read issues:write"));
    let grants2: Vec<String> = scope_srv
        .token_requests()
        .into_iter()
        .filter_map(|p| p.get("grant_type").cloned())
        .collect();
    assert_eq!(
        grants2,
        vec!["authorization_code".to_string(), "authorization_code".to_string()]
    );
    assert_eq!(
        McpOAuthStateStore::load(s_opts.store.as_ref())
            .await
            .unwrap()
            .unwrap()
            .tokens
            .unwrap()
            .scope
            .as_deref(),
        Some("issues:read issues:write")
    );
    scope_srv.close().await;
}

#[tokio::test]
async fn handles_client_metadata_documents_across_discovery_and_refresh() {
    let mut server = AuthorizationServer::start(StartAuthServerOptions {
        cimd: true,
        iss_parameter: true,
    })
    .await;
    let mut opts = sign_in_options(&server);
    opts.redirect_url = LOOPBACK.to_string();
    opts.client_metadata_documents = Some(default_docs());
    let auth = sign_in(opts.clone()).await;
    assert_eq!(
        query_param(&auth.authorization_url, "client_id").as_deref(),
        Some("https://app.example/oauth/client.json")
    );
    assert_eq!(auth.redirect_url, LOOPBACK);
    assert!(server.registrations().is_empty());
    assert_eq!(
        server.token_requests()[0].get("client_id").map(String::as_str),
        Some("https://app.example/oauth/client.json")
    );
    assert!(
        McpOAuthStateStore::load(opts.store.as_ref())
            .await
            .unwrap()
            .unwrap()
            .client_information
            .is_none()
    );
    server.close().await;

    // Server-specific document and redirect URI when iss parameter is omitted.
    let mut no_iss_srv = AuthorizationServer::start(StartAuthServerOptions {
        cimd: true,
        iss_parameter: false,
    })
    .await;
    let mut no_iss_opts = sign_in_options(&no_iss_srv);
    no_iss_opts.redirect_url = LOOPBACK.to_string();
    no_iss_opts.client_metadata_documents = Some(default_docs());
    let auth2 = sign_in(no_iss_opts.clone()).await;
    let id = pi_js::b64::encode_url(&pi_js::crypto::sha256(no_iss_srv.server_url.as_bytes())[..9]);
    assert_eq!(
        query_param(&auth2.authorization_url, "client_id").as_deref(),
        Some(format!("https://app.example/oauth/{id}/client.json").as_str())
    );
    assert_eq!(auth2.redirect_url, format!("{LOOPBACK}/{id}"));
    assert_eq!(
        no_iss_srv.token_requests()[0].get("redirect_uri").map(String::as_str),
        Some(format!("{LOOPBACK}/{id}").as_str())
    );

    // Trailing slash on redirectUrl does not double the slash.
    no_iss_opts.store = Arc::new(MemoryServerStore::new());
    no_iss_opts.redirect_url = format!("{LOOPBACK}/");
    no_iss_opts.client_metadata_documents = Some(all_docs());
    let auth3 = start_redirect(no_iss_opts).await;
    assert_eq!(auth3.redirect_url, format!("{LOOPBACK}/{id}"));
    no_iss_srv.close().await;

    // Falls back to dynamic registration when unsupported, unlisted, docs missing, or clientName set.
    let mut no_cimd = AuthorizationServer::start(StartAuthServerOptions {
        cimd: false,
        iss_parameter: true,
    })
    .await;
    let mut nc_opts = sign_in_options(&no_cimd);
    nc_opts.redirect_url = LOOPBACK.to_string();
    nc_opts.client_metadata_documents = Some(default_docs());
    sign_in(nc_opts.clone()).await;
    assert_eq!(no_cimd.registrations().len(), 1);
    assert_eq!(
        no_cimd.authorizations()[0].get("client_id").map(String::as_str),
        Some("client-1")
    );

    // cimd explicit error when unsupported:
    let mut explicit_cimd = nc_opts;
    explicit_cimd.store = Arc::new(MemoryServerStore::new());
    explicit_cimd.settings.client_registration = Some(McpClientRegistration::Cimd);
    let unsupported_err = start_mcp_sign_in(explicit_cimd).await.unwrap_err();
    assert!(
        unsupported_err
            .to_string()
            .contains("does not support Client ID Metadata Documents")
    );
    no_cimd.close().await;

    // Route-only docs refresh for both auto and cimd; fails when unlisted with cimd.
    for reg in [McpClientRegistration::Auto, McpClientRegistration::Cimd] {
        let mut srv = AuthorizationServer::start(StartAuthServerOptions {
            cimd: true,
            iss_parameter: true,
        })
        .await;
        let mut r_opts = sign_in_options(&srv);
        r_opts.client_metadata_documents = Some(route_docs());
        r_opts.settings.client_registration = Some(reg);
        let auth_r = sign_in(r_opts.clone()).await;
        assert_eq!(
            query_param(&auth_r.authorization_url, "client_id").as_deref(),
            Some("https://app.example/oauth/client.json")
        );
        assert_eq!(
            start_mcp_sign_in(r_opts.clone()).await.unwrap(),
            McpSignInStart::Authorized
        );
        assert_eq!(
            srv.token_requests()[1].get("grant_type").map(String::as_str),
            Some("refresh_token")
        );
        assert_eq!(
            srv.token_requests()[1].get("client_id").map(String::as_str),
            Some("https://app.example/oauth/client.json")
        );
        assert_eq!(
            McpOAuthStateStore::load(r_opts.store.as_ref())
                .await
                .unwrap()
                .unwrap()
                .tokens
                .unwrap()
                .access_token,
            "access-2"
        );
        srv.close().await;
    }

    let mut unlisted_srv = AuthorizationServer::start(StartAuthServerOptions {
        cimd: true,
        iss_parameter: true,
    })
    .await;
    let mut bad_cimd = sign_in_options(&unlisted_srv);
    bad_cimd.client_metadata_documents = Some(default_docs());
    bad_cimd.settings.client_registration = Some(McpClientRegistration::Cimd);
    let err = start_mcp_sign_in(bad_cimd).await.unwrap_err();
    assert!(
        err.to_string()
            .contains("needs Client ID Metadata Documents that list the redirect URI")
    );
    assert!(unlisted_srv.registrations().is_empty());
    unlisted_srv.close().await;
}

#[tokio::test]
async fn cancels_start_complete_lock_wait_and_already_aborted_signal() {
    let mut server = AuthorizationServer::start(Default::default()).await;
    server.add_stall("/register");
    let ctrl = AbortController::new();
    let mut opts = sign_in_options(&server);
    opts.signal = Some(ctrl.signal());
    let handle = tokio::spawn(async move { start_mcp_sign_in(opts).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    ctrl.abort(None);
    let err = handle.await.unwrap().unwrap_err();
    assert!(err.downcast_ref::<McpSignInCancelledError>().is_some());
    server.close().await;

    let mut srv2 = AuthorizationServer::start(Default::default()).await;
    let opts2 = sign_in_options(&srv2);
    let auth2 = start_redirect(opts2.clone()).await;
    let resp2 = route_response(&authorize_in_browser(&auth2.authorization_url).await);
    srv2.add_stall("/token");
    let ctrl2 = AbortController::new();
    let mut opts2_abort = opts2;
    opts2_abort.signal = Some(ctrl2.signal());
    let handle2 = tokio::spawn(async move { complete_mcp_sign_in(opts2_abort, auth2, resp2).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    ctrl2.abort(None);
    let err2 = handle2.await.unwrap().unwrap_err();
    assert!(err2.downcast_ref::<McpSignInCancelledError>().is_some());
    srv2.close().await;

    // Cancels while another process holds the refresh lock and refreshes nothing.
    let mut srv3 = AuthorizationServer::start(Default::default()).await;
    let recording = Arc::new(RecordingStore::new(None));
    let mut opts3 = sign_in_options(&srv3);
    opts3.store = recording.clone();
    sign_in(opts3.clone()).await;

    let release = recording.hold();
    let ctrl3 = AbortController::new();
    opts3.signal = Some(ctrl3.signal());
    let handle3 = tokio::spawn(async move { start_mcp_sign_in(opts3).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    ctrl3.abort(None);
    let err3 = handle3.await.unwrap().unwrap_err();
    assert!(err3.downcast_ref::<McpSignInCancelledError>().is_some());

    release();
    recording.wait_lock_runs().await;
    assert!(!recording.is_locked());
    let grants3: Vec<String> = srv3
        .token_requests()
        .into_iter()
        .filter_map(|p| p.get("grant_type").cloned())
        .collect();
    assert_eq!(grants3, vec!["authorization_code".to_string()]);
    assert_eq!(srv3.authorizations().len(), 1);
    srv3.close().await;

    // Fails at once with an already-aborted signal.
    let mut srv4 = AuthorizationServer::start(Default::default()).await;
    let ctrl4 = AbortController::new();
    ctrl4.abort(None);
    let mut opts4 = sign_in_options(&srv4);
    opts4.signal = Some(ctrl4.signal());
    let err4 = start_mcp_sign_in(opts4).await.unwrap_err();
    assert!(err4.downcast_ref::<McpSignInCancelledError>().is_some());
    assert!(srv4.registrations().is_empty());
    srv4.close().await;
}
