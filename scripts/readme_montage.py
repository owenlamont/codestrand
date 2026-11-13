# /// script
# requires-python = ">=3.15"
# dependencies = ["tomli-w>=1.2", "typer>=0.26.7"]
# ///

import shutil
import subprocess
import tomllib
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Annotated, Final

import tomli_w
import typer

_CAMERAS: Final = Path(__file__).parent / "readme_montage"
_FADE_SECONDS: Final = 0.3
_PLAYBACK_FPS: Final = 30
_WEBP_FILTER: Final = "fps=10,scale=600:-2:flags=lanczos"
_PINS: Final = {
    "aiomoto": "bb9992bb84834c29832227591796ad69465d9200",
    "codestrand": "61c0d9d0dc5c219f2c3aa8abcbacb70e71c9b4d9",
    "pyglobegl": "73c174129b6da46e2877dad812aa925cff961e71",
    "ruff": "71479629f40c8583e3ffc517b30d90abf10c818f",
    "ryl": "dea81cea6ae58f82aff8012b9a3fda651a602ee2",
    "ty": "62e097f98b11d2638d8dc600a00b184f2775d5fc",
    "uv": "9b4fe9027f1682ed3e3c96d990ce9a9e1bb51b5d",
    "uv-secure": "f357db8c436f73c4c0bcb728a1ca7b05bc37d373",
    "vscode": "a9db03664df339f6b77b1c7f250b7c7e9f474c99",
}
_PERSONAL: Final = ("uv-secure", "ryl", "aiomoto", "pyglobegl", "codestrand")
_ASTRAL: Final = ("ruff", "uv", "ty")
_ASTRAL_PACING: Final = (
    "--seconds-per-day=0.15",
    "--max-idle-seconds=0.3",
    "--contributor-inactivity-days=14",
)


@dataclass(frozen=True, kw_only=True, slots=True)
class _Shot:
    name: str
    theme: str
    source: str
    options: Sequence[str]
    first_frame: int
    frames: int
    export_fps: int = _PLAYBACK_FPS
    crop: str = ""


_SHOTS: Final = (
    _Shot(
        name="single-repository",
        theme="bevelled-desktop",
        source="codestrand",
        options=(),
        first_frame=30,
        frames=78,
    ),
    _Shot(
        name="repositories-created",
        theme="blueprint",
        source="personal",
        options=("--seconds-per-day=0.05", "--max-idle-seconds=0.1"),
        first_frame=40,
        frames=96,
        export_fps=10,
    ),
    _Shot(
        name="repository-archived",
        theme="phosphor",
        source="personal",
        options=("--start=333",),
        first_frame=236,
        frames=108,
    ),
    _Shot(
        name="ty-created",
        theme="neon",
        source="astral",
        options=(*_ASTRAL_PACING, "--start=16800", "--commits=300"),
        first_frame=84,
        frames=108,
    ),
    _Shot(
        name="vscode-growth",
        theme="atlas",
        source="vscode",
        options=(
            "--seconds-per-day=0.02",
            "--max-idle-seconds=0.03",
            "--travel-cap=0",
            "--contributor-inactivity-days=14",
            "--start=1900",
            "--commits=1300",
            "--collapse-directories=off",
        ),
        first_frame=65,
        frames=96,
    ),
    _Shot(
        name="file-heat",
        theme="forge",
        source="ryl",
        options=("--start=262", f"--camera-path={_CAMERAS / 'forge-heat.toml'}"),
        first_frame=54,
        frames=78,
    ),
    _Shot(
        name="diff-text",
        theme="neon",
        source="ryl",
        options=("--start=262", f"--camera-path={_CAMERAS / 'diff-text.toml'}"),
        first_frame=60,
        frames=78,
        crop="crop=720:405:380:230,scale=1280:720:flags=lanczos,",
    ),
    _Shot(
        name="behind-avatar",
        theme="outrun",
        source="ryl",
        options=("--start=262", f"--camera-path={_CAMERAS / 'behind-avatar.toml'}"),
        first_frame=48,
        frames=180,
    ),
)

app = typer.Typer(
    add_completion=False, context_settings={"help_option_names": ["-h", "--help"]}
)


def _run(*, command: Sequence[str | Path], log: Path, timeout_seconds: int) -> None:
    typer.echo(f"Running {log.stem}; output: {log}")
    with log.open("wb") as stream:
        subprocess.run(
            command,
            check=True,
            timeout=timeout_seconds,
            stdin=subprocess.DEVNULL,
            stdout=stream,
            stderr=subprocess.STDOUT,
        )


