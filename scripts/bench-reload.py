#!/usr/bin/env python3
"""Measure nnnyaz's directory reload time with NNNYAZ_DEBUG=1.

Creates a temporary directory with N files, starts nnnyaz in a pty, and reads
the `reload: N entries in ...` line it prints to stderr. Run after `cargo build`:

    python3 scripts/bench-reload.py [N] [path/to/nnnyaz]

Newer builds also print `collect` and `sort` phase timings, which are shown when
available. Like `scripts/pty-smoke.py`, this is a manual tool rather than a CI
check because pty timing is platform-dependent.
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

RELOAD = re.compile(
    r"reload: (\d+) entries in ([0-9.]+)(µs|ms|s)"
    r"(?: \(collect ([0-9.]+)(µs|ms|s), sort ([0-9.]+)(µs|ms|s)\))?"
)


def to_seconds(value, unit):
    return float(value) * {"ns": 1e-9, "µs": 1e-6, "ms": 1e-3, "s": 1.0}[unit]


def bench(count, binary):
    with tempfile.TemporaryDirectory(prefix="nnnyaz-bench-") as root:
        cwd = os.path.join(root, "files")
        home = os.path.join(root, "home")
        os.makedirs(cwd)
        os.makedirs(os.path.join(home, ".config", "nnnyaz"))
        for i in range(count):
            with open(os.path.join(cwd, f"file-{i:06d}.txt"), "w") as handle:
                handle.write("")
        pid, fd = pty.fork()
        if pid == 0:
            os.chdir(cwd)
            os.environ["HOME"] = home
            os.environ["TERM"] = "xterm-256color"
            os.environ["NNNYAZ_DEBUG"] = "1"
            os.execv(binary, [binary])
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
        out = b""
        end = time.time() + 30
        while time.time() < end and b"reload:" not in out:
            ready, _, _ = select.select([fd], [], [], 0.1)
            if ready:
                try:
                    out += os.read(fd, 65536)
                except OSError:
                    break
        # The first read may end mid-line; grab the rest of the debug line.
        ready, _, _ = select.select([fd], [], [], 0.3)
        if ready:
            try:
                out += os.read(fd, 65536)
            except OSError:
                pass
        try:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
        except (ProcessLookupError, ChildProcessError):
            pass
        os.close(fd)
        match = RELOAD.search(out.decode("utf-8", "replace"))
        if not match:
            raise RuntimeError(f"no reload timing found; tail: {out[-400:]!r}")
        if match.group(4):
            collect = to_seconds(match.group(4), match.group(5)) * 1000
            sort = to_seconds(match.group(6), match.group(7)) * 1000
            print(f"    collect={collect:.1f}ms sort={sort:.1f}ms")
        return int(match.group(1)), to_seconds(match.group(2), match.group(3))


def main():
    count = int(sys.argv[1]) if len(sys.argv) > 1 else 20000
    binary = os.path.abspath(sys.argv[2] if len(sys.argv) > 2 else "target/debug/nnnyaz")
    if not os.path.exists(binary):
        sys.exit(f"binary not found: {binary} (run cargo build first)")
    print(f"benchmarking {count} files with {binary}")
    for label in ("run 1", "run 2", "run 3"):
        entries, seconds = bench(count, binary)
        print(f"  {label}: {entries} entries in {seconds * 1000:.1f} ms")


if __name__ == "__main__":
    main()
