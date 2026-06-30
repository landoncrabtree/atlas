//! Integration tests for atlas-ipc.

use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use atlas_ipc::{
    client::Client,
    protocol::{Envelope, ErrorCode, Frame, Notification, Request, Response, PROTOCOL_VERSION},
    server::{Handler, Server},
};
use tempfile::TempDir;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

struct EchoHandler;

#[async_trait::async_trait]
impl Handler for EchoHandler {
    async fn handle(&self, req: Request) -> Response {
        match req {
            Request::Ping => Response::Pong,
            Request::Hello {
                client_name: _,
                client_version,
            } => Response::Hello {
                server_name: "test-server".into(),
                server_version: client_version,
                protocol_version: PROTOCOL_VERSION,
            },
            Request::Search {
                query_json,
                options_json: _,
            } => Response::SearchHits {
                hits_json: query_json,
            },
            Request::Stats => Response::Stats {
                docs: 42,
                on_disk_bytes: 1024,
            },
            Request::Shutdown => Response::Ok,
            _ => Response::Ok,
        }
    }
}

struct NotifyingHandler {
    tx: broadcast::Sender<Notification>,
}

#[async_trait::async_trait]
impl Handler for NotifyingHandler {
    async fn handle(&self, req: Request) -> Response {
        match req {
            Request::Ping => Response::Pong,
            Request::Reindex { path: _ } => {
                let root = std::path::PathBuf::from("/test");
                let _ = self.tx.send(Notification::IndexProgress {
                    root: root.clone(),
                    files: 1,
                    bytes: 100,
                });
                let _ = self.tx.send(Notification::IndexProgress {
                    root: root.clone(),
                    files: 2,
                    bytes: 200,
                });
                let _ = self
                    .tx
                    .send(Notification::IndexComplete { root, took_ms: 42 });
                Response::Ok
            }
            Request::Shutdown => Response::Ok,
            _ => Response::Ok,
        }
    }

    fn notifications(&self) -> Option<broadcast::Receiver<Notification>> {
        Some(self.tx.subscribe())
    }
}

fn sock_path(dir: &TempDir) -> std::path::PathBuf {
    dir.path().join("test.sock")
}

async fn start_server<H: Handler>(
    path: &std::path::Path,
    handler: H,
) -> (tokio::task::JoinHandle<()>, CancellationToken) {
    let cancel = CancellationToken::new();
    let server = Server::bind(path, handler).await.unwrap();
    let cancel_clone = cancel.clone();
    let handle = tokio::spawn(async move {
        server.run(cancel_clone).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (handle, cancel)
}

fn env_lock() -> &'static parking_lot::Mutex<()> {
    static LOCK: OnceLock<parking_lot::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| parking_lot::Mutex::new(()))
}

#[test]
fn socket_path_env_override() {
    let _guard = env_lock().lock();
    // SAFETY: This test serializes access to process environment mutations.
    unsafe {
        std::env::set_var("ATLAS_IPC_SOCKET", "/custom/path.sock");
    }
    let path = atlas_ipc::transport::default_socket_path().unwrap();
    assert_eq!(path, std::path::PathBuf::from("/custom/path.sock"));
    // SAFETY: This test serializes access to process environment mutations.
    unsafe {
        std::env::remove_var("ATLAS_IPC_SOCKET");
    }
}

