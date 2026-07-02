#!/usr/bin/env python3
"""SFTP mock server for Atlas integration tests.

Backed by paramiko. Serves ``--data-dir`` as the SFTP root; supports:

* Password authentication (``--user`` / ``--password``), which is what the
  OpenDAL SFTP tests will drive when the "wrong credentials" case is
  exercised.
* Anonymous / any-key acceptance when ``--anon`` is passed. Handy for the
  ``connect_anon`` test — paramiko still requires *some* credential
  material to complete auth, so we accept any key or password and
  short-circuit the server-side check.

The heavy lifting is delegated to ``paramiko.SFTPServer`` with an in-tree
``SFTPServerInterface`` implementation. We deliberately keep the server
single-threaded per connection (paramiko spawns a Transport thread per
accept anyway) — Atlas's integration tests don't hammer this.

Ephemeral host keys are generated on startup so no test artefact touches
disk permanently.
"""

from __future__ import annotations

import errno
import logging
import os
import socket
import sys
import threading
from pathlib import Path

import paramiko

from mock_common import (  # type: ignore[import-not-found]
    build_argparser,
    configure_logging,
    ensure_data_dir,
    install_sigterm_handler,
    print_ready,
    print_shutdown,
)

LOG = logging.getLogger("mock-sftp")


class AtlasServerInterface(paramiko.ServerInterface):
    """Authenticates the SFTP client.

    * ``anon=True`` — any credential material passes; the client still has
      to *offer* something because paramiko refuses to complete the SSH
      handshake with an empty auth-request list.
    * ``authorized_key`` — if a ``paramiko.PKey`` is provided, only clients
      that present that exact public key pass ``check_auth_publickey``.
    * Otherwise the ``--user`` / ``--password`` pair is required.
    """

    def __init__(
        self,
        expected_user: str,
        expected_password: str,
        anon: bool,
        authorized_key: paramiko.PKey | None = None,
    ):
        self.expected_user = expected_user
        self.expected_password = expected_password
        self.anon = anon
        self.authorized_key = authorized_key

    def check_channel_request(self, kind: str, chanid: int) -> int:
        if kind == "session":
            return paramiko.OPEN_SUCCEEDED
        return paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED

    def check_auth_password(self, username: str, password: str) -> int:
        if self.anon:
            return paramiko.AUTH_SUCCESSFUL
        if username == self.expected_user and password == self.expected_password:
            return paramiko.AUTH_SUCCESSFUL
        return paramiko.AUTH_FAILED

    def check_auth_publickey(self, username: str, key: paramiko.PKey) -> int:
        if self.anon:
            return paramiko.AUTH_SUCCESSFUL
        if self.authorized_key is not None:
            if key.get_name() == self.authorized_key.get_name() and key.asbytes() == self.authorized_key.asbytes():
                return paramiko.AUTH_SUCCESSFUL
        return paramiko.AUTH_FAILED

    def get_allowed_auths(self, username: str) -> str:
        return "password,publickey"


class RootedSFTPHandle(paramiko.SFTPHandle):
    def stat(self) -> object:
        try:
            fd = self.readfile or self.writefile
            if fd is None:
                return paramiko.SFTP_OP_UNSUPPORTED
            path = getattr(fd, "name", None)
            if path is None:
                return paramiko.SFTP_OP_UNSUPPORTED
            return paramiko.SFTPAttributes.from_stat(os.stat(path))
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno or errno.EIO)

    def chattr(self, attr: paramiko.SFTPAttributes) -> int:
        try:
            fd = self.readfile or self.writefile
            if fd is None:
                return paramiko.SFTP_OP_UNSUPPORTED
            path = getattr(fd, "name", None)
            if path is None:
                return paramiko.SFTP_OP_UNSUPPORTED
            paramiko.SFTPServer.set_file_attr(path, attr)
            return paramiko.SFTP_OK
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno or errno.EIO)


