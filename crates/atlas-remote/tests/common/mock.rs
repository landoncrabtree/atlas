//! Test harness for spawning the mock Python servers under
//! `tools/mock-servers/`.
//!
//! Each `MockXxxServer` struct here spawns the matching Python CLI,
//! waits for its `READY port=<N>` sync line on stdout, and terminates
//! it with SIGTERM on `Drop`. The Rust integration tests in this
//! crate use them like:
//!
//! ```ignore
//! let server = MockSftpServer::start_anon()?;
//! let uri = server.uri();
//! let vm = open(&Location::Remote(uri, BackendKind::Sftp),
//!               Credentials::SshKey(server.client_key(), None),
//!               OpenOptions::default())?;
//! ```
//!
//! Server discovery + Python-interpreter resolution are cached in
//! process-global `OnceCell`s so the cost is paid once per `cargo test`
//! invocation, not per test.
//!
//! Set `MOCK_SERVERS_SKIP=1` in the environment to short-circuit every
//! `start_*` constructor with a friendly log message.

#![allow(dead_code, unreachable_pub)] // consumers pick and choose helpers per-test-binary

use std::env;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use atlas_core::{BackendKind, RemoteUri};
use rand::Rng;
use tempfile::TempDir;

/// Env var used to skip every mock-server-based integration test.
pub const SKIP_ENV: &str = "MOCK_SERVERS_SKIP";

/// Timeout for `READY port=<N>` to appear on the server's stdout.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Global mutex serialising `uv sync` / `pip install` so parallel test
/// binaries never race on `.venv/`.
static VENV_LOCK: Mutex<()> = Mutex::new(());

/// Cached Python interpreter resolution.
enum PythonInvoker {
    /// `uv run --project <dir> python <script> ...`
    Uv {
        uv_bin: PathBuf,
        project_dir: PathBuf,
    },
    /// `<venv>/bin/python <script> ...` — pip fallback path.
    Venv { python_bin: PathBuf },
}

impl PythonInvoker {
    fn build_command(&self, script: &Path) -> Command {
        match self {
            PythonInvoker::Uv {
                uv_bin,
                project_dir,
            } => {
                let mut cmd = Command::new(uv_bin);
                cmd.args([
                    "run",
                    "--project",
                    project_dir.to_str().expect("utf-8 project path"),
                    "python",
                    script.to_str().expect("utf-8 script path"),
                ]);
                cmd
            }
            PythonInvoker::Venv { python_bin } => {
                let mut cmd = Command::new(python_bin);
                cmd.arg(script);
                cmd
            }
        }
    }
}

fn mock_servers_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR points at crates/atlas-remote/. Walk up two
    // levels to reach the workspace root, then into tools/mock-servers.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir)
        .join("..")
        .join("..")
        .join("tools")
        .join("mock-servers")
        .canonicalize()
        .expect("mock-servers dir resolves")
}

fn resolve_python() -> Result<&'static PythonInvoker> {
    static INVOKER: OnceLock<PythonInvoker> = OnceLock::new();
    if let Some(inv) = INVOKER.get() {
        return Ok(inv);
    }
    // Serialize construction so we only sync once.
    let _guard = VENV_LOCK.lock().expect("venv lock poisoned");
    if let Some(inv) = INVOKER.get() {
        return Ok(inv);
    }

    let project_dir = mock_servers_dir();

    // Prefer uv when available: it's much faster and gives us hermetic
    // dep management.
    let uv_available = Command::new("uv")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if uv_available {
        // uv sync is idempotent; skip when .venv/ already exists to keep
        // the first-test cost near zero once the venv is warm.
        let venv_dir = project_dir.join(".venv");
        if !venv_dir.exists() {
            let status = Command::new("uv")
                .arg("sync")
                .current_dir(&project_dir)
                .status()
                .context("running `uv sync`")?;
            if !status.success() {
                bail!("uv sync failed with exit status {status}");
            }
        }
        let inv = PythonInvoker::Uv {
            uv_bin: PathBuf::from("uv"),
            project_dir,
        };
        INVOKER.set(inv).ok();
        return Ok(INVOKER.get().expect("just set"));
    }

    // Fallback: python3 + venv + pip.
    let python3 = which_binary("python3").ok_or_else(|| {
        anyhow!(
            "no `python3` on PATH (and no `uv` either); install one of them or run \
             `MOCK_SERVERS_SKIP=1 cargo test` to skip remote integration tests"
        )
    })?;

    let venv_dir = project_dir.join(".venv");
    if !venv_dir.exists() {
        let status = Command::new(&python3)
            .args(["-m", "venv", ".venv"])
            .current_dir(&project_dir)
            .status()
            .context("creating .venv")?;
        if !status.success() {
            bail!("python3 -m venv failed: {status}");
        }
        let pip = venv_dir.join("bin").join("pip");
        let status = Command::new(&pip)
            .args(["install", "-r", "requirements.txt"])
            .current_dir(&project_dir)
            .status()
            .context("pip install -r requirements.txt")?;
        if !status.success() {
            bail!("pip install failed: {status}");
        }
    }
    let inv = PythonInvoker::Venv {
        python_bin: venv_dir.join("bin").join("python"),
    };
    INVOKER.set(inv).ok();
    Ok(INVOKER.get().expect("just set"))
}

