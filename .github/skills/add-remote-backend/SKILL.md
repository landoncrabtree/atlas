---
name: add-remote-backend
description: Guide for adding a new remote-filesystem backend to Atlas (a new scheme like SFTP/FTP/WebDAV/S3, or an extension to an existing one). Use when the request is to support a new remote protocol or extend the atlas-remote crate.
---

Atlas's remote support lives in `crates/atlas-remote/` with one submodule per scheme under `src/vm/`. Every backend owns its full stack — connection, listing, streaming, retry, error mapping — and shares only the pool, retry envelope, TOFU flow, and tokio runtime.

**OpenDAL was removed in Phase 2.3.5.** Do not re-introduce a unified remote-fs adapter. If two backends have divergent semantics, they get divergent modules.

## The registration points

Adding a new backend touches:

1. **`atlas_core::BackendKind`** — add the enum variant in `crates/atlas-core/src/location.rs`. Update `as_str()` and `from_scheme(&str)`.
2. **`atlas-remote::vm::<scheme>`** — new submodule with a `Client`, `impl LocationViewModel`, streaming reader/writer, error mapping. Register in `src/vm/mod.rs`.
3. **`atlas-remote::backend::open`** — dispatch the new variant in `src/backend.rs`'s `open_remote(uri, kind, credentials, opts)` match.
4. **Mock server** — `tools/mock-servers/<scheme>_server.py`. Contract: accepts `--port <N>` (0 = OS-picks), `--data-dir <path>`, credentials flags; prints `READY port=<N>` on startup and `SHUTDOWN` on exit. Deps go in `pyproject.toml` (uv) + `requirements.txt` (pip fallback).
5. **Integration tests** — `crates/atlas-remote/tests/<scheme>.rs` using `crates/atlas-remote/tests/common/mock.rs`. Cover happy-path listing, nested dirs, large-file streaming, permission-denied, network drop mid-stream, cancellation. Respect `MOCK_SERVERS_SKIP=1`.
6. **Cross-backend transfer tests** — add cases to `crates/atlas-remote/tests/cross_backend_stream.rs` for Local ↔ YourScheme and YourScheme ↔ every other backend.
7. **UI plumbing** — extend the Connect modal (`assets/ui/components/connect-server.slint`), the `crates/atlas-ui/src/remote/` controllers, the `servers.toml` schema (opaque `credential_ref` only — no secrets), and the palette's saved-servers listing.
8. **Documentation** — update `docs/multi-pane.md` (supported backends, auth modes, capability quirks) and `.github/instructions/architecture.instructions.md` (per-backend crate table).

## The invariants you must respect

- **Runtime**: all async work runs on `atlas_remote::runtime::handle()`. Never `tokio::main` and never spawn a fresh runtime.
- **Pool**: every connection is acquired via `atlas_remote::pool::global()` with a `PoolKey::new(kind, host, port, user)`. Idle connections evict by TTL. Never open a raw client outside the pool.
- **Retry**: wrap network calls in `atlas_remote::retry::Retry`. Classify errors: `Transient` retries with exponential backoff, `Permanent` fails fast, auth failures are always permanent.
- **Host-key / TOFU** (SSH-derivative protocols): integrate with `atlas_remote::host_key` + `atlas_remote::known_hosts` (OpenSSH format). On first connection to an unknown host, emit `HostKeyPrompt` for the UI; on user Accept, persist to `~/.config/atlas/known_hosts`.
- **Secrets**: never persist to `servers.toml`. Use `atlas_remote::secrets` to store into the OS keychain under the `com.atlas.credentials` namespace, referenced by an opaque `credential_ref`.

## Verify

```bash
cargo test -p atlas-remote <scheme>::                     # your backend's tests
cargo test -p atlas-remote cross_backend_stream           # cross-backend transfers
MOCK_SERVERS_SKIP=1 cargo nextest run --workspace       # rest of workspace without Python
```

Live smoke: launch app, `Cmd+K` → pick the new backend → connect to a real (non-mock) server → list / read / copy / delete / rename. Screenshot the Connect modal + a populated pane via `computer-use-*` MCP and attach to the PR.

For the full walkthrough including anti-patterns, read [`.github/instructions/remote-backend-authoring.instructions.md`](../../instructions/remote-backend-authoring.instructions.md).