#[tokio::test]
async fn ping_pong() {
    let dir = TempDir::new().unwrap();
    let path = sock_path(&dir);
    let (handle, cancel) = start_server(&path, EchoHandler).await;

    let client = Client::connect(&path).await.unwrap();
    client.ping().await.unwrap();

    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn hello_handshake() {
    let dir = TempDir::new().unwrap();
    let path = sock_path(&dir);
    let (handle, cancel) = start_server(&path, EchoHandler).await;

    let client = Client::connect(&path).await.unwrap();
    let resp = client
        .request(Request::Hello {
            client_name: "atlas-app".into(),
            client_version: "0.0.1".into(),
        })
        .await
        .unwrap();

    assert!(matches!(resp, Response::Hello { server_name, .. } if server_name == "test-server"));

    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn protocol_version_mismatch() {
    let dir = TempDir::new().unwrap();
    let path = sock_path(&dir);
    let (handle, cancel) = start_server(&path, EchoHandler).await;

    use atlas_ipc::codec::write_frame;
    use atlas_ipc::transport::connect;

    let mut stream = connect(&path).await.unwrap();
    let env = Envelope {
        version: 999,
        correlation: 1,
        payload: Frame::Request(Request::Ping),
    };
    write_frame(&mut stream.send, &env).await.unwrap();

    let resp_env = atlas_ipc::codec::read_frame(&mut stream.recv)
        .await
        .unwrap();
    assert!(matches!(
        resp_env.payload,
        Frame::Response(Response::Error {
            code: ErrorCode::ProtocolMismatch,
            ..
        })
    ));

    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn search_echo() {
    let dir = TempDir::new().unwrap();
    let path = sock_path(&dir);
    let (handle, cancel) = start_server(&path, EchoHandler).await;

    let client = Client::connect(&path).await.unwrap();
    let resp = client
        .request(Request::Search {
            query_json: r#"{"term":"foo"}"#.into(),
            options_json: "{}".into(),
        })
        .await
        .unwrap();

    assert!(matches!(resp, Response::SearchHits { hits_json } if hits_json == r#"{"term":"foo"}"#));

    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn notifications_received() {
    let (tx, _) = broadcast::channel(16);
    let handler = NotifyingHandler { tx: tx.clone() };

    let dir = TempDir::new().unwrap();
    let path = sock_path(&dir);
    let (handle, cancel) = start_server(&path, handler).await;

    let client = Client::connect(&path).await.unwrap();
    let mut notif_rx = client.notifications();

    let resp = client
        .request(Request::Reindex { path: None })
        .await
        .unwrap();
    assert!(matches!(resp, Response::Ok));

    let mut received = Vec::new();
    for _ in 0..3 {
        let notification = tokio::time::timeout(Duration::from_secs(2), notif_rx.recv())
            .await
            .expect("timeout")
            .expect("channel error");
        received.push(notification);
    }
    assert_eq!(received.len(), 3);
    assert!(matches!(
        received[0],
        Notification::IndexProgress { files: 1, .. }
    ));
    assert!(matches!(
        received[1],
        Notification::IndexProgress { files: 2, .. }
    ));
    assert!(matches!(
        received[2],
        Notification::IndexComplete { took_ms: 42, .. }
    ));

    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn frame_too_large_closes_connection() {
    let dir = TempDir::new().unwrap();
    let path = sock_path(&dir);
    let (handle, cancel) = start_server(&path, EchoHandler).await;

    let mut stream = atlas_ipc::transport::connect(&path).await.unwrap();

    use tokio::io::AsyncWriteExt;
    let big_len: u32 = (32 * 1024 * 1024) as u32;
    stream.send.write_all(&big_len.to_le_bytes()).await.unwrap();

    let result = atlas_ipc::codec::read_frame(&mut stream.recv).await;
    assert!(
        matches!(
            result,
            Err(atlas_ipc::IpcError::Closed) | Err(atlas_ipc::IpcError::Io(_))
        ),
        "expected Closed or Io, got {result:?}"
    );

    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn concurrent_pings() {
    let dir = TempDir::new().unwrap();
    let path = sock_path(&dir);
    let (handle, cancel) = start_server(&path, EchoHandler).await;

    let client = Arc::new(Client::connect(&path).await.unwrap());
    let mut tasks = Vec::new();
    for _ in 0..100 {
        let client = Arc::clone(&client);
        tasks.push(tokio::spawn(async move { client.ping().await }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }

    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn graceful_shutdown_via_cancel() {
    let dir = TempDir::new().unwrap();
    let path = sock_path(&dir);
    let cancel = CancellationToken::new();
    let server = Server::bind(&path, EchoHandler).await.unwrap();
    let cancel_clone = cancel.clone();
    let handle = tokio::spawn(async move { server.run(cancel_clone).await });

    tokio::time::sleep(Duration::from_millis(20)).await;

    let client = Client::connect(&path).await.unwrap();
    client.ping().await.unwrap();

    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("server did not stop in time")
        .unwrap();
    assert!(result.is_ok());
}
