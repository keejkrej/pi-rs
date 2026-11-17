use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame, Incoming};
use hyper::header::{HeaderName, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{HeaderMap, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use pi_js::BoxFuture;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

#[derive(Clone, Debug)]
pub struct TestRequest {
    pub method: String,
    pub uri: String,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl TestRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn body_json(&self) -> pi_js::Result<Value> {
        pi_js::json::parse(&self.body_text())
    }
}

pub enum TestBody {
    Bytes(Bytes),
    Stream(mpsc::UnboundedReceiver<Bytes>),
}

pub struct TestResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: TestBody,
    pub keepalive: Option<mpsc::UnboundedSender<Bytes>>,
}

impl TestResponse {
    pub fn empty(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: TestBody::Bytes(Bytes::new()),
            keepalive: None,
        }
    }

    pub fn text(status: u16, body: impl Into<Bytes>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: TestBody::Bytes(body.into()),
            keepalive: None,
        }
    }

    pub fn with_headers(status: u16, headers: &[(&str, &str)], body: impl Into<Bytes>) -> Self {
        Self {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            body: TestBody::Bytes(body.into()),
            keepalive: None,
        }
    }

    pub fn json(status: u16, value: &Value) -> Self {
        Self::json_with_headers(status, &[], value)
    }

    pub fn json_with_headers(status: u16, headers: &[(&str, &str)], value: &Value) -> Self {
        let mut all_headers = vec![("content-type".to_string(), "application/json".to_string())];
        for (k, v) in headers {
            all_headers.push(((*k).to_string(), (*v).to_string()));
        }
        let body = pi_js::json::stringify(value);
        Self {
            status,
            headers: all_headers,
            body: TestBody::Bytes(Bytes::from(body)),
            keepalive: None,
        }
    }

    pub fn redirect(status: u16, location: &str) -> Self {
        Self::with_headers(status, &[("location", location)], Bytes::new())
    }

    pub fn sse_end(status: u16, body: impl Into<Bytes>) -> Self {
        Self::with_headers(status, &[("content-type", "text/event-stream")], body)
    }

    pub fn sse_open(status: u16, initial_chunk: impl Into<Bytes>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let _ = tx.send(initial_chunk.into());
        Self {
            status,
            headers: vec![("content-type".to_string(), "text/event-stream".to_string())],
            body: TestBody::Stream(rx),
            keepalive: Some(tx),
        }
    }

    pub fn stream(status: u16, headers: &[(&str, &str)], rx: mpsc::UnboundedReceiver<Bytes>) -> Self {
        Self {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            body: TestBody::Stream(rx),
            keepalive: None,
        }
    }
}

struct ChannelBody {
    rx: mpsc::UnboundedReceiver<Bytes>,
}

impl Body for ChannelBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<Frame<Self::Data>, Self::Error>>> {
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(chunk)) => Poll::Ready(Some(Ok(Frame::data(chunk)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub type TestHandler = Arc<dyn Fn(TestRequest, String) -> BoxFuture<TestResponse> + Send + Sync>;

struct ServerEntry {
    shutdown: watch::Sender<bool>,
    accept_handle: JoinHandle<()>,
    conn_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
    keepalives: Arc<Mutex<Vec<mpsc::UnboundedSender<Bytes>>>>,
}

pub struct TestServers {
    servers: Vec<ServerEntry>,
}

impl Default for TestServers {
    fn default() -> Self {
        Self::new()
    }
}

impl TestServers {
    pub fn new() -> Self {
        Self { servers: Vec::new() }
    }

    pub async fn listen<F>(&mut self, handler: F) -> String
    where
        F: Fn(TestRequest, String) -> BoxFuture<TestResponse> + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback test listener");
        let addr: SocketAddr = listener.local_addr().expect("listener local_addr");
        let origin = format!("http://127.0.0.1:{}", addr.port());
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let conn_handles: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
        let keepalives: Arc<Mutex<Vec<mpsc::UnboundedSender<Bytes>>>> = Arc::new(Mutex::new(Vec::new()));
        let handler: TestHandler = Arc::new(handler);

        let origin_clone = origin.clone();
        let conn_handles_clone = conn_handles.clone();
        let keepalives_clone = keepalives.clone();
        let accept_handle = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    biased;
                    _ = shutdown_rx.changed() => break,
                    res = listener.accept() => res,
                };
                let Ok((stream, _)) = accepted else {
                    break;
                };
                let handler = handler.clone();
                let origin = origin_clone.clone();
                let mut conn_shutdown = shutdown_rx.clone();
                let keepalives = keepalives_clone.clone();
                let conn_handle = tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let service = service_fn(move |req: Request<Incoming>| {
                        let handler = handler.clone();
                        let origin = origin.clone();
                        let keepalives = keepalives.clone();
                        async move {
                            let method = req.method().as_str().to_string();
                            let uri = req.uri().to_string();
                            let headers = req.headers().clone();
                            let body = req
                                .into_body()
                                .collect()
                                .await
                                .map(|c| c.to_bytes())
                                .unwrap_or_default();
                            let test_req = TestRequest {
                                method,
                                uri,
                                headers,
                                body,
                            };
                            let resp = handler(test_req, origin).await;
                            Ok::<_, Infallible>(into_hyper_response(resp, &keepalives))
                        }
                    });
                    let conn = http1::Builder::new().serve_connection(io, service);
                    tokio::select! {
                        _ = conn => {}
                        _ = conn_shutdown.changed() => {}
                    }
                });
                conn_handles_clone
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(conn_handle);
            }
        });

        self.servers.push(ServerEntry {
            shutdown: shutdown_tx,
            accept_handle,
            conn_handles,
            keepalives,
        });
        origin
    }

    pub async fn close(&mut self) {
        let entries = std::mem::take(&mut self.servers);
        for entry in entries {
            let _ = entry.shutdown.send(true);
            entry.keepalives.lock().unwrap_or_else(|e| e.into_inner()).clear();
            let _ = entry.accept_handle.await;
            let conns = std::mem::take(&mut *entry.conn_handles.lock().unwrap_or_else(|e| e.into_inner()));
            for handle in conns {
                let _ = handle.await;
            }
        }
    }
}

impl Drop for TestServers {
    fn drop(&mut self) {
        for entry in self.servers.drain(..) {
            let _ = entry.shutdown.send(true);
            entry.keepalives.lock().unwrap_or_else(|e| e.into_inner()).clear();
            entry.accept_handle.abort();
            let conns = std::mem::take(&mut *entry.conn_handles.lock().unwrap_or_else(|e| e.into_inner()));
            for handle in conns {
                handle.abort();
            }
        }
    }
}

fn into_hyper_response(
    resp: TestResponse,
    keepalives: &Arc<Mutex<Vec<mpsc::UnboundedSender<Bytes>>>>,
) -> Response<BoxBody<Bytes, Infallible>> {
    if let Some(tx) = resp.keepalive {
        keepalives.lock().unwrap_or_else(|e| e.into_inner()).push(tx);
    }
    let body: BoxBody<Bytes, Infallible> = match resp.body {
        TestBody::Bytes(bytes) => Full::new(bytes).boxed(),
        TestBody::Stream(rx) => ChannelBody { rx }.boxed(),
    };
    let mut builder =
        Response::builder().status(StatusCode::from_u16(resp.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR));
    for (k, v) in resp.headers {
        if let (Ok(name), Ok(val)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(&v)) {
            builder = builder.header(name, val);
        }
    }
    builder
        .body(body)
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()).boxed()))
}
