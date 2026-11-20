use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use indexmap::IndexMap;
use pi_mcp::client::ClientState;
use pi_mcp::transports::pipe::{McpPipe, McpPipeHandlers, PipeOpenFn, PipeTransport, PipeTransportOptions};
use pi_mcp::{JsonRpcMessage, McpClient, McpClientOptions, McpConnectionClosedError, McpTransport, Tool};
use pretty_assertions::assert_eq;
use serde_json::{from_value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const FIXTURE: &str = env!("CARGO_BIN_EXE_fixture-stdio-server");

#[derive(Clone)]
struct FakePipe {
    written: Arc<Mutex<Vec<String>>>,
    calls: Arc<Mutex<Vec<String>>>,
    exited_tx: Arc<Mutex<Option<tokio::sync::oneshot::Sender<pi_js::Result<()>>>>>,
    exited_rx: Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<pi_js::Result<()>>>>>,
}

impl FakePipe {
    fn new() -> Self {
        let (tx, rx) = tokio::sync::oneshot::channel();
        Self {
            written: Arc::new(Mutex::new(Vec::new())),
            calls: Arc::new(Mutex::new(Vec::new())),
            exited_tx: Arc::new(Mutex::new(Some(tx))),
            exited_rx: Arc::new(tokio::sync::Mutex::new(Some(rx))),
        }
    }

    fn resolve(&self) {
        if let Some(tx) = self.exited_tx.lock().unwrap().take() {
            let _ = tx.send(Ok(()));
        }
    }

    fn reject(&self, msg: &str) {
        if let Some(tx) = self.exited_tx.lock().unwrap().take() {
            let _ = tx.send(Err(pi_js::Error::msg(msg)));
        }
    }
}

#[async_trait::async_trait]
impl McpPipe for FakePipe {
    fn write(&self, data: &[u8]) {
        self.written
            .lock()
            .unwrap()
            .push(String::from_utf8_lossy(data).into_owned());
    }

    fn close_stdin(&self) {
        self.calls.lock().unwrap().push("closeStdin".to_string());
    }

    fn close(&self) {
        self.calls.lock().unwrap().push("close".to_string());
        self.reject("aborted");
    }

    async fn exited(&self) -> pi_js::Result<()> {
        let rx = self.exited_rx.lock().await.take();
        match rx {
            Some(rx) => rx.await.unwrap_or(Ok(())),
            None => Ok(()),
        }
    }
}

struct FakeProgram {
    transport: PipeTransport,
    pipe: FakePipe,
    handlers: McpPipeHandlers,
    messages: Arc<Mutex<Vec<JsonRpcMessage>>>,
    errors: Arc<Mutex<Vec<String>>>,
    closed: Arc<Mutex<bool>>,
}

impl FakeProgram {
    fn stdout(&self, text: &str) {
        (self.handlers.on_stdout)(text.as_bytes());
    }

    fn stderr(&self, text: &str) {
        (self.handlers.on_stderr)(text.as_bytes());
    }

    fn exit(&self) {
        self.pipe.resolve();
    }

    fn fail(&self, msg: &str) {
        self.pipe.reject(msg);
    }

    fn calls(&self) -> Vec<String> {
        self.pipe.calls.lock().unwrap().clone()
    }

    fn written(&self) -> Vec<String> {
        self.pipe.written.lock().unwrap().clone()
    }

    fn messages(&self) -> Vec<JsonRpcMessage> {
        self.messages.lock().unwrap().clone()
    }

    fn errors(&self) -> Vec<String> {
        self.errors.lock().unwrap().clone()
    }

    fn is_closed(&self) -> bool {
        *self.closed.lock().unwrap()
    }
}

async fn start_fake(
    max_message_bytes: Option<i64>,
    max_stderr_bytes: Option<i64>,
    on_stderr: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    close_timeout_ms: Option<i64>,
) -> FakeProgram {
    let pipe = FakePipe::new();
    let pipe_for_open = Arc::new(pipe.clone());
    let handlers_slot = Arc::new(Mutex::new(None::<McpPipeHandlers>));
    let handlers_writer = handlers_slot.clone();

    let open: PipeOpenFn = Arc::new(move |h| {
        *handlers_writer.lock().unwrap() = Some(h);
        let p = pipe_for_open.clone();
        Box::pin(async move { Ok(p as Arc<dyn McpPipe>) })
    });

    let transport = PipeTransport::new(PipeTransportOptions {
        open,
        max_message_bytes,
        max_stderr_bytes,
        on_stderr,
        close_timeout_ms,
    });

    let messages = Arc::new(Mutex::new(Vec::new()));
    let errors = Arc::new(Mutex::new(Vec::new()));
    let closed = Arc::new(Mutex::new(false));
    let m_cb = messages.clone();
    let e_cb = errors.clone();
    let c_cb = closed.clone();
    let _u1 = transport.on_message(Arc::new(move |m| m_cb.lock().unwrap().push(m.clone())));
    let _u2 = transport.on_error(Arc::new(move |e| e_cb.lock().unwrap().push(e.to_string())));
    let _u3 = transport.on_close(Arc::new(move || *c_cb.lock().unwrap() = true));

    transport.start().await.expect("start");
    let handlers = handlers_slot.lock().unwrap().take().expect("handlers set");
    FakeProgram {
        transport,
        pipe,
        handlers,
        messages,
        errors,
        closed,
    }
}

fn ping(id: i64) -> JsonRpcMessage {
    from_value(json!({ "jsonrpc": "2.0", "id": id, "method": "ping" })).unwrap()
}

async fn settle() {
    for _ in 0..5 {
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

#[tokio::test]
async fn pipe_framing_splits_joins_crlf_invalid_and_oversized() {
    let program = start_fake(None, None, None, None).await;
    let first = pi_js::json::stringify(&ping(1));
    let second = pi_js::json::stringify(&ping(2));
    let third = pi_js::json::stringify(&ping(3));
    program.stdout(&first[..10]);
    program.stdout(&format!("{}\n{second}", &first[10..]));
    program.stdout(&format!("\n{third}\n"));
    assert_eq!(program.messages(), vec![ping(1), ping(2), ping(3)]);
    assert!(program.errors().is_empty());

    let p2 = start_fake(None, None, None, None).await;
    p2.stdout(&format!("\n\r\n   \n{}\r\n\n", pi_js::json::stringify(&ping(1))));
    assert_eq!(p2.messages(), vec![ping(1)]);
    assert!(p2.errors().is_empty());

    let p3 = start_fake(None, None, None, None).await;
    p3.stdout(&format!(
        "not json\n{{\"jsonrpc\":\"1.0\"}}\n{}\n",
        pi_js::json::stringify(&ping(1))
    ));
    assert_eq!(p3.messages(), vec![ping(1)]);
    assert_eq!(p3.errors().len(), 2);

    let p4 = start_fake(Some(48), None, None, None).await;
    let oversized = pi_js::json::stringify(&json!({ "jsonrpc": "2.0", "method": "x".repeat(48) }));
    p4.stdout(&format!("{oversized}\n{}\n", pi_js::json::stringify(&ping(1))));
    p4.stdout(&"y".repeat(49));
    p4.stdout(&format!("\n{}\n", pi_js::json::stringify(&ping(2))));
    assert_eq!(p4.messages(), vec![ping(1), ping(2)]);
    assert_eq!(
        p4.errors(),
        vec![
            "MCP stdio message exceeds 48 bytes".to_string(),
            "MCP stdio message exceeds 48 bytes".to_string(),
        ]
    );
}

#[tokio::test]
async fn pipe_keeps_bounded_stderr_tail_and_passes_every_chunk_to_on_stderr() {
    let chunks = Arc::new(Mutex::new(Vec::<String>::new()));
    let cb = chunks.clone();
    let program = start_fake(
        None,
        Some(8),
        Some(Arc::new(move |c| cb.lock().unwrap().push(c.to_string()))),
        None,
    )
    .await;
    program.stderr("starting\n");
    program.stderr("ready\n");
    assert_eq!(
        *chunks.lock().unwrap(),
        vec!["starting\n".to_string(), "ready\n".to_string()]
    );
    assert_eq!(program.transport.stderr(), "g\nready\n");
}

#[tokio::test]
async fn pipe_writes_each_message_as_one_line_and_handles_exit_errors() {
    let program = start_fake(None, None, None, None).await;
    program.transport.send(ping(1)).await.unwrap();
    assert_eq!(
        program.written(),
        vec![format!("{}\n", pi_js::json::stringify(&ping(1)))]
    );

    let p_incomplete = start_fake(None, None, None, None).await;
    p_incomplete.stdout(r#"{"jsonrpc":"2.0""#);
    p_incomplete.exit();
    settle().await;
    assert_eq!(
        p_incomplete.errors(),
        vec!["MCP stdio server closed with an incomplete JSON-RPC message".to_string()]
    );
    assert!(p_incomplete.is_closed());

    let p_fail = start_fake(None, None, None, None).await;
    p_fail.fail("connection lost");
    settle().await;
    assert_eq!(p_fail.errors(), vec!["connection lost".to_string()]);
    assert!(p_fail.is_closed());
}

#[tokio::test]
async fn pipe_rejects_send_after_close_and_exit() {
    let closed_by_us = start_fake(None, None, None, None).await;
    closed_by_us.exit();
    closed_by_us.transport.close().await.unwrap();
    assert!(
        closed_by_us
            .transport
            .send(ping(1))
            .await
            .unwrap_err()
            .downcast_ref::<McpConnectionClosedError>()
            .is_some()
    );

    let ended = start_fake(None, None, None, None).await;
    ended.exit();
    settle().await;
    assert!(
        ended
            .transport
            .send(ping(1))
            .await
            .unwrap_err()
            .downcast_ref::<McpConnectionClosedError>()
            .is_some()
    );
    assert!(ended.written().is_empty());
}

#[tokio::test]
async fn pipe_closes_stdin_gracefully_and_idempotently() {
    let program = start_fake(None, None, None, None).await;
    let transport_close = program.transport.clone();
    let closing = tokio::spawn(async move { transport_close.close().await });
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_eq!(program.calls(), vec!["closeStdin".to_string()]);
    assert!(
        program
            .transport
            .send(ping(1))
            .await
            .unwrap_err()
            .downcast_ref::<McpConnectionClosedError>()
            .is_some()
    );
    assert!(program.written().is_empty());
    program.exit();
    closing.await.unwrap().unwrap();
    assert_eq!(program.calls(), vec!["closeStdin".to_string()]);
    assert!(program.is_closed());
    assert!(program.errors().is_empty());

    let p2 = start_fake(None, None, None, None).await;
    let t1 = p2.transport.clone();
    let t2 = p2.transport.clone();
    let c1 = tokio::spawn(async move { t1.close().await });
    let c2 = tokio::spawn(async move { t2.close().await });
    tokio::time::sleep(Duration::from_millis(5)).await;
    p2.exit();
    c1.await.unwrap().unwrap();
    c2.await.unwrap().unwrap();
    p2.transport.close().await.unwrap();
    assert_eq!(p2.calls(), vec!["closeStdin".to_string()]);
    assert!(p2.is_closed());
}

#[tokio::test(start_paused = true)]
async fn pipe_kills_a_program_only_after_close_timeout_ms() {
    let program = start_fake(None, None, None, Some(1_000)).await;
    let transport_close = program.transport.clone();
    let closing = tokio::spawn(async move { transport_close.close().await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(999)).await;
    tokio::task::yield_now().await;
    assert_eq!(program.calls(), vec!["closeStdin".to_string()]);

    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    closing.await.unwrap().unwrap();
    assert_eq!(program.calls(), vec!["closeStdin".to_string(), "close".to_string()]);
    assert!(program.is_closed());
    assert!(program.errors().is_empty());
}

#[tokio::test]
async fn pipe_handles_failed_open_and_close_while_starting() {
    let err_transport = PipeTransport::new(PipeTransportOptions {
        open: Arc::new(|_| Box::pin(async { Err(pi_js::Error::msg("not_supported")) })),
        max_message_bytes: None,
        max_stderr_bytes: None,
        on_stderr: None,
        close_timeout_ms: None,
    });
    assert!(
        err_transport
            .start()
            .await
            .unwrap_err()
            .to_string()
            .contains("not_supported")
    );

    let (gate_tx, gate_rx) = tokio::sync::watch::channel(false);
    let pipe = FakePipe::new();
    let pipe_clone = pipe.clone();
    let transport = PipeTransport::new(PipeTransportOptions {
        open: Arc::new(move |_| {
            let p = Arc::new(pipe_clone.clone()) as Arc<dyn McpPipe>;
            let mut rx = gate_rx.clone();
            Box::pin(async move {
                while !*rx.borrow() {
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
                Ok(p)
            })
        }),
        max_message_bytes: None,
        max_stderr_bytes: None,
        on_stderr: None,
        close_timeout_ms: None,
    });
    let starting = tokio::spawn({
        let t = transport.clone();
        async move { t.start().await }
    });
    tokio::task::yield_now().await;
    transport.close().await.unwrap();
    let _ = gate_tx.send(true);
    let start_err = starting.await.unwrap().unwrap_err();
    assert!(start_err.downcast_ref::<McpConnectionClosedError>().is_some());
    assert_eq!(*pipe.calls.lock().unwrap(), vec!["close".to_string()]);
}

struct ChildPipe {
    stdin: Arc<Mutex<Option<tokio::process::ChildStdin>>>,
    child: Arc<Mutex<Option<tokio::process::Child>>>,
    exited_rx: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<pi_js::Result<()>>>>,
}

#[async_trait::async_trait]
impl McpPipe for ChildPipe {
    fn write(&self, data: &[u8]) {
        if let Some(mut stdin) = self.stdin.lock().unwrap().take() {
            let slot = self.stdin.clone();
            let bytes = data.to_vec();
            // Serialized best-effort write on tokio runtime for the test fixture.
            tokio::spawn(async move {
                let _ = stdin.write_all(&bytes).await;
                let _ = stdin.flush().await;
                *slot.lock().unwrap() = Some(stdin);
            });
        }
    }

    fn close_stdin(&self) {
        let _ = self.stdin.lock().unwrap().take();
    }

    fn close(&self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.start_kill();
        }
    }

    async fn exited(&self) -> pi_js::Result<()> {
        let rx = self.exited_rx.lock().await.take();
        match rx {
            Some(rx) => rx.await.unwrap_or(Ok(())),
            None => Ok(()),
        }
    }
}

fn spawn_pipe(
    command: String,
    handlers: McpPipeHandlers,
) -> Pin<Box<dyn Future<Output = pi_js::Result<Arc<dyn McpPipe>>> + Send>> {
    Box::pin(async move {
        let mut child = tokio::process::Command::new(&command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| pi_js::Error::from_io(e, "spawn", Some(&command), None))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdout_task = stdout.map(|mut out| {
            let on_stdout = handlers.on_stdout.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    match out.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => on_stdout(&buf[..n]),
                    }
                }
            })
        });
        let stderr_task = stderr.map(|mut err| {
            let on_stderr = handlers.on_stderr.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    match err.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => on_stderr(&buf[..n]),
                    }
                }
            })
        });

        let (tx, rx) = tokio::sync::oneshot::channel();
        let child_slot = Arc::new(Mutex::new(Some(child)));
        let child_waiter = child_slot.clone();
        tokio::spawn(async move {
            loop {
                let exited = {
                    let mut guard = child_waiter.lock().unwrap();
                    match guard.as_mut() {
                        Some(c) => matches!(c.try_wait(), Ok(Some(_)) | Err(_)),
                        None => true,
                    }
                };
                if exited {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            if let Some(t) = stdout_task {
                let _ = t.await;
            }
            if let Some(t) = stderr_task {
                let _ = t.await;
            }
            let _ = tx.send(Ok(()));
        });

        Ok(Arc::new(ChildPipe {
            stdin: Arc::new(Mutex::new(stdin)),
            child: child_slot,
            exited_rx: tokio::sync::Mutex::new(Some(rx)),
        }) as Arc<dyn McpPipe>)
    })
}

