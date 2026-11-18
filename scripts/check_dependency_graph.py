# /// script
# requires-python = ">=3.15"
# dependencies = []
# ///
"""Fail when the resolved dependency graph gains a second network path."""

import json
import subprocess
import sys
from typing import Any, Final

_ROOT: Final = "codestrand"
_NO_HTTP_FEATURES: Final = ("bevy_asset", "gix-transport")


def problems(metadata: dict[str, Any]) -> list[str]:
    """Check normal and build edges for every target platform."""
    names = {package["id"]: package["name"] for package in metadata["packages"]}
    edges: dict[str, set[str]] = {}
    features: dict[str, set[str]] = {}
    for node in metadata["resolve"]["nodes"]:
        name = names[node["id"]]
        features.setdefault(name, set()).update(node["features"])
        edges.setdefault(name, set()).update(
            names[dependency["pkg"]]
            for dependency in node["deps"]
            if any(kind["kind"] != "dev" for kind in dependency["dep_kinds"])
        )
    found = [
        f"ureq is a dependency of {parent}"
        for parent, children in sorted(edges.items())
        if parent != _ROOT and "ureq" in children
    ]
    if "rustls" in _reachable(edges, without="ureq"):
        found.append("rustls is reachable without going through ureq")
    found.extend(
        f"{package} enables HTTP feature {feature}"
        for package in _NO_HTTP_FEATURES
        for feature in sorted(features.get(package, set()))
        if "http" in feature or "web" in feature
    )
    return found


def _reachable(edges: dict[str, set[str]], without: str) -> set[str]:
    seen = {_ROOT}
    pending: list[str] = [_ROOT]
    while pending:
        for child in edges.get(pending.pop(), set()):
            if child != without and child not in seen:
                seen.add(child)
                pending.append(child)
    return seen


def main() -> int:
    metadata = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--locked", "--format-version", "1"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    found = problems(metadata)
    for problem in found:
        print(problem, file=sys.stderr)
    return int(bool(found))


if __name__ == "__main__":
    raise SystemExit(main())
