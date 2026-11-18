//! The one reader for the TOML files a user names: the `--config` file, the
//! repositories and contributors files and the camera path.

use std::{
    fs::{self, File, Metadata},
    io::{self, Read},
    path::Path,
};

const MAX_BYTES: u64 = 1024 * 1024;

/// Reads a regular file of at most 1 MiB, following symlinks.
///
/// Anything else is refused with `InvalidData` before any read can block on it;
/// `NotFound` passes through unchanged for callers with an optional default path.
pub(super) fn read(path: &Path) -> io::Result<String> {
    check(&fs::metadata(path)?)?;
    let file = open(path)?;
    // The path may have been replaced since it was checked.
    check(&file.metadata()?)?;
    let mut text = String::new();
    file.take(MAX_BYTES + 1).read_to_string(&mut text)?;
    if text.len() as u64 > MAX_BYTES {
        return Err(refuse("exceeds 1 MiB"));
    }
    Ok(text)
}

fn check(metadata: &Metadata) -> io::Result<()> {
    if !metadata.is_file() {
        return Err(refuse("is not a regular file"));
    }
    if metadata.len() > MAX_BYTES {
        return Err(refuse("exceeds 1 MiB"));
    }
    Ok(())
}

/// Opens for reading without waiting for a writer, should a FIFO have replaced
/// the checked file; the flag does not change reads of a regular file.
fn open(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NONBLOCK);
    options.open(path)
}

fn refuse(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let error = read(root.path()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "is not a regular file");
    }

    #[test]
    fn missing_file_stays_not_found() {
        let root = tempfile::tempdir().unwrap();
        let error = read(&root.path().join("absent.toml")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_a_regular_file_is_read() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target.toml");
        fs::write(&target, "fps = 30\n").unwrap();
        let link = root.path().join("link.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(read(&link).unwrap(), "fps = 30\n");
    }

    /// Makes a FIFO with no writer, which a blocking open would wait on forever.
    #[cfg(unix)]
    fn fifo(root: &Path) -> std::path::PathBuf {
        let path = root.join("fifo.toml");
        let status = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        path
    }

    /// Runs `call` on a thread so a blocking open fails the test instead of hanging it.
    #[cfg(unix)]
    fn within_five_seconds<T: Send + 'static>(
        call: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || sender.send(call()).unwrap());
        receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the call returns without waiting for a writer")
    }

    #[cfg(unix)]
    #[test]
    fn fifo_is_refused_without_blocking() {
        let root = tempfile::tempdir().unwrap();
        let path = fifo(root.path());
        let error = within_five_seconds(move || read(&path)).unwrap_err();
        assert_eq!(error.to_string(), "is not a regular file");
    }

    #[cfg(unix)]
    #[test]
    fn fifo_swapped_in_after_the_check_does_not_block_the_open() {
        let root = tempfile::tempdir().unwrap();
        let path = fifo(root.path());
        let file = within_five_seconds(move || open(&path)).unwrap();
        assert_eq!(
            check(&file.metadata().unwrap()).unwrap_err().to_string(),
            "is not a regular file"
        );
    }
}
