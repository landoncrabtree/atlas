#!/usr/bin/env python3
"""S3 mock server for Atlas integration tests.

Backed by ``moto`` running its in-process server + ``boto3`` for
one-shot bucket bootstrap. This wrapper:

* Binds to an OS-assigned port (or ``--port <N>`` if the caller pins
  one), so tests can run in parallel without racing.
* Creates ``--bucket`` (default: ``atlas-test``) via ``boto3`` after the
  moto server is up, so the OpenDAL S3 client sees an existing bucket
  and doesn't have to be a bucket admin.
* ``--data-dir`` is accepted for CLI consistency but ignored: moto keeps
  everything in-process.
* Uses fixed IAM keys ``atlas-mock`` / ``atlas-mock-secret`` under a
  ``us-east-1`` region so the Rust harness knows exactly what to pass
  as ``Credentials::Iam``.
"""

from __future__ import annotations

import logging
import os
import socket
import sys
import time
from pathlib import Path

# moto ≥5 exposes a plain wsgi app via moto.server; the ThreadedMotoServer
# helper wraps it in werkzeug and is what we want.
from moto.server import ThreadedMotoServer  # type: ignore[import-not-found]
import boto3  # type: ignore[import-not-found]
from botocore.config import Config as BotoConfig  # type: ignore[import-not-found]

from mock_common import (  # type: ignore[import-not-found]
    build_argparser,
    configure_logging,
    ensure_data_dir,
    install_sigterm_handler,
    print_ready,
    print_shutdown,
)

LOG = logging.getLogger("mock-s3")

# Fixed credentials the Rust harness passes verbatim as
# ``Credentials::Iam { access_key_id, secret_key, .. }``.
ACCESS_KEY = "atlas-mock"
SECRET_KEY = "atlas-mock-secret"
REGION = "us-east-1"


def _pick_port(requested: int) -> int:
    if requested != 0:
        return requested
    # Ask the OS for a free port, then close so moto can bind it.
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def _wait_until_bound(port: int, deadline: float) -> bool:
    while time.time() < deadline:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
            try:
                probe.connect(("127.0.0.1", port))
                return True
            except OSError:
                time.sleep(0.05)
    return False


def _create_bucket(endpoint: str, bucket: str) -> None:
    session = boto3.session.Session(
        aws_access_key_id=ACCESS_KEY,
        aws_secret_access_key=SECRET_KEY,
        region_name=REGION,
    )
    s3 = session.client(
        "s3",
        endpoint_url=endpoint,
        config=BotoConfig(signature_version="s3v4", retries={"max_attempts": 3}),
    )
    s3.create_bucket(Bucket=bucket)


def run_server(port: int, data_dir: Path, bucket: str) -> None:
    ensure_data_dir(data_dir)
    actual_port = _pick_port(port)
    server = ThreadedMotoServer(ip_address="127.0.0.1", port=actual_port, verbose=False)
    server.start()

    endpoint = f"http://127.0.0.1:{actual_port}"
    if not _wait_until_bound(actual_port, deadline=time.time() + 10.0):
        LOG.error("moto server never accepted connections on %s", endpoint)
        server.stop()
        sys.exit(1)

    try:
        _create_bucket(endpoint, bucket)
    except Exception:  # noqa: BLE001 — surface but don't crash; tests fail loud
        LOG.exception("failed to pre-create bucket %s at %s", bucket, endpoint)

    print_ready(actual_port)
    LOG.info(
        "s3 mock listening on 127.0.0.1:%d bucket=%s endpoint=%s access=%s",
        actual_port,
        bucket,
        endpoint,
        ACCESS_KEY,
    )

    shutdown = install_sigterm_handler()
    shutdown.wait()
    try:
        server.stop()
    except Exception:  # noqa: BLE001
        LOG.exception("stopping moto server failed")
    print_shutdown()


def main() -> None:
    parser = build_argparser("s3_server")
    parser.add_argument("--bucket", default="atlas-test", help="Bucket to pre-create")
    args = parser.parse_args()
    configure_logging(args.verbose)
    if not args.verbose:
        # moto/werkzeug are chatty at INFO.
        logging.getLogger("werkzeug").setLevel(logging.WARNING)
        logging.getLogger("moto").setLevel(logging.WARNING)
        logging.getLogger("botocore").setLevel(logging.WARNING)
        logging.getLogger("boto3").setLevel(logging.WARNING)
        logging.getLogger("urllib3").setLevel(logging.WARNING)
    # Silence boto3 credential resolution warnings about looking for
    # ~/.aws by shoving fixed creds into the env.
    os.environ.setdefault("AWS_ACCESS_KEY_ID", ACCESS_KEY)
    os.environ.setdefault("AWS_SECRET_ACCESS_KEY", SECRET_KEY)
    os.environ.setdefault("AWS_DEFAULT_REGION", REGION)
    run_server(args.port, args.data_dir, args.bucket)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(0)
