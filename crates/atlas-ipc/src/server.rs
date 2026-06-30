//! Async IPC server with pluggable [`Handler`] trait.

use std::path::Path;
use std::sync::Arc;

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use crate::codec::{read_frame, write_frame};
use crate::error::Result;
use crate::protocol::{
    Envelope, ErrorCode, Frame, Notification, Request, Response, PROTOCOL_VERSION,
};
use crate::transport::{listen, Listener};

/// Implement this trait to provide request-handling logic for the daemon.
#[async_trait::async_trait]
pub trait Handler: Send + Sync + 'static {
    /// Handle one request and return a response.
    async fn handle(&self, req: Request) -> Response;

    /// Optional broadcast receiver for notifications to fan-out to clients.
    fn notifications(&self) -> Option<broadcast::Receiver<Notification>> {
        None
    }
}

/// The IPC server. Bind once, then call [`Server::run`].
pub struct Server {
    listener: Listener,
    handler: Arc<dyn Handler>,
}

impl Server {
    /// Bind a server at `path` with the given handler.
    pub async fn bind<H: Handler>(path: &Path, handler: H) -> Result<Self> {
        let listener = listen(path).await?;
        Ok(Self {
            listener,
            handler: Arc::new(handler),
        })
    }

    /// Run the accept loop until `cancel` is triggered.
    pub async fn run(self, cancel: CancellationToken) -> Result<()> {
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    debug!("server: cancellation requested, stopping accept loop");
                    break;
                }
                result = self.listener.accept() => {
                    match result {
                        Ok(stream) => {
                            let handler = Arc::clone(&self.handler);
                            let cancel_child = cancel.child_token();
                            tokio::spawn(handle_connection(stream, handler, cancel_child));
                        }
                        Err(error) => {
                            error!("server: accept error: {error}");
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

async fn handle_connection(
    stream: crate::transport::Stream,
    handler: Arc<dyn Handler>,
    cancel: CancellationToken,
) {
    let crate::transport::Stream { mut recv, mut send } = stream;

    let mut notif_rx = handler.notifications();

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                debug!("connection task: cancelled");
                break;
            }
            notif = async {
                match notif_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match notif {
                    Ok(notification) => {
                        let env = Envelope {
                            version: PROTOCOL_VERSION,
                            correlation: 0,
                            payload: Frame::Notification(notification),
                        };
                        if let Err(error) = write_frame(&mut send, &env).await {
                            debug!("connection task: error writing notification: {error}");
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        warn!("connection task: missed {count} notifications (lagged)");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        notif_rx = None;
                    }
                }
            }
            frame_result = read_frame(&mut recv) => {
                match frame_result {
                    Err(crate::error::IpcError::Closed) => {
                        debug!("connection task: client disconnected");
                        break;
                    }
                    Err(crate::error::IpcError::FrameTooLarge(size)) => {
                        warn!("connection task: frame too large ({size} bytes), closing");
                        break;
                    }
                    Err(error) => {
                        debug!("connection task: read error: {error}");
                        break;
                    }
                    Ok(env) => {
                        if env.version != PROTOCOL_VERSION {
                            let resp = Envelope {
                                version: PROTOCOL_VERSION,
                                correlation: env.correlation,
                                payload: Frame::Response(Response::Error {
                                    code: ErrorCode::ProtocolMismatch,
                                    message: format!(
                                        "expected protocol version {PROTOCOL_VERSION}, got {}",
                                        env.version
                                    ),
                                }),
                            };
                            let _ = write_frame(&mut send, &resp).await;
                            break;
                        }
                        let correlation = env.correlation;
                        match env.payload {
                            Frame::Request(req) => {
                                let response = handler.handle(req).await;
                                let resp_env = Envelope {
                                    version: PROTOCOL_VERSION,
                                    correlation,
                                    payload: Frame::Response(response),
                                };
                                if let Err(error) = write_frame(&mut send, &resp_env).await {
                                    debug!("connection task: write error: {error}");
                                    break;
                                }
                            }
                            other => {
                                warn!("connection task: unexpected frame from client: {other:?}");
                            }
                        }
                    }
                }
            }
        }
    }
    debug!("connection task: done");
}
