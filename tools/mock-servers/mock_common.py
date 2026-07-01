#!/usr/bin/env python3
"""Common helpers for the mock-servers CLIs.

Every mock server shares:

* a ``build_argparser`` that emits ``--port`` / ``--data-dir`` / ``--user`` /
  ``--password`` / ``--anon`` / ``--verbose`` in a consistent order,
* logging pinned to *stderr* (so stdout only ever carries the two sync
  signals the Rust harness parses),
* ``print_ready(port)`` and ``print_shutdown()`` writers that flush
  immediately so the harness sees the marker line without buffering,
* a small SIGTERM handler that flips a threading.Event the caller can
  wait on, letting each server run its own accept loop and shut down
  cleanly.

The two sync markers on stdout are intentionally the only lines a Rust
consumer needs to look for:

    READY port=<N>
    SHUTDOWN
"""

from __future__ import annotations

import argparse
import logging
import signal
import sys
import threading
from pathlib import Path


def build_argparser(name: str, default_user: str = "atlas") -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog=name, description=f"{name} mock server for Atlas tests")
    parser.add_argument("--port", type=int, default=0, help="TCP port; 0 = ask the OS")
    parser.add_argument(
        "--data-dir",
        type=Path,
        required=True,
        help="Directory to serve; created if missing",
    )
    parser.add_argument("--user", default=default_user, help="Username for password auth")
    parser.add_argument("--password", default="atlas", help="Password for password auth")
    parser.add_argument(
        "--anon", action="store_true", help="Allow anonymous login (no password required)"
    )
    parser.add_argument("--verbose", action="store_true", help="DEBUG-level logging")
    return parser


def configure_logging(verbose: bool) -> None:
    logging.basicConfig(
        level=logging.DEBUG if verbose else logging.INFO,
        stream=sys.stderr,
        format="[%(asctime)s %(levelname)s %(name)s] %(message)s",
    )


def ensure_data_dir(data_dir: Path) -> None:
    data_dir.mkdir(parents=True, exist_ok=True)


def print_ready(port: int) -> None:
    """Emit the one sync-marker line the Rust harness parses.

    stdout MUST carry exactly this format so ``MockXxxServer::start`` can
    discover the OS-assigned port and unblock the calling test.
    """
    sys.stdout.write(f"READY port={port}\n")
    sys.stdout.flush()


def print_shutdown() -> None:
    sys.stdout.write("SHUTDOWN\n")
    sys.stdout.flush()


def install_sigterm_handler() -> threading.Event:
    """Return an Event that is set when SIGTERM / SIGINT is received."""
    event = threading.Event()

    def _handle(signum: int, _frame: object) -> None:
        logging.getLogger(__name__).info("caught signal %s, shutting down", signum)
        event.set()

    signal.signal(signal.SIGTERM, _handle)
    signal.signal(signal.SIGINT, _handle)
    return event
