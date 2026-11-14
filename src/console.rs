//! Windows gives a console program started without a terminal a console of its own;
//! `preview` releases that console so only the render window shows.

use std::{fs::File, io::Write as _, path::Path};

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
    let log = || open(&crate::cache::root()?.join(FAILURE_LOG));
    if let Some(log) = release_lone_console(log) {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic| {
            record(&log, &panic.to_string());
            previous(panic);
        }));
    }
}

/// Detaches from a console nothing else shares, but only once `outlet` has produced
/// somewhere else for failures to go, and returns that outlet.
#[cfg(windows)]
#[expect(unsafe_code, reason = "the console is only reachable through Win32")]
fn release_lone_console<T>(outlet: impl FnOnce() -> Option<T>) -> Option<T> {
    use windows_sys::Win32::System::Console::{
        FreeConsole, GetConsoleMode, GetConsoleProcessList, GetStdHandle,
        STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
    };

    let mut process = 0;
    // SAFETY: `process` is a live buffer of the one element the call is told about.
    let attached = unsafe { GetConsoleProcessList(&raw mut process, 1) };
    let outlet = owned_alone(attached).then(outlet).flatten()?;
    let on_console =
        [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].map(|stream| {
            let mut mode = 0;
            // SAFETY: both calls accept any handle value and `mode` is a live `u32`.
            unsafe { GetConsoleMode(GetStdHandle(stream), &raw mut mode) != 0 }
                .then_some(stream)
        });
    // SAFETY: `FreeConsole` takes no arguments and only detaches this process.
    if unsafe { FreeConsole() } == 0 {
        return None;
    }
    for stream in on_console.into_iter().flatten() {
        // SAFETY: a null handle is valid here; `FreeConsole` closed the old one but left
        // its value behind, where a file opened later could reuse it and receive the output.
        unsafe { SetStdHandle(stream, std::ptr::null_mut()) };
    }
    Some(outlet)
}

/// Opens `log` for appending without following a link out of its directory.
fn open(log: &Path) -> Option<File> {
    crate::cache::ConfinedFile::at(log, true)
        .ok()?
        .append()
        .ok()
}

fn record(mut log: &File, failure: &str) {
    let _ = log.write_all(format!("{failure}\n").as_bytes());
}

#[cfg(test)]
mod tests {
    use super::{open, owned_alone, record};

    #[test]
    fn only_a_console_with_no_other_process_is_released() {
        assert!(owned_alone(1));
        for shared_or_absent in [0, 2, 3] {
            assert!(!owned_alone(shared_or_absent));
        }
    }

    #[test]
    fn failures_accumulate_in_a_log_whose_directory_is_created() {
        let directory = crate::cache::test_directory().unwrap();
        let log = directory.path().join("nested/preview-error.log");
        record(&open(&log).unwrap(), "first");
        record(&open(&log).unwrap(), "second");
        assert_eq!(std::fs::read_to_string(log).unwrap(), "first\nsecond\n");
    }

    #[test]
    fn a_linked_directory_or_a_directory_at_the_log_path_gives_no_outlet() {
        let directory = crate::cache::test_directory().unwrap();
        let outside = crate::cache::test_directory().unwrap();
        let linked = directory.path().join("linked");
        crate::cache::tests::directory_link(outside.path(), &linked);
        assert!(open(&linked.join("preview-error.log")).is_none());
        assert!(open(directory.path()).is_none());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
