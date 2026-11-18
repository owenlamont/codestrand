# /// script
# requires-python = ">=3.15"
# dependencies = ["pytest>=9", "pyyaml>=6"]
# ///

import json
import os
import shutil
import subprocess
import sys
import tomllib
import zipfile
from pathlib import Path
from typing import Final

import pytest
import yaml

_ROOT: Final = Path(__file__).resolve().parents[1]
_JOBS: Final = yaml.safe_load((_ROOT / ".github/workflows/release.yml").read_text())[
    "jobs"
]


@pytest.mark.parametrize(
    ("response", "status", "success"),
    [
        pytest.param("true", 0, True, id="draft"),
        pytest.param("false", 0, False, id="published"),
        pytest.param("release not found", 1, True, id="missing"),
        pytest.param("HTTP 403 forbidden", 1, False, id="api error"),
    ],
)
def test_preflight_release_guard(
    tmp_path: Path, response: str, status: int, *, success: bool
) -> None:
    bash = shutil.which("bash")
    assert bash is not None
    step = _JOBS["preflight-release-guard"]["steps"][0]
    stub = 'gh() { printf "%s\\n" "$RESPONSE" >&2; return "$STATUS"; }\n'
    if status == 0:
        stub = 'gh() { printf "%s\\n" "$RESPONSE"; return "$STATUS"; }\n'
    result = subprocess.run(
        [bash, "-c", stub + step["run"]],
        cwd=tmp_path,
        env=os.environ
        | {
            "RESPONSE": response,
            "STATUS": str(status),
            "RELEASE_TAG": "v0.1.1",
            "GITHUB_REPOSITORY": "owenlamont/codestrand",
        },
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    assert (result.returncode == 0) is success, result.stdout + result.stderr


@pytest.mark.parametrize(
    "target", ["x86_64-unknown-linux-gnu", "aarch64-pc-windows-msvc"]
)
def test_binary_zip_compresses_and_preserves_executable_mode(
    tmp_path: Path, target: str
) -> None:
    executable = "codestrand.exe" if "windows" in target else "codestrand"
    (tmp_path / "dist").mkdir()
    data = b"synthetic executable" * 1000
    with zipfile.ZipFile(tmp_path / "dist/test.whl", "w") as wheel:
        wheel.writestr(f"codestrand.data/scripts/{executable}", data)
    for name in (
        "LICENSE",
        "LICENSE-APACHE",
        "assets/material/LICENSE.txt",
        "THIRDPARTY.txt",
    ):
        path = tmp_path / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("License fixture\n", newline="\n")
    (step,) = (
        step
        for step in _JOBS["wheels"]["steps"]
        if step.get("name") == "Package standalone binary from wheel"
    )
    source = step["run"].removeprefix("python - <<'PY'\n").removesuffix("PY\n")
    script = tmp_path / "package.py"
    script.write_text(source)
    subprocess.run(
        [sys.executable, str(script)],
        cwd=tmp_path,
        env=os.environ | {"TARGET": target},
        check=True,
        capture_output=True,
        timeout=30,
    )
    with zipfile.ZipFile(tmp_path / f"binaries/codestrand-{target}.zip") as archive:
        member = archive.getinfo(executable)
        assert member.compress_type == zipfile.ZIP_DEFLATED
        assert member.compress_size < member.file_size
        assert member.external_attr >> 16 == 0o100755
        assert archive.read(executable) == data
        assert archive.read("THIRDPARTY.txt") == b"License fixture\n"


def test_checksums_cover_every_asset_with_bare_filenames(tmp_path: Path) -> None:
    bash = shutil.which("bash")
    assert bash is not None
    names = [
        "THIRDPARTY.txt",
        "codestrand-0.1.1.tar.gz",
        "codestrand-linux.zip",
        "codestrand.whl",
    ]
    for name in names:
        (tmp_path / name).write_bytes(name.encode())
    (step,) = (
        step
        for step in _JOBS["release-assets"]["steps"]
        if step.get("name") == "Generate SHA256SUMS"
    )
    subprocess.run(
        [bash, "-c", step["run"]],
        cwd=tmp_path,
        check=True,
        capture_output=True,
        timeout=30,
    )
    lines = (tmp_path / "SHA256SUMS").read_text().splitlines()
    assert [line.split(maxsplit=1)[1].removeprefix("*") for line in lines] == names
    (tmp_path / names[0]).write_bytes(b"tampered")
    result = subprocess.run(
        [bash, "-c", "sha256sum --check SHA256SUMS"],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        timeout=30,
    )
    assert result.returncode != 0


def test_smoke_matrix_covers_all_wheels_natively() -> None:
    builds = _JOBS["wheels"]["strategy"]["matrix"]["include"]
    smoke = _JOBS["smoke-wheels"]["strategy"]["matrix"]["include"]
    assert {entry["target"] for entry in builds} == {entry["target"] for entry in smoke}
    (arm,) = (entry for entry in smoke if entry["target"] == "aarch64-pc-windows-msvc")
    assert arm["os"] == "windows-11-arm"
    rendered = {entry["target"] for entry in smoke if entry["render"]}
    targets = {entry["target"] for entry in builds}
    assert rendered == targets - {"aarch64-pc-windows-msvc"}


@pytest.mark.parametrize(
    ("second_frame", "success"),
    [
        pytest.param("present", True, id="two nonempty frames"),
        pytest.param("missing", False, id="second frame missing"),
        pytest.param("empty", False, id="second frame empty"),
    ],
)
def test_wheel_render_requires_both_frames(
    tmp_path: Path, second_frame: str, *, success: bool
) -> None:
    bash = shutil.which("bash")
    assert bash is not None
    (step,) = (
        step
        for step in _JOBS["smoke-wheels"]["steps"]
        if step.get("name") == "Render installed wheel"
    )
    assert step["if"] == "matrix.render"
    stub = r"""
uv() {
  mkdir smoke-render
  printf png > smoke-render/frame-00000.png
  case "$SECOND_FRAME" in
    present) printf png > smoke-render/frame-00001.png ;;
    empty) touch smoke-render/frame-00001.png ;;
  esac
}
"""
    result = subprocess.run(
        [bash, "-c", stub + step["run"]],
        cwd=tmp_path,
        env=os.environ | {"RUNNER_OS": "macOS", "SECOND_FRAME": second_frame},
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    assert (result.returncode == 0) is success, result.stdout + result.stderr


def test_publication_requires_smoke_and_preflight_but_dry_builds_do_not() -> None:
    for name in (
        "draft-github-release",
        "publish-crates",
        "publish-pypi",
        "stage_npm_packages",
        "publish_npm_platforms",
        "publish_npm_meta",
    ):
        assert {"smoke-wheels", "preflight-release-guard"} <= set(_JOBS[name]["needs"])
    pending = ["release-assets"]
    while pending:
        name = pending.pop()
        job = _JOBS[name]
        assert "if" not in job
        needs = job.get("needs", [])
        pending.extend([needs] if isinstance(needs, str) else needs)


@pytest.mark.parametrize(
    "name",
    [
        name
        for name, job in _JOBS.items()
        if any(
            job.get("permissions", {}).get(permission) == "write"
            for permission in ("id-token", "contents")
        )
    ],
)
def test_write_permissions_require_publication_guard(name: str) -> None:
    assert _JOBS[name].get("if") == (
        "startsWith(github.ref, 'refs/tags/v') && "
        "(github.event_name == 'push' || "
        "(github.event_name == 'workflow_dispatch' && inputs.publish == true))"
    )


def test_winget_waits_for_public_release_and_uses_separate_credentials() -> None:
    job = _JOBS["publish-winget"]
    assert job["needs"] == ["finalize-github-release"]
    assert job["if"] == _JOBS["finalize-github-release"]["if"]
    assert job["environment"] == "automation"
    assert job["permissions"] == {"contents": "read"}
    download, sync, submit = job["steps"]
    assert set(download["env"]) == {"WINGETCREATE_VERSION", "WINGETCREATE_SHA256"}
    assert sync["env"]["GH_TOKEN"] == "${{ secrets.WINGET_FORK_SYNC_PAT }}"
    assert submit["env"] == {"WINGET_CREATE_GITHUB_TOKEN": "${{ secrets.WINGET_PAT }}"}
    assert "update owenlamont.codestrand" in submit["run"]
    assert "codestrand-x86_64-pc-windows-msvc.zip|x64" in submit["run"]
    assert "codestrand-aarch64-pc-windows-msvc.zip|arm64" in submit["run"]


@pytest.fixture
def powershell() -> str:
    executable = shutil.which("pwsh")
    if executable is None:
        assert not os.environ.get("CI"), "CI must execute the PowerShell tests"
        pytest.skip("PowerShell is not installed")
    return executable


@pytest.mark.parametrize(
    ("actual_hash", "success"),
    [
        pytest.param("reviewed", True, id="verified download"),
        pytest.param("tampered", False, id="checksum mismatch"),
    ],
)
def test_winget_rejects_unverified_download(
    *, tmp_path: Path, powershell: str, actual_hash: str, success: bool
) -> None:
    step = _JOBS["publish-winget"]["steps"][0]
    stub = """
function Invoke-WebRequest { param($Uri, $OutFile) }
function Get-FileHash { param($Path, $Algorithm)
  return @{ Hash = $env:ACTUAL_HASH }
}
"""
    result = subprocess.run(
        [powershell, "-NoProfile", "-NonInteractive", "-Command", stub + step["run"]],
        cwd=tmp_path,
        env=os.environ
        | {
            "ACTUAL_HASH": actual_hash,
            "WINGETCREATE_SHA256": "reviewed",
            "WINGETCREATE_VERSION": "v1.12.13.0",
        },
        capture_output=True,
        timeout=30,
        check=False,
    )
    assert (result.returncode == 0) is success


@pytest.mark.parametrize(
    "status",
    [pytest.param(0, id="fork synced"), pytest.param(1, id="fork sync refused")],
)
def test_winget_fork_sync_failure_stops_job(
    *, tmp_path: Path, powershell: str, status: int
) -> None:
    step = _JOBS["publish-winget"]["steps"][1]
    stub = "function gh { $global:LASTEXITCODE = [int]$env:STATUS }\n"
    result = subprocess.run(
        [powershell, "-NoProfile", "-NonInteractive", "-Command", stub + step["run"]],
        cwd=tmp_path,
        env=os.environ | {"STATUS": str(status), "WINGET_FORK": "owner/winget-pkgs"},
        capture_output=True,
        timeout=30,
        check=False,
    )
    assert result.returncode == status


def test_notices_ship_in_each_distribution() -> None:
    cargo = tomllib.loads((_ROOT / "Cargo.toml").read_text())
    python = tomllib.loads((_ROOT / "pyproject.toml").read_text())
    npm = json.loads((_ROOT / "package.json").read_text())
    assert "/THIRDPARTY.txt" in cargo["package"]["include"]
    assert "THIRDPARTY.txt" in python["project"]["license-files"]
    assert "THIRDPARTY.txt" in npm["files"]
    verify = next(
        step
        for step in _JOBS["version-check"]["steps"]
        if step.get("name") == "Verify third-party notices"
    )
    assert verify["run"] == "uv run --script scripts/licenses.py --check"
    hooks = tomllib.loads((_ROOT / "prek.toml").read_text())
    assert all(
        "scripts/licenses.py" not in hook.get("entry", "")
        for repo in hooks["repos"]
        for hook in repo["hooks"]
    )
    stage = next(
        step
        for step in _JOBS["release-assets"]["steps"]
        if step.get("name") == "Stage third-party notices"
    )
    assert stage["run"] == "cp source/THIRDPARTY.txt release-assets/THIRDPARTY.txt"


def test_embedded_font_notice_matches_locked_bevy() -> None:
    lock = tomllib.loads((_ROOT / "Cargo.lock").read_text())
    bevy_text = next(p for p in lock["package"] if p["name"] == "bevy_text")
    template = (_ROOT / "about.hbs").read_text()
    assert f"Bevy bevy_text {bevy_text['version']} default font" in template
    assert (
        f"https://github.com/bevyengine/bevy/blob/v{bevy_text['version']}/"
        "assets/fonts/FiraMono-LICENSE"
    ) in template
    assert "SIL OPEN FONT LICENSE Version 1.1" in template


def test_notices_contain_no_html_entities() -> None:
    notices = (_ROOT / "THIRDPARTY.txt").read_text()
    assert all(entity not in notices for entity in ("&amp;", "&quot;", "&#"))


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-q"]))
