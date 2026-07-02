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

use atlas_config::servers::{add_or_replace, SavedServer};
use atlas_core::{BackendKind, Location, LocationParseError};
use atlas_fs::{OpenOptions, ViewModelEvent};
use atlas_remote::{
    backend::{open as backend_open, BackendError, Credentials},
    secrets,
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

    #[allow(dead_code)] // used by test module + reserved for the port-placeholder logic
    fn default_port(self) -> u16 {
        match self {
            Self::Sftp => 22,
            Self::Ftp => 21,
            Self::WebDav | Self::S3 => 443,
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
    connection_string: String,
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
    /// Set to `true` when the parser echoes an update BACK into the field
    /// bindings — prevents an infinite ping-pong (`field-changed` → parse
    /// → set-field → `field-changed`…).
    suppress_field_echo: bool,
    /// Cancellation flag for the currently-active worker thread. Setting
    /// this to `true` causes the worker to bail out before mounting.
    cancel_flag: Option<Arc<AtomicBool>>,
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
        {
            let mut st = self.state.lock();
            *st = State {
                save_to_keychain: true,
                visible: true,
                target_pane: Some(pane_id),
                backend: Some(BackendChoice::Sftp),
                auth: Some(AuthChoice::Password),
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

    /// Freeform-connection-string edit. Attempts to reparse and echo the
    /// canonical host / port / path / user into the individual fields.
    /// The `suppress_field_echo` flag is set on the state briefly so the
    /// resulting host-changed / port-changed callbacks (fired by Slint
    /// after the property push) don't loop back and clobber what we just
    /// wrote.
    pub fn set_connection_string(&self, s: String) {
        {
            let mut st = self.state.lock();
            st.connection_string = s.clone();
            st.status_text.clear();
            st.status_is_error = false;

            // Attempt to parse and echo into fields.
            match Location::parse_freeform(&s) {
                Ok(Location::Remote(uri, kind)) => {
                    st.suppress_field_echo = true;
                    st.backend = Some(match kind {
                        BackendKind::Sftp | BackendKind::Local => BackendChoice::Sftp,
                        BackendKind::Ftp => BackendChoice::Ftp,
                        BackendKind::WebDav => BackendChoice::WebDav,
                        BackendKind::S3 => BackendChoice::S3,
                    });
                    st.host = uri.host.clone().unwrap_or_default();
                    st.port = uri.port.map(|p| p.to_string()).unwrap_or_default();
                    st.path = uri.path.clone();
                    st.username = uri.username.clone().unwrap_or_default();
                    if st.label.is_empty() {
                        st.label = default_label(&uri.username, &uri.host);
                    }
                }
                Ok(Location::Local(_)) => {
                    // Local paths aren't a remote connect target — leave the
                    // individual fields alone.
                }
                Err(_) => {
                    // Unparseable → keep manual entry live.
                }
            }
        }
        self.push_to_ui();
        // Clear the suppression on the next tick so future field edits
        // still update the connection string.
        {
            let mut st = self.state.lock();
            st.suppress_field_echo = false;
        }
    }

    pub fn set_host(&self, s: String) {
        {
            let mut st = self.state.lock();
            if st.suppress_field_echo {
                return;
            }
            st.host = s;
            st.status_text.clear();
            st.status_is_error = false;
            resync_connection_string(&mut st);
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
            resync_connection_string(&mut st);
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
            resync_connection_string(&mut st);
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
            resync_connection_string(&mut st);
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
        // Step 1 — build the vm (fast; just constructs the OpenDAL operator).
        let vm = match backend_open(&location, credentials.clone(), OpenOptions::default()) {
            Ok(vm) => vm,
            Err(err) => {
                self.report_error(err_from_backend(&err), format!("{err}"));
                return;
            }
        };

        // Step 2 — probe the connection by listening for the first
        // `Loaded` or `Error` event. Bounded timeout so a black-hole
        // server can't hang the modal.
        const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
        let events = vm.subscribe();
        let start = Instant::now();
        let mut probe_error: Option<String> = None;
        loop {
            if cancel.load(Ordering::SeqCst) {
                tracing::info!("connect: cancelled");
                return;
            }
            let remaining = PROBE_TIMEOUT.saturating_sub(start.elapsed());
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
            shell.open_remote_location(pane_id, location, vm);
        } else {
            tracing::warn!("connect: shell weak-ref lost before mount");
        }

        {
            let mut st = self.state.lock();
            st.connecting = false;
            st.status_text.clear();
            st.status_is_error = false;
            st.visible = false;
            st.cancel_flag = None;
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
            let Some(win) = window.upgrade() else {
                return;
            };
            win.set_connect_modal_visible(snapshot.visible);
            win.set_connect_backend_index(snapshot.backend_index);
            win.set_connect_auth_index(snapshot.auth_index);
            win.set_connect_connection_string(snapshot.connection_string.into());
            win.set_connect_host(snapshot.host.into());
            win.set_connect_port(snapshot.port.into());
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
    pub connection_string: String,
    pub host: String,
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
}

impl State {
    fn snapshot(&self) -> StateSnapshot {
        StateSnapshot {
            visible: self.visible,
            backend_index: self.backend.map(|b| b as i32).unwrap_or(0),
            auth_index: self.auth.map(|a| a as i32).unwrap_or(0),
            connection_string: self.connection_string.clone(),
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
        }
    }
}

/// Rebuild the connection-string from the individual fields. Called
/// after any host/port/user/path edit that came from the UI (i.e. not
/// echoed from the freeform parser).
fn resync_connection_string(st: &mut State) {
    let backend = st.backend.unwrap_or(BackendChoice::Sftp);
    let scheme = backend.as_kind().scheme();
    let mut s = String::new();
    s.push_str(scheme);
    s.push_str("://");
    if !st.username.is_empty() {
        s.push_str(&st.username);
        s.push('@');
    }
    if !st.host.is_empty() {
        s.push_str(&st.host);
    }
    if !st.port.is_empty() {
        s.push(':');
        s.push_str(&st.port);
    }
    if st.path.is_empty() {
        s.push('/');
    } else if !st.path.starts_with('/') {
        s.push('/');
        s.push_str(&st.path);
    } else {
        s.push_str(&st.path);
    }
    st.connection_string = s;
}

fn default_label(user: &Option<String>, host: &Option<String>) -> String {
    match (user.as_deref(), host.as_deref()) {
        (Some(u), Some(h)) if !u.is_empty() && !h.is_empty() => format!("{u}@{h}"),
        (_, Some(h)) if !h.is_empty() => h.to_owned(),
        _ => String::new(),
    }
}

/// Assemble the [`Location`] the worker will hand to
/// [`atlas_remote::backend::open`] from the current state.
fn assemble_location(st: &State) -> Result<Location, ErrorKind> {
    // If the freeform connection string parses cleanly, prefer it — it
    // already carries every field including path.
    if !st.connection_string.trim().is_empty() {
        match Location::parse_freeform(&st.connection_string) {
            Ok(Location::Remote(mut uri, kind)) => {
                // Override with backend picker if the user changed it
                // after typing the URL (e.g. picked WebDAV but typed
                // sftp://…). Trust the picker.
                let picked = st.backend.unwrap_or(BackendChoice::Sftp).as_kind();
                if picked != kind {
                    uri.scheme = picked.scheme().to_string();
                    return Ok(Location::Remote(uri, picked));
                }
                return Ok(Location::Remote(uri, kind));
            }
            Ok(Location::Local(_)) => return Err(ErrorKind::Malformed),
            Err(LocationParseError::EmptyAuthority) => return Err(ErrorKind::Malformed),
            Err(LocationParseError::InvalidPort(_)) => return Err(ErrorKind::Malformed),
            Err(LocationParseError::UnknownScheme(_)) => {
                // Fall through to per-field assembly.
            }
        }
    }

    let backend = st.backend.unwrap_or(BackendChoice::Sftp);
    let kind = backend.as_kind();
    if st.host.trim().is_empty() {
        return Err(ErrorKind::Malformed);
    }
    let port = if st.port.trim().is_empty() {
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
    let uri = atlas_core::RemoteUri {
        scheme: kind.scheme().to_string(),
        host: Some(st.host.trim().to_owned()),
        port,
        username: if st.username.is_empty() {
            None
        } else {
            Some(st.username.clone())
        },
        path,
        credential_ref: None,
    };
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
    fn connection_string_populates_fields() {
        let c = ctrl_with_pane();
        c.set_connection_string("sftp://alice@example.com:2222/var/log".into());
        let snap = c.snapshot();
        assert_eq!(snap.host, "example.com");
        assert_eq!(snap.port, "2222");
        assert_eq!(snap.path, "/var/log");
        assert_eq!(snap.username, "alice");
        assert_eq!(snap.backend_index, BackendChoice::Sftp as i32);
    }

    #[test]
    fn connection_string_freeform_bare_user_host_becomes_sftp() {
        let c = ctrl_with_pane();
        c.set_connection_string("landon@myhost.com".into());
        let snap = c.snapshot();
        assert_eq!(snap.host, "myhost.com");
        assert_eq!(snap.username, "landon");
        assert_eq!(snap.backend_index, BackendChoice::Sftp as i32);
    }

    #[test]
    fn field_edits_resync_connection_string() {
        let c = ctrl_with_pane();
        c.set_host("h.example.com".into());
        c.set_port("22".into());
        c.set_username("bob".into());
        c.set_path("/srv".into());
        let snap = c.snapshot();
        assert_eq!(snap.connection_string, "sftp://bob@h.example.com:22/srv");
    }

    #[test]
    fn assemble_location_prefers_connection_string() {
        let mut st = State {
            save_to_keychain: true,
            backend: Some(BackendChoice::Sftp),
            auth: Some(AuthChoice::Password),
            connection_string: "sftp://alice@host:22/p".into(),
            ..State::default()
        };
        let loc = assemble_location(&st).unwrap();
        let Location::Remote(uri, kind) = loc else {
            panic!()
        };
        assert_eq!(kind, BackendKind::Sftp);
        assert_eq!(uri.username.as_deref(), Some("alice"));
        assert_eq!(uri.host.as_deref(), Some("host"));
        assert_eq!(uri.port, Some(22));

        // With no connection string, per-field assembly kicks in.
        st.connection_string.clear();
        st.host = "h2".into();
        st.port = "".into();
        st.path = "sub".into();
        let loc = assemble_location(&st).unwrap();
        let Location::Remote(uri, _) = loc else {
            panic!()
        };
        assert_eq!(uri.host.as_deref(), Some("h2"));
        assert_eq!(uri.path, "/sub");
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
    fn default_backend_port_matches_scheme() {
        assert_eq!(BackendChoice::Sftp.default_port(), 22);
        assert_eq!(BackendChoice::Ftp.default_port(), 21);
        assert_eq!(BackendChoice::WebDav.default_port(), 443);
        assert_eq!(BackendChoice::S3.default_port(), 443);
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
