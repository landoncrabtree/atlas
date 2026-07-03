---
applyTo: "**"
description: "Atlas architecture: process model, crate layout, design principles, threading and storage. Apply when reasoning about cross-crate boundaries, the daemon, or where new code belongs."
---

# Architecture

Atlas is a Cargo workspace of small, focused Rust crates. The app process owns the UI and most application state; a separate `atlas-indexd` daemon owns the search index and filesystem watchers for indexed roots. They communicate over a local Unix domain socket.

## High-level diagram

```
                 ┌────────────────────────────────┐
                 │          atlas-app             │
                 │  (Slint window, event loop)    │
                 └───────────────┬────────────────┘
                                 │
   ┌─────────────────────────────┼───────────────────────────────┐
   │                             │                               │
   ▼                             ▼                               ▼
atlas-ui                     atlas-keymap                     atlas-config
(AppShell, models, theme)    (chord seqs, actions)            (TOML, hot reload)
   │                             │                               │
   └──────────────┬──────────────┴─────────────┬─────────────────┘
                  │                            │
                  ▼                            ▼
              atlas-fs                     atlas-search
              (async streaming             (content + fuzzy + index)
               filesystem, view-models,        │
               sort, filter)                   │
                                                ▼
                                            atlas-index
                                            (tantivy schema + queries)
                                                ▲
                                                │  IPC over UDS (bincode)
                                                │
                          ┌─────────────────────┴──────────────────────┐
                          │             atlas-indexd                   │
                          │  (tokio runtime, tantivy writer,           │
                          │   atlas-watch for incremental updates,     │
                          │   atlas-ipc server)                        │
                          └────────────────────────────────────────────┘
```

`atlas-ops` (file ops queue) and `atlas-thumbs` (thumbnail generator + cache) plug into `atlas-ui` views as needed. `atlas-remote` provides SFTP/FTP/WebDAV/S3 backends behind the same `Location`-driven interface. `atlas-core` provides the error type, path helpers, and the `Location` enum used by every other crate.

## Process model

- **`atlas-app`**: one process per user session. Owns the window, all views, in-memory state. Uses Slint's event loop on the main thread; offloads I/O to worker threads via `crossbeam-channel` and `std::thread`.
- **`atlas-indexd`**: one process per user. Started lazily by the app on first launch (or by `launchd` LaunchAgent on macOS). Holds the persistent tantivy index, runs filesystem watchers for indexed roots, serves queries over a Unix domain socket at:
  - macOS: `~/Library/Application Support/Atlas/indexd.sock`
  - Linux: `$XDG_RUNTIME_DIR/atlas/indexd.sock`
  - Windows: a named pipe (e.g. `\\.\pipe\atlas-indexd`)

When the daemon is unreachable, the app falls back to **embedded mode** — running an in-process index for the current session only.

## Crate inventory

| Crate | Purpose | Heavy dependencies |
|---|---|---|
| `atlas-app` | Slint binary, ties everything together | slint, tracing-subscriber |
| `atlas-ui` | Rust-side models and `AppShell` adapter for Slint | slint, smallvec |
| `atlas-core` | Shared error type, path helpers, `Location` + `RemoteUri` + `BackendKind` enums | thiserror |
| `atlas-fs` | Async streaming directory listing + walker + sort/filter + `LocationViewModel` | ignore, rayon, crossbeam-channel |
| `atlas-watch` | `notify` wrapper with debouncing (local roots only) | notify, notify-debouncer-full |
| `atlas-index` | Tantivy schema + Prefix/Substring/Fuzzy/Extension/InSubtree/KindAnyOf queries | tantivy |
| `atlas-indexd` | Daemon binary: tokio runtime, tantivy writer, watchers, IPC server | tokio, interprocess, bincode |
| `atlas-search` | Content search (ripgrep engine) + fuzzy (nucleo) + index facade | grep, grep-regex, nucleo, ignore |
| `atlas-ops` | File operations queue: copy/move/delete/rename/mkdir with progress + cancel, streaming cross-backend transfers | trash, futures, tokio |
| `atlas-keymap` | Chord sequences, layered keymap, action registry, TOML loader | (none beyond serde + toml) |
| `atlas-config` | Typed TOML config with comment-preserving save and hot reload; also owns `servers.toml` and `known_hosts` | toml, toml_edit, notify, arc-swap |
| `atlas-ipc` | Daemon ↔ app protocol + transport | tokio, interprocess, bincode |
| `atlas-thumbs` | Thumbnail decode + resize + WebP/PNG encode + SQLite-cached LRU (local paths only) | image, resvg, rusqlite |
| `atlas-remote` | Remote filesystems: one submodule per scheme, shared runtime, connection pool, retry, TOFU host-key flow | tokio, russh, russh-sftp, suppaftp, reqwest, quick-xml, object_store |

