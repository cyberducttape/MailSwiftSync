#!/usr/bin/env python3
"""Exercise imapsync's OAuth refresh callback with a credential-free IMAP peer.

This verifies the pinned engine contract: after a forced IMAP socket drop, it
runs each refresh command before XOAUTH2 reauthentication and rereads the
callback-updated access-token file. It does not qualify token expiry or
provider refresh-token rotation against a real OAuth provider.
"""

from __future__ import annotations

import base64
import os
from pathlib import Path
import shlex
import shutil
import socket
import subprocess
import tempfile
import threading
import time


ENGINE = os.environ.get("MAILSWIFTSYNC_TEST_IMAPSYNC") or shutil.which("imapsync")
EXPECTED_VERSION = "2.314"


class MockImapPeer:
    def __init__(self, expected_users: dict[str, str]) -> None:
        self.expected_users = expected_users
        self.authenticated: set[str] = set()
        self.auth_counts = {user: 0 for user in expected_users}
        self.logged_out: set[str] = set()
        self.errors: list[str] = []
        self.commands: list[str] = []
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(4)
        self.listener.settimeout(0.5)
        self.port = self.listener.getsockname()[1]
        self.thread = threading.Thread(target=self._serve, daemon=True)

    def start(self) -> None:
        self.thread.start()

    def close(self) -> None:
        self.listener.close()
        self.thread.join(timeout=3)

    def _serve(self) -> None:
        deadline = time.monotonic() + 25
        handlers: list[threading.Thread] = []
        while time.monotonic() < deadline and len(self.logged_out) < len(self.expected_users):
            try:
                connection, _ = self.listener.accept()
            except socket.timeout:
                continue
            except OSError:
                break
            handler = threading.Thread(target=self._handle, args=(connection,), daemon=True)
            handlers.append(handler)
            handler.start()
        for handler in handlers:
            handler.join(timeout=1)

    def _handle(self, connection: socket.socket) -> None:
        with connection:
            connection.settimeout(5)
            stream = connection.makefile("rwb", buffering=0)
            stream.write(b"* OK MailSwiftSync OAuth callback fixture ready\r\n")
            authenticated_user: str | None = None
            try:
                while True:
                    raw = stream.readline(8192)
                    if not raw:
                        break
                    line = raw.decode("ascii", errors="replace").strip()
                    fields = line.split(" ", 2)
                    if len(fields) < 2:
                        self.errors.append("malformed IMAP command")
                        break
                    tag, command = fields[0], fields[1].upper()
                    self.commands.append(command)
                    if command == "CAPABILITY":
                        stream.write(
                            (
                                "* CAPABILITY IMAP4rev1 AUTH=XOAUTH2 SASL-IR\r\n"
                                f"{tag} OK CAPABILITY\r\n"
                            ).encode()
                        )
                    elif command == "AUTHENTICATE":
                        if len(fields) < 3 or fields[2].split(" ", 1)[0].upper() != "XOAUTH2":
                            self.errors.append("engine did not use XOAUTH2")
                            stream.write(f"{tag} NO expected XOAUTH2\r\n".encode())
                            break
                        parts = fields[2].split(" ", 1)
                        if len(parts) == 2:
                            payload = parts[1]
                        else:
                            stream.write(b"+ \r\n")
                            payload = stream.readline(8192).decode("ascii", errors="replace").strip()
                        try:
                            decoded = base64.b64decode(payload, validate=True).decode("utf-8")
                        except (ValueError, UnicodeDecodeError):
                            decoded = ""
                        user = next(
                            (
                                name
                                for name in self.expected_users
                                if decoded.startswith(f"user={name}\x01")
                            ),
                            None,
                        )
                        if user is not None:
                            self.auth_counts[user] += 1
                            token = f"{self.expected_users[user]}-{self.auth_counts[user]}"
                            expected = f"user={user}\x01auth=Bearer {token}\x01\x01"
                        else:
                            expected = None
                        if user is None or decoded != expected:
                            self.errors.append("XOAUTH2 did not contain the callback-written token")
                            stream.write(f"{tag} NO OAuth fixture rejected credentials\r\n".encode())
                            break
                        self.authenticated.add(user)
                        authenticated_user = user
                        stream.write(f"{tag} OK authenticated\r\n".encode())
                        # Force an authenticated session to die before the
                        # caller's next IMAP operation. imapsync must reopen
                        # it and run the callback again for XOAUTH2.
                        if self.auth_counts[user] == 1:
                            break
                    elif command == "LOGOUT":
                        if authenticated_user is not None:
                            self.logged_out.add(authenticated_user)
                        stream.write(f"* BYE logging out\r\n{tag} OK LOGOUT\r\n".encode())
                        break
                    elif command == "ID":
                        stream.write(f"* ID NIL\r\n{tag} OK ID\r\n".encode())
                    elif command == "GETQUOTAROOT":
                        mailbox = fields[2] if len(fields) > 2 else "INBOX"
                        stream.write(
                            f"* QUOTAROOT {mailbox} \"\"\r\n{tag} OK QUOTAROOT\r\n".encode()
                        )
                    elif command == "GETQUOTA":
                        stream.write(f"* QUOTA \"\" (STORAGE 0 0)\r\n{tag} OK QUOTA\r\n".encode())
                    else:
                        stream.write(f"{tag} OK fixture command accepted\r\n".encode())
            except (OSError, TimeoutError):
                self.errors.append("IMAP client disconnected before fixture authentication completed")


