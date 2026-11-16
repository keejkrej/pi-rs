//! Port of packages/ai/test/bedrock-response-headers.test.ts

use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::Empty;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use pi_ai::api::bedrock_converse_stream::{BedrockOptions, stream_with_options};
use pi_ai::providers::amazon_bedrock_models::amazon_bedrock_models;
use pi_ai::types::{
    CacheRetention, Context, Message, ProviderEnv, ProviderResponse, StopReason, StreamOptions, UserMessage,
    UserMessageContent,
};
use pi_ai::utils::transcript::normalize_context;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

const MODEL_ID: &str = "us.anthropic.claude-haiku-4-5-20251001-v1:0";

struct TestServer {
    base_url: String,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl TestServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback test listener");
        let addr = listener.local_addr().expect("listener local addr");
        let (tx, mut rx) = oneshot::channel::<()>();

        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _peer)) = accepted else {
                            break;
                        };
                        tokio::spawn(async move {
                            let io = TokioIo::new(stream);
                            let svc = service_fn(|_req: Request<hyper::body::Incoming>| async move {
                                let resp = Response::builder()
                                    .status(200)
                                    .header("content-type", "application/vnd.amazon.eventstream")
                                    .header("x-bifrost-provider", "bedrock")
                                    .header("x-bifrost-resolved-model", MODEL_ID)
                                    .header("x-amzn-requestid", "req-123")
                                    .body(Empty::<Bytes>::new())
                                    .expect("valid HTTP response");
                                Ok::<_, Infallible>(resp)
                            });
                            let _ = http1::Builder::new()
                                .keep_alive(false)
                                .serve_connection(io, svc)
                                .await;
                        });
                    }
                }
            }
        });

        Self {
            base_url: format!("http://127.0.0.1:{}", addr.port()),
            shutdown: Some(tx),
            task: Some(task),
        }
    }

    async fn close(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

#[tokio::test]
async fn forwards_raw_smithy_response_headers_to_on_response() {
    let server = TestServer::start().await;
    let mut model = amazon_bedrock_models()
        .get(MODEL_ID)
        .expect("haiku model fixture")
        .clone();
    model.base_url = server.base_url.clone();

    let responses: Arc<Mutex<Vec<ProviderResponse>>> = Arc::new(Mutex::new(Vec::new()));
    let responses_ref = Arc::clone(&responses);

    let mut env = ProviderEnv::new();
    env.insert("AWS_BEDROCK_FORCE_HTTP1".to_string(), "1".to_string());
    env.insert("AWS_BEDROCK_SKIP_AUTH".to_string(), "1".to_string());
    env.insert("NO_PROXY".to_string(), "127.0.0.1,localhost".to_string());

    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hello".to_string()),
            timestamp: pi_js::time::now_ms(),
        })],
        tools: None,
    });

    let options = BedrockOptions {
        base: StreamOptions {
            cache_retention: Some(CacheRetention::None),
            env: Some(env),
            on_response: Some(Arc::new(move |response, _model| {
                let responses_inner = Arc::clone(&responses_ref);
                Box::pin(async move {
                    responses_inner.lock().unwrap().push(response);
                    Ok(())
                })
            })),
            ..StreamOptions::default()
        },
        ..BedrockOptions::default()
    };

    let result = stream_with_options(&model, &context, Some(options)).result().await;
    server.close().await;

    // The fake server intentionally returns an empty event stream; this assertion
    // documents that the header callback still fires before stream consumption.
    assert_eq!(result.stop_reason, StopReason::Error);
    let captured = responses.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].status, 200);
    assert_eq!(
        captured[0].headers.get("x-amzn-requestid").map(String::as_str),
        Some("req-123")
    );
    assert_eq!(
        captured[0].headers.get("x-bifrost-provider").map(String::as_str),
        Some("bedrock")
    );
    assert_eq!(
        captured[0].headers.get("x-bifrost-resolved-model").map(String::as_str),
        Some(MODEL_ID)
    );
}
