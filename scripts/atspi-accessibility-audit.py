#!/usr/bin/env python3
"""Audit a running MailSwiftSync window through the Linux AT-SPI bus.

This inspects the accessibility tree that Orca and other Linux screen readers
consume (AccessKit -> AT-SPI), not egui's internal state. It reports every
interactive control without an accessible name and summarizes roles, which
complements the in-process `ui::accessibility_audit` tests. It does not
replace a human screen-reader pass: it cannot judge reading order, announcement
wording, or whether focus moves sensibly.

Usage: atspi-accessibility-audit.py <pid> [label]
Exit status: 0 when every interactive control is named, 1 otherwise, 2 when the
application is not on the accessibility bus.
"""

import sys
import time

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402

INTERACTIVE_ROLES = {
    Atspi.Role.PUSH_BUTTON,
    Atspi.Role.TOGGLE_BUTTON,
    Atspi.Role.CHECK_BOX,
    Atspi.Role.RADIO_BUTTON,
    Atspi.Role.COMBO_BOX,
    Atspi.Role.ENTRY,
    Atspi.Role.PASSWORD_TEXT,
    Atspi.Role.SPIN_BUTTON,
    Atspi.Role.SLIDER,
    Atspi.Role.LINK,
    Atspi.Role.MENU_ITEM,
    Atspi.Role.LIST_ITEM,
    Atspi.Role.PAGE_TAB,
}
MAX_NODES = 20_000


def find_application(pid: int, attempts: int = 20):
    for _ in range(attempts):
        desktop = Atspi.get_desktop(0)
        for index in range(desktop.get_child_count()):
            app = desktop.get_child_at_index(index)
            if app is not None and app.get_process_id() == pid:
                return app
        time.sleep(0.5)
    return None


def walk(root):
    stack = [(root, 0)]
    visited = 0
    while stack and visited < MAX_NODES:
        node, depth = stack.pop()
        visited += 1
        yield node, depth
        try:
            count = node.get_child_count()
        except Exception:  # noqa: BLE001 - nodes can vanish mid-walk
            continue
        for index in reversed(range(count)):
            child = node.get_child_at_index(index)
            if child is not None:
                stack.append((child, depth + 1))


def main() -> int:
    if len(sys.argv) not in (2, 3):
        print(__doc__.strip().splitlines()[-4], file=sys.stderr)
        return 2
    pid = int(sys.argv[1])
    label = sys.argv[2] if len(sys.argv) == 3 else str(pid)
    app = find_application(pid)
    if app is None:
        print(f"FAIL {label}: application {pid} is not on the AT-SPI bus", file=sys.stderr)
        return 2
    roles: dict[str, int] = {}
    interactive = 0
    unnamed = []
    for node, depth in walk(app):
        try:
            role = node.get_role()
            name = (node.get_name() or "").strip()
            role_name = node.get_role_name()
        except Exception:  # noqa: BLE001
            continue
        roles[role_name] = roles.get(role_name, 0) + 1
        if role in INTERACTIVE_ROLES:
            interactive += 1
            if not name:
                description = (node.get_description() or "").strip()
                unnamed.append(f"{role_name} at depth {depth}" + (f" ({description})" if description else ""))
    summary = ", ".join(f"{role}={count}" for role, count in sorted(roles.items(), key=lambda item: -item[1])[:8])
    if unnamed:
        print(f"FAIL {label}: {len(unnamed)} of {interactive} interactive controls are unnamed")
        for entry in unnamed[:20]:
            print(f"  unnamed {entry}")
        return 1
    print(f"PASS {label}: {interactive} interactive controls named; roles: {summary}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