class RootedSFTPServer(paramiko.SFTPServerInterface):
    """Rooted SFTP interface — every path is resolved against ``ROOT``.

    ROOT is set as a class attribute by ``run_server`` before the paramiko
    Transport is started; each per-connection ``RootedSFTPServer`` reads it
    verbatim.
    """

    ROOT: Path = Path("/")

    def _realpath(self, path: str) -> Path:
        # SFTP paths are always absolute-in-root. Strip leading slashes and
        # rejoin against ROOT to prevent traversal. We deliberately do NOT
        # follow the *final* component's symlink so callers like
        # ``lstat`` and ``readlink`` see the link itself; the parent
        # chain is resolved so upstream directory symlinks work.
        rel = path.lstrip("/")
        parent_rel, _, name = rel.rpartition("/")
        parent = (self.ROOT / parent_rel).resolve() if parent_rel else self.ROOT.resolve()
        root_resolved = self.ROOT.resolve()
        try:
            parent.relative_to(root_resolved)
        except ValueError as exc:
            raise PermissionError(f"path traversal denied: {path}") from exc
        return parent / name if name else parent

    def list_folder(self, path: str) -> object:
        try:
            real = self._realpath(path)
            # `real` may itself be a symlink (e.g. `link_to_dir/`) — for
            # listing we want to enumerate the *target's* children, so
            # ``os.listdir`` follows symlinks automatically. For each
            # child we use ``os.lstat`` so link-kind is preserved and
            # our SFTP backend's `is_symlink()` branch fires.
            entries = []
            for name in os.listdir(real):
                full = real / name
                attr = paramiko.SFTPAttributes.from_stat(os.lstat(full))
                attr.filename = name
                entries.append(attr)
            return entries
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno or errno.EIO)

    def stat(self, path: str) -> object:
        try:
            return paramiko.SFTPAttributes.from_stat(os.stat(self._realpath(path)))
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno or errno.EIO)

    def lstat(self, path: str) -> object:
        try:
            return paramiko.SFTPAttributes.from_stat(os.lstat(self._realpath(path)))
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno or errno.EIO)

    def readlink(self, path: str) -> object:
        try:
            return os.readlink(self._realpath(path))
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno or errno.EIO)

    def symlink(self, target_path: str, path: str) -> int:
        try:
            os.symlink(target_path, self._realpath(path))
            return paramiko.SFTP_OK
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno or errno.EIO)

    def open(self, path: str, flags: int, attr: paramiko.SFTPAttributes) -> object:
        try:
            real = self._realpath(path)
            fdflags = 0
            if flags & os.O_RDONLY == os.O_RDONLY:
                fdflags |= os.O_RDONLY
            if flags & os.O_WRONLY:
                fdflags |= os.O_WRONLY
            if flags & os.O_RDWR:
                fdflags |= os.O_RDWR
            if flags & os.O_APPEND:
                fdflags |= os.O_APPEND
            if flags & os.O_CREAT:
                fdflags |= os.O_CREAT
            if flags & os.O_TRUNC:
                fdflags |= os.O_TRUNC
            if flags & os.O_EXCL:
                fdflags |= os.O_EXCL
            mode = getattr(attr, "st_mode", 0o644) or 0o644
            fd = os.open(str(real), fdflags, mode)
            fobj = os.fdopen(
                fd, "rb+" if (fdflags & (os.O_WRONLY | os.O_RDWR)) else "rb"
            )
            handle = RootedSFTPHandle(flags)
            handle.readfile = fobj
            handle.writefile = fobj if (fdflags & (os.O_WRONLY | os.O_RDWR)) else None
            return handle
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno or errno.EIO)

    def remove(self, path: str) -> int:
        try:
            os.remove(self._realpath(path))
            return paramiko.SFTP_OK
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno or errno.EIO)

    def rename(self, oldpath: str, newpath: str) -> int:
        try:
            os.rename(self._realpath(oldpath), self._realpath(newpath))
            return paramiko.SFTP_OK
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno or errno.EIO)

    def mkdir(self, path: str, attr: paramiko.SFTPAttributes) -> int:
        try:
            mode = getattr(attr, "st_mode", 0o755) or 0o755
            os.mkdir(self._realpath(path), mode & 0o777)
            return paramiko.SFTP_OK
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno or errno.EIO)

    def rmdir(self, path: str) -> int:
        try:
            os.rmdir(self._realpath(path))
            return paramiko.SFTP_OK
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno or errno.EIO)

    def chattr(self, path: str, attr: paramiko.SFTPAttributes) -> int:
        try:
            paramiko.SFTPServer.set_file_attr(str(self._realpath(path)), attr)
            return paramiko.SFTP_OK
        except OSError as exc:
            return paramiko.SFTPServer.convert_errno(exc.errno or errno.EIO)