#[tokio::test]
async fn pipe_transport_with_real_program_runs_initialize_list_and_call() {
    let transport = Arc::new(PipeTransport::new(PipeTransportOptions {
        open: Arc::new(|handlers| spawn_pipe(FIXTURE.to_string(), handlers)),
        max_message_bytes: None,
        max_stderr_bytes: None,
        on_stderr: None,
        close_timeout_ms: None,
    }));
    let client = McpClient::new(McpClientOptions {
        name: "pipe-test".to_string(),
        version: "1.0.0".to_string(),
        title: None,
        capabilities: None,
        protocol_version: None,
        request_timeout_ms: None,
        roots: None,
    });
    let init = client.connect(transport.clone()).await.expect("connect");
    assert_eq!(init.server_info.name, "stdio-fixture");
    assert_eq!(init.server_info.version, "1.0.0");

    let tools = client.list_tools(None).await.expect("list_tools");
    let expected_tools: Vec<Tool> =
        from_value(json!([{ "name": "echo", "inputSchema": { "type": "object" } }])).unwrap();
    assert_eq!(tools, expected_tools);

    let mut args = IndexMap::new();
    args.insert("text".to_string(), json!("hello"));
    let res = client.call_tool("echo", Some(args), None).await.expect("call_tool");
    assert_eq!(
        res,
        from_value(json!({ "content": [{ "type": "text", "text": "hello" }] })).unwrap()
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(transport.stderr().contains("stdio fixture ready"));
    client.close().await.expect("close");
    assert_eq!(client.connection_state(), ClientState::Closed);
}
