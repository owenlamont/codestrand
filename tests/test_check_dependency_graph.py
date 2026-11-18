# /// script
# requires-python = ">=3.15"
# dependencies = ["pytest>=9"]
# ///

import sys
from pathlib import Path
from typing import Any

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "scripts"))

from check_dependency_graph import problems


def _metadata(
    edges: dict[str, list[str]],
    features: dict[str, list[str]] | None = None,
    dev: tuple[str, str] | None = None,
) -> dict[str, Any]:
    names = set(edges) | {child for children in edges.values() for child in children}
    nodes = [
        {
            "id": name,
            "features": (features or {}).get(name, []),
            "deps": [
                {
                    "pkg": child,
                    "dep_kinds": [{"kind": "dev" if (name, child) == dev else None}],
                }
                for child in edges.get(name, [])
            ],
        }
        for name in sorted(names)
    ]
    return {
        "packages": [{"id": name, "name": name} for name in sorted(names)],
        "resolve": {"nodes": nodes},
    }


_ALLOWED = {
    "codestrand": ["ureq", "bevy_asset", "gix-transport"],
    "ureq": ["rustls-platform-verifier", "rustls"],
    "rustls-platform-verifier": ["rustls"],
}


def test_ureq_and_rustls_under_codestrand_pass() -> None:
    assert problems(_metadata(_ALLOWED)) == []


def test_second_ureq_dependent_fails() -> None:
    graph = _ALLOWED | {"bevy_asset": ["ureq"]}
    assert problems(_metadata(graph)) == ["ureq is a dependency of bevy_asset"]


def test_rustls_beside_ureq_fails() -> None:
    graph = _ALLOWED | {"gix-transport": ["rustls"]}
    assert problems(_metadata(graph)) == [
        "rustls is reachable without going through ureq"
    ]


def test_dev_only_rustls_passes() -> None:
    graph = _ALLOWED | {"gix-transport": ["rustls"]}
    assert problems(_metadata(graph, dev=("gix-transport", "rustls"))) == []


@pytest.mark.parametrize(
    ("package", "feature"),
    [
        ("bevy_asset", "https"),
        ("bevy_asset", "web_asset_cache"),
        ("gix-transport", "http-client"),
    ],
)
def test_http_features_fail(package: str, feature: str) -> None:
    found = problems(_metadata(_ALLOWED, {package: ["std", feature]}))
    assert found == [f"{package} enables HTTP feature {feature}"]


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-q"]))
