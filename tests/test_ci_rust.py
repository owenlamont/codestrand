# /// script
# requires-python = ">=3.15"
# dependencies = ["pytest>=9"]
# ///

import os
import shutil
import subprocess
from collections.abc import Sequence
from pathlib import Path
from types import MappingProxyType
from typing import Final

import pytest

_SCRIPT: Final = Path(__file__).resolve().parents[1] / "scripts/ci_rust.sh"
_ENVIRONMENT: Final = MappingProxyType(
    {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    | {"GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1"}
)


def _git(repository: Path, arguments: Sequence[str]) -> None:
    subprocess.run(
        ["git", "-C", str(repository), *arguments],
        env=_ENVIRONMENT,
        check=True,
        capture_output=True,
        timeout=30,
    )


@pytest.fixture
def repository(tmp_path: Path) -> Path:
    _git(tmp_path, ["init", "--initial-branch=main"])
    _git(tmp_path, ["config", "user.name", "CI test"])
    _git(tmp_path, ["config", "user.email", "ci@example.invalid"])
    _git(tmp_path, ["config", "core.hooksPath", str(tmp_path / "disabled-hooks")])
    _git(tmp_path, ["config", "commit.gpgsign", "false"])
    (tmp_path / "src").mkdir()
    (tmp_path / "src/main.rs").write_text("fn main() {}\n", encoding="utf-8")
    _git(tmp_path, ["add", "."])
    _git(tmp_path, ["commit", "-m", "base"])
    _git(tmp_path, ["switch", "-c", "change"])
    return tmp_path


def _merge(repository: Path) -> None:
    _git(repository, ["add", "--all"])
    _git(repository, ["commit", "-m", "change"])
    _git(repository, ["switch", "main"])
    _git(repository, ["merge", "--no-ff", "--no-edit", "change"])


def _classify(*, repository: Path, event: str) -> subprocess.CompletedProcess[str]:
    bash = shutil.which("bash")
    assert bash is not None
    return subprocess.run(
        [bash, str(_SCRIPT)],
        cwd=repository,
        env=_ENVIRONMENT | {"GITHUB_EVENT_NAME": event},
        capture_output=True,
        text=True,
        timeout=30,
    )


@pytest.mark.parametrize(
    ("path", "expected"),
    [
        pytest.param("README.md", "false", id="readme"),
        pytest.param("AGENTS.md", "false", id="agent guidance"),
        pytest.param("docs/guide.md", "false", id="documentation"),
        pytest.param("docs/nested/guide.md", "false", id="nested documentation"),
        pytest.param("assets/codestrand.svg", "false", id="logo"),
        pytest.param("CONTRIBUTING.md", "false", id="contributing"),
        pytest.param("zensical.toml", "false", id="site config"),
        pytest.param("lychee.toml", "false", id="link config"),
        pytest.param("docs/assets/codestrand.svg", "false", id="docs logo"),
        pytest.param("docs/assets/nested/image.png", "false", id="nested docs asset"),
        pytest.param("scripts/check_docs_logo.sh", "false", id="docs logo check"),
        pytest.param("pyproject.toml", "true", id="python manifest"),
        pytest.param("uv.lock", "true", id="python lockfile"),
        pytest.param(".gitignore", "true", id="shared ignore rules"),
        pytest.param("src/main.rs", "true", id="rust"),
        pytest.param("Cargo.toml", "true", id="manifest"),
        pytest.param("Cargo.lock", "true", id="lockfile"),
        pytest.param("rust-toolchain.toml", "true", id="toolchain"),
        pytest.param("clippy.toml", "true", id="clippy config"),
        pytest.param("rustfmt.toml", "true", id="rustfmt config"),
        pytest.param(".cargo/config.toml", "true", id="cargo config"),
        pytest.param("prek.toml", "true", id="hook config"),
        pytest.param(".github/workflows/ci.yml", "true", id="workflow"),
        pytest.param("tests/fixture.md", "true", id="markdown fixture"),
        pytest.param("assets/runtime.png", "true", id="unknown asset"),
        pytest.param(
            "new\nfile.rs",
            "true",
            id="newline in path",
            marks=pytest.mark.skipif(
                os.name == "nt", reason="Windows forbids newlines in filenames"
            ),
        ),
    ],
)
def test_pull_request_paths(repository: Path, path: str, expected: str) -> None:
    changed = repository / path
    changed.parent.mkdir(parents=True, exist_ok=True)
    changed.write_text("change\n", encoding="utf-8")
    _merge(repository)
    result = _classify(repository=repository, event="pull_request")
    assert (result.returncode, result.stdout) == (0, f"{expected}\n")


def test_renaming_source_to_documentation_runs_rust(repository: Path) -> None:
    (repository / "src/main.rs").rename(repository / "README.md")
    _merge(repository)
    result = _classify(repository=repository, event="pull_request")
    assert (result.returncode, result.stdout) == (0, "true\n")


def test_deleting_source_runs_rust(repository: Path) -> None:
    (repository / "src/main.rs").unlink()
    _merge(repository)
    result = _classify(repository=repository, event="pull_request")
    assert (result.returncode, result.stdout) == (0, "true\n")


def test_missing_comparison_commit_fails(repository: Path) -> None:
    result = _classify(repository=repository, event="pull_request")
    assert result.returncode > 1
    assert result.stdout == ""


def test_push_always_runs_rust(repository: Path) -> None:
    result = _classify(repository=repository, event="push")
    assert (result.returncode, result.stdout) == (0, "true\n")


def test_shallow_merge_checkout(
    repository: Path, tmp_path_factory: pytest.TempPathFactory
) -> None:
    (repository / "README.md").write_text("guide\n", encoding="utf-8")
    _merge(repository)
    checkout = tmp_path_factory.mktemp("checkout")
    _git(checkout, ["clone", "--depth", "2", repository.as_uri(), "."])
    result = _classify(repository=checkout, event="pull_request")
    assert (result.returncode, result.stdout) == (0, "false\n")


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-q", "-W", "error"]))
