//! Handing things to the operating system: a URL or file to its default application, a
//! file to Finder.
//!
//! Test builds never start those programs, which would open windows on the machine running
//! the tests: each request is written to a list kept per thread, which
//! [`take_requests`] hands to the test.

use std::path::Path;

/// Open a URL or a file with what the system uses for it (`open` on macOS, `xdg-open`
/// elsewhere). Does not wait; a failure to start is ignored, as the system shows nothing
/// useful for it either.
pub fn open(target: &str) {
    run(OsRequest::Open(target.to_string()));
}

/// Show a file selected in the file manager (Finder's `open -R`). Elsewhere its folder is
/// opened.
pub fn reveal(path: &Path) {
    run(OsRequest::Reveal(path.to_string_lossy().into_owned()));
}

/// What was asked of the operating system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OsRequest {
    Open(String),
    Reveal(String),
}

#[cfg(not(test))]
fn run(request: OsRequest) {
    use std::process::{Command, Stdio};
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let mut command = Command::new(opener);
    match &request {
        OsRequest::Open(target) => {
            command.arg(target);
        }
        OsRequest::Reveal(path) if cfg!(target_os = "macos") => {
            command.arg("-R").arg(path);
        }
        OsRequest::Reveal(path) => {
            let folder = Path::new(path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
            command.arg(folder);
        }
    }
    let _ = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

#[cfg(test)]
thread_local! {
    static REQUESTS: std::cell::RefCell<Vec<OsRequest>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn run(request: OsRequest) {
    REQUESTS.with(|requests| requests.borrow_mut().push(request));
}

/// What this thread asked of the operating system so far, taken out. Test builds only.
#[cfg(test)]
pub fn take_requests() -> Vec<OsRequest> {
    REQUESTS.with(|requests| std::mem::take(&mut *requests.borrow_mut()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tests_record_what_would_have_been_opened() {
        open("https://example.com");
        reveal(Path::new("/tmp/a.txt"));
        assert_eq!(take_requests(), vec![OsRequest::Open("https://example.com".into()), OsRequest::Reveal("/tmp/a.txt".into())]);
        assert!(take_requests().is_empty());
    }
}