fn which_binary(name: &str) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;
    for entry in env::split_paths(&path_var) {
        let candidate = entry.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// True if the caller set `MOCK_SERVERS_SKIP=1` (or any non-empty
/// value except `0`) in the environment.
pub fn should_skip() -> bool {
    match env::var(SKIP_ENV) {
        Ok(v) => !v.is_empty() && v != "0",
        Err(_) => false,
    }
}

/// Spawn one of the `tools/mock-servers/*.py` scripts, parse its
/// `READY port=<N>` line, and return the running child + resolved port.
///
/// The caller is expected to send SIGTERM (via [`send_sigterm`]) when
/// dropping the returned value.
fn spawn_server(script_name: &str, extra_args: &[&str]) -> Result<(Child, u16)> {
    let inv = resolve_python()?;
    let script = mock_servers_dir().join(script_name);
    let mut cmd = inv.build_command(&script);
    cmd.args(extra_args);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    tracing::debug!(script = %script.display(), args = ?extra_args, "spawning mock server");
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning {}", script.display()))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("no stdout on {script_name}"))?;
    let mut reader = BufReader::new(stdout);

    let deadline = Instant::now() + READY_TIMEOUT;
    let mut line = String::new();
    let port = loop {
        line.clear();
        if Instant::now() >= deadline {
            let _ = send_sigterm(child.id());
            let _ = child.wait();
            bail!(
                "{script_name} did not print READY within {:?}",
                READY_TIMEOUT
            );
        }
        match reader.read_line(&mut line) {
            Ok(0) => {
                let _ = child.wait();
                bail!("{script_name} exited before printing READY");
            }
            Ok(_) => {
                if let Some(rest) = line.trim().strip_prefix("READY port=") {
                    let port: u16 = rest
                        .parse()
                        .with_context(|| format!("parsing READY port from {line:?}"))?;
                    break port;
                }
                // Any other stdout line is unexpected; log and keep
                // reading.
                tracing::debug!(script = script_name, line = %line.trim(), "unexpected pre-READY stdout");
            }
            Err(e) => {
                let _ = send_sigterm(child.id());
                let _ = child.wait();
                return Err(anyhow!("reading {script_name} stdout: {e}"));
            }
        }
    };

    // We could keep draining stdout on a thread to prevent PIPE stall,
    // but the servers only print `SHUTDOWN` on exit; the buffer is more
    // than large enough for that.
    Ok((child, port))
}

/// Send SIGTERM to `pid`. No-op on non-Unix platforms (the mock servers
/// don't ship on Windows CI yet).
pub fn send_sigterm(pid: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    unsafe {
        // SAFETY: libc::kill(pid, sig) is safe when pid is a valid PID;
        // we own the child, so its pid is at worst an already-reaped
        // process, in which case kill returns ESRCH which we ignore.
        if libc::kill(pid as i32, libc::SIGTERM) == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(err)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "SIGTERM only supported on Unix",
        ))
    }
}