def main() -> int:
    if not ENGINE:
        raise SystemExit("FAIL: imapsync executable not found")
    version = subprocess.run(
        [ENGINE, "--version"], capture_output=True, text=True, timeout=10, check=False
    )
    if version.returncode != 0 or version.stdout.strip() != EXPECTED_VERSION:
        raise SystemExit(f"FAIL: this fixture requires imapsync {EXPECTED_VERSION}")

    with tempfile.TemporaryDirectory(prefix="mailswiftsync oauth fixture ") as temporary:
        root = Path(temporary)
        expected = {
            "source@example.test": "refreshed-source-token",
            "dest@example.test": "refreshed-destination-token",
        }
        peer = MockImapPeer(expected)
        for index, (user, token) in enumerate(expected.items(), start=1):
            token_file = root / f"token {index}.txt"
            token_file.write_text("stale-token\n", encoding="utf-8")
            marker = root / f"callback {index}.marker"
            marker_path = shlex.quote(str(marker))
            token_path = shlex.quote(str(token_file))
            token_prefix = shlex.quote(token)
            command = " ".join(
                (
                    "/bin/sh -c",
                    shlex.quote(
                        f"if [ -f {marker_path} ]; then count=$(cat {marker_path}); else count=0; fi; "
                        f"count=$((count + 1)); printf '%s\\n' \"$count\" > {marker_path}; "
                        f"printf '%s-%s\\n' {token_prefix} \"$count\" > {token_path}"
                    ),
                )
            )
            if index == 1:
                source_token, source_command = token_file, command
            else:
                destination_token, destination_command = token_file, command

        peer.start()
        arguments = [
            ENGINE,
            "--host1", "127.0.0.1", "--port1", str(peer.port),
            "--nossl1", "--notls1",
            "--user1", "source@example.test", "--authmech1", "XOAUTH2",
            "--oauthaccesstoken1", str(source_token),
            "--oauthrefreshcmd1", source_command,
            "--host2", "127.0.0.1", "--port2", str(peer.port),
            "--nossl2", "--notls2",
            "--user2", "dest@example.test", "--authmech2", "XOAUTH2",
            "--oauthaccesstoken2", str(destination_token),
            "--oauthrefreshcmd2", destination_command,
            "--justlogin", "--nolog", "--noemailreport1", "--noemailreport2",
        ]
        try:
            try:
                result = subprocess.run(
                    arguments,
                    capture_output=True,
                    text=True,
                    timeout=15,
                    check=False,
                    env={**os.environ, "LC_ALL": "C"},
                )
            except subprocess.TimeoutExpired as error:
                raise SystemExit(
                    "FAIL: imapsync timed out in the OAuth fixture; "
                    f"IMAP commands observed: {peer.commands or 'none'}"
                ) from error
        finally:
            peer.close()

        callbacks_present = all(
            (root / f"callback {index}.marker").is_file() for index in (1, 2)
        )
        callbacks_match_auth = all(
            (root / f"callback {index}.marker").is_file()
            and (root / f"callback {index}.marker").read_text(encoding="utf-8").strip()
            == str(peer.auth_counts[user])
            and (root / f"token {index}.txt").read_text(encoding="utf-8").strip()
            == f"{token}-{peer.auth_counts[user]}"
            for index, (user, token) in enumerate(expected.items(), start=1)
        )
        if (
            result.returncode != 0
            or peer.errors
            or peer.authenticated != set(expected)
            or any(count < 2 for count in peer.auth_counts.values())
            or not callbacks_present
            or not callbacks_match_auth
        ):
            # Do not print imapsync's raw transcript: it is external process
            # output and must never become a channel for accidental secrets.
            detail = "; ".join(peer.errors) or "engine exited unsuccessfully or skipped a callback"
            raise SystemExit(f"FAIL: imapsync OAuth refresh callback fixture failed: {detail}")

    print(
        f"PASS: imapsync {EXPECTED_VERSION} invoked OAuth callbacks for "
        f"all XOAUTH2 authentications after forced reconnect "
        f"(source={peer.auth_counts['source@example.test']}, "
        f"destination={peer.auth_counts['dest@example.test']})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
