# /// script
# requires-python = ">=3.15"
# dependencies = ["pytest>=9"]
# ///

import sys
from pathlib import Path
from typing import Final

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "scripts"))

from trace_network import parse

_EXECVE: Final = (
    '100 execve("/bin/codestrand", ["codestrand"], 0x7ffd /* 9 vars */) = 0\n'
)


@pytest.mark.parametrize(
    "line",
    [
        pytest.param(
            "101 socket(AF_INET, SOCK_STREAM|SOCK_CLOEXEC, IPPROTO_TCP) = 3",
            id="IPv4 socket",
        ),
        pytest.param(
            "101 socket(AF_INET6, SOCK_DGRAM, IPPROTO_IP <unfinished ...>",
            id="unfinished IPv6 socket",
        ),
        pytest.param(
            "101 socket(AF_INET, SOCK_STREAM, IPPROTO_IP) = -1 EACCES (Denied)",
            id="refused IP socket",
        ),
        pytest.param(
            '101 connect(4, {sa_family=AF_UNIX, sun_path="/run/x"}, 110) = -1 ENOENT',
            id="failed Unix connect",
        ),
        pytest.param(
            '101 connect(4, {sa_family=AF_UNIX, sun_path=@"/tmp/.X11-unix/X0"}, 20 '
            "<unfinished ...>",
            id="abstract Unix connect",
        ),
    ],
)
def test_network_attempts_are_violations(line: str) -> None:
    result = parse(_EXECVE + line)
    assert result.executed
    assert len(result.violations) == 1
    assert line in result.violations[0]


@pytest.mark.parametrize(
    "line",
    [
        pytest.param("101 socket(AF_UNIX, SOCK_STREAM|SOCK_CLOEXEC, 0) = 5", id="Unix"),
        pytest.param(
            "101 socket(AF_NETLINK, SOCK_RAW|SOCK_CLOEXEC, NETLINK_ROUTE) = 5",
            id="netlink",
        ),
        pytest.param("101 <... connect resumed>) = 0", id="resumed half"),
        pytest.param(
            '101 connect(4, {sa_family=AF_UNIX, sun_path="/allowed"}, 110) = 0',
            id="allowed Unix path",
        ),
    ],
)
def test_local_sockets_and_allowed_paths_pass(line: str) -> None:
    assert parse(_EXECVE + line, frozenset({"/allowed"})).violations == ()


@pytest.mark.parametrize(
    "log",
    [
        pytest.param('100 execve("/missing", ["x"], 0x1) = -1 ENOENT', id="failed"),
        pytest.param("", id="empty trace"),
    ],
)
def test_without_a_successful_execve_nothing_ran(log: str) -> None:
    assert not parse(log).executed


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-q"]))
