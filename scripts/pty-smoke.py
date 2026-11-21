#!/usr/bin/env python3
"""Manual pty smoke test for nnnyaz.

Exercises the help/search overlay and picker mode against a real terminal.
This is intentionally *not* wired into CI: pty timing is platform-dependent
and would make the build flaky. Run it locally after UI changes:

    python3 scripts/pty-smoke.py [path/to/nnnyaz]

It needs only the Python standard library and a POSIX pty.
"""

import fcntl
import os
import pty
import re
import select
import signal
import struct
import sys
import tempfile
import termios
import time

ANSI = re.compile(
    rb"\x1b\[[0-9;?<>]*[a-zA-Z]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[_P^][^\x1b\\]*(?:\x1b\\)?"
)


def strip_ansi(data):
    return ANSI.sub(b"", data).decode("utf-8", "replace")


class Session:
    def __init__(self, argv, cwd, home):
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.chdir(cwd)
            os.environ["HOME"] = home
            os.environ["TERM"] = "xterm-256color"
            os.execv(argv[0], argv)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))

    def read(self, seconds=0.2):
        end = time.time() + seconds
        chunks = []
        while time.time() < end:
            ready, _, _ = select.select([self.fd], [], [], 0.05)
            if ready:
                try:
                    chunks.append(os.read(self.fd, 65536))
                except OSError:
                    break
        return b"".join(chunks)

    def wait_for(self, needle, timeout=6.0):
        raw = b""
        end = time.time() + timeout
        while time.time() < end:
            raw += self.read(0.2)
            if needle in strip_ansi(raw):
                return strip_ansi(raw)
        raise AssertionError(f"never saw {needle!r}; captured: {strip_ansi(raw)[-600:]!r}")

    def press(self, key, seconds=0.5):
        os.write(self.fd, key)
        return self.read(seconds)

    def send_and_wait(self, key, needle, timeout=6.0):
        os.write(self.fd, key)
        raw = self.read(0.4)
        end = time.time() + timeout
        while needle not in strip_ansi(raw) and time.time() < end:
            raw += self.read(0.2)
        if needle not in strip_ansi(raw):
            raise AssertionError(
                f"after {key!r} never saw {needle!r}; captured: {strip_ansi(raw)[-600:]!r}"
            )
        return strip_ansi(raw)

    def close(self):
        try:
            os.kill(self.pid, signal.SIGKILL)
            os.waitpid(self.pid, 0)
        except (ProcessLookupError, ChildProcessError):
            pass
        os.close(self.fd)


def binary():
    if len(sys.argv) > 1:
        return os.path.abspath(sys.argv[1])
    default = os.path.join(os.path.dirname(__file__), "..", "target", "debug", "nnnyaz")
    return os.path.abspath(default)


def test_help_search(bin_path, cwd, home):
    session = Session([bin_path], cwd, home)
    try:
        session.wait_for("1/2")
        session.send_and_wait(b"~", "nnnyaz keys")
        session.send_and_wait(b"/", "search")
        # Partial redraws use cursor moves, so a plain ANSI strip cannot
        # reconstruct the final line; just confirm typing keeps the app alive
        # and painting instead of crashing or freezing.
        assert len(session.press(b"nav")) > 0, "typing in help search produced no redraw"
        print("  help overlay + search: ok")
    finally:
        session.close()


def test_picker(bin_path, cwd, home):
    chosen = os.path.join(cwd, "..", "chosen.txt")
    session = Session([bin_path, "-p", chosen], cwd, home)
    try:
        session.wait_for("1/2")
        session.press(b"\r")
    finally:
        session.close()
    with open(chosen, "rb") as handle:
        data = handle.read().decode()
    assert data.rstrip("\n") == os.path.join(cwd, "a.txt"), data
    print("  picker Enter writes hovered path: ok")


def main():
    bin_path = binary()
    if not os.path.exists(bin_path):
        sys.exit(f"nnnyaz binary not found: {bin_path} (run cargo build first)")
    with tempfile.TemporaryDirectory(prefix="nnnyaz-pty-") as root:
        cwd = os.path.join(root, "work")
        home = os.path.join(root, "home")
        os.makedirs(cwd)
        os.makedirs(os.path.join(home, ".config", "nnnyaz"))
        for name in ("a.txt", "b.txt"):
            with open(os.path.join(cwd, name), "w") as handle:
                handle.write(name)
        print(f"pty smoke test: {bin_path}")
        test_help_search(bin_path, cwd, home)
        test_picker(bin_path, cwd, home)
    print("all pty checks passed")


if __name__ == "__main__":
    main()