def _handle_connection(
    sock: socket.socket,
    host_key: paramiko.PKey,
    user: str,
    password: str,
    anon: bool,
    authorized_key: paramiko.PKey | None,
) -> None:
    try:
        transport = paramiko.Transport(sock)
        transport.add_server_key(host_key)
        transport.set_subsystem_handler("sftp", paramiko.SFTPServer, RootedSFTPServer)
        server = AtlasServerInterface(user, password, anon, authorized_key)
        transport.start_server(server=server)
        chan = transport.accept(30)
        if chan is None:
            LOG.warning("client did not open a channel within 30s")
            transport.close()
            return
        # Block until the transport dies. paramiko drives the SFTP loop
        # on its own worker thread once accept() returns a channel.
        transport.join()
    except Exception:  # noqa: BLE001 — server keeps running on per-conn errors
        LOG.exception("connection handling failed")
        try:
            sock.close()
        except OSError:
            pass


def _load_authorized_key(path: Path) -> paramiko.PKey:
    """Load a public key from an OpenSSH-format ``.pub`` file.

    Supports RSA, ECDSA, and Ed25519. The Rust integration harness
    generates keys with ``ssh-keygen`` and hands the ``.pub`` path in.
    """
    text = path.read_text().strip()
    parts = text.split()
    if len(parts) < 2:
        raise ValueError(f"malformed authorized_key file: {path}")
    keytype, b64 = parts[0], parts[1]
    import base64

    data = base64.b64decode(b64)
    msg = paramiko.Message(data)
    if keytype == "ssh-rsa":
        return paramiko.RSAKey(msg=msg, data=data)
    if keytype in ("ssh-ed25519", "ecdsa-sha2-nistp256", "ecdsa-sha2-nistp384", "ecdsa-sha2-nistp521"):
        return paramiko.PKey.from_type_string(keytype, data)
    raise ValueError(f"unsupported key type: {keytype}")


def run_server(
    port: int,
    data_dir: Path,
    user: str,
    password: str,
    anon: bool,
    authorized_key_path: Path | None,
) -> None:
    ensure_data_dir(data_dir)
    RootedSFTPServer.ROOT = data_dir.resolve()

    authorized_key: paramiko.PKey | None = None
    if authorized_key_path is not None:
        authorized_key = _load_authorized_key(authorized_key_path)

    # Ephemeral 2048-bit RSA host key. Regenerated on every start; the
    # tests never pin it because paramiko's client trusts any host key
    # when we pass an AutoAddPolicy (and OpenDAL likewise defaults to
    # accept-any).
    host_key = paramiko.RSAKey.generate(bits=2048)

    server_sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server_sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server_sock.bind(("127.0.0.1", port))
    server_sock.listen(16)
    actual_port = server_sock.getsockname()[1]

    print_ready(actual_port)
    LOG.info("sftp mock listening on 127.0.0.1:%d root=%s", actual_port, RootedSFTPServer.ROOT)

    shutdown = install_sigterm_handler()

    # Use a background acceptor thread so the main thread can pump the
    # shutdown Event without blocking on accept().
    def accept_loop() -> None:
        server_sock.settimeout(0.25)
        while not shutdown.is_set():
            try:
                client, _addr = server_sock.accept()
            except socket.timeout:
                continue
            except OSError:
                if shutdown.is_set():
                    return
                raise
            worker = threading.Thread(
                target=_handle_connection,
                args=(client, host_key, user, password, anon, authorized_key),
                daemon=True,
            )
            worker.start()

    acceptor = threading.Thread(target=accept_loop, daemon=True)
    acceptor.start()

    shutdown.wait()
    try:
        server_sock.close()
    except OSError:
        pass
    print_shutdown()


def main() -> None:
    parser = build_argparser("sftp_server")
    parser.add_argument(
        "--authorized-key",
        type=Path,
        default=None,
        help="Path to a single OpenSSH .pub file to accept for publickey auth",
    )
    args = parser.parse_args()
    configure_logging(args.verbose)
    # Suppress paramiko's own INFO chatter unless --verbose was requested.
    if not args.verbose:
        logging.getLogger("paramiko").setLevel(logging.WARNING)
    run_server(
        args.port,
        args.data_dir,
        args.user,
        args.password,
        args.anon,
        args.authorized_key,
    )


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(0)