def _verify_pins(*, git: str, repositories: Path) -> None:
    for name, pin in _PINS.items():
        head = subprocess.run(
            [git, "-C", repositories / name, "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
            timeout=30,
        ).stdout.strip()
        if head != pin:
            raise typer.BadParameter(f"{name} is at {head}; check out {pin}")


def _manifest(*, path: Path, repositories: Path, names: Sequence[str]) -> Path:
    document = {
        "repositories": [
            {"path": str(repositories / name)}
            | ({"archived": True} if name == "uv-secure" else {})
            for name in names
        ]
    }
    text = tomli_w.dumps(document)
    if tomllib.loads(text) != document:
        raise typer.BadParameter(f"Repository paths under {repositories} are not TOML")
    path.write_text(text, encoding="utf-8")
    return path


def _montage_filter() -> str:
    filters = [
        f"[{index}:v]{shot.crop}format=yuv420p,setpts=PTS-STARTPTS[shot{index}]"
        for index, shot in enumerate(_SHOTS)
    ]
    previous = "shot0"
    offset = 0.0
    for index in range(1, len(_SHOTS)):
        offset += _SHOTS[index - 1].frames / _PLAYBACK_FPS - _FADE_SECONDS
        filters.append(
            f"[{previous}][shot{index}]xfade=transition=fade"
            f":duration={_FADE_SECONDS}:offset={offset:.3f}[fade{index}]"
        )
        previous = f"fade{index}"
    filters.append(f"[{previous}]{_WEBP_FILTER}[montage]")
    return ";".join(filters)


@app.command()
def main(
    *,
    binary: Annotated[Path, typer.Argument(help="Built Codestrand executable.")],
    repositories: Annotated[
        Path, typer.Argument(help="Directory holding the pinned clones.")
    ],
    output: Annotated[
        Path, typer.Argument(help="New output directory; must not exist.")
    ],
    fetch_avatars: Annotated[
        bool, typer.Option(help="Fetch portraits before each export.")
    ] = False,
) -> None:
    """Render the README montage to OUTPUT/montage.webp; needs FFmpeg with libwebp.

    REPOSITORIES holds complete clones named after each pin in this script, checked
    out at that commit: owenlamont/{uv-secure,ryl,aiomoto,pyglobegl,codestrand},
    astral-sh/{ruff,uv,ty} and microsoft/vscode. Every export uses the contributor
    manifest beside the camera paths. Portraits come from the avatar cache unless
    --fetch-avatars is passed. Frames stay in OUTPUT for inspection.
    """
    binary = binary.resolve(strict=True)
    repositories = repositories.resolve(strict=True)
    ffmpeg = shutil.which("ffmpeg")
    git = shutil.which("git")
    if ffmpeg is None or git is None:
        raise typer.BadParameter("FFmpeg and Git must be available on PATH")
    _verify_pins(git=git, repositories=repositories)
    output = output.resolve()
    output.mkdir()
    sources = {
        "personal": [
            "--repositories-file",
            _manifest(
                path=output / "personal.toml",
                repositories=repositories,
                names=_PERSONAL,
            ),
        ],
        "astral": [
            "--repositories-file",
            _manifest(
                path=output / "astral.toml", repositories=repositories, names=_ASTRAL
            ),
        ],
        **{
            name: ["--repository", repositories / name]
            for name in ("codestrand", "ryl", "vscode")
        },
    }
    shared: list[str | Path] = ["--contributors", _CAMERAS / "contributors.toml"]
    if fetch_avatars:
        shared.append("--fetch-avatars")
    inputs: list[str | Path] = []
    for shot in _SHOTS:
        frames = output / shot.name
        _run(
            command=[
                binary,
                "export",
                frames,
                *sources[shot.source],
                *shared,
                *shot.options,
                f"--theme={shot.theme}",
                f"--fps={shot.export_fps}",
                f"--frames={shot.first_frame + shot.frames}",
            ],
            log=output / f"{shot.name}.log",
            timeout_seconds=1800,
        )
        inputs += [
            "-framerate",
            str(_PLAYBACK_FPS),
            "-start_number",
            str(shot.first_frame),
            "-i",
            frames / "frame-%05d.png",
        ]
    _run(
        command=[
            ffmpeg,
            "-hide_banner",
            "-nostdin",
            *inputs,
            "-filter_complex",
            _montage_filter(),
            "-map",
            "[montage]",
            "-c:v",
            "libwebp",
            "-q:v",
            "62",
            "-compression_level",
            "6",
            "-loop",
            "0",
            output / "montage.webp",
        ],
        log=output / "montage.log",
        timeout_seconds=1800,
    )
    typer.echo(f"Montage: {output / 'montage.webp'}")


if __name__ == "__main__":
    app()
