use std::{
    ffi::OsString,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};

const TIMEOUT: Duration = Duration::from_secs(30);
const STDERR_LIMIT: usize = 65_536;
const INSTALL: &str = "install FFmpeg (Windows: winget install Gyan.FFmpeg; macOS: brew install ffmpeg; Linux: your distribution's ffmpeg package), or set --ffmpeg / CODESTRAND_FFMPEG; PNG export still works without --video";

#[derive(clap::Args, Default)]
pub(super) struct Options {
    /// Stream video instead of PNGs; the directory still receives frames.csv.
    #[arg(long)]
    video: Option<PathBuf>,
    /// Installed `FFmpeg` executable; overrides `CODESTRAND_FFMPEG` and PATH.
    #[arg(long, requires = "video")]
    ffmpeg: Option<PathBuf>,
    /// Replace default encoding options; repeat once per argument, e.g. --ffmpeg-arg=-crf.
    #[arg(long, requires = "video", allow_hyphen_values = true)]
    ffmpeg_arg: Vec<OsString>,
}

fn executable(explicit: Option<&Path>, environment: Option<OsString>) -> PathBuf {
    explicit.map_or_else(
        || environment.map_or_else(|| PathBuf::from("ffmpeg"), PathBuf::from),
        Path::to_path_buf,
    )
}

impl Options {
    pub(super) fn start(
        &self,
        fps: u16,
        resolution: crate::resolution::Resolution,
    ) -> anyhow::Result<Option<Encoder>> {
        let Some(output) = &self.video else {
            return Ok(None);
        };
        if output.exists() {
            bail!(
                "video output already exists: {} (choose a new filename)",
                output.display()
            );
        }
        let path = executable(
            self.ffmpeg.as_deref(),
            std::env::var_os("CODESTRAND_FFMPEG"),
        );
        let mut version = command(&path);
        version.arg("-version");
        Encoder::spawn(&mut version)
            .and_then(|mut process| process.finish())
            .with_context(|| {
                format!("cannot run FFmpeg at {}; {INSTALL}", path.display())
            })?;
        if self.ffmpeg_arg.is_empty() {
            let mut probe = command(&path);
            probe.args(input_args(30, "2x2"));
            probe.args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-f", "null", "-"]);
            let mut encoder = Encoder::spawn(&mut probe)?;
            let result = encoder.write(vec![0; 12]).and_then(|()| encoder.finish());
            result.context("FFmpeg could not encode with libx264; install a libx264-enabled build or select another encoder with repeated --ffmpeg-arg options")?;
        }
        let mut encode = command(&path);
        encode.args(input_args(fps, &resolution.to_string()));
        encode.args(output_args(&self.ffmpeg_arg));
        encode.arg(output);
        Encoder::spawn(&mut encode).map(Some)
    }
}

