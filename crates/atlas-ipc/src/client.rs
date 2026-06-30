//! Async IPC client with correlation-matched request/response and broadcast notifications.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::{broadcast, oneshot};
use tracing::{debug, warn};

use crate::codec::{read_frame, write_frame};
use crate::error::{IpcError, Result};
use crate::protocol::{Envelope, Frame, Notification, Request, Response, PROTOCOL_VERSION};
use crate::transport::connect;

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Response>>>>;

/// Async IPC client.
///
/// Connects to the daemon, sends typed requests, and receives responses matched
/// by correlation ID. The client does **not** reconnect automatically; wrap it
/// in a reconnecting layer if needed.
pub struct Client {
    /// Shared write half — wrapped in a Mutex so callers can request concurrently.
    send: Arc<tokio::sync::Mutex<interprocess::local_socket::tokio::SendHalf>>,
    /// Pending request waiters keyed by correlation ID.
    pending: PendingMap,
    /// Monotonic counter for correlation IDs.
    next_id: Arc<AtomicU64>,
    /// Broadcast channel for server-pushed notifications.
    notif_tx: broadcast::Sender<Notification>,
}

impl Client {
    /// Connect to the daemon at `path`.
    pub async fn connect(path: &Path) -> Result<Self> {
        let stream = connect(path).await?;
        let crate::transport::Stream { recv, send } = stream;
        let send = Arc::new(tokio::sync::Mutex::new(send));
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let next_id = Arc::new(AtomicU64::new(1));
        let (notif_tx, _) = broadcast::channel(256);

        {
            let pending = Arc::clone(&pending);
            let notif_tx = notif_tx.clone();
            tokio::spawn(reader_task(recv, pending, notif_tx));
        }

        Ok(Self {
            send,
            pending,
            next_id,
            notif_tx,
        })
    }

    /// Send a request and await the matched response.
    pub async fn request(&self, req: Request) -> Result<Response> {
        let correlation = self.next_id.fetch_add(1, Ordering::Relaxed);
        let env = Envelope {
            version: PROTOCOL_VERSION,
            correlation,
            payload: Frame::Request(req),
        };

        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(correlation, tx);

        {
            let mut send = self.send.lock().await;
            if let Err(error) = write_frame(&mut *send, &env).await {
                self.pending.lock().remove(&correlation);
                return Err(error);
            }
        }

        match rx.await.map_err(|_| IpcError::Closed)? {
            Response::Error { code, message } => Err(IpcError::ServerError { code, message }),
            response => Ok(response),
        }
    }

    /// Send a [`Request::Ping`] and expect a [`Response::Pong`].
    pub async fn ping(&self) -> Result<()> {
        match self.request(Request::Ping).await? {
            Response::Pong => Ok(()),
            other => Err(IpcError::Codec(format!("expected Pong, got {other:?}"))),
        }
    }

    /// Subscribe to server-pushed [`Notification`]s.
    pub fn notifications(&self) -> broadcast::Receiver<Notification> {
        self.notif_tx.subscribe()
    }

    /// Graceful shutdown: send [`Request::Shutdown`] and consume the client.
    pub async fn shutdown(self) -> Result<()> {
        let _ = self.request(Request::Shutdown).await?;
        Ok(())
    }
}

async fn reader_task(
    mut recv: interprocess::local_socket::tokio::RecvHalf,
    pending: PendingMap,
    notif_tx: broadcast::Sender<Notification>,
) {
    loop {
        match read_frame(&mut recv).await {
            Err(IpcError::Closed) => {
                debug!("client reader: connection closed");
                break;
            }
            Err(error) => {
                debug!("client reader: error: {error}");
                break;
            }
            Ok(env) => match env.payload {
                Frame::Response(resp) => {
                    let tx = pending.lock().remove(&env.correlation);
                    if let Some(tx) = tx {
                        let _ = tx.send(resp);
                    } else {
                        warn!(
                            "client reader: unmatched correlation id {}",
                            env.correlation
                        );
                    }
                }
                Frame::Notification(notification) => {
                    let _ = notif_tx.send(notification);
                }
                Frame::Request(_) => {
                    warn!("client reader: unexpected Request frame from server");
                }
            },
        }
    }

    let waiters: Vec<_> = pending.lock().drain().map(|(_, tx)| tx).collect();
    for tx in waiters {
        let _ = tx.send(Response::Error {
            code: crate::protocol::ErrorCode::InternalError,
            message: "connection closed".into(),
        });
    }
}
