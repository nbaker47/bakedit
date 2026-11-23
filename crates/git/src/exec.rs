//! Runs the `git` command line.
//!
//! Every git process this crate starts goes through [`Git`]. It fixes the environment so
//! the output is machine readable and nothing can ask a question on the terminal, captures
//! both output streams, and enforces an optional time limit.

use std::ffi::{OsStr, OsString};
use std::io::{self, PipeReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::{GitError, Result};

/// Variables that point git at one particular repository, index or object store. They are
/// removed so that `-C <root>` alone decides what a command works on, also when the IDE
/// itself was started by git (as its editor, or from a hook).
const REPOSITORY_ENV: [&str; 10] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_GRAFT_FILE",
];

/// How long a process that ran out of time gets to clean up after the polite signal
/// before it is killed outright.
const TERMINATE_GRACE: Duration = Duration::from_secs(2);

/// How long to keep reading output after git has exited. The pipes normally close with
/// git; they only stay open when something git started (a hook's background job) still
/// holds them, and that must not keep the caller waiting.
const PIPE_GRACE: Duration = Duration::from_secs(2);

/// The longest pause between two checks on a process that has a time limit.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// What a finished git process left behind.
#[derive(Debug)]
pub(crate) struct Output {
    /// The exit code; `None` when a signal ended the process.
    pub code: Option<i32>,
    /// Standard output, or both streams in the order they were written when the command
    /// was run with [`Git::merged`].
    pub stdout: Vec<u8>,
    /// Standard error; empty when the streams were merged.
    pub stderr: Vec<u8>,
}

impl Output {
    /// Standard output without the one line feed git ends an answer with. Anything else
    /// (including further line feeds, which a path may contain) is kept.
    pub fn line(&self) -> &[u8] {
        self.stdout.strip_suffix(b"\n").unwrap_or(&self.stdout)
    }

    /// [`Output::line`] as text, with invalid UTF-8 replaced.
    pub fn line_text(&self) -> String {
        String::from_utf8_lossy(self.line()).into_owned()
    }

    /// Standard output as text, with invalid UTF-8 replaced.
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// The typed error for a run of `command` that ended with this output.
    pub fn into_error(self, command: &str) -> GitError {
        GitError::Failed {
            command: command.to_string(),
            code: self.code,
            stdout: String::from_utf8_lossy(&self.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&self.stderr).into_owned(),
        }
    }
}

/// One git invocation in the making: `git -C <dir> <fixed options> <args>`.
pub(crate) struct Git<'a> {
    dir: &'a Path,
    args: Vec<OsString>,
    stdin: Option<Vec<u8>>,
    timeout: Option<Duration>,
    merge_streams: bool,
}

impl<'a> Git<'a> {
    /// A git command that runs as if started in `dir`.
    pub fn new(dir: &'a Path) -> Git<'a> {
        Git {
            dir,
            args: Vec::new(),
            stdin: None,
            timeout: None,
            merge_streams: false,
        }
    }

    /// Appends one argument. The first argument is the subcommand.
    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    /// Appends several arguments.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_os_string()));
        self
    }

    /// Feeds `bytes` to git's standard input and then closes it. Without this, standard
    /// input is closed from the start.
    pub fn stdin(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(bytes.into());
        self
    }

    /// Stops the command when it runs longer than `limit`; `None` lets it run to the end.
    pub fn timeout(mut self, limit: Option<Duration>) -> Self {
        self.timeout = limit;
        self
    }

    /// Captures standard output and standard error through one pipe, so the result reads
    /// the way it would in a terminal.
    pub fn merged(mut self) -> Self {
        self.merge_streams = true;
        self
    }

    /// Runs the command and requires exit code 0.
    pub fn run(self) -> Result<Output> {
        self.run_accepting(&[0])
    }

    /// Runs a command whose exit code 1 is an answer ("no", "not found") rather than a
    /// failure. The caller reads the answer from [`Output::code`].
    pub fn probe(self) -> Result<Output> {
        self.run_accepting(&[0, 1])
    }

    fn run_accepting(self, accepted: &[i32]) -> Result<Output> {
        let label = self.label();
        let output = self.output()?;
        match output.code {
            Some(code) if accepted.contains(&code) => Ok(output),
            _ => Err(output.into_error(&label)),
        }
    }