### Remote backends

Each remote scheme has a dedicated module under `crates/atlas-remote/src/vm/`:

| Backend | Module | Crate(s) | Notes |
|---|---|---|---|
| SFTP | `vm/sftp.rs` | `russh` + `russh-sftp` | Host-key TOFU via `known_hosts`; password + keyfile auth |
| FTP / FTPS | `vm/ftp.rs` | `suppaftp` | Explicit + implicit TLS |
| WebDAV / WebDAVS | `vm/webdav.rs` | `reqwest` + `quick-xml` | PROPFIND-driven listing |
| S3 (and compatibles) | `vm/s3.rs` | `object_store` | AWS S3, MinIO, R2, etc. |

**OpenDAL was removed in Phase 2.3.5.** Do not re-introduce a unified remote-fs abstraction — every backend owns its dedicated crate stack. Adding a new backend follows the workflow in [`remote-backend-authoring.instructions.md`](remote-backend-authoring.instructions.md).

## Design principles

### Async-first, channel-based APIs

Library crates **never** block the caller. They return either futures (when consumer-driven) or `crossbeam_channel::Receiver<Event>` (when producer-driven). The Slint event loop or the tokio runtime in the daemon drives the consumption.

This means:
- `atlas-fs::list_directory(req) -> Receiver<ListEvent>` — entries stream in.
- `atlas-fs::walk(req) -> Receiver<ListEvent>` — same shape, but recursive.
- `atlas-search::content::run(req) -> SearchHandle` — multi-threaded search emitting `SearchEvent`s; cancellable.
- `atlas-thumbs::Generator` — request channel in, result channel out, bounded worker pool.

### Process isolation for the indexer

A separate daemon lets us:
- Keep memory pressure off the UI process when indexing very large roots.
- Survive app restarts and upgrades without re-indexing.
- Share one index across multiple Atlas windows (and, eventually, CLI tools).
- Sandbox the indexer's filesystem traversal independently.

### Decoupled view models

Every view (Details/Grid/Miller/Gallery/Dual-pane) consumes the same `LocationViewModel` trait from `atlas-fs`. Switching view modes never re-reads the directory.

### Locations, not paths

Panes, tabs, and every listing/op consumer take an `atlas_core::Location`, not a `PathBuf`. Two variants:

- `Location::Local(PathBuf)` — an OS-native path.
- `Location::Remote(RemoteUri, BackendKind)` — a scheme-tagged URI plus backend hint.

Local-only fast paths (thumbnails, native trash, `notify` watcher, free-space queries) call `Location::as_local()` and gracefully no-op / return `None` for remote. Backend-agnostic paths route through `atlas-fs` (listing), `atlas-ops::execute_op` (mutations), and `atlas_remote::stream::stream_copy` (cross-backend transfers).

### Remote connection pooling, retry, and TOFU

- **Pool**: `atlas_remote::pool::ConnectionPool` keys on `(scheme, host, port, user)`. Idle connections are evicted after a configurable TTL. **Never** open a raw SSH/HTTP/FTP client outside the pool — respect it, or the pane will race the retry envelope.
- **Retry envelope**: every backend op runs inside a shared exponential-backoff `Retry` combinator. Transient errors (network, timeout) retry; permission/auth failures fail fast.
- **TOFU**: the first SFTP connection to a new host emits a `HostKeyPrompt` event with the fingerprint. The UI shows a Connect-modal prompt; on Accept we persist the fingerprint into `~/.config/atlas/known_hosts` (OpenSSH-compatible format). Subsequent connections verify against that store.

### Configuration: read freely, write rarely

The Slint UI reads from an `ArcSwap<Config>` populated by `atlas-config`. The file watcher reloads on every save; reads are lock-free clones of the `Arc`. Writes only happen when the user changes a setting.

### Keymap as data

`atlas-keymap` resolves chord sequences against a layered keymap (default + user). Actions are *strings* (`"command_palette::Toggle"`), not function pointers — so the user's TOML keymap can rebind anything the app exposes. The UI's `ActionSink` knows what to do with each action ID.

## Storage layout