fn command(path: &Path) -> Command {
    #[cfg_attr(
        not(windows),
        expect(unused_mut, reason = "only Windows sets creation flags")
    )]
    let mut command = Command::new(path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

fn input_args(fps: u16, size: &str) -> Vec<OsString> {
    [
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-n",
        "-f",
        "rawvideo",
        "-pixel_format",
        "rgb24",
        "-video_size",
        size,
        "-framerate",
        &fps.to_string(),
        "-i",
        "pipe:0",
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

fn output_args(custom: &[OsString]) -> Vec<OsString> {
    if custom.is_empty() {
        [
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-crf",
            "18",
            "-movflags",
            "+faststart",
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    } else {
        custom.to_vec()
    }
}

pub(super) struct Encoder {
    child: Child,
    input: Option<Sender<Vec<u8>>>,
    written: Receiver<std::io::Result<()>>,
    stderr: Receiver<std::io::Result<Vec<u8>>>,
}

impl Encoder {
    fn spawn(command: &mut Command) -> anyhow::Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("start FFmpeg subprocess")?;
        let mut stdin = child.stdin.take().context("open FFmpeg stdin")?;
        let mut stderr = child.stderr.take().context("open FFmpeg stderr")?;
        let (input, frames) = mpsc::channel::<Vec<u8>>();
        let (ack, written) = mpsc::channel();
        thread::spawn(move || {
            for frame in frames {
                let result = stdin.write_all(&frame);
                let failed = result.is_err();
                if ack.send(result).is_err() || failed {
                    break;
                }
            }
        });
        let (errors, diagnostics) = mpsc::channel();
        thread::spawn(move || {
            let _ = errors.send(read_tail(&mut stderr));
        });
        Ok(Self {
            child,
            input: Some(input),
            written,
            stderr: diagnostics,
        })
    }

    pub(super) fn write(&mut self, frame: Vec<u8>) -> anyhow::Result<()> {
        let delivered = self
            .input
            .as_ref()
            .context("FFmpeg input is closed")?
            .send(frame);
        let result = match delivered {
            Ok(()) => self
                .written
                .recv_timeout(TIMEOUT)
                .context("FFmpeg frame delivery timed out or disconnected")
                .and_then(|result| result.context("write frame to FFmpeg")),
            Err(error) => Err(anyhow::anyhow!(error)),
        };
        if let Err(error) = result {
            let finish = self.finish();
            bail!(
                "{error:#}; {}",
                finish.err().map_or_else(
                    || "FFmpeg closed its input early".to_owned(),
                    |error| format!("{error:#}")
                )
            );
        }
        Ok(())
    }

    pub(super) fn finish(&mut self) -> anyhow::Result<()> {
        self.input.take();
        let status = wait(&mut self.child, TIMEOUT);
        if status.is_err() {
            let _ = self.child.kill();
            let _ = wait(&mut self.child, Duration::from_secs(2));
        }
        let stderr = self
            .stderr
            .recv_timeout(Duration::from_secs(2))
            .context("FFmpeg stderr did not close")??;
        let diagnostic = String::from_utf8_lossy(&stderr);
        let status =
            status.with_context(|| format!("FFmpeg did not finish: {diagnostic}"))?;
        if !status.success() {
            bail!("FFmpeg exited with {status}: {diagnostic}");
        }
        Ok(())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.input.take();
        let _ = self.child.kill();
        let _ = wait(&mut self.child, Duration::from_secs(2));
    }
}

fn wait(child: &mut Child, timeout: Duration) -> anyhow::Result<ExitStatus> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if start.elapsed() >= timeout {
            bail!("timed out waiting for FFmpeg after {timeout:?}");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn read_tail(reader: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut tail = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(tail);
        }
        tail.extend_from_slice(&buffer[..count]);
        if tail.len() > STDERR_LIMIT {
            tail.drain(..tail.len() - STDERR_LIMIT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_precedence_and_encoding_arguments() {
        assert_eq!(
            executable(Some(Path::new("explicit")), Some("environment".into())),
            PathBuf::from("explicit")
        );
        assert_eq!(
            executable(None, Some("environment".into())),
            PathBuf::from("environment")
        );
        assert_eq!(executable(None, None), PathBuf::from("ffmpeg"));
        let input = input_args(24, "1280x720");
        assert!(input.windows(2).any(|pair| pair == ["-framerate", "24"]));
        assert!(
            input
                .windows(2)
                .any(|pair| pair == ["-pixel_format", "rgb24"])
        );
        assert_eq!(input.last().unwrap(), "pipe:0");
        let defaults = output_args(&[]);
        assert!(defaults.windows(2).any(|pair| pair == ["-c:v", "libx264"]));
        assert!(
            defaults
                .windows(2)
                .any(|pair| pair == ["-pix_fmt", "yuv420p"])
        );
        let custom = vec!["-c:v".into(), "ffv1".into()];
        assert_eq!(output_args(&custom), custom);
    }

    #[test]
    fn stderr_tail_is_bounded() {
        let mut bytes = vec![b'x'; STDERR_LIMIT * 3];
        bytes.extend_from_slice(b"final diagnostic");
        let tail = read_tail(&mut bytes.as_slice()).unwrap();
        assert_eq!(tail.len(), STDERR_LIMIT);
        assert!(tail.ends_with(b"final diagnostic"));
    }

    fn fake_ffmpeg() -> (tempfile::TempDir, PathBuf) {
        let scratch = tempfile::tempdir().unwrap();
        let source = scratch.path().join("fake.rs");
        let binary = scratch
            .path()
            .join(format!("fake{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(
            &source,
            r#"
use std::io::{Read, Write};
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "-version") { return; }
    if std::env::current_exe().unwrap().file_stem().unwrap() == "no-x264" {
        eprintln!("Unknown encoder 'libx264'");
        std::process::exit(1);
    }
    if args.iter().any(|arg| arg == "fail") {
        let mut stderr = std::io::stderr().lock();
        stderr.write_all(&vec![b'x'; 200_000]).unwrap();
        writeln!(stderr, "deliberate encoder failure").unwrap();
        std::process::exit(7);
    }
    if args.iter().any(|arg| arg == "stall") {
        std::thread::sleep(std::time::Duration::from_secs(60));
        return;
    }
    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes).unwrap();
    if let Some(path) = args.last().filter(|path| path.as_str() != "-") {
        std::fs::write(path, bytes).unwrap();
    }
}
"#,
        )
        .unwrap();
        let status = Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .status()
            .unwrap();
        assert!(status.success());
        (scratch, binary)
    }

    #[test]
    fn fake_ffmpeg_handles_streaming_failures_and_discovery() {
        let (scratch, binary) = fake_ffmpeg();
        let output = scratch.path().join("video with spaces.mp4");
        let options = Options {
            video: Some(output.clone()),
            ffmpeg: Some(binary.clone()),
            ffmpeg_arg: vec!["custom".into()],
        };
        let mut encoder = options
            .start(30, crate::resolution::Resolution::default())
            .unwrap()
            .unwrap();
        encoder.write(vec![1, 2, 3, 4]).unwrap();
        encoder.finish().unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), [1, 2, 3, 4]);
        assert!(
            options
                .start(30, crate::resolution::Resolution::default())
                .err()
                .unwrap()
                .to_string()
                .contains("already exists")
        );
        let mut failing = command(&binary);
        failing.arg("fail");
        let mut encoder = Encoder::spawn(&mut failing).unwrap();
        let error = encoder.write(vec![0; 2_000_000]).unwrap_err();
        assert!(format!("{error:#}").contains("deliberate encoder failure"));
        let mut failing = command(&binary);
        failing.arg("fail");
        let error = Encoder::spawn(&mut failing).unwrap().finish().unwrap_err();
        assert!(format!("{error:#}").contains("deliberate encoder failure"));
        let mut stalled = command(&binary);
        stalled.arg("stall");
        let mut encoder = Encoder::spawn(&mut stalled).unwrap();
        assert!(wait(&mut encoder.child, Duration::from_millis(20)).is_err());
        drop(encoder);
        let options = Options {
            video: Some(scratch.path().join("missing.mp4")),
            ffmpeg: Some(scratch.path().join("missing")),
            ffmpeg_arg: Vec::new(),
        };
        let error = options
            .start(30, crate::resolution::Resolution::default())
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("PNG export still works"));
        assert!(
            Options::default()
                .start(30, crate::resolution::Resolution::default())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn default_encoder_is_checked_before_rendering() {
        let (scratch, binary) = fake_ffmpeg();
        let mut options = Options {
            video: Some(scratch.path().join("default.mp4")),
            ffmpeg: Some(binary.clone()),
            ffmpeg_arg: Vec::new(),
        };
        let mut encoder = options
            .start(30, crate::resolution::Resolution::default())
            .unwrap()
            .unwrap();
        encoder.write(vec![5; 12]).unwrap();
        encoder.finish().unwrap();
        let no_x264 = scratch
            .path()
            .join(format!("no-x264{}", std::env::consts::EXE_SUFFIX));
        std::fs::copy(binary, &no_x264).unwrap();
        let output = scratch.path().join("unavailable.mp4");
        options.video = Some(output.clone());
        options.ffmpeg = Some(no_x264);
        let error = options
            .start(30, crate::resolution::Resolution::default())
            .err()
            .unwrap();
        let message = format!("{error:#}");
        assert!(message.contains("Unknown encoder 'libx264'"));
        assert!(message.contains("--ffmpeg-arg"));
        assert!(!output.exists());
    }
}
