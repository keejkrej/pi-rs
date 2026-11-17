//! Port of packages/evals/test/acme-server.test.ts

use pi_evals::evals::acme_server::{
    AcmeServer, AcmeServerMode, OPENAI_MODEL_ID, OPENAI_PROBE_PROMPT, OPENAI_PROBE_RESPONSE, STREAM_MODEL_ID,
    STREAM_PROBE_PROMPT, STREAM_PROBE_RESPONSE, create_acme_server,
};
use pi_js::fetch::{Body, Headers, Request, fetch};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

#[derive(Clone, Copy)]
struct Fixture {
    mode: AcmeServerMode,
    auth_header: (&'static str, &'static str),
    bad_auth_header: (&'static str, &'static str),
    model: &'static str,
    prompt: &'static str,
    response: &'static str,
    content_type: &'static str,
    unauthorized: &'static str,
    invalid: &'static str,
}

impl Fixture {
    fn url(&self, server: &AcmeServer) -> String {
        match self.mode {
            AcmeServerMode::OpenAi => format!("{}/chat/completions", server.base_url()),
            AcmeServerMode::Stream => format!("{}/generate", server.origin()),
        }
    }
}

const FIXTURES: [Fixture; 2] = [
    Fixture {
        mode: AcmeServerMode::OpenAi,
        auth_header: ("authorization", "Bearer resolved-acme-key"),
        bad_auth_header: ("authorization", "Bearer wrong-key"),
        model: OPENAI_MODEL_ID,
        prompt: OPENAI_PROBE_PROMPT,
        response: OPENAI_PROBE_RESPONSE,
        content_type: "text/event-stream",
        unauthorized: "Invalid Acme credential",
        invalid: "Invalid OpenAI-compatible request",
    },
    Fixture {
        mode: AcmeServerMode::Stream,
        auth_header: ("x-acme-key", "resolved-stream-key"),
        bad_auth_header: ("x-acme-key", "wrong-key"),
        model: STREAM_MODEL_ID,
        prompt: STREAM_PROBE_PROMPT,
        response: STREAM_PROBE_RESPONSE,
        content_type: "application/x-ndjson",
        unauthorized: "Invalid Acme Stream credential",
        invalid: "Invalid Acme Stream request",
    },
];

fn payload_body(model: &str, prompt: &str, stream: bool) -> Value {
    json!({
        "model": model,
        "messages": [{ "role": "user", "content": prompt }],
        "stream": stream,
    })
}

async fn post(url: String, auth: (&str, &str), payload: Value) -> pi_js::fetch::Response {
    let mut headers = Headers::new();
    headers.set("content-type", "application/json");
    headers.set(auth.0, auth.1);
    fetch(Request {
        method: "POST".to_string(),
        url,
        headers,
        body: Some(Body::Text(pi_js::json::stringify(&payload))),
        signal: None,
    })
    .await
    .unwrap()
}

fn streamed_text(mode: AcmeServerMode, raw: &str) -> String {
    if mode == AcmeServerMode::Stream {
        return raw
            .split('\n')
            .filter(|line| !line.is_empty())
            .filter_map(|line| {
                let ev = pi_js::json::parse(line).ok()?;
                if ev.get("type").and_then(Value::as_str) == Some("text_delta") {
                    ev.get("text").and_then(Value::as_str).map(ToString::to_string)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("");
    }
    raw.split('\n')
        .filter(|line| line.starts_with("data: ") && *line != "data: [DONE]")
        .filter_map(|line| {
            let chunk = pi_js::json::parse(&line["data: ".len()..]).ok()?;
            chunk
                .get("choices")?
                .get(0)?
                .get("delta")?
                .get("content")?
                .as_str()
                .map(ToString::to_string)
        })
        .collect::<Vec<_>>()
        .join("")
}

#[tokio::test]
async fn acme_server_fixtures_behave_deterministically() {
    for fixture in FIXTURES {
        let server = create_acme_server(fixture.mode);
        server.start().await.unwrap();

        // 1. Accepts the probe request and records it
        server.reset();
        let response = post(
            fixture.url(&server),
            fixture.auth_header,
            payload_body(fixture.model, fixture.prompt, true),
        )
        .await;
        assert_eq!(response.status(), 200);
        let ct = response.headers().get("content-type").unwrap_or_default();
        assert!(ct.contains(fixture.content_type), "unexpected content-type: {ct}");
        let body_text = response.text().await.unwrap();
        assert_eq!(streamed_text(fixture.mode, &body_text), fixture.response);
        assert!(server.valid_request_received());

        // 2. Rejects a bad credential with 401 and does not record a probe
        server.reset();
        let response = post(
            fixture.url(&server),
            fixture.bad_auth_header,
            payload_body(fixture.model, fixture.prompt, true),
        )
        .await;
        assert_eq!(response.status(), 401);
        assert_eq!(response.text().await.unwrap(), fixture.unauthorized);
        assert!(!server.valid_request_received());

        // 3. Rejects a malformed request with 422 and does not record a probe
        server.reset();
        let response = post(
            fixture.url(&server),
            fixture.auth_header,
            payload_body(fixture.model, fixture.prompt, false),
        )
        .await;
        assert_eq!(response.status(), 422);
        assert_eq!(response.text().await.unwrap(), fixture.invalid);
        assert!(!server.valid_request_received());

        // 4. Does not treat a non-probe success as the probe
        server.reset();
        let response = post(
            fixture.url(&server),
            fixture.auth_header,
            payload_body(fixture.model, "hello", true),
        )
        .await;
        assert_eq!(response.status(), 200);
        assert!(!server.valid_request_received());

        // 5. Clears the probe flag on reset
        server.reset();
        post(
            fixture.url(&server),
            fixture.auth_header,
            payload_body(fixture.model, fixture.prompt, true),
        )
        .await;
        assert!(server.valid_request_received());
        server.reset();
        assert!(!server.valid_request_received());

        server.stop().await.unwrap();
    }
}
