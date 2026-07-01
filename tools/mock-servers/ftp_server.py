#!/usr/bin/env python3
"""FTP mock server for Atlas integration tests.

Backed by pyftpdlib. The server:

* Serves ``--data-dir`` as the root of a single user (``--user`` /
  ``--password``).
* When ``--anon`` is passed, additionally accepts the standard
  ``anonymous`` username with any password. OpenDAL's FTP backend sends
  ``anonymous`` when we hand it ``Credentials::Anonymous``.
* Prints ``READY port=<N>`` once the listener is bound so the Rust
  harness knows the OS-assigned port.
"""

from __future__ import annotations

import logging
import sys
from pathlib import Path

from pyftpdlib.authorizers import DummyAuthorizer
from pyftpdlib.handlers import FTPHandler
from pyftpdlib.servers import ThreadedFTPServer

from mock_common import (  # type: ignore[import-not-found]
    build_argparser,
    configure_logging,
    ensure_data_dir,
    install_sigterm_handler,
    print_ready,
    print_shutdown,
)

LOG = logging.getLogger("mock-ftp")


def build_server(port: int, data_dir: Path, user: str, password: str, anon: bool) -> ThreadedFTPServer:
    authorizer = DummyAuthorizer()
    perms = "elradfmwMT"  # full: read, write, delete, rename, mkdir, chmod, mtime
    authorizer.add_user(user, password, str(data_dir), perm=perms)
    if anon:
        authorizer.add_anonymous(str(data_dir), perm=perms)

    handler = FTPHandler
    handler.authorizer = authorizer
    handler.banner = "atlas mock ftp"
    # Pin passive ports to a narrow, deterministic range so the OS
    # firewall doesn't clash across test runs.
    handler.passive_ports = range(60000, 60100)

    server = ThreadedFTPServer(("127.0.0.1", port), handler)
    server.max_cons = 32
    server.max_cons_per_ip = 16
    return server


def run_server(port: int, data_dir: Path, user: str, password: str, anon: bool) -> None:
    ensure_data_dir(data_dir)
    server = build_server(port, data_dir, user, password, anon)
    actual_port = server.address[1]
    print_ready(actual_port)
    LOG.info("ftp mock listening on 127.0.0.1:%d root=%s anon=%s", actual_port, data_dir, anon)

    shutdown = install_sigterm_handler()
    # pyftpdlib's serve_forever takes a timeout arg for its select() loop
    # so we can check the shutdown Event periodically.
    while not shutdown.is_set():
        server.serve_forever(timeout=0.25, blocking=False)
    server.close_all()
    print_shutdown()


def main() -> None:
    parser = build_argparser("ftp_server")
    args = parser.parse_args()
    configure_logging(args.verbose)
    if not args.verbose:
        logging.getLogger("pyftpdlib").setLevel(logging.WARNING)
    run_server(args.port, args.data_dir, args.user, args.password, args.anon)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(0)
