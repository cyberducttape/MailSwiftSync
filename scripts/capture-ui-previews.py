#!/usr/bin/env python3
"""Capture real desktop views using isolated, synthetic demo state.

Requires a debug build, an X11 display (XWayland works), xwininfo, xprop,
and ImageMagick's import. Only this script's own application windows are
captured; personal configuration and ledgers are never opened.
"""

import argparse
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time


def own_window(pid: int) -> str | None:
    tree = subprocess.check_output(["xwininfo", "-root", "-tree"], text=True, timeout=5)
    for window in re.findall(r'(0x[0-9a-f]+) "MailSwiftSync"', tree):
        properties = subprocess.check_output(
            ["xprop", "-id", window, "_NET_WM_PID"], text=True, timeout=5
        )
        if re.search(rf"= {pid}\s*$", properties):
            return window
    return None


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/mailswiftsync"))
    parser.add_argument("--output", type=Path, default=Path("docs/wiki/assets"))
    parser.add_argument("--window", default="1440x1000", help="Preview size, e.g. 900x640")
    parser.add_argument("--theme", default="Default", help="Theme name as stored in appearance.toml, e.g. HighContrast")
    parser.add_argument("--light", action="store_true", help="Capture the light variant")
    args = parser.parse_args()
    for program in ("xwininfo", "xprop", "import"):
        if not shutil.which(program):
            parser.error(f"required program unavailable: {program}")
    if not os.environ.get("DISPLAY"):
        parser.error("an X11 DISPLAY is required")
    binary = args.binary.resolve(strict=True)
    args.output.mkdir(parents=True, exist_ok=True)
    for view, filename in [
        ("overview", "migration-overview.png"),
        ("plan", "migration-plan.png"),
        ("mailboxes", "batch-queue.png"),
        ("verification", "verification-review.png"),
    ]:
        with tempfile.TemporaryDirectory(prefix="mailswiftsync-preview-") as temporary:
            environment = dict(os.environ)
            environment.pop("WAYLAND_DISPLAY", None)
            # Do not inherit unrelated debug scenes from the caller.
            for key in list(environment):
                if key.startswith("MAILSWIFTSYNC_DEBUG_"):
                    environment.pop(key)
            appearance = Path(temporary, "config", "mailswiftsync", "appearance.toml")
            appearance.parent.mkdir(parents=True)
            # dark_mode and ui_scale are required, or the file is ignored.
            appearance.write_text(
                f'theme = "{args.theme}"\ndark_mode = {str(not args.light).lower()}\nui_scale = 1.0\n'
            )
            environment.update(
                XDG_CONFIG_HOME=f"{temporary}/config",
                XDG_DATA_HOME=f"{temporary}/data",
                MAILSWIFTSYNC_STATE_PATH=f"{temporary}/state.db",
                MAILSWIFTSYNC_DEBUG_DEMO="1",
                # Keep the wave approver placeholder free of the local username.
                USER="operator",
                MAILSWIFTSYNC_DEBUG_VIEW=view,
                MAILSWIFTSYNC_DEBUG_WINDOW=args.window,
            )
            if view == "verification":
                # The generic scene has status fixtures without proof records;
                # show the real onboarding state rather than invented evidence.
                environment.pop("MAILSWIFTSYNC_DEBUG_DEMO")
            process = subprocess.Popen(
                [str(binary)], env=environment,
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
            try:
                deadline = time.monotonic() + 20
                window = None
                while process.poll() is None and time.monotonic() < deadline:
                    window = own_window(process.pid)
                    if window:
                        break
                    time.sleep(0.2)
                if not window:
                    raise RuntimeError(f"no preview window appeared for {view}")
                time.sleep(2)  # Let fonts, layout, and the background snapshot settle.
                subprocess.run(
                    ["import", "-window", window, str(args.output / filename)],
                    check=True, timeout=15,
                )
                print(f"Captured {view}: {args.output / filename}")
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
    # The wiki home page shows the same overview under its older name.
    shutil.copyfile(args.output / "migration-overview.png", args.output / "overview.png")


if __name__ == "__main__":
    main()
