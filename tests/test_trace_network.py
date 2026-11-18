# /// script
# requires-python = ">=3.15"
# dependencies = ["pytest>=9"]
# ///

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "scripts"))

from trace_network import parse

_EXECVE = '100 execve("/bin/codestrand", ["codestrand"], 0x7ffd /* 9 vars */) = 0\n'


@pytest.mark.parametrize(
    "line",
    [
        "101 socket(AF_INET, SOCK_STREAM|SOCK_CLOEXEC, IPPROTO_TCP) = 3",
        "101 socket(AF_INET6, SOCK_DGRAM, IPPROTO_IP <unfinished ...>",
        "101 socket(AF_INET, SOCK_STREAM, IPPROTO_IP) = -1 EACCES (Permission denied)",
        '101 connect(4, {sa_family=AF_UNIX, sun_path="/run/x"}, 110) = -1 ENOENT',
        '101 connect(4, {sa_family=AF_UNIX, sun_path=@"/tmp/.X11-unix/X0"}, 20 '
        "<unfinished ...>",
    ],
)
def test_network_attempts_are_violations(line: str) -> None:
    result = parse(_EXECVE + line)
    assert result.executed
    assert result.violations == [result.violations[0]]
    assert line in result.violations[0]


@pytest.mark.parametrize(
    "line",
    [
        "101 socket(AF_UNIX, SOCK_STREAM|SOCK_CLOEXEC, 0) = 5",
        "101 socket(AF_NETLINK, SOCK_RAW|SOCK_CLOEXEC, NETLINK_ROUTE) = 5",
        "101 <... connect resumed>) = 0",
        '101 connect(4, {sa_family=AF_UNIX, sun_path="/allowed"}, 110) = 0',
    ],
)
def test_local_sockets_and_allowed_paths_pass(line: str) -> None:
    assert parse(_EXECVE + line, frozenset({"/allowed"})).violations == []


def test_failed_or_missing_execve_is_not_a_run() -> None:
    assert not parse('100 execve("/missing", ["x"], 0x1) = -1 ENOENT').executed
    assert not parse("").executed


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-q"]))
