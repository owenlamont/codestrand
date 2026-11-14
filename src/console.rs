//! Windows gives a console program started without a terminal a console of its own;
//! `preview` releases that console so only the render window shows.

use std::io::Write as _;
use std::path::Path;

/// Name of the file, in the cache directory, that receives start-up failures once
/// the console is gone.
#[cfg(windows)]
const FAILURE_LOG: &str = "preview-error.log";

/// Whether a console with `attached` processes exists only for this process;
/// `0` is `GetConsoleProcessList` failing, as it does without a console.
const fn owned_alone(attached: u32) -> bool {
    attached == 1
}

/// Releases the console when no other process shares it, then sends panics to
/// [`FAILURE_LOG`] because standard error no longer reaches the user.
#[cfg(windows)]
pub(super) fn release_if_alone() {
    if release_lone_console()
        && let Some(log) = crate::cache::root().map(|root| root.join(FAILURE_LOG))
    {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic| {
            record(&log, &panic.to_string());
            previous(panic);
        }));
    }
}

#[cfg(windows)]
#[expect(unsafe_code, reason = "the console is only reachable through Win32")]
fn release_lone_console() -> bool {
    use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList};

    let mut process = 0;
    // SAFETY: `process` is a live buffer of the one element the call is told about.
    let attached = unsafe { GetConsoleProcessList(&raw mut process, 1) };
    // SAFETY: `FreeConsole` takes no arguments and only detaches this process.
    owned_alone(attached) && unsafe { FreeConsole() } != 0
}

/// Appends `failure` to `log`; an unwritable log has no remaining outlet to report to.
fn record(log: &Path, failure: &str) {
    if let Some(directory) = log.parent() {
        let _ = std::fs::create_dir_all(directory);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
    {
        let _ = writeln!(file, "{failure}");
    }
}

#[cfg(test)]
mod tests {
    use super::{owned_alone, record};

    #[test]
    fn only_a_console_with_no_other_process_is_released() {
        assert!(owned_alone(1));
        for shared_or_absent in [0, 2, 3] {
            assert!(!owned_alone(shared_or_absent));
        }
    }

    #[test]
    fn failures_accumulate_in_a_log_whose_directory_is_created() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("nested/preview-error.log");
        record(&log, "first");
        record(&log, "second");
        assert_eq!(std::fs::read_to_string(log).unwrap(), "first\nsecond\n");
    }
}