| Path | Contents |
|---|---|
| `~/.config/atlas/config.toml` | User config |
| `~/.config/atlas/keymaps/default.toml` | Checked-in default keymap seed (regenerated from `atlas-keymap`) |
| `~/.config/atlas/keymaps/user.toml` | Optional user keymap override |
| `~/.config/atlas/themes/` | User themes (TOML) |
| `~/.config/atlas/servers.toml` | Saved-server entries (**no secrets**; only opaque `credential_ref` handles) |
| `~/.config/atlas/known_hosts` | OpenSSH-compatible host-key store for SFTP TOFU |
| `~/Library/Caches/dev.atlas.atlas/thumbs.db` (macOS) | SQLite thumbnail cache |
| `~/Library/Application Support/dev.atlas.atlas/index/<root-hash>/` | Per-root tantivy index |
| `~/Library/Application Support/dev.atlas.atlas/indexd.sock` | Daemon socket |
| `~/Library/Logs/Atlas/` | Daemon + app logs |

On macOS the config dir is `~/.config/atlas/`, **not** `~/Library/Application Support/` — this matches the XDG-style convention shared with Linux. `ATLAS_CONFIG_DIR` overrides in tests. Linux/Windows mirror the same layout via `directories::ProjectDirs` for cache/support/log dirs; config lives at `~/.config/atlas/` on Linux and `%APPDATA%\atlas\` on Windows.

**Secret storage.** `servers.toml` never holds passwords, passphrases, tokens, or private keys. Secrets are stored in the OS keychain (macOS Keychain, `libsecret` on Linux, Windows Credential Manager) under the `com.atlas.credentials` namespace, indexed by the opaque `credential_ref` string persisted in `servers.toml`.

## Threading model

| Thread | Owner | Purpose |
|---|---|---|
| Slint event loop (main) | `atlas-app` | UI rendering, input |
| Background worker pool | `atlas-fs` walker | Directory enumeration |
| Thumbnail workers | `atlas-thumbs::Generator` | Image decode + resize |
| Operation workers | `atlas-ops` | Copy/move/delete (local + cross-backend) |
| Config watcher | `atlas-config::ConfigWatcher` | Reload on file change |
| Index watchers | `atlas-indexd` (in daemon) | Notify-driven incremental updates |
| Tokio multi-threaded (indexer) | `atlas-indexd` | IPC server + index writes |
| Tokio multi-threaded (remote) | `atlas-remote::runtime` (shared by `atlas-ops`) | All SFTP/FTP/WebDAV/S3 I/O + streaming transfers |

`atlas-remote::runtime::handle()` returns a shared tokio `Handle`. Library crates outside `atlas-remote` / `atlas-ops` **must not** spawn their own tokio runtime; if they need to drive a future, use that handle or expose channel-based APIs and let the caller pick an executor.

Cross-thread state crossing the UI boundary goes through `slint::invoke_from_event_loop`. Shared mutable state uses `parking_lot::RwLock` or `arc_swap::ArcSwap`.

## Failure model

- I/O errors are surfaced as `atlas_core::AtlasError::Io { path, source }` carrying the offending path for actionable user messages.
- The daemon connection is treated as best-effort: if it dies, the app reconnects on next query and falls back to embedded mode for the gap.
- Filesystem operations are atomic where possible (rename-on-write for config) and produce undo entries for trash/rename in `atlas-ops`.

## What lives outside this repo (future)

- The Slint live preview tool (`slint-viewer`, installed separately).
- An optional CLI (`atlas`) for headless operations — post-MVP.
- Plugin runtime (WASM or similar) — post-MVP, but the action-ID indirection in `atlas-keymap` is intentional groundwork.

## When applying this architecture

- **Adding functionality** — pick the crate whose purpose covers it; if none fit, propose a new crate rather than expanding an existing one beyond its single concern.
- **Cross-crate types** — go through `atlas-core` or define a trait in the consumer; never reach into another crate's internals.
- **Anything UI** — the Rust ↔ Slint bridge is `AppShell` in `atlas-ui`. Don't bypass it. New surfaces follow [`ui-composition.instructions.md`](ui-composition.instructions.md).
- **Anything daemon-bound** — talk to it via `atlas-ipc`; never poke its files directly from the app.
- **Anything remote** — go through `atlas-remote`; use `Location::as_local()` before assuming local semantics. New backends follow [`remote-backend-authoring.instructions.md`](remote-backend-authoring.instructions.md).
- **Adding a keybind** — follow [`keybind-authoring.instructions.md`](keybind-authoring.instructions.md); never invent an action ID outside `atlas-keymap::defaults::default_actions()`.
- **Threading** — UI thread is sacred (see `performance.instructions.md`); cross-thread state crossing the UI boundary goes through `slint::invoke_from_event_loop`. All remote I/O rides `atlas_remote::runtime::handle()`; never `tokio::main` in a library crate.
