//! Port of packages/server/src/testing/client.ts

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_chord::ServiceCall;
use pi_js::{Error, Result};
use pi_protocol::protocol::ServerTarget;
use pi_protocol::{
    ClientHello, ClientMessage, PROTOCOL_VERSION, RequestEnvelope, ResponseEnvelope, RpcTarget, ServerMessage,
    ServerMessageDecoder, encode_client_message,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{oneshot, watch};

use crate::errors::clone_js_error;
use crate::transports::unix::listener::connect_unix_stream_socket;

struct MessageWaiter {
    predicate: Box<dyn Fn(&ServerMessage) -> bool + Send + Sync>,
    tx: oneshot::Sender<Result<ServerMessage>>,
}

#[async_trait]
pub trait WireChannel: Send + Sync {
    async fn send(&self, chunk: &[u8]) -> Result<()>;
    async fn send_fragmented(&self, chunk: &[u8], split_at: usize) -> Result<()>;
    async fn close(&self) -> Result<()>;
}

struct AttachmentRoute {
    session_id: String,
    attachment_id: String,
}

struct ClientMutableState {
    messages: Vec<ServerMessage>,
    waiters: Vec<MessageWaiter>,
    attachment: Option<AttachmentRoute>,
    closed: bool,
}

struct ProtocolTestClientInner {
    channel: Arc<dyn WireChannel>,
    decoder: ServerMessageDecoder,
    request_sequence: AtomicUsize,
    closed_tx: watch::Sender<bool>,
    closed_rx: watch::Receiver<bool>,
    state: Mutex<ClientMutableState>,
}

#[derive(Clone)]
pub struct ProtocolTestClient {
    inner: Arc<ProtocolTestClientInner>,
}

enum PreparedWait {
    Immediate(ServerMessage),
    Pending(oneshot::Receiver<Result<ServerMessage>>),
}

impl ProtocolTestClient {
    pub fn new(channel: Arc<dyn WireChannel>) -> Self {
        let (closed_tx, closed_rx) = watch::channel(false);
        Self {
            inner: Arc::new(ProtocolTestClientInner {
                channel,
                decoder: ServerMessageDecoder::new(None),
                request_sequence: AtomicUsize::new(0),
                closed_tx,
                closed_rx,
                state: Mutex::new(ClientMutableState {
                    messages: Vec::new(),
                    waiters: Vec::new(),
                    attachment: None,
                    closed: false,
                }),
            }),
        }
    }

    pub fn messages(&self) -> Vec<ServerMessage> {
        self.inner.state.lock().expect("client mutex poisoned").messages.clone()
    }

    pub fn closed(&self) -> bool {
        self.inner.state.lock().expect("client mutex poisoned").closed
    }

    pub async fn hello(&self, version: Option<i64>) -> Result<ServerMessage> {
        let wait = self.prepare_wait(0, |msg| {
            matches!(msg, ServerMessage::Hello(_) | ServerMessage::HelloError(_))
        })?;
        self.send_message(&ClientMessage::Hello(ClientHello {
            version: version.unwrap_or(PROTOCOL_VERSION),
        }))
        .await?;
        await_prepared(wait).await
    }

    pub async fn request_service(
        &self,
        target: RpcTarget,
        call: ServiceCall,
        id: Option<&str>,
    ) -> Result<ResponseEnvelope> {
        let req_id = match id {
            Some(id) => id.to_string(),
            None => {
                let seq = self.inner.request_sequence.fetch_add(1, Ordering::SeqCst) + 1;
                format!("request-{seq}")
            }
        };
        let match_id = req_id.clone();
        let wait = self.prepare_wait(
            0,
            move |msg| matches!(msg, ServerMessage::Response(env) if env.id == match_id),
        )?;
        let call_value = serde_json::to_value(&call).map_err(|e| Error::msg(e.to_string()))?;
        self.send_message(&ClientMessage::Request(RequestEnvelope {
            id: req_id,
            target,
            call: call_value,
        }))
        .await?;
        match await_prepared(wait).await? {
            ServerMessage::Response(env) => Ok(env),
            other => Err(Error::msg(format!("Unexpected non-response message: {other:?}"))),
        }
    }

    pub async fn attach(&self, server_id: &str, session_id: &str) -> Result<ResponseEnvelope> {
        self.request_service(
            RpcTarget::Server(ServerTarget {
                server_id: server_id.to_string(),
            }),
            ServiceCall {
                service_id: "pi.session-management".to_string(),
                instance: None,
                member: "attach".to_string(),
                args: vec![json!(session_id)],
            },
            None,
        )
        .await
    }

    pub async fn request_session_service(
        &self,
        server_id: &str,
        session_id: &str,
        call: ServiceCall,
        id: Option<&str>,
    ) -> Result<ResponseEnvelope> {
        let target = {
            let guard = self.inner.state.lock().expect("client mutex poisoned");
            match &guard.attachment {
                Some(att) if att.session_id == session_id => RpcTarget::Session(pi_protocol::SessionTarget {
                    server_id: server_id.to_string(),
                    session_id: att.session_id.clone(),
                    attachment_id: att.attachment_id.clone(),
                }),
                _ => RpcTarget::Session(pi_protocol::SessionTarget {
                    server_id: server_id.to_string(),
                    session_id: session_id.to_string(),
                    attachment_id: "missing-attachment".to_string(),
                }),
            }
        };
        self.request_service(target, call, id).await
    }

    pub async fn send_message(&self, message: &ClientMessage) -> Result<()> {
        let bytes = encode_client_message(message, None)?;
        self.inner.channel.send(&bytes).await
    }

    pub async fn send_bytes(&self, chunk: &[u8]) -> Result<()> {
        self.inner.channel.send(chunk).await
    }

    pub async fn send_fragmented_message(&self, message: &ClientMessage, split_at: usize) -> Result<()> {
        let bytes = encode_client_message(message, None)?;
        self.inner.channel.send_fragmented(&bytes, split_at).await
    }

    pub async fn next(
        &self,
        predicate: impl Fn(&ServerMessage) -> bool + Send + Sync + 'static,
    ) -> Result<ServerMessage> {
        self.next_from(0, predicate).await
    }

    pub async fn next_from(
        &self,
        index: usize,
        predicate: impl Fn(&ServerMessage) -> bool + Send + Sync + 'static,
    ) -> Result<ServerMessage> {
        let wait = self.prepare_wait(index, predicate)?;
        await_prepared(wait).await
    }

    pub async fn wait_for_close(&self) {
        if self.closed() {
            return;
        }
        let mut rx = self.inner.closed_rx.clone();
        let _ = rx.wait_for(|closed| *closed).await;
    }

    pub async fn close(&self) -> Result<()> {
        self.inner.channel.close().await
    }

    pub fn receive(&self, chunk: &[u8]) {
        let decoded = match self.inner.decoder.push(chunk) {
            Ok(messages) => messages,
            Err(error) => {
                self.fail(error);
                return;
            }
        };
        let mut guard = self.inner.state.lock().expect("client mutex poisoned");
        for message in decoded {
            if let ServerMessage::Attachment(ref env) = message {
                guard.attachment = env.attachment.as_ref().map(|att| AttachmentRoute {
                    session_id: att.session_id.clone(),
                    attachment_id: att.attachment_id.clone(),
                });
            }
            guard.messages.push(message.clone());
            let waiters = std::mem::take(&mut guard.waiters);
            let mut remaining = Vec::with_capacity(waiters.len());
            for waiter in waiters {
                if (waiter.predicate)(&message) {
                    let _ = waiter.tx.send(Ok(message.clone()));
                } else {
                    remaining.push(waiter);
                }
            }
            guard.waiters = remaining;
        }
    }

    pub fn mark_closed(&self) {
        let already = {
            let mut guard = self.inner.state.lock().expect("client mutex poisoned");
            if guard.closed {
                true
            } else {
                guard.closed = true;
                false
            }
        };
        if already {
            return;
        }
        self.inner.closed_tx.send_replace(true);
        self.fail(Error::msg("Wire connection closed"));
    }

    pub fn fail(&self, error: Error) {
        let waiters = {
            let mut guard = self.inner.state.lock().expect("client mutex poisoned");
            std::mem::take(&mut guard.waiters)
        };
        for waiter in waiters {
            let _ = waiter.tx.send(Err(clone_js_error(&error)));
        }
    }

    fn prepare_wait(
        &self,
        index: usize,
        predicate: impl Fn(&ServerMessage) -> bool + Send + Sync + 'static,
    ) -> Result<PreparedWait> {
        let mut guard = self.inner.state.lock().expect("client mutex poisoned");
        if let Some(existing) = guard
            .messages
            .get(index..)
            .and_then(|slice| slice.iter().find(|msg| predicate(msg)))
        {
            return Ok(PreparedWait::Immediate(existing.clone()));
        }
        if guard.closed {
            return Err(Error::msg("Wire client is closed"));
        }
        let (tx, rx) = oneshot::channel();
        guard.waiters.push(MessageWaiter {
            predicate: Box::new(predicate),
            tx,
        });
        Ok(PreparedWait::Pending(rx))
    }
}

async fn await_prepared(wait: PreparedWait) -> Result<ServerMessage> {
    match wait {
        PreparedWait::Immediate(msg) => Ok(msg),
        PreparedWait::Pending(rx) => rx.await.unwrap_or_else(|_| Err(Error::msg("Wire connection closed"))),
    }
}

struct StreamWireChannel<W> {
    writer: tokio::sync::Mutex<Option<W>>,
    destroyed: AtomicBool,
    close_tx: watch::Sender<bool>,
    closed_rx: watch::Receiver<bool>,
}

#[async_trait]
impl<W: tokio::io::AsyncWrite + Unpin + Send + 'static> WireChannel for StreamWireChannel<W> {
    async fn send(&self, chunk: &[u8]) -> Result<()> {
        let mut guard = self.writer.lock().await;
        let Some(writer) = guard.as_mut() else {
            return Err(Error::msg("Wire connection closed"));
        };
        writer
            .write_all(chunk)
            .await
            .map_err(|e| Error::from_io(e, "write", None, None))?;
        writer.flush().await.map_err(|e| Error::from_io(e, "flush", None, None))
    }

    async fn send_fragmented(&self, chunk: &[u8], split_at: usize) -> Result<()> {
        let split = split_at.min(chunk.len());
        self.send(&chunk[..split]).await?;
        self.send(&chunk[split..]).await?;
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        if self.destroyed.swap(true, Ordering::SeqCst) {
            let mut rx = self.closed_rx.clone();
            let _ = rx.wait_for(|closed| *closed).await;
            return Ok(());
        }
        {
            let mut guard = self.writer.lock().await;
            if let Some(mut writer) = guard.take() {
                let _ = writer.shutdown().await;
            }
        }
        self.close_tx.send_replace(true);
        let mut rx = self.closed_rx.clone();
        let _ = rx.wait_for(|closed| *closed).await;
        Ok(())
    }
}

pub async fn connect_unix_test_client(path: &str) -> Result<ProtocolTestClient> {
    let stream = connect_unix_stream_socket(path).await?;
    let (mut read_half, write_half) = tokio::io::split(stream);
    let (close_tx, mut abort_read_rx) = watch::channel(false);
    let (done_tx, done_rx) = watch::channel(false);

    let channel = Arc::new(StreamWireChannel {
        writer: tokio::sync::Mutex::new(Some(write_half)),
        destroyed: AtomicBool::new(false),
        close_tx,
        closed_rx: done_rx,
    });
    let client = ProtocolTestClient::new(channel);
    let reader_client = client.clone();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            tokio::select! {
                biased;
                _ = abort_read_rx.wait_for(|c| *c) => break,
                res = read_half.read(&mut buf) => {
                    match res {
                        Ok(0) => break,
                        Ok(n) => reader_client.receive(&buf[..n]),
                        Err(io_err) => {
                            reader_client.fail(Error::from_io(io_err, "read", None, None));
                            break;
                        }
                    }
                }
            }
        }
        reader_client.mark_closed();
        done_tx.send_replace(true);
    });
    Ok(client)
}
