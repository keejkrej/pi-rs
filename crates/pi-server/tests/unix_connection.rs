//! Port of packages/server/test/unix-connection.test.ts

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_js::Result;
use pi_protocol::{ProtocolError, ServerHelloError, ServerMessage, ServerMessageDecoder, encode_server_message};
use pi_server::connection::ByteConnection;
use pi_server::testing::poll_until;
use pi_server::transports::unix::listener::{UnixByteConnection, UnixByteSocket};
use tokio::sync::{oneshot, watch};

struct ControlledSocket {
    destroyed: AtomicBool,
    writable: AtomicBool,
    writable_length: AtomicUsize,
    ended: AtomicBool,
    final_chunk: Mutex<Option<Vec<u8>>>,
    write_callback: Mutex<Option<oneshot::Sender<Result<()>>>>,
    closed_tx: watch::Sender<bool>,
    closed_rx: watch::Receiver<bool>,
}

impl ControlledSocket {
    fn new() -> Arc<Self> {
        let (closed_tx, closed_rx) = watch::channel(false);
        Arc::new(Self {
            destroyed: AtomicBool::new(false),
            writable: AtomicBool::new(true),
            writable_length: AtomicUsize::new(0),
            ended: AtomicBool::new(false),
            final_chunk: Mutex::new(None),
            write_callback: Mutex::new(None),
            closed_tx,
            closed_rx,
        })
    }

    fn complete_write(&self) {
        let tx = self
            .write_callback
            .lock()
            .expect("write callback mutex poisoned")
            .take()
            .expect("No pending write");
        self.writable_length.store(0, Ordering::SeqCst);
        let _ = tx.send(Ok(()));
    }
}

#[async_trait]
impl UnixByteSocket for ControlledSocket {
    fn writable(&self) -> bool {
        self.writable.load(Ordering::SeqCst)
    }

    fn destroyed(&self) -> bool {
        self.destroyed.load(Ordering::SeqCst)
    }

    async fn write_chunk(&self, _chunk: &[u8]) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.writable_length.store(1, Ordering::SeqCst);
        *self.write_callback.lock().expect("write callback mutex poisoned") = Some(tx);
        rx.await.expect("write callback channel dropped")
    }

    async fn end(&self, final_chunk: Option<&[u8]>) -> Result<()> {
        self.ended.store(true, Ordering::SeqCst);
        *self.final_chunk.lock().expect("final_chunk mutex poisoned") = final_chunk.map(|bytes| bytes.to_vec());
        Ok(())
    }

    fn destroy(&self) {
        if self.destroyed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.writable.store(false, Ordering::SeqCst);
        self.closed_tx.send_replace(true);
    }

    fn subscribe_closed(&self) -> watch::Receiver<bool> {
        self.closed_rx.clone()
    }
}

#[tokio::test]
async fn queues_a_final_protocol_error_behind_pending_output_before_closing() {
    let socket = ControlledSocket::new();
    let connection = UnixByteConnection::new(socket.clone(), 1_000, 64 * 1024);

    let send_conn = connection.clone();
    let pending_write = tokio::spawn(async move { send_conn.send(&[1, 2, 3]).await });

    poll_until(|| socket.writable_length.load(Ordering::SeqCst) == 1)
        .await
        .expect("pending write observed");

    let final_message = ServerMessage::HelloError(ServerHelloError {
        error: ProtocolError {
            code: "invalid_request".to_string(),
            message: "Protocol violation".to_string(),
        },
    });
    let final_bytes = encode_server_message(&final_message, None).expect("encode final message");
    let close_conn = connection.clone();
    let closing = tokio::spawn(async move { close_conn.close(Some(&final_bytes)).await });

    assert!(!socket.ended.load(Ordering::SeqCst));
    assert!(!socket.destroyed.load(Ordering::SeqCst));

    socket.complete_write();
    pending_write.await.expect("send task").expect("send succeeded");

    poll_until(|| socket.ended.load(Ordering::SeqCst))
        .await
        .expect("socket ended");

    let captured = socket
        .final_chunk
        .lock()
        .expect("final_chunk mutex")
        .clone()
        .expect("final chunk captured");
    let decoded = ServerMessageDecoder::new(None)
        .push(&captured)
        .expect("decode final message");
    assert_eq!(decoded, vec![final_message]);

    connection.mark_closed();
    closing.await.expect("close task").expect("close succeeded");
}
