#!/usr/bin/env python3
"""WebDAV mock server for Atlas integration tests.

Backed by wsgidav on top of cheroot's WSGI server.

* ``--data-dir`` is served as the root collection.
* ``--user`` / ``--password`` grant full access. When ``--anon`` is passed
  the domain controller is set to ``AnonymousDomainController`` so the
  ``connect_anon`` test path works without HTTP Basic auth.
* Prints ``READY port=<N>`` once the socket is bound.
"""

from __future__ import annotations

import logging
import socket
import sys
import threading
from pathlib import Path

from cheroot import wsgi as cheroot_wsgi  # type: ignore[import-not-found]
from wsgidav.wsgidav_app import WsgiDAVApp  # type: ignore[import-not-found]

from mock_common import (  # type: ignore[import-not-found]
    build_argparser,
    configure_logging,
    ensure_data_dir,
    install_sigterm_handler,
    print_ready,
    print_shutdown,
)

LOG = logging.getLogger("mock-webdav")


def build_wsgi_app(data_dir: Path, user: str, password: str, anon: bool):
    if anon:
        # ``"*": True`` marks the share as anonymous-writable.
        user_mapping = {"*": True}
    else:
        user_mapping = {"*": {user: {"password": password}}}
    config = {
        "provider_mapping": {"/": str(data_dir)},
        "simple_dc": {"user_mapping": user_mapping},
        "http_authenticator": {
            "domain_controller": None,  # let WsgiDAVApp instantiate SimpleDomainController
            "accept_basic": True,
            "accept_digest": False,
            "default_to_digest": False,
        },
        "verbose": 1,
        "logging": {"enable_loggers": []},
        "property_manager": True,
        "lock_storage": True,
        "dir_browser": {"enable": False},
    }
    return WsgiDAVApp(config)


def _bind_socket(port: int) -> tuple[socket.socket, int]:
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("127.0.0.1", port))
    s.listen(16)
    return s, s.getsockname()[1]


def _wait_ready(server: cheroot_wsgi.Server, timeout: float = 10.0) -> None:
    """Poll cheroot until it reports ``ready`` or timeout elapses.

    Cheroot flips ``server.ready`` inside its main ``start()`` loop right
    after ``bind()`` + ``listen()`` succeed, so this is our sync point.
    """
    import time as _time

    deadline = _time.monotonic() + timeout
    while _time.monotonic() < deadline:
        if getattr(server, "ready", False):
            return
        _time.sleep(0.02)
    raise RuntimeError("cheroot server did not become ready in time")


def run_server(port: int, data_dir: Path, user: str, password: str, anon: bool) -> None:
    ensure_data_dir(data_dir)

    app = build_wsgi_app(data_dir, user, password, anon)
    # Let cheroot own the bind so we never race with an intermediate close().
    server = cheroot_wsgi.Server(("127.0.0.1", port), app)

    def serve() -> None:
        try:
            server.start()
        except Exception:  # noqa: BLE001
            LOG.exception("cheroot server crashed")

    thread = threading.Thread(target=serve, daemon=True)
    thread.start()

    _wait_ready(server)
    actual_port = server.bind_addr[1]

    print_ready(actual_port)
    LOG.info("webdav mock listening on 127.0.0.1:%d root=%s anon=%s", actual_port, data_dir, anon)

    shutdown = install_sigterm_handler()
    shutdown.wait()
    try:
        server.stop()
    except Exception:  # noqa: BLE001
        LOG.exception("stopping cheroot server failed")
    print_shutdown()


def main() -> None:
    parser = build_argparser("webdav_server")
    args = parser.parse_args()
    configure_logging(args.verbose)
    if not args.verbose:
        logging.getLogger("wsgidav").setLevel(logging.WARNING)
        logging.getLogger("cheroot").setLevel(logging.WARNING)
    run_server(args.port, args.data_dir, args.user, args.password, args.anon)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(0)
