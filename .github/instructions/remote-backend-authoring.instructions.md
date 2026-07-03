---
applyTo: "crates/atlas-remote/**,crates/atlas-core/src/location.rs,tools/mock-servers/**"
description: "End-to-end workflow for adding a new remote-filesystem backend in Atlas (SFTP, FTP, WebDAV, S3, and future protocols)."
---

# Remote-backend authoring

Atlas's remote support is built on a per-scheme submodule pattern under `crates/atlas-remote/src/vm/`. Every backend owns its full stack — connection setup, listing, streaming reads/writes, retry, error mapping — and shares only the pool, retry envelope, and runtime with the others.

**OpenDAL was removed in Phase 2.3.5.** Do not re-introduce a unified remote-fs abstraction or a "just wrap that crate" shortcut. If a backend needs behaviour that would be awkward to express as a `LocationViewModel`, extend the trait or add a capability query — do not compress two backends into one adapter.

## Registration points

Adding a new backend touches:

1. **`atlas_core::BackendKind`** — add the enum variant (`crates/atlas-core/src/location.rs`).
2. **`atlas-remote::vm::<scheme>`** — a new submodule implementing the backend.
3. **`atlas-remote::backend::open`** — dispatch the new `BackendKind`.
4. **Mock server** under `tools/mock-servers/` — Python program that speaks the protocol on a local port.
5. **Integration tests** under `crates/atlas-remote/tests/<scheme>.rs` spawning the mock.
6. **Cross-backend transfer tests** under `crates/atlas-remote/tests/cross_backend_stream.rs`.
7. **Connect modal + saved-server schema** in `crates/atlas-ui/src/remote/`.
8. **User-facing documentation** in `docs/multi-pane.md` (remote panes section).

## 1. Extend `BackendKind`

In `crates/atlas-core/src/location.rs`:

```rust
pub enum BackendKind {
    Local,
    Sftp,
    Ftp,
    WebDav,
    S3,
    YourScheme,
}
```

Update:

- `BackendKind::as_str()` — return the URL scheme.
- `BackendKind::from_scheme(&str)` — parse the URL scheme (accept common aliases).
- Any glyph mapping in the UI layer (`crates/atlas-ui/src/remote/…`) — add the emoji/glyph used in the connection chip in the per-pane status bar. See the design doc's connection-chip table.

## 2. Add the backend submodule

Create `crates/atlas-remote/src/vm/<scheme>.rs` with:

- A `Client` struct wrapping the underlying protocol crate.
- An `open(uri, credentials, opts) -> Result<Arc<dyn LocationViewModel>, BackendError>` entry point invoked from `backend.rs`.
- `impl LocationViewModel` — the same trait consumed by every view (`atlas-fs::LocationViewModel`), so views need zero changes.
- Streaming reader/writer methods used by `atlas_remote::stream::stream_copy` for cross-backend transfers.
- Error mapping from the underlying crate's error type into `atlas_remote::error::BackendError` (transient vs permanent — the retry envelope reads this bit).

Register the module in `crates/atlas-remote/src/vm/mod.rs`.

**Runtime.** All async work runs on the shared handle from `atlas_remote::runtime::handle()`. Do **not** create a new tokio runtime; do **not** call `tokio::main`. Library crates outside `atlas-remote` / `atlas-ops` must not depend on tokio.

**Pool.** Route every connection acquisition through `atlas_remote::pool::global()`:

```rust
let pool = crate::pool::global();
let key = crate::pool::PoolKey::new(BackendKind::YourScheme, host, port, user);
let conn = pool.acquire(key, |k| create_new_connection(k)).await?;
```

Idle connections are evicted by TTL. Never open a raw client outside the pool — you'll race the retry envelope and confuse observability.

**Retry.** Wrap every network call in the shared `atlas_remote::retry::Retry` combinator. Classify errors: `Transient` retries with exponential backoff; `Permanent` fails fast. Auth failures are always permanent.

**Host-key / TOFU.** If the protocol has a host-identity concept (SFTP, SSH), integrate with `atlas_remote::host_key` + `atlas_remote::known_hosts`. On first connection to an unknown host, emit a `HostKeyPrompt` event to the UI; on user Accept, persist the fingerprint into `~/.config/atlas/known_hosts` (OpenSSH-compatible format). On subsequent connections, verify the fingerprint before authenticating.

**Secrets.** Never persist passwords, passphrases, tokens, or private keys into `~/.config/atlas/servers.toml`. Store them in the OS keychain via `atlas_remote::secrets` under the `com.atlas.credentials` namespace, referenced by an opaque `credential_ref` handle that goes into `servers.toml`.

## 3. Wire `backend::open`

In `crates/atlas-remote/src/backend.rs`, `open_remote(uri, kind, credentials, opts)` dispatches on `BackendKind`. Add the new match arm:

```rust
BackendKind::YourScheme => vm::your_scheme::open(uri, credentials, opts),
```

## 4. Add a mock server

Under `tools/mock-servers/`, add `your_scheme_server.py` following the pattern of the existing mocks. Contract:

- Accepts CLI flags: `--port <N>`, `--data-dir <path>`, credentials (`--user`, `--password`, or an equivalent).
- Serves the given data-dir over the target protocol.
- On startup, prints `READY port=<N>` to stdout (used by the Rust test harness to know when to begin the test).
- On termination, cleans up sockets/files and prints `SHUTDOWN`.
- Shares common utilities from `mock_common.py`.

Deps go into `tools/mock-servers/pyproject.toml` (uv-managed) and `requirements.txt` (pip fallback). Existing mocks use pure-Python protocol libraries where possible to avoid native-lib compile steps.

Test the mock standalone:

```bash
cd tools/mock-servers
uv run your_scheme_server.py --port 0 --data-dir /tmp/atlas-mock --user atlas --password atlas
```

Port `0` binds to an OS-chosen free port; watch stdout for the `READY port=N` line.

## 5. Add integration tests

In `crates/atlas-remote/tests/<scheme>.rs`, use `crates/atlas-remote/tests/common/mock.rs::MockServer` (or add a variant for your scheme). Each test:

- Skips itself when `MOCK_SERVERS_SKIP=1` is set (see `mock::skip_if_requested()` — the shared helper).
- Spawns the mock server on a random port.
- Opens a `Location::Remote(uri, BackendKind::YourScheme)` via `atlas_remote::backend::open`.
- Exercises list / read / write / stat / delete / rename / mkdir behaviour.
- Asserts against expected outcomes.

Cover:

- Happy-path listing.
- Nested directories.
- Large-file streaming reads and writes.
- Permission-denied handling (returns `BackendError::PermissionDenied`, not a retryable error).
- Network drop mid-stream (should surface the transient error and let the retry envelope re-establish).
- Cancellation via `CancellationToken`.

## 6. Cross-backend transfer tests

Add cases to `crates/atlas-remote/tests/cross_backend_stream.rs`:

- Local → YourScheme.
- YourScheme → Local.
- YourScheme ↔ every other backend (SFTP, FTP, WebDAV, S3).

The transfer routes through `atlas_remote::stream::stream_copy`, which chunks reads from the source and writes to the destination; it must respect the shared `CancellationToken` and emit progress events on the ops queue.

## 7. UI plumbing — Connect modal and saved servers

Extend `crates/atlas-ui/src/remote/`:

- Add the backend to the Connect-modal segmented control (`assets/ui/components/connect-server.slint`).
- Add per-backend fields (host, port, user, plus scheme-specific extras like an S3 endpoint URL or a WebDAV base path).
- Extend the `servers.toml` schema in `atlas-config` for the new backend. **Only opaque handles** — never a password field.
- Extend the palette's saved-servers listing (`Cmd+P`) so entries for the new backend are reachable.

Every text input in the modal must bubble `input-focused` up to the root `keymap-bypass-active` disjunction — see [`ui-composition.instructions.md`](ui-composition.instructions.md) §5.

## 8. Documentation

Update:

- `docs/multi-pane.md` — remote-panes section: add the backend, list supported auth modes, note any capability quirks (e.g. no ranged reads, immutable object semantics for S3).
- `docs/keymap.md` — if the backend introduces new actions (rare; usually `remote::Connect` covers everything).
- `.github/instructions/architecture.instructions.md` — per-backend crate table + storage / capability notes if they diverge from the norm.

## Verification

- `cargo test -p atlas-remote your_scheme::…` — mock-backed integration tests green.
- `cargo test -p atlas-remote cross_backend_stream` — every cross-backend permutation green.
- `MOCK_SERVERS_SKIP=1 cargo nextest run --workspace` — the rest of the workspace still passes without a Python environment.
- Live smoke: launch the app, `Cmd+K` → pick the new backend → connect to a real (non-mock) server → list, read, copy, delete, rename.
- Screenshot the Connect modal + the pane with a remote listing via `computer-use-*` MCP; attach to the PR.

## Anti-patterns

| Don't | Why |
|---|---|
| Introduce a "unified" remote-fs adapter | We removed OpenDAL in Phase 2.3.5 for a reason; every backend has divergent semantics that unified layers erase. |
| Open a raw client outside the pool | Bypasses eviction, retry, and observability. |
| Store secrets in `servers.toml` | Secrets belong in the OS keychain via `atlas_remote::secrets`. `servers.toml` gets an opaque handle only. |
| Spawn a new tokio runtime | Locks the process into duplicate runtimes; use `atlas_remote::runtime::handle()`. |
| Skip host-key TOFU for SSH-like protocols | Users get MITM'd. Every new SSH-derivative backend integrates with `known_hosts`. |
| Silently follow HTTP redirects | Backends that speak HTTP must expose an option to disable redirects (WebDAV in particular; symlinks and moves are semantically distinct from redirects). |

## Cross-references

- [`.github/instructions/architecture.instructions.md`](architecture.instructions.md) — crate boundaries, storage layout, keychain namespace.
- [`ui-composition.instructions.md`](ui-composition.instructions.md) — Connect-modal focus routing.
- `crates/atlas-remote/src/backend.rs` — the dispatch layer.
- `crates/atlas-remote/src/pool.rs`, `retry.rs`, `stream.rs`, `host_key.rs`, `known_hosts.rs`, `secrets.rs` — shared plumbing every backend uses.
