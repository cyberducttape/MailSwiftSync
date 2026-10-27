#!/usr/bin/env python3
"""Exercise Fleet API authentication and role boundaries over live HTTP."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import http.client
import json
import os
import re
import socket
import sys
from urllib.parse import urlsplit


MAX_RESPONSE_BYTES = 64 * 1024
MISSING_PROJECT_ID = "dast-no-such-project-71c9a2"
MISSING_WAVE_ID = "dast-no-such-wave-71c9a2"


def request(base_url: str, method: str, path: str, token: str | None = None,
            extra_headers: tuple[tuple[str, str], ...] = ()) -> int:
    parsed = urlsplit(base_url)
    if parsed.scheme != "http" or not parsed.hostname:
        raise ValueError("the DAST smoke target must be an http URL with a hostname")
    if parsed.path or parsed.query or parsed.fragment or parsed.username:
        raise ValueError("the DAST smoke target must contain only scheme, host, and port")

    headers = {"Host": parsed.netloc, "Connection": "close"}
    if token is not None:
        headers["Authorization"] = f"Bearer {token}"
    headers.update(dict(extra_headers))
    connection = http.client.HTTPConnection(parsed.hostname, parsed.port or 80, timeout=5)
    try:
        connection.request(method, path, headers=headers)
        response = connection.getresponse()
        body = response.read(MAX_RESPONSE_BYTES + 1)
        if len(body) > MAX_RESPONSE_BYTES:
            raise RuntimeError("Fleet API response exceeded the DAST smoke limit")
        return response.status
    finally:
        connection.close()


def duplicate_authorization_status(base_url: str, reader_token: str,
                                   operator_token: str) -> int:
    parsed = urlsplit(base_url)
    host = parsed.hostname
    if parsed.scheme != "http" or not host:
        raise ValueError("the DAST smoke target must be an http URL with a hostname")
    port = parsed.port or 80
    request_head = (
        f"GET /v1/fleet HTTP/1.1\r\n"
        f"Host: {host}:{port}\r\n"
        f"Authorization: Bearer {reader_token}\r\n"
        f"Authorization: Bearer {operator_token}\r\n"
        "Connection: close\r\n\r\n"
    ).encode("ascii")
    with socket.create_connection((host, port), timeout=5) as stream:
        stream.settimeout(5)
        stream.sendall(request_head)
        response = bytearray()
        while len(response) <= MAX_RESPONSE_BYTES:
            chunk = stream.recv(4096)
            if not chunk:
                break
            response.extend(chunk)
    if len(response) > MAX_RESPONSE_BYTES:
        raise RuntimeError("Fleet API response exceeded the DAST smoke limit")
    status_line = bytes(response).split(b"\r\n", 1)[0]
    fields = status_line.split(b" ", 2)
    if len(fields) < 2 or not fields[1].isdigit():
        raise RuntimeError("Fleet API returned a malformed HTTP status line")
    return int(fields[1])


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="http://127.0.0.1:9464")
    parser.add_argument("--report", required=True)
    args = parser.parse_args()

    reader = os.environ.get("FLEET_API_DAST_TOKEN", "")
    operator = os.environ.get("FLEET_API_DAST_OPERATOR_TOKEN", "")
    workflow_commit = os.environ.get("GITHUB_SHA", "")
    head_commit = os.environ.get("GITHUB_HEAD_SHA", workflow_commit)
    if len(reader) < 32 or len(operator) < 32 or reader == operator:
        parser.error("distinct reader and operator tokens of at least 32 characters are required")
    full_sha = r"(?:[0-9a-fA-F]{40}|[0-9a-fA-F]{64})"
    if not re.fullmatch(full_sha, workflow_commit):
        parser.error("GITHUB_SHA must contain a full 40- or 64-character commit identifier")
    if not re.fullmatch(full_sha, head_commit):
        parser.error("GITHUB_HEAD_SHA must contain a full 40- or 64-character commit identifier")

    cases: list[tuple[str, str, str, int]] = [
        ("anonymous", "GET", "/v1/fleet", 401),
        ("invalid", "GET", "/v1/fleet", 401),
        ("reader", "GET", "/v1/fleet", 200),
        ("operator", "GET", "/v1/fleet", 200),
        ("anonymous", "POST", f"/v1/projects/{MISSING_PROJECT_ID}/hold?reason=dast", 401),
        ("reader", "POST", f"/v1/projects/{MISSING_PROJECT_ID}/hold?reason=dast", 403),
        # A fresh empty DAST ledger makes 404 evidence that authorization
        # passed and the request reached object lookup, without changing state.
        ("operator", "POST", f"/v1/projects/{MISSING_PROJECT_ID}/hold?reason=dast", 404),
        ("anonymous", "POST", f"/v1/waves/{MISSING_WAVE_ID}/approve", 401),
        ("reader", "POST", f"/v1/waves/{MISSING_WAVE_ID}/approve", 403),
        ("operator", "POST", f"/v1/waves/{MISSING_WAVE_ID}/approve", 404),
        ("operator", "POST", f"/v1/projects/{MISSING_PROJECT_ID}/hold?reason=dast", 413),
    ]
    tokens = {"reader": reader, "operator": operator}
    results = []
    for role, method, path, expected in cases:
        headers: tuple[tuple[str, str], ...] = ()
        if role == "invalid":
            token = "invalid-dast-token"
        elif role == "operator" and method == "POST" and expected == 413:
            token = operator
            headers = (("Content-Length", "1"),)
        else:
            token = tokens.get(role)
        actual = request(args.base_url, method, path, token, headers)
        results.append({
            "role": role,
            "method": method,
            "path": path.split("?", 1)[0],
            "expected_status": expected,
            "actual_status": actual,
            "result": "pass" if actual == expected else "fail",
        })

    duplicate_status = duplicate_authorization_status(args.base_url, reader, operator)
    results.append({
        "role": "ambiguous-headers",
        "method": "GET",
        "path": "/v1/fleet",
        "expected_status": 400,
        "actual_status": duplicate_status,
        "result": "pass" if duplicate_status == 400 else "fail",
    })

    report = {
        "schema_version": 1,
        "commit": head_commit.lower(),
        "workflow_commit": workflow_commit.lower(),
        "workflow_run_id": os.environ.get("GITHUB_RUN_ID"),
        "started_at": datetime.now(timezone.utc)
        .isoformat(timespec="seconds")
        .replace("+00:00", "Z"),
        "target": args.base_url,
        "transport": "http",
        "authentication_profiles": ["anonymous", "invalid", "reader", "operator"],
        "checks": results,
        "summary": {
            "passed": sum(check["result"] == "pass" for check in results),
            "failed": sum(check["result"] == "fail" for check in results),
        },
    }
    with open(args.report, "w", encoding="utf-8") as output:
        json.dump(report, output, indent=2)
        output.write("\n")
    print(json.dumps(report["summary"], sort_keys=True))
    return 0 if report["summary"]["failed"] == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