    /// Runs the command and returns whatever it produced, whatever its exit code. Errors
    /// only when git could not be run at all or ran out of time.
    pub fn output(self) -> Result<Output> {
        let label = self.label();
        let started = Instant::now();
        let mut command = self.command();
        let merged = if self.merge_streams {
            Some(merge_streams(&mut command).map_err(GitError::Spawn)?)
        } else {
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
            None
        };
        let mut child = command.spawn().map_err(GitError::Spawn)?;
        // `command` still owns this process's copies of the merged pipe's write end, and
        // the reader only sees the end of the output once they are closed.
        drop(command);

        let finished = match supervise(&mut child, self.stdin, merged, self.timeout) {
            Ok(finished) => finished,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(GitError::Spawn(error));
            }
        };
        log::debug!(
            "git {:?} in {}: {:?} after {} ms",
            self.args,
            self.dir.display(),
            finished.status,
            started.elapsed().as_millis()
        );
        match finished.status {
            Some(status) => Ok(Output {
                code: status.code(),
                stdout: finished.stdout,
                stderr: finished.stderr,
            }),
            None => {
                let mut output = finished.stdout;
                output.extend_from_slice(&finished.stderr);
                Err(GitError::TimedOut {
                    command: label,
                    limit: self.timeout.unwrap_or_default(),
                    output: String::from_utf8_lossy(&output).into_owned(),
                })
            }
        }
    }

    /// The subcommand, for error messages.
    fn label(&self) -> String {
        self.args
            .first()
            .map(|arg| arg.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// The process to start, with everything but the output streams set up.
    fn command(&self) -> Command {
        let mut command = Command::new("git");
        command.arg("-C").arg(self.dir);
        command.args(["-c", "core.quotepath=off", "-c", "color.ui=never"]);
        command.args(&self.args);
        for name in REPOSITORY_ENV {
            command.env_remove(name);
        }
        // With GPG_TTY set, a terminal pinentry would draw over the IDE and fight it for
        // the keyboard. Without it gpg fails with a message instead.
        command.env_remove("GPG_TTY");
        command
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("LC_ALL", "C")
            .env("GIT_EDITOR", "true")
            .env("GIT_SEQUENCE_EDITOR", "true");
        command.stdin(if self.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        // A session of its own leaves git, and whatever it starts (ssh, gpg, hooks),
        // without a controlling terminal: none of them can prompt on the IDE's screen,
        // and they all share one process group that can be signalled as a whole.
        //
        // SAFETY: the closure runs in the forked child just before `exec`. It only calls
        // `setsid`, which is async-signal-safe, and reads `errno`; it does not allocate,
        // lock, or touch any state of this process.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command
    }
}

/// Points both output streams of `command` at one pipe and returns its reading end.
fn merge_streams(command: &mut Command) -> io::Result<PipeReader> {
    let (reader, writer) = io::pipe()?;
    let second_writer = writer.try_clone()?;
    command.stdout(writer).stderr(second_writer);
    Ok(reader)
}

/// The outcome of watching a git process to its end.
struct Finished {
    /// `None` when the process ran out of time and was stopped.
    status: Option<ExitStatus>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Feeds the child its input, collects its output and waits for it, stopping it when
/// `limit` runs out.
fn supervise(
    child: &mut Child,
    input: Option<Vec<u8>>,
    merged: Option<PipeReader>,
    limit: Option<Duration>,
) -> io::Result<Finished> {
    if let (Some(mut pipe), Some(bytes)) = (child.stdin.take(), input) {
        // Written from a thread of its own so a child that starts answering before it
        // has read everything cannot leave both sides waiting on a full pipe.
        thread::Builder::new()
            .name("bakedit-git-stdin".to_string())
            .spawn(move || {
                // A command that fails early exits without reading its input. That is
                // git's answer, reported through its exit code, not an error here.
                let _ = pipe.write_all(&bytes);
            })?;
    }
    let stdout = match merged {
        Some(reader) => Drain::start(reader)?,
        None => Drain::start(
            child
                .stdout
                .take()
                .ok_or_else(|| io::Error::other("git's standard output is not captured"))?,
        )?,
    };
    let stderr = match child.stderr.take() {
        Some(pipe) => Some(Drain::start(pipe)?),
        None => None,
    };

    let status = match limit {
        None => Some(child.wait()?),
        Some(limit) => match wait_until(child, Instant::now() + limit)? {
            Some(status) => Some(status),
            None => {
                stop(child)?;
                None
            }
        },
    };

    let deadline = Instant::now() + PIPE_GRACE;
    Ok(Finished {
        status,
        stdout: stdout.finish(deadline),
        stderr: stderr
            .map(|drain| drain.finish(deadline))
            .unwrap_or_default(),
    })
}

/// Waits for the child until `deadline`. `None` means it is still running.
fn wait_until(child: &mut Child, deadline: Instant) -> io::Result<Option<ExitStatus>> {
    let mut pause = Duration::from_millis(1);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(None);
        }
        thread::sleep(pause.min(left));
        pause = (pause * 2).min(POLL_INTERVAL);
    }
}

/// Ends a git process that ran out of time, together with everything it started: first
/// politely, so git can remove its lock files, then for good.
fn stop(child: &mut Child) -> io::Result<()> {
    signal_group(child, libc::SIGTERM);
    if wait_until(child, Instant::now() + TERMINATE_GRACE)?.is_none() {
        signal_group(child, libc::SIGKILL);
        child.wait()?;
    }
    Ok(())
}

/// Sends `signal` to the child's process group. Must only be called while the child has
/// not been waited for.
fn signal_group(child: &Child, signal: libc::c_int) {
    let Ok(group) = libc::pid_t::try_from(child.id()) else {
        return;
    };
    if group <= 1 {
        return;
    }
    // SAFETY: `kill` takes two integers and has no memory-safety preconditions. The child
    // called `setsid`, so its pid is also its process group id, and because it has not
    // been waited for yet that id cannot have been handed to another process.
    unsafe {
        libc::kill(-group, signal);
    }
}

/// A pipe that is read to its end on a thread of its own.
struct Drain {
    bytes: Arc<Mutex<Vec<u8>>>,
    finished: Receiver<()>,
}

impl Drain {
    fn start(mut source: impl Read + Send + 'static) -> io::Result<Drain> {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&bytes);
        let (done, finished) = mpsc::channel();
        thread::Builder::new()
            .name("bakedit-git-pipe".to_string())
            .spawn(move || {
                let mut chunk = vec![0u8; 64 * 1024];
                loop {
                    match source.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(count) => lock(&sink).extend_from_slice(&chunk[..count]),
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
                let _ = done.send(());
            })?;
        Ok(Drain { bytes, finished })
    }

    /// Everything that was read, waiting until `deadline` at the latest for the end of
    /// the pipe.
    fn finish(self, deadline: Instant) -> Vec<u8> {
        let wait = deadline.saturating_duration_since(Instant::now());
        let _ = self.finished.recv_timeout(wait);
        std::mem::take(&mut *lock(&self.bytes))
    }
}

/// Locks a buffer; a reader thread that panicked cannot make the bytes it collected
/// unusable.
fn lock(buffer: &Mutex<Vec<u8>>) -> MutexGuard<'_, Vec<u8>> {
    buffer.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(command: &Command, name: &str) -> Option<Option<OsString>> {
        command
            .get_envs()
            .find(|(key, _)| *key == OsStr::new(name))
            .map(|(_, value)| value.map(OsStr::to_os_string))
    }

    #[test]
    fn command_line_starts_with_the_fixed_options() {
        let git = Git::new(Path::new("/some/repo")).args(["status", "--porcelain=v2"]);
        let command = git.command();
        assert_eq!(command.get_program(), OsStr::new("git"));
        let args: Vec<&OsStr> = command.get_args().collect();
        assert_eq!(
            args,
            [
                "-C",
                "/some/repo",
                "-c",
                "core.quotepath=off",
                "-c",
                "color.ui=never",
                "status",
                "--porcelain=v2"
            ]
        );
        assert_eq!(git.label(), "status");
    }

    #[test]
    fn environment_is_fixed_and_non_interactive() {
        let command = Git::new(Path::new(".")).arg("status").command();
        for (name, value) in [
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_OPTIONAL_LOCKS", "0"),
            ("LC_ALL", "C"),
            ("GIT_EDITOR", "true"),
            ("GIT_SEQUENCE_EDITOR", "true"),
        ] {
            assert_eq!(
                env_of(&command, name),
                Some(Some(OsString::from(value))),
                "{name}"
            );
        }
    }

    #[test]
    fn repository_pointers_are_removed_from_the_environment() {
        let command = Git::new(Path::new(".")).arg("status").command();
        for name in REPOSITORY_ENV.iter().chain(["GPG_TTY"].iter()) {
            assert_eq!(env_of(&command, name), Some(None), "{name}");
        }
    }

    #[test]
    fn output_line_strips_exactly_one_line_feed() {
        let output = Output {
            code: Some(0),
            stdout: b"/odd\npath\n\n".to_vec(),
            stderr: Vec::new(),
        };
        assert_eq!(output.line(), b"/odd\npath\n");
        let output = Output {
            code: Some(0),
            stdout: b"no terminator".to_vec(),
            stderr: Vec::new(),
        };
        assert_eq!(output.line_text(), "no terminator");
    }

    #[test]
    fn output_becomes_a_typed_failure() {
        let output = Output {
            code: Some(128),
            stdout: b"out".to_vec(),
            stderr: b"fatal: bad\n".to_vec(),
        };
        match output.into_error("log") {
            GitError::Failed {
                command,
                code,
                stdout,
                stderr,
            } => {
                assert_eq!(command, "log");
                assert_eq!(code, Some(128));
                assert_eq!(stdout, "out");
                assert_eq!(stderr, "fatal: bad\n");
            }
            other => panic!("unexpected error {other:?}"),
        }
    }
}
