//! [`ConnectController`] — bridge for the Cmd+K "Connect to Server" modal.
//!
//! # Threading model
//!
//! - All public methods (`open`, `close`, `set_*`, `connect`, `cancel`, …)
//!   are safe to call from any thread. They update shared state under a
//!   [`parking_lot::Mutex`] and push UI updates back into the Slint event
//!   loop via [`slint::invoke_from_event_loop`].
//! - The actual OpenDAL handshake happens on a dedicated
//!   `atlas-connect-worker` OS thread so a slow SFTP host can never freeze
//!   the UI. The thread is signalled via a
//!   [`Arc<AtomicBool>`](AtomicBool) which the controller flips to `true`
//!   from [`cancel`] or when another connect attempt supersedes the
//!   current one.
//!
//! The controller does NOT own the shell — it only holds a
//! [`Weak<AppShell>`] and upgrades on each callback. This keeps the
//! reference graph acyclic (`AppShell -> ConnectController -> weak
//! AppShell`) and lets the controller outlive a soft window destroy
//! without a panic.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Weak,
    },
    time::{Duration, Instant},
};

use atlas_config::servers::{add_or_replace, delete, list, SavedServer};
use atlas_core::{BackendKind, Location};
use atlas_fs::{OpenOptions, ViewModelEvent};
use atlas_remote::{
    backend::{BackendError, Credentials},
    host_key::{HostKeyDecision, HostKeyRequest, HostKeyResolver, KnownHostsMode},
    known_hosts::HostKeyStatus,
    secrets,
    vm::sftp::SftpOptions,
    RemoteLocationViewModel,
};
use parking_lot::Mutex;

use crate::{models::split::PaneId, shell::AppShell, AtlasWindow};

/// Enum mirror of the Slint backend-picker index. Kept in sync with the
/// order defined in `assets/ui/components/connect-server.slint`
/// (`backend-labels` array). Do NOT reorder without updating both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendChoice {
    Sftp = 0,
    Ftp = 1,
    WebDav = 2,
    S3 = 3,
}

impl BackendChoice {
    fn from_index(idx: i32) -> Self {
        match idx {
            1 => Self::Ftp,
            2 => Self::WebDav,
            3 => Self::S3,
            _ => Self::Sftp,
        }
    }

    fn as_kind(self) -> BackendKind {
        match self {
            Self::Sftp => BackendKind::Sftp,
            Self::Ftp => BackendKind::Ftp,
            Self::WebDav => BackendKind::WebDav,
            Self::S3 => BackendKind::S3,
        }
    }
}

/// Enum mirror of the Slint auth-method-picker index. Kept in sync with
/// the order defined in `connect-server.slint` (`auth-labels` array).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthChoice {
    Password = 0,
    SshKey = 1,
    Iam = 2,
    Anonymous = 3,
}

impl AuthChoice {
    fn from_index(idx: i32) -> Self {
        match idx {
            1 => Self::SshKey,
            2 => Self::Iam,
            3 => Self::Anonymous,
            _ => Self::Password,
        }
    }
}

/// Categorised connect-time error, used to drive both the status message
/// AND field-highlight hints in future revisions of the modal.
#[derive(Debug, Clone, Copy)]
enum ErrorKind {
    Auth,
    Network,
    Server,
    Malformed,
}

impl ErrorKind {
    fn banner(self) -> &'static str {
        match self {
            Self::Auth => "Authentication failed. Check your credentials and try again.",
            Self::Network => "Could not reach the server. Check the host and port.",
            Self::Server => "The server rejected the request.",
            Self::Malformed => "The connection string is malformed.",
        }
    }
}

/// Shared per-modal state — held behind a [`Mutex`] so any thread can
/// update the flag/fields between callbacks.
#[derive(Default)]
struct State {
    visible: bool,
    /// Which pane will receive the vm on success.
    target_pane: Option<PaneId>,
    backend: Option<BackendChoice>,
    auth: Option<AuthChoice>,
    host: String,
    port: String,
    path: String,
    username: String,
    password: String,
    ssh_key_path: String,
    iam_access_key: String,
    iam_secret_key: String,
    iam_session_token: String,
    s3_endpoint: String,
    label: String,
    save_to_keychain: bool,
    connecting: bool,
    status_text: String,
    status_is_error: bool,
    /// Set to `true` when a saved-server selection or similar bulk update
    /// echoes multiple fields at once. Individual setters skip re-echoing
    /// UI changes back into the state while this flag is on so a
    /// select_saved_server → push_to_ui → *-changed → set_* → push_to_ui
    /// loop never re-enters and clobbers what we just wrote.
    suppress_field_echo: bool,
    /// Cancellation flag for the currently-active worker thread. Setting
    /// this to `true` causes the worker to bail out before mounting.
    cancel_flag: Option<Arc<AtomicBool>>,
    /// The saved-servers list currently rendered inside the modal. This
    /// is a materialised snapshot of `atlas_config::servers::list()`;
    /// callers must call [`ConnectController::refresh_saved_servers`]
    /// after any add / delete to keep it in sync.
    saved_servers: Vec<SavedServerRow>,
    /// TOFU state — Some when the SFTP handshake is currently paused
    /// waiting for a Trust decision. See
    /// [`ConnectController::start_host_key_prompt`].
    host_key_prompt: Option<HostKeyPrompt>,
}

/// Slint-side view of a single outstanding TOFU decision.
///
/// The [`HostKeyResolver`] hands us a [`HostKeyRequest`] from the SFTP
/// handshake thread; we stash a `HostKeyPrompt` here, push the banner
/// state to Slint, and complete the `oneshot::Sender` when the user
/// clicks Trust / Cancel.
struct HostKeyPrompt {
    /// Reply channel back to the SFTP handshake.
    reply: tokio::sync::oneshot::Sender<HostKeyDecision>,
    /// Host string being connected to (for banner display).
    host: String,
    /// Offered fingerprint, formatted `SHA256:<b64>`.
    offered_fingerprint: String,
    /// When the current status is [`HostKeyStatus::Mismatch`], the
    /// previously-known fingerprint; empty otherwise.
    known_fingerprint: String,
}

/// A single saved-server row rendered in the modal's list. Kept
/// separate from the on-disk [`SavedServer`] because the UI needs a
/// preformatted relative-time string, a per-row "delete armed" flag,
/// and a rendered backend glyph.
#[derive(Debug, Clone)]
pub(crate) struct SavedServerRow {
    /// Stable id (matches [`SavedServer::id`]).
    pub id: String,
    /// User-facing label.
    pub label: String,
    /// Full URI string, e.g. `sftp://landon@host:22/var/log`.
    pub address: String,
    /// Backend glyph — one of the five monochrome unicode markers.
    pub glyph: String,
    /// Preformatted relative time ("just now", "5m ago", "never").
    pub last_connected_relative: String,
    /// True while this row is showing the inline "Confirm?" chip.
    pub delete_pending: bool,
}

/// Bridge between the Connect-to-Server Slint modal and
/// [`atlas_remote::backend::open`].
///
/// Construct once with [`ConnectController::new`], call
/// [`ConnectController::attach_window`] before the first
/// [`ConnectController::open`], and route the Slint callback closures
/// (see `AppShell::wire_callbacks`) through the setters below.
pub struct ConnectController {
    state: Mutex<State>,
    window: Mutex<slint::Weak<AtlasWindow>>,
    /// Weak back-reference used to invoke
    /// [`AppShell::open_remote_location`] on successful connect.
    /// The parent is populated by [`ConnectController::set_shell`] during
    /// [`AppShell::new`].
    shell: Mutex<Weak<AppShell>>,
}

