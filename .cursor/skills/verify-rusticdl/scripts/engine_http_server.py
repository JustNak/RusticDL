#!/usr/bin/env python3
"""Local HTTP fixture for the download-engine recipe.

One route, ``/payload.bin``. HEAD and GET are configured separately:
Content-Encoding, Content-Length, Accept-Ranges, and Range/206 behaviour.
Stdlib only (``http.server``).
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

ROUTE = "/payload.bin"
IDENTITY_LEN = 4096

# Resolved per process before the server starts.
CONFIG: dict = {}
LOG_PATH = ""
MANIFEST_PATH = ""
LOG_LOCK = threading.Lock()


def identity_bytes(length: int) -> bytes:
    return bytes((i * 17 + 3) % 251 for i in range(length))


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def preset(name: str) -> dict:
    """Built-in cases. Lengths are resolved after gzip is computed."""
    if name == "identity":
        return {
            "case": name,
            "head": {
                "status": 200,
                "content_encoding": None,
                "content_length": "identity",
                "accept_ranges": False,
            },
            "get": {
                "status": 200,
                "content_encoding": None,
                "content_length": "identity",
                "body": "identity",
                "accept_ranges": False,
                "ranges": "ignore",
                "cut_after": None,
            },
        }
    if name == "gzip-both":
        return {
            "case": name,
            "head": {
                "status": 200,
                "content_encoding": "gzip",
                "content_length": "gzip",
                "accept_ranges": False,
            },
            "get": {
                "status": 200,
                "content_encoding": "gzip",
                "content_length": "gzip",
                "body": "gzip",
                "accept_ranges": False,
                "ranges": "ignore",
                "cut_after": None,
            },
        }
    if name == "length-mismatch":
        return {
            "case": name,
            "head": {
                "status": 200,
                "content_encoding": None,
                "content_length": 8192,
                "accept_ranges": False,
            },
            "get": {
                "status": 200,
                "content_encoding": None,
                "content_length": "identity",
                "body": "identity",
                "accept_ranges": False,
                "ranges": "ignore",
                "cut_after": None,
            },
        }
    if name == "resume-206":
        return {
            "case": name,
            "head": {
                "status": 200,
                "content_encoding": None,
                "content_length": "identity",
                "accept_ranges": True,
            },
            "get": {
                "status": 200,
                "content_encoding": None,
                "content_length": "identity",
                "body": "identity",
                "accept_ranges": True,
                "ranges": "206",
                "cut_after": 1024,
            },
        }
    raise SystemExit(f"unknown case {name!r}")


def load_scenario(path: str) -> dict:
    with open(path, encoding="utf-8") as handle:
        data = json.load(handle)
    if not isinstance(data, dict):
        raise SystemExit("scenario must be a JSON object")
    data.setdefault("case", "scenario")
    for side in ("head", "get"):
        if side not in data or not isinstance(data[side], dict):
            raise SystemExit(f"scenario missing object {side!r}")
    return data


def resolve_length(spec, identity: bytes, gz: bytes):
    if spec == "identity":
        return len(identity)
    if spec == "gzip":
        return len(gz)
    if spec == "omit" or spec is None:
        return None
    if isinstance(spec, int) and spec >= 0:
        return spec
    raise SystemExit(f"bad content_length {spec!r}")


def body_for(kind: str, identity: bytes, gz: bytes) -> bytes:
    if kind == "identity":
        return identity
    if kind == "gzip":
        return gz
    if kind == "empty":
        return b""
    raise SystemExit(f"bad body {kind!r}")


def prepare(raw: dict) -> dict:
    identity = identity_bytes(int(raw.get("identity_len", IDENTITY_LEN)))
    gz = gzip.compress(identity, mtime=0)
    head = raw["head"]
    get = raw["get"]
    get_body = body_for(get.get("body", "identity"), identity, gz)
    resolved = {
        "case": raw.get("case", "scenario"),
        "route": ROUTE,
        "identity": identity,
        "gzip": gz,
        "get_body": get_body,
        "head": {
            "status": int(head.get("status", 200)),
            "content_encoding": head.get("content_encoding"),
            "content_length": resolve_length(head.get("content_length", "identity"), identity, gz),
            "accept_ranges": bool(head.get("accept_ranges", False)),
        },
        "get": {
            "status": int(get.get("status", 200)),
            "content_encoding": get.get("content_encoding"),
            "content_length": resolve_length(get.get("content_length", "identity"), identity, gz),
            "accept_ranges": bool(get.get("accept_ranges", False)),
            "ranges": get.get("ranges", "ignore"),
            "cut_after": get.get("cut_after"),
        },
    }
    if resolved["get"]["ranges"] not in ("ignore", "206"):
        raise SystemExit("get.ranges must be 'ignore' or '206'")
    return resolved


def write_manifest(resolved: dict, path: str) -> None:
    head = resolved["head"]
    get = resolved["get"]
    payload = {
        "case": resolved["case"],
        "route": ROUTE,
        "identity_len": len(resolved["identity"]),
        "identity_sha256": sha256_hex(resolved["identity"]),
        "gzip_len": len(resolved["gzip"]),
        "gzip_sha256": sha256_hex(resolved["gzip"]),
        "wire_len": len(resolved["get_body"]),
        "wire_sha256": sha256_hex(resolved["get_body"]),
        "head_status": head["status"],
        "head_content_encoding": head["content_encoding"],
        "head_content_length": head["content_length"],
        "head_accept_ranges": head["accept_ranges"],
        "get_status": get["status"],
        "get_content_encoding": get["content_encoding"],
        "get_content_length": get["content_length"],
        "get_accept_ranges": get["accept_ranges"],
        "get_ranges": get["ranges"],
        "get_cut_after": get["cut_after"],
    }
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(payload, handle, indent=2)
        handle.write("\n")


def append_log(line: str) -> None:
    with LOG_LOCK:
        with open(LOG_PATH, "a", encoding="utf-8") as handle:
            handle.write(line + "\n")


def parse_range(header: str, total: int):
    """Return (start, end) inclusive, or None when the header is not a single byte range."""
    if not header:
        return None
    text = header.strip()
    if not text.lower().startswith("bytes="):
        return None
    spec = text[6:].strip()
    if "," in spec or "-" not in spec:
        return None
    start_s, end_s = spec.split("-", 1)
    if start_s == "":
        return None
    try:
        start = int(start_s)
    except ValueError:
        return None
    if start < 0 or start >= total:
        return None
    if end_s == "":
        end = total - 1
    else:
        try:
            end = int(end_s)
        except ValueError:
            return None
        end = min(end, total - 1)
    if end < start:
        return None
    return start, end


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt: str, *args) -> None:
        return

    def do_HEAD(self) -> None:  # noqa: N802
        self._respond("HEAD")

    def do_GET(self) -> None:  # noqa: N802
        self._respond("GET")

    def _respond(self, method: str) -> None:
        path = urlparse(self.path).path
        if path != ROUTE:
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.send_header("Connection", "close")
            self.end_headers()
            append_log(f"REQ {method} {path} range=- -> 404")
            return

        side = CONFIG["head"] if method == "HEAD" else CONFIG["get"]
        range_header = self.headers.get("Range")
        body = CONFIG["get_body"]
        status = side["status"]
        encoding = side["content_encoding"]
        length = side["content_length"]
        send = b""
        content_range = None

        if method == "GET":
            parsed = parse_range(range_header or "", len(body))
            if parsed is not None and CONFIG["get"]["ranges"] == "206":
                start, end = parsed
                send = body[start : end + 1]
                status = 206
                length = len(send)
                content_range = f"bytes {start}-{end}/{len(body)}"
            else:
                send = body
                cut = CONFIG["get"].get("cut_after")
                if isinstance(cut, int) and cut >= 0:
                    send = body[:cut]

        self.send_response(status)
        if encoding:
            self.send_header("Content-Encoding", encoding)
        if side["accept_ranges"]:
            self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("ETag", '"verify-engine-1"')
        if content_range:
            self.send_header("Content-Range", content_range)
        if length is not None:
            self.send_header("Content-Length", str(length))
        self.send_header("Connection", "close")
        self.end_headers()
        if method == "GET" and send:
            self.wfile.write(send)
        append_log(
            "REQ {method} {path} range={range} -> {status} encoding={enc} "
            "advertised_length={length} sent={sent}".format(
                method=method,
                path=path,
                range=range_header or "-",
                status=status,
                enc=encoding or "-",
                length="-" if length is None else length,
                sent=0 if method == "HEAD" else len(send),
            )
        )


def main() -> None:
    parser = argparse.ArgumentParser(description="download-engine HTTP fixture")
    parser.add_argument("--case", choices=["identity", "gzip-both", "length-mismatch", "resume-206"])
    parser.add_argument("--scenario")
    parser.add_argument("--bind", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--log", required=True)
    parser.add_argument("--manifest", required=True)
    args = parser.parse_args()
    if bool(args.case) == bool(args.scenario):
        raise SystemExit("pass exactly one of --case or --scenario")

    global CONFIG, LOG_PATH, MANIFEST_PATH
    raw = preset(args.case) if args.case else load_scenario(args.scenario)
    CONFIG = prepare(raw)
    LOG_PATH = args.log
    MANIFEST_PATH = args.manifest
    write_manifest(CONFIG, args.manifest)
    open(args.log, "w", encoding="utf-8").close()

    server = ThreadingHTTPServer((args.bind, args.port), Handler)
    server.daemon_threads = True
    host, port = server.server_address[:2]
    print(f"PORT {port}", flush=True)
    print(f"URL http://{host}:{port}{ROUTE}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
