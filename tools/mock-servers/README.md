# atlas mock servers

On-demand mock servers used by the `atlas-remote` integration test suite
to exercise SFTP / FTP / WebDAV / S3 backends without requiring a
network or cloud account.

Each server is a small Python CLI that:

1. Binds to `127.0.0.1:<port>` (or an OS-assigned port if `--port 0` /
   omitted).
2. Prints exactly one sync line to **stdout** once accepting
   connections:
   ```
   READY port=<N>
   ```
3. Serves requests until it receives `SIGTERM` (or Ctrl-C when run
   interactively), then prints `SHUTDOWN` and exits.

The Rust harness in `crates/atlas-remote/tests/common/mock.rs` spawns
each server, parses the `READY port=<N>` line to learn the port, and
sends `SIGTERM` when its `MockXxxServer` value is dropped.

## Managed by `uv`

The recommended entry point is [`uv`](https://docs.astral.sh/uv/):

```sh
cd tools/mock-servers
uv sync                    # one-shot: install pinned deps into .venv/
uv run python sftp_server.py --data-dir ./tmp/sftp --anon
```

`uv sync` reads `pyproject.toml` and writes a `.venv/` next to it. The
Rust harness runs this automatically the first time integration tests
are invoked.

## Fallback: bare `pip`

If `uv` isn't available:

```sh
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
.venv/bin/python sftp_server.py --data-dir ./tmp/sftp --anon
```

## Common CLI shape

All servers accept:

| Flag | Meaning |
| --- | --- |
| `--port <N>` | TCP port to bind. `0` (default) asks the OS. |
| `--data-dir <path>` | Directory to serve (created if missing). Ignored by S3 (moto keeps state in-process). |
| `--user <name>` | Username for password/basic auth. Default: `atlas`. |
| `--password <pw>` | Password for password/basic auth. Default: `atlas`. |
| `--anon` | Allow anonymous access (short-circuits auth). |
| `--verbose` | Bump logging to `DEBUG`. |

Everything except `READY port=<N>` / `SHUTDOWN` goes to **stderr**.

## Per-server notes

### `sftp_server.py`
Backed by [`paramiko`](https://www.paramiko.org). Serves the tree at
`--data-dir` over SFTP with password + publickey acceptance. When
`--anon` is set the server accepts any credential material (paramiko
still requires the client to *offer* something to complete the
handshake).

An ephemeral 2048-bit RSA host key is generated on every start.

### `ftp_server.py`
Backed by [`pyftpdlib`](https://github.com/giampaolo/pyftpdlib). Serves
`--data-dir` under `--user` / `--password`; when `--anon` is set the
standard `anonymous` login is also accepted. Passive ports are pinned
to `60000..60100`.

### `webdav_server.py`
Backed by [`wsgidav`](https://wsgidav.readthedocs.io) + `cheroot`.
Serves `--data-dir` at the root URL. When `--anon` is set the domain
controller is short-circuited so no Basic-auth challenge is issued.

### `s3_server.py`
Backed by [`moto.server`](https://docs.getmoto.org). The server binds
to `--port`, pre-creates `--bucket` (default `atlas-test`) via `boto3`,
and accepts requests signed with the fixed test credentials:

```
AWS_ACCESS_KEY_ID     = atlas-mock
AWS_SECRET_ACCESS_KEY = atlas-mock-secret
AWS_DEFAULT_REGION    = us-east-1
```

The Rust harness passes these verbatim as `Credentials::Iam { .. }`.

## Debugging tips

* Add `--verbose` to see the underlying library's DEBUG logs on stderr.
* Point a real client at the endpoint: `sftp -P <port> atlas@127.0.0.1`,
  `curl http://127.0.0.1:<port>/atlas-test`, etc.
* If `READY port=<N>` never prints, the server crashed during
  bind — check stderr.
* To skip all mock-server-based integration tests set
  `MOCK_SERVERS_SKIP=1` before `cargo test`.