impl ConnectController {
    /// Construct an empty controller. Call [`attach_window`] before the
    /// first [`open`] and [`set_shell`] before the first `connect`.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                save_to_keychain: true,
                ..State::default()
            }),
            window: Mutex::new(slint::Weak::default()),
            shell: Mutex::new(Weak::new()),
        })
    }

    /// Attach the Slint window so the controller can push UI updates.
    pub fn attach_window(&self, window: slint::Weak<AtlasWindow>) {
        *self.window.lock() = window;
    }

    /// Attach the parent shell, used to inject the completed remote vm
    /// into the target pane.
    pub fn set_shell(&self, shell: Weak<AppShell>) {
        *self.shell.lock() = shell;
    }

    /// Open the modal, targeting `pane_id`. Resets every field to the
    /// default state and auto-focuses the connection-string input.
    pub fn open(&self, pane_id: PaneId) {
        let saved = load_saved_server_rows();
        {
            let mut st = self.state.lock();
            *st = State {
                save_to_keychain: true,
                visible: true,
                target_pane: Some(pane_id),
                backend: Some(BackendChoice::Sftp),
                auth: Some(AuthChoice::Password),
                saved_servers: saved,
                ..State::default()
            };
        }
        self.push_to_ui();
    }

    /// Close the modal, cancelling any in-flight connect worker.
    pub fn close(&self) {
        let cancel = {
            let mut st = self.state.lock();
            st.visible = false;
            st.target_pane = None;
            st.connecting = false;
            st.status_text.clear();
            st.status_is_error = false;
            st.cancel_flag.take()
        };
        if let Some(flag) = cancel {
            flag.store(true, Ordering::SeqCst);
        }
        self.push_to_ui();
    }

    /// Whether the modal is currently visible.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.state.lock().visible
    }

    // ── Field setters ────────────────────────────────────────────────────────

    pub fn set_backend(&self, idx: i32) {
        let choice = BackendChoice::from_index(idx);
        {
            let mut st = self.state.lock();
            st.backend = Some(choice);
            // Reset auth to a backend-appropriate default so an invalid
            // combo (e.g. WebDAV + SSH key) never lingers.
            let default_auth = match choice {
                BackendChoice::S3 => AuthChoice::Iam,
                BackendChoice::Sftp | BackendChoice::Ftp | BackendChoice::WebDav => {
                    AuthChoice::Password
                }
            };
            st.auth = Some(default_auth);
            st.status_text.clear();
            st.status_is_error = false;
        }
        self.push_to_ui();
    }

    pub fn set_auth(&self, idx: i32) {
        {
            let mut st = self.state.lock();
            st.auth = Some(AuthChoice::from_index(idx));
            st.status_text.clear();
            st.status_is_error = false;
        }
        self.push_to_ui();
    }

    /// Setter for the merged host input in the modal.
    ///
    /// Accepts either `host.example.com` or `host.example.com:2222`.
    /// If a `:PORT` suffix is present, split it off into the port
    /// field. IPv6 literals in brackets (`[::1]:22`) are respected —
    /// only the trailing `:PORT` (after the closing `]`) is treated as
    /// a port separator. This keeps the modal a single input while
    /// still populating the structured `port` state field that
    /// [`assemble_location`] consumes.
    pub fn set_host(&self, s: String) {
        {
            let mut st = self.state.lock();
            if st.suppress_field_echo {
                return;
            }
            let trimmed = s.trim();
            match split_host_port(trimmed) {
                Some((host, port_str)) => {
                    st.host = host;
                    st.port = port_str;
                }
                None => {
                    st.host = s;
                }
            }
            st.status_text.clear();
            st.status_is_error = false;
        }
        self.push_to_ui();
    }

    pub fn set_port(&self, s: String) {
        {
            let mut st = self.state.lock();
            if st.suppress_field_echo {
                return;
            }
            st.port = s;
        }
        self.push_to_ui();
    }

    pub fn set_path(&self, s: String) {
        {
            let mut st = self.state.lock();
            if st.suppress_field_echo {
                return;
            }
            st.path = s;
        }
        self.push_to_ui();
    }

    pub fn set_username(&self, s: String) {
        {
            let mut st = self.state.lock();
            if st.suppress_field_echo {
                return;
            }
            st.username = s;
        }
        self.push_to_ui();
    }

    pub fn set_password(&self, s: String) {
        let clear_status;
        {
            let mut st = self.state.lock();
            st.password = s;
            clear_status = st.status_is_error;
            if clear_status {
                st.status_text.clear();
                st.status_is_error = false;
            }
        }
        if clear_status {
            self.push_to_ui();
        }
    }

    pub fn set_ssh_key_path(&self, s: String) {
        self.state.lock().ssh_key_path = s;
    }

    pub fn set_iam_access_key(&self, s: String) {
        self.state.lock().iam_access_key = s;
    }

    pub fn set_iam_secret_key(&self, s: String) {
        self.state.lock().iam_secret_key = s;
    }

    pub fn set_iam_session_token(&self, s: String) {
        self.state.lock().iam_session_token = s;
    }

    pub fn set_s3_endpoint(&self, s: String) {
        self.state.lock().s3_endpoint = s;
    }

    pub fn set_label(&self, s: String) {
        self.state.lock().label = s;
    }

    pub fn toggle_save_to_keychain(&self) {
        {
            let mut st = self.state.lock();
            st.save_to_keychain = !st.save_to_keychain;
        }
        self.push_to_ui();
    }

    /// SSH-key browse handler — the Slint modal invokes this when the
    /// user clicks the "Browse…" button. Today this is a no-op stub;
    /// wiring native-file-open here is scheduled for phase 2.5.
    pub fn browse_ssh_key(&self) {
        tracing::info!("connect: browse SSH key requested (no-op stub — phase 2.5)");
    }

    // ── TOFU host-key handling (Phase 2.6) ─────────────────────────────────

    /// Build a [`HostKeyResolver`] whose prompter dispatches into this
    /// controller's state. The returned resolver is cheap to clone —
    /// internally it wraps an `Arc<dyn Fn>` — so each connect worker
    /// grabs a fresh copy via this helper.
    ///
    /// Prompt semantics:
    /// * The resolver's closure is called from inside the tokio runtime
    ///   driving the SFTP handshake. It captures a `Weak<Self>` so it
    ///   never keeps the controller alive past window teardown.
    /// * The closure installs a `HostKeyPrompt` into
    ///   [`State::host_key_prompt`] and pushes the banner state to
    ///   Slint via [`Self::push_to_ui`]. It returns a fresh
    ///   `oneshot::Receiver` which the Trust / Cancel callbacks
    ///   complete on the UI thread.
    /// * Only one prompt can be in flight at a time (the modal is
    ///   modal). A second prompt arriving while one is queued gets an
    ///   immediate Cancel — the user can retry from the modal.
    fn host_key_resolver(self: &Arc<Self>) -> HostKeyResolver {
        let weak = Arc::downgrade(self);
        HostKeyResolver::new(move |req: HostKeyRequest| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let Some(this) = weak.upgrade() else {
                // Controller went away; fail closed.
                let _ = tx.send(HostKeyDecision::Cancel);
                return rx;
            };
            let known_fp = match &req.current_status {
                HostKeyStatus::Mismatch { known_fingerprint } => known_fingerprint.clone(),
                _ => String::new(),
            };
            let mut st = this.state.lock();
            if st.host_key_prompt.is_some() {
                // Another prompt is already displayed — reject the new
                // request rather than losing it silently.
                let _ = tx.send(HostKeyDecision::Cancel);
                return rx;
            }
            st.host_key_prompt = Some(HostKeyPrompt {
                reply: tx,
                host: req.host.clone(),
                offered_fingerprint: req.offered_fingerprint.clone(),
                known_fingerprint: known_fp,
            });
            drop(st);
            this.push_to_ui();
            rx
        })
    }

    /// User clicked **Trust once** on the TOFU banner. Complete the
    /// resolver's reply channel with [`HostKeyDecision::TrustOnce`] so
    /// the SFTP handshake resumes without persisting anything.
    pub fn host_key_trust_once(&self) {
        self.finish_host_key_prompt(HostKeyDecision::TrustOnce);
    }

    /// User clicked **Trust always** (or **Replace and continue** on a
    /// mismatch). Complete the reply channel with
    /// [`HostKeyDecision::TrustAlways`] — the SFTP handler writes the
    /// key into `~/.config/atlas/known_hosts`.
    pub fn host_key_trust_always(&self) {
        self.finish_host_key_prompt(HostKeyDecision::TrustAlways);
    }

    /// User clicked **Cancel** (or hit Escape / Enter). Complete the
    /// reply channel with [`HostKeyDecision::Cancel`] — the SFTP
    /// handshake aborts and the modal surfaces a "Timed out" style
    /// error banner.
    pub fn cancel_host_key(&self) {
        self.finish_host_key_prompt(HostKeyDecision::Cancel);
    }

    /// Common tail for all three Trust / Cancel callbacks.
    fn finish_host_key_prompt(&self, decision: HostKeyDecision) {
        let prompt = self.state.lock().host_key_prompt.take();
        if let Some(prompt) = prompt {
            // Best-effort send — if the receiver was dropped (handshake
            // aborted from the other side) the resolver already handed
            // back Cancel via its timeout path.
            let _ = prompt.reply.send(decision);
        }
        self.push_to_ui();
    }

    // ── Saved-servers list ──────────────────────────────────────────────────

    /// Rebuild the saved-servers list from `servers.toml` and push to
    /// the UI. Call after add/delete/successful-connect.
    pub fn refresh_saved_servers(&self) {
        let rows = load_saved_server_rows();
        {
            let mut st = self.state.lock();
            st.saved_servers = rows;
        }
        self.push_to_ui();
    }

    /// User single-clicked a saved-server row — populate the connection
    /// fields so they can review + Connect. The password field is left
    /// empty; the user re-enters it (or the on-disk credential_ref will
    /// be re-fetched by [`run_connect_saved`] for the palette flow).
    pub fn select_saved_server(&self, id: &str) {
        let (server, credential_ref) = {
            let st = self.state.lock();
            if !st.saved_servers.iter().any(|r| r.id == id) {
                return;
            }
            drop(st);
            match list()
                .ok()
                .and_then(|all| all.into_iter().find(|s| s.id == id))
            {
                Some(s) => {
                    let cred = s.credential_ref.clone();
                    (Some(s), cred)
                }
                None => (None, None),
            }
        };
        let Some(server) = server else { return };

        // Best-effort fetch of the stored secret so a single-click on a
        // password-backed server auto-fills the password field. If the
        // keychain lookup fails (user denied access, secret missing) we
        // leave the field blank so the user can retype.
        let password = credential_ref
            .as_deref()
            .and_then(|handle| secrets::retrieve(handle).ok())
            .unwrap_or_default();

        {
            let mut st = self.state.lock();
            st.suppress_field_echo = true;

            let backend = match server.backend {
                BackendKind::Sftp | BackendKind::Local => BackendChoice::Sftp,
                BackendKind::Ftp => BackendChoice::Ftp,
                BackendKind::WebDav => BackendChoice::WebDav,
                BackendKind::S3 => BackendChoice::S3,
            };
            st.backend = Some(backend);
            st.auth = Some(AuthChoice::Password);
            // Render the merged host field: `host` when the port is
            // the backend default (or unset), `host:port` when the
            // user picked a non-default port. The structured `port`
            // state field mirrors the underlying value so
            // `assemble_location` still emits a well-formed URI.
            let default_port = server.backend.default_port();
            let show_port = match (server.port, default_port) {
                (Some(p), Some(dp)) if p == dp => false,
                (Some(_), _) => true,
                (None, _) => false,
            };
            st.host = if show_port {
                format!("{}:{}", server.address, server.port.unwrap())
            } else {
                server.address.clone()
            };
            st.port = server.port.map(|p| p.to_string()).unwrap_or_default();
            st.path = server.path.clone();
            st.username = server.username.clone().unwrap_or_default();
            st.password = password;
            st.label = server.label.clone();
            st.status_text.clear();
            st.status_is_error = false;
            for row in &mut st.saved_servers {
                row.delete_pending = false;
            }
            st.suppress_field_echo = false;
        }
        self.push_to_ui();
    }

    /// First-stage delete: flip `delete_pending` on the target row so
    /// the modal renders the inline "Confirm?" chip.
    pub fn request_delete_saved_server(&self, id: &str) {
        {
            let mut st = self.state.lock();
            for row in &mut st.saved_servers {
                row.delete_pending = row.id == id;
            }
        }
        self.push_to_ui();
    }

    /// Confirmed delete: remove the entry from `servers.toml` AND purge
    /// its keychain secret. Refreshes the list on success or failure.
    pub fn confirm_delete_saved_server(&self, id: &str) {
        match delete(id) {
            Ok(Some(removed)) => {
                if let Some(handle) = removed.credential_ref.as_deref() {
                    if let Err(err) = secrets::delete(handle) {
                        tracing::warn!(handle, error = %err, "connect: keychain purge failed");
                    }
                }
                tracing::info!(id, "connect: saved server deleted");
            }
            Ok(None) => {
                tracing::debug!(id, "connect: delete of unknown saved-server id");
            }
            Err(err) => {
                tracing::warn!(id, error = %err, "connect: delete saved server failed");
            }
        }
        self.refresh_saved_servers();
    }

    /// Cancel any pending delete-arm (user clicked Cancel next to the
    /// Confirm chip, or clicked a different row).
    pub fn cancel_delete_saved_server(&self) {
        {
            let mut st = self.state.lock();
            for row in &mut st.saved_servers {
                row.delete_pending = false;
            }
        }
        self.push_to_ui();
    }

    /// Palette dispatch: connect to the saved server with `id` and mount
    /// on `pane_id` — without opening the modal. Runs the connect worker
    /// in the background just like [`connect`], but skips the form.
    pub fn run_connect_saved(self: &Arc<Self>, id: &str, pane_id: PaneId) {
        let Some(server) = list()
            .ok()
            .and_then(|all| all.into_iter().find(|s| s.id == id))
        else {
            tracing::warn!(id, "connect: run_connect_saved for unknown id");
            return;
        };
        let credentials = credentials_from_saved(&server);
        // Normalise the persisted port to the backend default when
        // absent. Older `servers.toml` files (pre-Phase 2.12) may
        // carry `port: None` for entries the user saved before URI
        // normalisation moved to `assemble_location`; without this
        // call the run-once path would produce a different pool /
        // cred_key / known_hosts key than the connect-modal path, so
        // credentials cached at connect time would miss on the very
        // next re-open. See `atlas_core::RemoteUri::with_default_port`.
        let uri = atlas_core::RemoteUri {
            scheme: server.backend.scheme().to_string(),
            host: Some(server.address.clone()),
            port: server.port,
            username: server.username.clone(),
            path: if server.path.is_empty() {
                "/".to_owned()
            } else {
                server.path.clone()
            },
            credential_ref: server.credential_ref.clone(),
        }
        .with_default_port(server.backend);
        let location = Location::Remote(uri, server.backend);

        {
            let mut st = self.state.lock();
            if st.connecting {
                return;
            }
            st.target_pane = Some(pane_id);
            st.connecting = true;
            st.visible = false;
            let cancel = Arc::new(AtomicBool::new(false));
            st.cancel_flag = Some(Arc::clone(&cancel));
        }

        let cancel = self
            .state
            .lock()
            .cancel_flag
            .clone()
            .expect("cancel flag just set");
        let this = Arc::clone(self);

        std::thread::Builder::new()
            .name("atlas-connect-worker-saved".to_owned())
            .spawn(move || {
                this.run_connect(pane_id, location, credentials, None, cancel);
            })
            .expect("failed to spawn atlas-connect-worker-saved");
    }

    // ── Connect / Save+Connect ───────────────────────────────────────────────

    /// Dry-run connect (no persistence). Spawns the worker thread.
    pub fn connect(self: &Arc<Self>) {
        self.launch_connect(false);
    }

    /// Save the entered server to `~/.config/atlas/servers.toml`, stash
    /// the secret in the OS keychain (when applicable), then connect.
    pub fn save_and_connect(self: &Arc<Self>) {
        self.launch_connect(true);
    }

    fn launch_connect(self: &Arc<Self>, persist: bool) {
        let (pane_id, location, credentials, save_data) = {
            let mut st = self.state.lock();
            // Guard against double-clicks.
            if st.connecting {
                return;
            }
            let Some(pane) = st.target_pane else {
                return;
            };

            let (location, err_kind) = match assemble_location(&st) {
                Ok(loc) => (loc, None),
                Err(kind) => (Location::local(""), Some(kind)),
            };
            if let Some(kind) = err_kind {
                st.status_text = kind.banner().to_owned();
                st.status_is_error = true;
                drop(st);
                self.push_to_ui();
                return;
            }

            let (credentials, cred_err) = assemble_credentials(&st);
            if let Some(msg) = cred_err {
                st.status_text = msg;
                st.status_is_error = true;
                drop(st);
                self.push_to_ui();
                return;
            }

            let save_data = if persist {
                Some(SaveIntent::from_state(&st, &location))
            } else {
                None
            };

            st.connecting = true;
            st.status_text = "Connecting…".to_owned();
            st.status_is_error = false;
            let cancel = Arc::new(AtomicBool::new(false));
            st.cancel_flag = Some(Arc::clone(&cancel));

            (pane, location, credentials, save_data)
        };

        self.push_to_ui();

        let cancel = self
            .state
            .lock()
            .cancel_flag
            .clone()
            .expect("cancel flag just set");
        let this = Arc::clone(self);

        std::thread::Builder::new()
            .name("atlas-connect-worker".to_owned())
            .spawn(move || {
                this.run_connect(pane_id, location, credentials, save_data, cancel);
            })
            .expect("failed to spawn atlas-connect-worker");
    }

    fn run_connect(
        self: Arc<Self>,
        pane_id: PaneId,
        location: Location,
        credentials: Credentials,
        save_data: Option<SaveIntent>,
        cancel: Arc<AtomicBool>,
    ) {
        // Step 1 — build the vm (fast; just constructs the backend
        // handshake handle). We call the concrete constructor here
        // rather than `atlas_remote::backend::open` so we can attach
        // retry-observer hooks before mounting.
        let (remote_uri, backend_kind) = match &location {
            Location::Remote(uri, kind) => (uri.clone(), *kind),
            Location::Local(_) => {
                self.report_error(
                    ErrorKind::Malformed,
                    "connect: local location cannot be mounted through the connect modal"
                        .to_owned(),
                );
                return;
            }
        };
        let concrete = match backend_kind {
            BackendKind::Sftp => RemoteLocationViewModel::open_live_sftp_with_options(
                remote_uri.clone(),
                credentials.clone(),
                OpenOptions::default(),
                SftpOptions {
                    known_hosts_mode: KnownHostsMode::Prompt,
                    resolver: Some(self.host_key_resolver()),
                },
            ),
            _ => RemoteLocationViewModel::open_live(
                remote_uri.clone(),
                backend_kind,
                credentials.clone(),
                OpenOptions::default(),
            ),
        };
        let concrete = match concrete {
            Ok(vm) => vm,
            Err(err) => {
                self.report_error(err_from_backend(&err), format!("{err}"));
                return;
            }
        };
        let vm: Arc<dyn atlas_fs::LocationViewModel> = concrete.clone();

        // Step 2 — probe the connection by listening for the first
        // `Loaded` or `Error` event. Bounded timeout so a black-hole
        // server can't hang the modal. The TOFU prompt (Phase 2.6) can
        // pause the handshake for up to 60 s while the user reads and
        // clicks — we detect that state via `host_key_prompt.is_some()`
        // and skip timeout accounting for those ticks.
        const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
        let events = vm.subscribe();
        let start = Instant::now();
        let mut prompt_pause = Duration::ZERO;
        let mut prompt_started: Option<Instant> = None;
        let mut probe_error: Option<String> = None;
        loop {
            if cancel.load(Ordering::SeqCst) {
                tracing::info!("connect: cancelled");
                return;
            }
            // Track how long we've spent waiting for the user on a TOFU
            // prompt; subtract it from the probe budget so the modal
            // doesn't time-out under the user's fingers.
            let prompt_active = self.state.lock().host_key_prompt.is_some();
            match (prompt_active, prompt_started) {
                (true, None) => prompt_started = Some(Instant::now()),
                (false, Some(t0)) => {
                    prompt_pause = prompt_pause.saturating_add(t0.elapsed());
                    prompt_started = None;
                }
                _ => {}
            }
            let elapsed = start
                .elapsed()
                .saturating_sub(prompt_pause)
                .saturating_sub(prompt_started.map(|t| t.elapsed()).unwrap_or_default());
            let remaining = PROBE_TIMEOUT.saturating_sub(elapsed);
            if remaining.is_zero() {
                self.report_error(
                    ErrorKind::Network,
                    "Timed out waiting for the server response.".to_owned(),
                );
                return;
            }
            match events.recv_timeout(Duration::from_millis(200)) {
                Ok(ViewModelEvent::Loaded) => break,
                Ok(ViewModelEvent::Error(msg)) => {
                    probe_error = Some(msg);
                    break;
                }
                Ok(ViewModelEvent::EntriesChanged) => {
                    if vm.is_loaded() {
                        break;
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    if vm.is_loaded() {
                        break;
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    break;
                }
            }
        }

        if let Some(msg) = probe_error {
            let kind = classify_probe_error(&msg);
            self.report_error(kind, msg);
            return;
        }

        if cancel.load(Ordering::SeqCst) {
            return;
        }

        // Step 3 — persist (if the user checked "Save to keychain") and
        // mount the vm onto the pane. When persist succeeds it returns
        // the credential handle; we splice it into the pane's
        // Location so atlas-ops (which re-opens the location for
        // copy/paste ops) can retrieve the secret from the keychain.
        let mut location = location;
        if let Some(save) = save_data {
            match save.persist(&credentials) {
                Ok(Some(cred_ref)) => {
                    if let Location::Remote(uri, _) = &mut location {
                        uri.credential_ref = Some(cred_ref);
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "connect: failed to persist saved server");
                }
            }
        }

        // Step 3.5 — cache the credentials in atlas-ops for this session
        // so cross-backend copy/paste can reconnect without touching the
        // OS keychain (which would trigger a dialog on macOS).
        if let Location::Remote(uri, _) = &location {
            atlas_ops::cache_session_credentials(uri, credentials.clone());
        }

        let shell = self.shell.lock().upgrade();
        if let Some(shell) = shell {
            shell.open_remote_location(pane_id, location.clone(), vm, Some(concrete));
        } else {
            tracing::warn!("connect: shell weak-ref lost before mount");
        }

        // Bump `last_connected` for the matching saved server if any.
        // This also runs on Save+Connect (harmless double-write) so the
        // sort in the modal viewer is always accurate.
        bump_last_connected_for(&location);

        {
            let mut st = self.state.lock();
            st.connecting = false;
            st.status_text.clear();
            st.status_is_error = false;
            st.visible = false;
            st.cancel_flag = None;
        }
        // Refresh the saved-servers list so the modal picks up the bumped
        // recency (harmless when the modal is closed — cheap toml read).
        {
            let rows = load_saved_server_rows();
            self.state.lock().saved_servers = rows;
        }
        self.push_to_ui();
    }

    fn report_error(&self, kind: ErrorKind, detail: String) {
        {
            let mut st = self.state.lock();
            st.connecting = false;
            st.status_text = format!("{}\n{}", kind.banner(), detail);
            st.status_is_error = true;
            st.cancel_flag = None;
            // Auth failures: prefill password (blank) so the user can
            // immediately retype. We only clear the *stored* password
            // buffer; the Slint TextInput will pick this up on the next
            // property push.
            if matches!(kind, ErrorKind::Auth) {
                st.password.clear();
            }
        }
        self.push_to_ui();
    }

    // ── UI push ───────────────────────────────────────────────────────────────

    fn push_to_ui(&self) {
        let snapshot = self.state.lock().snapshot();
        let window = self.window.lock().clone();
        let _ = slint::invoke_from_event_loop(move || {
            use slint::{ModelRc, SharedString, VecModel};

            let Some(win) = window.upgrade() else {
                return;
            };
            win.set_connect_modal_visible(snapshot.visible);
            win.set_connect_backend_index(snapshot.backend_index);
            win.set_connect_auth_index(snapshot.auth_index);
            win.set_connect_host(snapshot.host.into());
            win.set_connect_path(snapshot.path.into());
            win.set_connect_username(snapshot.username.into());
            win.set_connect_password(snapshot.password.into());
            win.set_connect_ssh_key_path(snapshot.ssh_key_path.into());
            win.set_connect_iam_access_key(snapshot.iam_access_key.into());
            win.set_connect_iam_secret_key(snapshot.iam_secret_key.into());
            win.set_connect_iam_session_token(snapshot.iam_session_token.into());
            win.set_connect_s3_endpoint(snapshot.s3_endpoint.into());
            win.set_connect_label(snapshot.label.into());
            win.set_connect_save_to_keychain(snapshot.save_to_keychain);
            win.set_connect_connecting(snapshot.connecting);
            win.set_connect_status_text(snapshot.status_text.into());
            win.set_connect_status_is_error(snapshot.status_is_error);
            win.set_connect_host_key_prompt_visible(snapshot.host_key_prompt_visible);
            win.set_connect_host_key_host(snapshot.host_key_host.into());
            win.set_connect_host_key_fingerprint(snapshot.host_key_fingerprint.into());
            win.set_connect_host_key_mismatch_known_fingerprint(
                snapshot.host_key_mismatch_known_fingerprint.into(),
            );

            let rows: Vec<crate::SavedServerRow> = snapshot
                .saved_servers
                .into_iter()
                .map(|r| crate::SavedServerRow {
                    id: SharedString::from(r.id),
                    label: SharedString::from(r.label),
                    address: SharedString::from(r.address),
                    glyph: SharedString::from(r.glyph),
                    #[allow(non_snake_case)]
                    last_connected_relative: SharedString::from(r.last_connected_relative),
                    delete_pending: r.delete_pending,
                })
                .collect();
            win.set_connect_saved_servers(ModelRc::new(VecModel::from(rows)));
        });
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> StateSnapshot {
        self.state.lock().snapshot()
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub(crate) struct StateSnapshot {
    pub visible: bool,
    pub backend_index: i32,
    pub auth_index: i32,
    pub host: String,
    /// Not surfaced to Slint (Host input now accepts `host:port`) but
    /// still readable via [`ConnectController::snapshot`] and tests.
    #[allow(dead_code)]
    pub port: String,
    pub path: String,
    pub username: String,
    pub password: String,
    pub ssh_key_path: String,
    pub iam_access_key: String,
    pub iam_secret_key: String,
    pub iam_session_token: String,
    pub s3_endpoint: String,
    pub label: String,
    pub save_to_keychain: bool,
    pub connecting: bool,
    pub status_text: String,
    pub status_is_error: bool,
    pub saved_servers: Vec<SavedServerRow>,
    /// True while a TOFU host-key banner is being displayed.
    pub host_key_prompt_visible: bool,
    pub host_key_host: String,
    pub host_key_fingerprint: String,
    /// Empty if the current prompt is Unknown; non-empty ⇒ Mismatch.
    pub host_key_mismatch_known_fingerprint: String,
}

impl State {
    fn snapshot(&self) -> StateSnapshot {
        StateSnapshot {
            visible: self.visible,
            backend_index: self.backend.map(|b| b as i32).unwrap_or(0),
            auth_index: self.auth.map(|a| a as i32).unwrap_or(0),
            host: self.host.clone(),
            port: self.port.clone(),
            path: self.path.clone(),
            username: self.username.clone(),
            password: self.password.clone(),
            ssh_key_path: self.ssh_key_path.clone(),
            iam_access_key: self.iam_access_key.clone(),
            iam_secret_key: self.iam_secret_key.clone(),
            iam_session_token: self.iam_session_token.clone(),
            s3_endpoint: self.s3_endpoint.clone(),
            label: self.label.clone(),
            save_to_keychain: self.save_to_keychain,
            connecting: self.connecting,
            status_text: self.status_text.clone(),
            status_is_error: self.status_is_error,
            saved_servers: self.saved_servers.clone(),
            host_key_prompt_visible: self.host_key_prompt.is_some(),
            host_key_host: self
                .host_key_prompt
                .as_ref()
                .map(|p| p.host.clone())
                .unwrap_or_default(),
            host_key_fingerprint: self
                .host_key_prompt
                .as_ref()
                .map(|p| p.offered_fingerprint.clone())
                .unwrap_or_default(),
            host_key_mismatch_known_fingerprint: self
                .host_key_prompt
                .as_ref()
                .map(|p| p.known_fingerprint.clone())
                .unwrap_or_default(),
        }
    }
}

fn default_label(user: &Option<String>, host: &Option<String>) -> String {
    match (user.as_deref(), host.as_deref()) {
        (Some(u), Some(h)) if !u.is_empty() && !h.is_empty() => format!("{u}@{h}"),
        (_, Some(h)) if !h.is_empty() => h.to_owned(),
        _ => String::new(),
    }
}

/// Split a `host` or `host:port` literal into `(host, port_string)`.
///
/// * Returns `Some((host, port_str))` when the input contains a valid
///   `u16` port suffix after a `:` (respecting IPv6 bracket literals so
///   `[::1]:22` yields host = `[::1]`, port = `22`).
/// * Returns `None` when there is no explicit port suffix — the caller
///   should treat the whole input as the host and leave the port
///   unchanged (or default from the backend).
fn split_host_port(s: &str) -> Option<(String, String)> {
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_prefix('[') {
        let close = rest.find(']')?;
        let host = format!("[{}]", &rest[..close]);
        let after = &rest[close + 1..];
        let port = after.strip_prefix(':')?;
        if port.parse::<u16>().is_ok() {
            return Some((host, port.to_owned()));
        }
        return None;
    }
    let colon = s.rfind(':')?;
    if s[..colon].contains(':') {
        return None;
    }
    let port = &s[colon + 1..];
    if port.parse::<u16>().is_ok() {
        Some((s[..colon].to_owned(), port.to_owned()))
    } else {
        None
    }
}

/// Assemble the [`Location`] the worker will hand to
/// [`atlas_remote::backend::open`] from the current state.
fn assemble_location(st: &State) -> Result<Location, ErrorKind> {
    let backend = st.backend.unwrap_or(BackendChoice::Sftp);
    let kind = backend.as_kind();
    if st.host.trim().is_empty() {
        return Err(ErrorKind::Malformed);
    }
    let explicit_port = if st.port.trim().is_empty() {
        None
    } else {
        Some(
            st.port
                .trim()
                .parse::<u16>()
                .map_err(|_| ErrorKind::Malformed)?,
        )
    };
    let path = if st.path.is_empty() {
        "/".to_owned()
    } else if st.path.starts_with('/') {
        st.path.clone()
    } else {
        format!("/{}", st.path)
    };
    // Normalise the port to the backend default when the user omitted
    // it. All downstream layers (connection pool key, credentials
    // cache key, known-hosts store, servers.toml dedup, keychain
    // account) see the same value regardless of whether the user
    // typed `test.rebex.net` or `test.rebex.net:22`. This is the
    // canonical fix for the reported bug where `sftp://user@host`
    // failed but `sftp://user@host:22` succeeded — cache and pool
    // lookups were keyed on `port: None` and could not reuse entries
    // stored under `port: Some(22)`. See
    // `atlas_core::RemoteUri::with_default_port` for the single-source
    // helper every construction site now routes through.
    let uri = atlas_core::RemoteUri {
        scheme: kind.scheme().to_string(),
        host: Some(st.host.trim().to_owned()),
        port: explicit_port,
        username: if st.username.is_empty() {
            None
        } else {
            Some(st.username.clone())
        },
        path,
        credential_ref: None,
    }
    .with_default_port(kind);
    Ok(Location::Remote(uri, kind))
}

fn assemble_credentials(st: &State) -> (Credentials, Option<String>) {
    let auth = st.auth.unwrap_or(AuthChoice::Password);
    match auth {
        AuthChoice::Password => {
            if st.password.is_empty() {
                (
                    Credentials::Password(String::new()),
                    Some("Password is required.".to_owned()),
                )
            } else {
                (Credentials::Password(st.password.clone()), None)
            }
        }
        AuthChoice::SshKey => {
            let path = st.ssh_key_path.trim();
            if path.is_empty() {
                (
                    Credentials::Anonymous,
                    Some("SSH key path is required.".to_owned()),
                )
            } else {
                let expanded = atlas_core::path::expand_tilde(PathBuf::from(path));
                let password_opt = if st.password.is_empty() {
                    None
                } else {
                    Some(st.password.clone())
                };
                (Credentials::SshKey(expanded, password_opt), None)
            }
        }
        AuthChoice::Iam => {
            if st.iam_access_key.is_empty() || st.iam_secret_key.is_empty() {
                (
                    Credentials::Iam {
                        access_key_id: st.iam_access_key.clone(),
                        secret_key: st.iam_secret_key.clone(),
                        session_token: None,
                    },
                    Some("Access key ID and secret key are required.".to_owned()),
                )
            } else {
                let session = if st.iam_session_token.is_empty() {
                    None
                } else {
                    Some(st.iam_session_token.clone())
                };
                (
                    Credentials::Iam {
                        access_key_id: st.iam_access_key.clone(),
                        secret_key: st.iam_secret_key.clone(),
                        session_token: session,
                    },
                    None,
                )
            }
        }
        AuthChoice::Anonymous => (Credentials::Anonymous, None),
    }
}

fn err_from_backend(err: &BackendError) -> ErrorKind {
    match err {
        BackendError::InvalidCredentials { .. } => ErrorKind::Auth,
        BackendError::UnsupportedBackend(_) => ErrorKind::Malformed,
        BackendError::Backend(msg) => classify_probe_error(msg),
    }
}

/// Classify a probe error message into one of the four coarse buckets
/// used by the modal status area.
fn classify_probe_error(msg: &str) -> ErrorKind {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("permission")
        || lower.contains("denied")
        || lower.contains("unauthorized")
        || lower.contains("authentication")
        || lower.contains("password")
        || lower.contains("access denied")
        || lower.contains("credentials")
    {
        ErrorKind::Auth
    } else if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("unreachable")
        || lower.contains("refused")
        || lower.contains("no route")
        || lower.contains("resolve")
        || lower.contains("dns")
        || lower.contains("connection")
    {
        ErrorKind::Network
    } else {
        // "not found" / "no such" / everything else — bucket as Server.
        // Kept as a single arm because there's currently no field-level
        // differentiation between them; expand when the modal gains
        // per-field highlights.
        ErrorKind::Server
    }
}

/// Unicode glyph rendered next to a saved-server row and in the pane
/// header for remote panes. Kept as a re-export around
/// [`BackendKind::glyph`] so tests and callers within this module can
/// find it under the same name.
#[must_use]
pub(crate) fn backend_glyph(kind: BackendKind) -> &'static str {
    kind.glyph()
}

/// Human-friendly relative time. Returns "just now" for < 60s,
/// "Nm ago" for minutes, "Nh ago" for hours, "Nd ago" for days,
/// or the epoch seconds itself for very old timestamps.  "never"
/// when the timestamp is missing.
fn relative_time(epoch_secs: Option<u64>) -> String {
    let Some(then) = epoch_secs else {
        return "never".to_owned();
    };
    let now = now_unix();
    if then >= now {
        return "just now".to_owned();
    }
    let delta = now - then;
    if delta < 60 {
        "just now".to_owned()
    } else if delta < 3600 {
        format!("{}m ago", delta / 60)
    } else if delta < 86_400 {
        format!("{}h ago", delta / 3600)
    } else if delta < 30 * 86_400 {
        format!("{}d ago", delta / 86_400)
    } else if delta < 365 * 86_400 {
        format!("{}mo ago", delta / (30 * 86_400))
    } else {
        format!("{}y ago", delta / (365 * 86_400))
    }
}

/// Build the display URI shown in the saved-servers list. Mirrors
/// `Location::Remote(uri, kind)`'s `Display` impl but resilient to
/// missing username/port fields.
fn saved_server_uri(server: &SavedServer) -> String {
    let mut s = String::new();
    s.push_str(server.backend.scheme());
    s.push_str("://");
    if let Some(user) = &server.username {
        s.push_str(user);
        s.push('@');
    }
    s.push_str(&server.address);
    if let Some(port) = server.port {
        s.push(':');
        s.push_str(&port.to_string());
    }
    if server.path.is_empty() {
        s.push('/');
    } else if !server.path.starts_with('/') {
        s.push('/');
        s.push_str(&server.path);
    } else {
        s.push_str(&server.path);
    }
    s
}

/// Load all saved servers and convert them into UI-ready rows sorted
/// last-connected desc. Never returns an error — failures degrade to
/// an empty list plus a warn log.
fn load_saved_server_rows() -> Vec<SavedServerRow> {
    match list() {
        Ok(list) => list
            .into_iter()
            .map(|server| SavedServerRow {
                glyph: backend_glyph(server.backend).to_owned(),
                last_connected_relative: relative_time(server.last_connected),
                address: saved_server_uri(&server),
                label: if server.label.is_empty() {
                    default_label(&server.username, &Some(server.address.clone()))
                } else {
                    server.label.clone()
                },
                id: server.id,
                delete_pending: false,
            })
            .collect(),
        Err(err) => {
            tracing::warn!(error = %err, "connect: failed to load servers.toml");
            Vec::new()
        }
    }
}

/// Build a [`Credentials`] value from a saved-server record, fetching
/// the associated secret from the OS keychain if a `credential_ref` is
/// present. Returns [`Credentials::Anonymous`] on keychain failure so
/// the connect flow can still surface a coherent error banner rather
/// than exploding.
pub(crate) fn credentials_from_saved(server: &SavedServer) -> Credentials {
    let Some(handle) = server.credential_ref.as_deref() else {
        return Credentials::Anonymous;
    };
    match secrets::retrieve(handle) {
        Ok(secret) => match server.backend {
            BackendKind::S3 => Credentials::Iam {
                access_key_id: server.username.clone().unwrap_or_default(),
                secret_key: secret,
                session_token: None,
            },
            _ => Credentials::Password(secret),
        },
        Err(err) => {
            tracing::warn!(handle, error = %err, "connect: keychain fetch failed");
            Credentials::Anonymous
        }
    }
}

/// Bump `last_connected` on any saved server whose dedup-key tuple
/// matches the location we just mounted. Silent no-op if the location
/// isn't remote or no saved entry matches.
fn bump_last_connected_for(location: &Location) {
    let Location::Remote(uri, kind) = location else {
        return;
    };
    let host = match &uri.host {
        Some(h) => h.clone(),
        None => return,
    };
    let saved = match list() {
        Ok(v) => v,
        Err(_) => return,
    };
    let matching = saved.into_iter().find(|s| {
        s.backend == *kind && s.address == host && s.port == uri.port && s.username == uri.username
    });
    if let Some(mut server) = matching {
        server.last_connected = Some(now_unix());
        if let Err(err) = add_or_replace(server) {
            tracing::debug!(error = %err, "connect: could not bump last_connected");
        }
    }
}

/// A tiny value type carrying everything needed to persist a
/// [`SavedServer`] + keychain entry. Assembled at connect-time under the
/// state lock, then consumed on the worker thread once the probe
/// succeeds.
struct SaveIntent {
    label: String,
    backend: BackendKind,
    host: String,
    port: Option<u16>,
    path: String,
    username: Option<String>,
}

impl SaveIntent {
    fn from_state(st: &State, location: &Location) -> Self {
        let (host, port, path, username) = match location {
            Location::Remote(uri, _) => (
                uri.host.clone().unwrap_or_default(),
                uri.port,
                uri.path.clone(),
                uri.username.clone(),
            ),
            Location::Local(_) => (String::new(), None, String::new(), None),
        };
        let label = if st.label.is_empty() {
            default_label(&username, &Some(host.clone()))
        } else {
            st.label.clone()
        };
        Self {
            label,
            backend: location.backend(),
            host,
            port,
            path,
            username,
        }
    }

    fn persist(&self, credentials: &Credentials) -> anyhow::Result<Option<String>> {
        let credential_ref = match credentials {
            Credentials::Password(secret) if !secret.is_empty() => {
                let namespace = keychain_namespace(self.backend);
                let account = self.keychain_account();
                let handle = store_or_recover(&namespace, &account, secret)?;
                Some(handle)
            }
            Credentials::Iam {
                access_key_id,
                secret_key,
                ..
            } if !access_key_id.is_empty() && !secret_key.is_empty() => {
                let namespace = keychain_namespace(self.backend);
                let account = format!("{}#{}", self.keychain_account(), access_key_id);
                let handle = store_or_recover(&namespace, &account, secret_key)?;
                Some(handle)
            }
            _ => None,
        };

        let server = SavedServer {
            id: uuid_like_id(&self.backend, &self.host, self.port, &self.username),
            label: self.label.clone(),
            backend: self.backend,
            address: self.host.clone(),
            port: self.port,
            path: self.path.clone(),
            username: self.username.clone(),
            credential_ref: credential_ref.clone(),
            last_connected: Some(now_unix()),
        };
        add_or_replace(server).map_err(|e| anyhow::anyhow!("save servers.toml: {e}"))?;
        Ok(credential_ref)
    }

    fn keychain_account(&self) -> String {
        let user = self.username.as_deref().unwrap_or("_");
        let port = self.port.map(|p| p.to_string()).unwrap_or_default();
        format!("{}@{}:{}", user, self.host, port)
    }
}

fn keychain_namespace(kind: BackendKind) -> String {
    format!("com.atlas.remote.{}", kind.scheme())
}

/// Store `secret` under (`namespace`, `account`); on macOS if the
/// entry already exists with the same value, avoid re-writing (which
/// would trigger the OS keychain-access dialog). Returns the
/// deterministic credential handle in either case. If the existing
/// entry has a different secret we still try to write and propagate
/// the OS's error message on failure.
fn store_or_recover(namespace: &str, account: &str, secret: &str) -> anyhow::Result<String> {
    let handle_str = format!("{namespace}::{account}");

    // Fast path — if we've stored this exact secret before, the OS
    // keychain lookup returns it without prompting (the app has
    // read-access from the initial `set_password` call, and read is
    // silent even after app relaunch).
    if let Ok(existing) = secrets::retrieve(&handle_str) {
        if existing == secret {
            return Ok(handle_str);
        }
    }

    match secrets::store(namespace, account, secret) {
        Ok(handle) => Ok(handle.into_string()),
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("already exists") || msg.contains("Duplicate") {
                tracing::info!(
                    namespace = %namespace,
                    "keychain entry already exists; reusing existing credential_ref"
                );
                Ok(handle_str)
            } else {
                Err(anyhow::anyhow!("keychain store failed: {err}"))
            }
        }
    }
}

fn now_unix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn uuid_like_id(
    backend: &BackendKind,
    host: &str,
    port: Option<u16>,
    user: &Option<String>,
) -> String {
    let port_s = port.map(|p| p.to_string()).unwrap_or_default();
    let user_s = user.as_deref().unwrap_or("");
    format!("{}::{}::{}::{}", backend.scheme(), user_s, host, port_s)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl_with_pane() -> Arc<ConnectController> {
        let c = ConnectController::new();
        c.open(PaneId(0));
        c
    }

    #[test]
    fn open_sets_defaults_and_focuses_sftp_password() {
        let c = ctrl_with_pane();
        let snap = c.snapshot();
        assert!(snap.visible);
        assert_eq!(snap.backend_index, BackendChoice::Sftp as i32);
        assert_eq!(snap.auth_index, AuthChoice::Password as i32);
        assert!(snap.save_to_keychain);
        assert!(snap.status_text.is_empty());
    }

    #[test]
    fn close_hides_and_clears_status() {
        let c = ctrl_with_pane();
        c.set_password("secret".into());
        c.close();
        let snap = c.snapshot();
        assert!(!snap.visible);
        assert!(snap.status_text.is_empty());
    }

    #[test]
    fn set_backend_s3_defaults_auth_to_iam() {
        let c = ctrl_with_pane();
        c.set_backend(BackendChoice::S3 as i32);
        let snap = c.snapshot();
        assert_eq!(snap.backend_index, BackendChoice::S3 as i32);
        assert_eq!(snap.auth_index, AuthChoice::Iam as i32);
    }

    #[test]
    fn set_backend_webdav_defaults_auth_to_password() {
        let c = ctrl_with_pane();
        c.set_backend(BackendChoice::WebDav as i32);
        let snap = c.snapshot();
        assert_eq!(snap.auth_index, AuthChoice::Password as i32);
    }

    #[test]
    fn field_edits_populate_state() {
        let c = ctrl_with_pane();
        c.set_host("h.example.com".into());
        c.set_port("22".into());
        c.set_username("bob".into());
        c.set_path("/srv".into());
        let snap = c.snapshot();
        assert_eq!(snap.host, "h.example.com");
        assert_eq!(snap.port, "22");
        assert_eq!(snap.username, "bob");
        assert_eq!(snap.path, "/srv");
    }

    #[test]
    fn assemble_location_uses_structured_fields() {
        let st = State {
            save_to_keychain: true,
            backend: Some(BackendChoice::Sftp),
            auth: Some(AuthChoice::Password),
            host: "h2".into(),
            port: String::new(),
            path: "sub".into(),
            ..State::default()
        };
        let loc = assemble_location(&st).unwrap();
        let Location::Remote(uri, kind) = loc else {
            panic!()
        };
        assert_eq!(kind, BackendKind::Sftp);
        assert_eq!(uri.host.as_deref(), Some("h2"));
        assert_eq!(uri.path, "/sub");
        // Empty port defaults to SFTP's IANA port so downstream
        // pool / cred / known-hosts layers agree on the effective
        // port with the sftp VM (which also defaults to 22).
        assert_eq!(uri.port, Some(22));
    }

    #[test]
    fn assemble_location_normalises_port_per_backend() {
        for (backend, expected) in [
            (BackendChoice::Sftp, Some(22)),
            (BackendChoice::Ftp, Some(21)),
            (BackendChoice::WebDav, Some(443)),
            (BackendChoice::S3, None),
        ] {
            let st = State {
                save_to_keychain: true,
                backend: Some(backend),
                auth: Some(AuthChoice::Anonymous),
                host: "h".into(),
                ..State::default()
            };
            let Location::Remote(uri, _) = assemble_location(&st).unwrap() else {
                panic!()
            };
            assert_eq!(
                uri.port, expected,
                "backend {backend:?} port must default to {expected:?}"
            );
        }
    }

    #[test]
    fn assemble_location_explicit_port_wins_over_default() {
        let st = State {
            save_to_keychain: true,
            backend: Some(BackendChoice::Sftp),
            auth: Some(AuthChoice::Anonymous),
            host: "h".into(),
            port: "2222".into(),
            ..State::default()
        };
        let Location::Remote(uri, _) = assemble_location(&st).unwrap() else {
            panic!()
        };
        assert_eq!(uri.port, Some(2222));
    }

    #[test]
    fn assemble_location_missing_host_is_malformed() {
        let st = State {
            save_to_keychain: true,
            backend: Some(BackendChoice::Sftp),
            auth: Some(AuthChoice::Password),
            ..State::default()
        };
        assert!(matches!(assemble_location(&st), Err(ErrorKind::Malformed)));
    }

    #[test]
    fn assemble_credentials_password_empty_errors() {
        let st = State {
            save_to_keychain: true,
            auth: Some(AuthChoice::Password),
            ..State::default()
        };
        let (_, err) = assemble_credentials(&st);
        assert!(err.is_some());
    }

    #[test]
    fn assemble_credentials_iam_needs_key_pair() {
        let mut st = State {
            save_to_keychain: true,
            auth: Some(AuthChoice::Iam),
            ..State::default()
        };
        let (_, err) = assemble_credentials(&st);
        assert!(err.is_some());
        st.iam_access_key = "AKIA".into();
        st.iam_secret_key = "SECRET".into();
        let (creds, err) = assemble_credentials(&st);
        assert!(err.is_none());
        assert!(matches!(creds, Credentials::Iam { .. }));
    }

    #[test]
    fn assemble_credentials_anonymous_never_errors() {
        let st = State {
            save_to_keychain: true,
            auth: Some(AuthChoice::Anonymous),
            ..State::default()
        };
        let (creds, err) = assemble_credentials(&st);
        assert!(err.is_none());
        assert!(matches!(creds, Credentials::Anonymous));
    }

    #[test]
    fn classify_probe_error_buckets() {
        assert!(matches!(
            classify_probe_error("permission denied"),
            ErrorKind::Auth
        ));
        assert!(matches!(
            classify_probe_error("Authentication failed"),
            ErrorKind::Auth
        ));
        assert!(matches!(
            classify_probe_error("connection refused"),
            ErrorKind::Network
        ));
        assert!(matches!(
            classify_probe_error("dns lookup failed"),
            ErrorKind::Network
        ));
        assert!(matches!(
            classify_probe_error("not found"),
            ErrorKind::Server
        ));
    }

    #[test]
    fn default_label_uses_user_at_host() {
        let out = default_label(&Some("alice".into()), &Some("host".into()));
        assert_eq!(out, "alice@host");
        let out = default_label(&None, &Some("host".into()));
        assert_eq!(out, "host");
        let out = default_label(&None, &None);
        assert!(out.is_empty());
    }

    #[test]
    fn split_host_port_parses_variants() {
        assert_eq!(
            split_host_port("test.rebex.net:22"),
            Some(("test.rebex.net".into(), "22".into()))
        );
        assert_eq!(
            split_host_port("[::1]:2222"),
            Some(("[::1]".into(), "2222".into()))
        );
        assert_eq!(split_host_port("test.rebex.net"), None);
        assert_eq!(split_host_port(""), None);
        // Non-numeric port suffix — treat whole string as host.
        assert_eq!(split_host_port("host:abc"), None);
        // Bare IPv6 without port stays a host.
        assert_eq!(split_host_port("[::1]"), None);
    }

    #[test]
    fn set_host_splits_host_and_port() {
        let c = ctrl_with_pane();
        c.set_host("test.rebex.net:2222".into());
        let snap = c.snapshot();
        assert_eq!(snap.host, "test.rebex.net");
        assert_eq!(snap.port, "2222");
    }

    #[test]
    fn set_host_without_port_leaves_port_field_alone() {
        let c = ctrl_with_pane();
        c.set_port("2200".into());
        c.set_host("test.rebex.net".into());
        let snap = c.snapshot();
        assert_eq!(snap.host, "test.rebex.net");
        assert_eq!(snap.port, "2200");
    }

    #[test]
    fn keychain_namespace_uses_backend_scheme() {
        assert_eq!(
            keychain_namespace(BackendKind::Sftp),
            "com.atlas.remote.sftp"
        );
        assert_eq!(keychain_namespace(BackendKind::S3), "com.atlas.remote.s3");
    }
}