/// Shared graceful-shutdown routine — SIGTERM, wait up to 3s, then
/// SIGKILL as a last resort.
fn shutdown_child(child: &mut Child) {
    let pid = child.id();
    let _ = send_sigterm(pid);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

/// Generate a random namespaced credential id for keychain-side tests
/// that need a unique account name.
pub fn random_account() -> String {
    let mut rng = rand::thread_rng();
    let n: u64 = rng.gen();
    format!("atlas-test-{}-{n:x}", std::process::id())
}

// ---------------------------------------------------------------------
// Per-backend structs
// ---------------------------------------------------------------------

/// SFTP mock server backed by paramiko.
pub struct MockSftpServer {
    port: u16,
    _data: TempDir,
    /// Directory that contains `id_rsa` + `id_rsa.pub` for the client
    /// half of publickey auth. `None` for anon mode.
    key_dir: Option<TempDir>,
    child: Child,
}

impl MockSftpServer {
    /// Install the process-wide default [`SftpOptions`] used by
    /// [`SftpBackend::new`] so integration tests that don't go through
    /// [`open_live_sftp_with_options`] still auto-trust the throwaway
    /// paramiko mock's ephemeral host key. Idempotent; safe to call
    /// from every `start_*` constructor.
    fn install_sftp_test_env() {
        use atlas_remote::vm::sftp::{set_default_sftp_options, SftpOptions};
        use atlas_remote::KnownHostsMode;
        set_default_sftp_options(SftpOptions {
            known_hosts_mode: KnownHostsMode::AutoTrust,
            resolver: None,
        });
    }

    /// Start the SFTP mock in anonymous mode (accepts any credential
    /// material).
    pub fn start_anon() -> Result<Self> {
        Self::install_sftp_test_env();
        let data = TempDir::new().context("creating sftp data dir")?;
        let key_dir = TempDir::new().context("creating sftp key dir")?;
        // Generate a keypair so tests can hand OpenDAL a valid key
        // path even in anon mode.
        generate_ssh_keypair(key_dir.path())?;
        let (child, port) = spawn_server(
            "sftp_server.py",
            &["--data-dir", data.path().to_str().expect("utf-8"), "--anon"],
        )?;
        Ok(Self {
            port,
            _data: data,
            key_dir: Some(key_dir),
            child,
        })
    }

    /// Start the SFTP mock in pinned-key mode. The client half of the
    /// generated keypair is accessible via [`Self::client_key`].
    pub fn start_with_pinned_key(user: &str) -> Result<Self> {
        Self::install_sftp_test_env();
        let data = TempDir::new().context("creating sftp data dir")?;
        let key_dir = TempDir::new().context("creating sftp key dir")?;
        generate_ssh_keypair(key_dir.path())?;
        let pubkey_path = key_dir.path().join("id_rsa.pub");
        let (child, port) = spawn_server(
            "sftp_server.py",
            &[
                "--data-dir",
                data.path().to_str().expect("utf-8"),
                "--user",
                user,
                "--authorized-key",
                pubkey_path.to_str().expect("utf-8"),
            ],
        )?;
        Ok(Self {
            port,
            _data: data,
            key_dir: Some(key_dir),
            child,
        })
    }

    /// TCP port the server bound to.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Root directory the server is serving.
    pub fn root_dir(&self) -> &Path {
        self._data.path()
    }

    /// Client-side private key path (for `Credentials::SshKey`). Panics
    /// in constructors that don't materialise one.
    pub fn client_key(&self) -> PathBuf {
        self.key_dir
            .as_ref()
            .expect("client key generated for this server")
            .path()
            .join("id_rsa")
    }

    /// URI pointing at this server's root.
    pub fn uri(&self, user: &str) -> RemoteUri {
        RemoteUri {
            scheme: "sftp".into(),
            host: Some("127.0.0.1".into()),
            port: Some(self.port),
            username: Some(user.into()),
            path: "/".into(),
            credential_ref: None,
        }
    }

    /// Backend kind (always `Sftp`). Kept for symmetry with the other
    /// mocks.
    pub fn backend_kind(&self) -> BackendKind {
        BackendKind::Sftp
    }
}

impl Drop for MockSftpServer {
    fn drop(&mut self) {
        shutdown_child(&mut self.child);
    }
}

/// FTP mock server backed by pyftpdlib.
pub struct MockFtpServer {
    port: u16,
    _data: TempDir,
    user: String,
    password: String,
    anon: bool,
    child: Child,
}

impl MockFtpServer {
    pub fn start_anon() -> Result<Self> {
        let data = TempDir::new().context("creating ftp data dir")?;
        let (child, port) = spawn_server(
            "ftp_server.py",
            &["--data-dir", data.path().to_str().expect("utf-8"), "--anon"],
        )?;
        Ok(Self {
            port,
            _data: data,
            user: "anonymous".into(),
            password: String::new(),
            anon: true,
            child,
        })
    }

    pub fn start_auth(user: &str, password: &str) -> Result<Self> {
        let data = TempDir::new().context("creating ftp data dir")?;
        let (child, port) = spawn_server(
            "ftp_server.py",
            &[
                "--data-dir",
                data.path().to_str().expect("utf-8"),
                "--user",
                user,
                "--password",
                password,
            ],
        )?;
        Ok(Self {
            port,
            _data: data,
            user: user.to_string(),
            password: password.to_string(),
            anon: false,
            child,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn root_dir(&self) -> &Path {
        self._data.path()
    }
    pub fn user(&self) -> &str {
        &self.user
    }
    pub fn password(&self) -> &str {
        &self.password
    }
    pub fn is_anon(&self) -> bool {
        self.anon
    }

    pub fn uri(&self) -> RemoteUri {
        RemoteUri {
            scheme: "ftp".into(),
            host: Some("127.0.0.1".into()),
            port: Some(self.port),
            username: Some(self.user.clone()),
            path: "/".into(),
            credential_ref: None,
        }
    }
}

impl Drop for MockFtpServer {
    fn drop(&mut self) {
        shutdown_child(&mut self.child);
    }
}

/// WebDAV mock server backed by wsgidav.
pub struct MockWebDavServer {
    port: u16,
    _data: TempDir,
    user: String,
    password: String,
    anon: bool,
    child: Child,
}

impl MockWebDavServer {
    pub fn start_anon() -> Result<Self> {
        let data = TempDir::new().context("creating webdav data dir")?;
        let (child, port) = spawn_server(
            "webdav_server.py",
            &["--data-dir", data.path().to_str().expect("utf-8"), "--anon"],
        )?;
        Ok(Self {
            port,
            _data: data,
            user: String::new(),
            password: String::new(),
            anon: true,
            child,
        })
    }

    pub fn start_auth(user: &str, password: &str) -> Result<Self> {
        let data = TempDir::new().context("creating webdav data dir")?;
        let (child, port) = spawn_server(
            "webdav_server.py",
            &[
                "--data-dir",
                data.path().to_str().expect("utf-8"),
                "--user",
                user,
                "--password",
                password,
            ],
        )?;
        Ok(Self {
            port,
            _data: data,
            user: user.into(),
            password: password.into(),
            anon: false,
            child,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn root_dir(&self) -> &Path {
        self._data.path()
    }
    pub fn user(&self) -> &str {
        &self.user
    }
    pub fn password(&self) -> &str {
        &self.password
    }
    pub fn is_anon(&self) -> bool {
        self.anon
    }

    pub fn uri(&self) -> RemoteUri {
        RemoteUri {
            scheme: "webdav".into(),
            host: Some("127.0.0.1".into()),
            port: Some(self.port),
            username: if self.anon {
                None
            } else {
                Some(self.user.clone())
            },
            path: "/".into(),
            credential_ref: None,
        }
    }
}

impl Drop for MockWebDavServer {
    fn drop(&mut self) {
        shutdown_child(&mut self.child);
    }
}

/// S3 mock server backed by moto.
pub struct MockS3Server {
    port: u16,
    _data: TempDir,
    bucket: String,
    child: Child,
}

impl MockS3Server {
    /// Fixed IAM access key the S3 mock accepts (matches
    /// `tools/mock-servers/s3_server.py`).
    pub const ACCESS_KEY: &'static str = "atlas-mock";
    /// Fixed IAM secret key.
    pub const SECRET_KEY: &'static str = "atlas-mock-secret";
    /// Region the mock advertises.
    pub const REGION: &'static str = "us-east-1";

    pub fn start(bucket: &str) -> Result<Self> {
        let data = TempDir::new().context("creating s3 data dir")?;
        let (child, port) = spawn_server(
            "s3_server.py",
            &[
                "--data-dir",
                data.path().to_str().expect("utf-8"),
                "--bucket",
                bucket,
            ],
        )?;
        Ok(Self {
            port,
            _data: data,
            bucket: bucket.into(),
            child,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn bucket(&self) -> &str {
        &self.bucket
    }
    pub fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// URI whose `host` is the S3 bucket, matching how
    /// `build_s3` in the S3 backend reads the URI.
    pub fn uri(&self) -> RemoteUri {
        RemoteUri {
            scheme: "s3".into(),
            host: Some(self.bucket.clone()),
            port: None,
            username: None,
            path: "/".into(),
            credential_ref: None,
        }
    }

    /// Point `build_s3` at this mock via the `ATLAS_S3_ENDPOINT` /
    /// `ATLAS_S3_REGION` process-global env vars. Idempotent: last writer
    /// wins, and since every mock uses the same fixed region, cross-test
    /// contention is limited to the endpoint URL (which is per-instance
    /// but tests are single-threaded within a binary via cargo's default).
    pub fn install_s3_test_env(&self) {
        // SAFETY: std::env::set_var is safe to call, but is not
        // thread-safe with concurrent reads on some platforms. Cargo runs
        // test binaries in parallel by default, but each mock lives in a
        // single binary and OpenDAL only reads these env vars during
        // `build_s3`.
        std::env::set_var("ATLAS_S3_ENDPOINT", self.endpoint());
        std::env::set_var("ATLAS_S3_REGION", Self::REGION);
    }

    /// Acquire an exclusive lock on the process-global S3 env vars,
    /// install them for this mock, and return the guard. Callers must
    /// build the S3 client (via
    /// [`atlas_remote::RemoteLocationViewModel::open_live`] etc.) while
    /// holding the guard — once the operator exists it snapshots the
    /// endpoint and the guard can be dropped safely.
    ///
    /// This is an async lock (`tokio::sync::Mutex`) so tests can hold
    /// the guard across `.await` points without tripping clippy's
    /// `await_holding_lock` lint.
    pub async fn install_s3_test_env_locked(&self) -> tokio::sync::MutexGuard<'static, ()> {
        let guard = S3_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        self.install_s3_test_env();
        guard
    }
}

static S3_ENV_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

impl Drop for MockS3Server {
    fn drop(&mut self) {
        shutdown_child(&mut self.child);
    }
}

/// Generate an OpenSSH `id_rsa` + `id_rsa.pub` keypair inside `dir` via
/// the system `ssh-keygen`. Returns the private-key path.
pub fn generate_ssh_keypair(dir: &Path) -> Result<PathBuf> {
    let key_path = dir.join("id_rsa");
    let status = Command::new("ssh-keygen")
        .args([
            "-t",
            "rsa",
            "-b",
            "2048",
            "-N",
            "",
            "-q",
            "-f",
            key_path.to_str().expect("utf-8 keypath"),
        ])
        .status()
        .context("running ssh-keygen — is it on PATH?")?;
    if !status.success() {
        bail!("ssh-keygen failed with exit status {status}");
    }
    Ok(key_path)
}

/// Convenience: wrap `fut` in a 30-second timeout, returning an error
/// (instead of hanging) when a mock server misbehaves.
///
/// Uses a spawned thread + `Receiver::recv_timeout` so the caller
/// doesn't need a full tokio runtime just to time-box a synchronous
/// operation.
pub fn timeout<F, T>(secs: u64, label: &str, f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(res) => {
            let _ = handle.join();
            res
        }
        Err(_) => bail!("timed out after {secs}s waiting for: {label}"),
    }
}
