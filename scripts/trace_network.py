# /// script
# requires-python = ">=3.15"
# dependencies = []
# ///
"""Run a command in an empty network namespace and fail on any network attempt.

Linux only; needs `unshare` and `strace`. Usage: trace_network.py -- COMMAND [ARGS...]
"""

import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Final

# Unix socket paths the traced CI export connected to, taken from one traced run.
ALLOWED_UNIX_PATHS: Final[frozenset[str]] = frozenset()

_CALL: Final = re.compile(r"^\d+\s+(?P<name>execve|socket|connect)\((?P<args>.*)$")
_IP_FAMILY: Final = re.compile(r"^AF_INET6?\b")
_UNIX_PATH: Final = re.compile(r'sa_family=AF_UNIX, sun_path=(?P<path>@?"[^"]*")')
_PROBE_PATH: Final = "/codestrand-trace-probe"
_IP_PROBE: Final = "import socket; socket.socket(socket.AF_INET).close()"
_UNIX_PROBE: Final = f"""
import socket
with socket.socket(socket.AF_UNIX) as probe:
    try:
        probe.connect({_PROBE_PATH!r})
    except OSError:
        pass
"""


@dataclass(frozen=True)
class Trace:
    executed: bool
    calls: int
    violations: list[str]


def parse(log: str, allowed: frozenset[str] = ALLOWED_UNIX_PATHS) -> Trace:
    """Classify strace lines; split `<unfinished ...>` calls carry their arguments
    on the first half, so resumed halves are ignored."""
    executed = False
    calls = 0
    violations = []
    for line in log.splitlines():
        call = _CALL.match(line)
        if call is None:
            continue
        name, args = call["name"], call["args"]
        calls += name != "execve"
        if name == "execve":
            executed = executed or line.endswith(" = 0")
        elif name == "socket" and _IP_FAMILY.match(args):
            violations.append(f"IP socket: {line}")
        elif name == "connect" and (path := _UNIX_PATH.search(args)):
            if path["path"].replace('"', "") not in allowed:
                violations.append(f"Unix connect outside the allow-list: {line}")
    return Trace(executed, calls, violations)


def trace(command: list[str]) -> tuple[int, Trace]:
    with tempfile.TemporaryDirectory() as scratch:
        log = Path(scratch) / "strace.log"
        status = subprocess.run(
            [
                *("unshare", "--map-root-user", "--net"),
                *("strace", "--follow-forks", "--seccomp-bpf", "-qq", "-s", "4096"),
                *("-e", "trace=execve,socket,connect", "-o", str(log)),
                *command,
            ],
            check=False,
        ).returncode
        return status, parse(log.read_text(encoding="utf-8", errors="replace"))


def require_caught(source: str, expected: str) -> None:
    status, result = trace([sys.executable, "-c", source])
    if status != 0 or not result.executed:
        raise SystemExit(f"tracer control did not run cleanly (exit {status})")
    if len(result.violations) != 1 or expected not in result.violations[0]:
        raise SystemExit(f"tracer missed {expected}: {result.violations}")


def main(command: list[str]) -> int:
    require_caught(_IP_PROBE, "AF_INET")
    require_caught(_UNIX_PROBE, _PROBE_PATH)
    status, result = trace(command)
    print(
        f"controls caught an IP socket and an off-list Unix connect; command made "
        f"{result.calls} socket and connect calls",
        file=sys.stderr,
    )
    for violation in result.violations:
        print(violation, file=sys.stderr)
    if not result.executed:
        print("tracer recorded no successful execve", file=sys.stderr)
    if status != 0:
        print(f"traced command exited with {status}", file=sys.stderr)
    return int(status != 0 or not result.executed or bool(result.violations))


if __name__ == "__main__":
    arguments = sys.argv[1:]
    if arguments[:1] != ["--"] or len(arguments) < 2:
        raise SystemExit(__doc__)
    raise SystemExit(main(arguments[1:]))
