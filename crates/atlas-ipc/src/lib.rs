//! `atlas-ipc` — typed, length-prefixed, bincode-serialized RPC between the
//! `atlas-app` UI process and the `atlas-indexd` daemon.
//!
//! # Quick start
//!
//! ## Server side
//! ```rust,no_run
//! use atlas_ipc::{server::{Server, Handler}, protocol::{Request, Response}};
//! use tokio_util::sync::CancellationToken;
//!
//! struct MyHandler;
//!
//! #[async_trait::async_trait]
//! impl Handler for MyHandler {
//!     async fn handle(&self, req: Request) -> Response {
//!         match req {
//!             Request::Ping => Response::Pong,
//!             _ => Response::Ok,
//!         }
//!     }
//! }
//!
//! # #[tokio::main] async fn main() -> atlas_ipc::Result<()> {
//! let cancel = CancellationToken::new();
//! let server = Server::bind(std::path::Path::new("/tmp/atlas-test.sock"), MyHandler).await?;
//! server.run(cancel).await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Client side
//! ```rust,no_run
//! use atlas_ipc::client::Client;
//! # #[tokio::main] async fn main() -> atlas_ipc::Result<()> {
//! let client = Client::connect(std::path::Path::new("/tmp/atlas-test.sock")).await?;
//! client.ping().await?;
//! # Ok(())
//! # }
//! ```

pub mod client;
pub mod codec;
pub mod error;
pub mod protocol;
pub mod server;
pub mod transport;

pub use error::{IpcError, Result};
