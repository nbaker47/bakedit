//! The pseudo-terminal and the child process on it.
//!
//! alacritty ships a pty of its own, and its event loop is used as it is, but its spawner
//! cannot remove variables from the child's environment, exits the whole process when a
//! resize fails and waits forever on a child that ignores `SIGHUP`. [`Pty`] is the same
//! idea without those: it implements alacritty's pty traits, so the event loop drives it.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use alacritty_terminal::event::{OnResize, WindowSize};
use alacritty_terminal::tty::{ChildEvent, EventedPty, EventedReadWrite};
use polling::{Event, PollMode, Poller};
use rustix_openpty::rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
use rustix_openpty::rustix::termios::{self, Winsize};
use signal_hook::consts::SIGCHLD;
use signal_hook::SigId;

use crate::config::TermConfig;
use crate::error::{Result, TermError};

/// The poller keys alacritty's event loop tells pty I/O and child events apart by. They are
/// `tty::PTY_READ_WRITE_TOKEN` and `tty::PTY_CHILD_EVENT_TOKEN` in alacritty_terminal 0.26,
/// which keeps them private; the integration tests that wait for output and for an exit
/// status fail if a later version changes them.
const PTY_READ_WRITE_TOKEN: usize = 0;
const PTY_CHILD_EVENT_TOKEN: usize = 1;

/// Reads at end-of-file that are let through before the reader starts to pause.
const FREE_EOF_READS: u32 = 64;
/// The pause between reads once the pty stays at end-of-file.
const EOF_PAUSE: Duration = Duration::from_millis(10);

/// Lock a mutex, taking the data even if a thread panicked while holding it: everything
/// behind these locks stays valid whatever happened to the thread that last touched it.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The child process as the rest of the crate sees it: a pid that is safe to signal.
#[derive(Debug)]
pub(crate) struct ChildHandle {
    pid: u32,
    /// Set once the pid must not be signalled again: the child has been reaped (so the
    /// number may already belong to another process), or handed to the reaper thread.
    /// Reaping only ever happens with this lock held.
    gone: Mutex<bool>,
}

impl ChildHandle {
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    /// Send `SIGHUP` to the child's process group, as closing a terminal window does.
    /// Returns whether a signal was sent.
    pub(crate) fn hang_up(&self) -> bool {
        let gone = lock(&self.gone);
        !*gone && signal_group(self.pid, libc::SIGHUP)
    }
}

/// Signal the process group led by `pid`, or `pid` alone if it leads no group.
fn signal_group(pid: u32, signal: libc::c_int) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // 0 and -1 mean "my own group" and "everything" to kill(2).
    if pid <= 1 {
        return false;
    }
    // SAFETY: kill has no memory safety requirements. Callers hold the `gone` lock with the
    // child not yet reaped, so the pid, and the group it leads after `setsid`, is still the
    // child's and cannot have been reused.
    unsafe { libc::kill(-pid, signal) == 0 || libc::kill(pid, signal) == 0 }
}

/// A function shown every chunk of the child's output, on the thread that reads the pty.
pub(crate) type Tap = Box<dyn FnMut(&[u8]) + Send>;

/// The reading side of the pty, which also shows every byte to a tap on its way to the
/// parser.
pub(crate) struct TapReader {
    file: File,
    tap: Tap,
    eof_reads: u32,
}

impl Read for TapReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let count = self.file.read(buf)?;
        if count == 0 {
            // End of file: nothing holds the other side open. It stays "readable" for as
            // long as that lasts, so the event loop comes straight back. That is over in a
            // moment when the child has exited; if the child lives on with its terminal
            // closed, pausing here keeps the reader thread from spinning.
            self.eof_reads = self.eof_reads.saturating_add(1);
            if self.eof_reads > FREE_EOF_READS {
                std::thread::sleep(EOF_PAUSE);
            }
        } else {
            self.eof_reads = 0;
            (self.tap)(&buf[..count]);
        }
        Ok(count)
    }
}

/// A pty with a child process on it.
pub(crate) struct Pty {
    /// The child, until it has been reaped.
    child: Option<Child>,
    handle: Arc<ChildHandle>,
    reader: TapReader,
    /// Receives a byte for every `SIGCHLD` the process gets.
    signals: UnixStream,
    signal_id: SigId,
    /// The poller the descriptors are registered with, while they are.
    poller: Option<Arc<Poller>>,
    /// Called when the pty goes away with the child still running.
    on_abandon: Option<Box<dyn FnOnce() + Send>>,
}

/// Start `config`'s program on a new pty of `size`.
///
/// `tap` sees everything the child writes, in order, on the thread that reads the pty.
/// `on_abandon` runs if the pty is dropped before the child has exited, which is how the
/// owner learns that no exit status will ever come.
pub(crate) fn spawn(
    config: &TermConfig,
    size: WindowSize,
    tap: Tap,
    on_abandon: Box<dyn FnOnce() + Send>,
) -> Result<Pty> {
    let pair = rustix_openpty::openpty(None, Some(&winsize(size)))
        .map_err(|err| TermError::Pty(err.into()))?;
    let (master, slave) = (pair.controller, pair.user);

    // Tell the kernel's line editor that input is UTF-8, so erasing a character in cooked
    // mode removes all of its bytes.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Ok(mut attributes) = termios::tcgetattr(&master) {
        attributes.input_modes.set(termios::InputModes::IUTF8, true);
        let _ = termios::tcsetattr(&master, termios::OptionalActions::Now, &attributes);
    }

    let (mut command, program) = command_for(config);
    let stdio = |err: io::Error| TermError::Pty(err);
    command.stdin(Stdio::from(slave.try_clone().map_err(stdio)?));
    command.stdout(Stdio::from(slave.try_clone().map_err(stdio)?));
    command.stderr(Stdio::from(slave));

    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env("TERM_PROGRAM", "bakedit");
    command.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
    match config.cwd.as_deref() {
        Some(cwd) if cwd.is_dir() => {
            command.current_dir(cwd);
            // A shell takes PWD at its word when it names the directory it starts in.
            if let Ok(absolute) = std::path::absolute(cwd) {
                command.env("PWD", absolute);
            }
        }
        Some(cwd) => log::warn!(
            "terminal working directory {} does not exist, starting in the current one",
            cwd.display()
        ),
        None => {}
    }
    for (name, value) in &config.env {
        command.env(name, value);
    }
    for name in &config.env_remove {
        command.env_remove(name);
    }

    // SAFETY: the closure runs in the forked child just before exec and only makes
    // async-signal-safe system calls (setsid, ioctl, signal); it allocates nothing and
    // touches no state shared with the parent.
    unsafe {
        command.pre_exec(|| {
            // A session of its own, so that the pty can become its controlling terminal
            // and the child leads a process group that can be signalled as one.
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            // Standard input is the pty by now. The request constant's type differs
            // between platforms, hence the cast.
            if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            // Ignored signals stay ignored across exec. Whatever this process does with
            // these, the program in the terminal starts with the defaults, or ctrl-c and
            // job control would not work in it.
            for signal in [
                libc::SIGHUP,
                libc::SIGINT,
                libc::SIGQUIT,
                libc::SIGTERM,
                libc::SIGALRM,
                libc::SIGCHLD,
                libc::SIGPIPE,
                libc::SIGTSTP,
                libc::SIGTTIN,
                libc::SIGTTOU,
                libc::SIGCONT,
                libc::SIGWINCH,
                libc::SIGUSR1,
                libc::SIGUSR2,
            ] {
                libc::signal(signal, libc::SIG_DFL);
            }
            Ok(())
        });
    }

    // The signal pipe has to exist before the child does, or a child that exits at once
    // could do so unnoticed.
    let (sender, signals) = UnixStream::pair()?;
    signals.set_nonblocking(true)?;
    let signal_id = signal_hook::low_level::pipe::register(SIGCHLD, sender)?;

    let child = match command.spawn() {
        Ok(child) => child,
        Err(source) => {
            signal_hook::low_level::unregister(signal_id);
            return Err(TermError::Spawn { program, source });
        }
    };
    // The parent's copies of the pty's other side live in `command`. They have to go, or
    // the pty would never report that the child closed it.
    drop(command);

    let handle = Arc::new(ChildHandle {
        pid: child.id(),
        gone: Mutex::new(false),
    });
    let mut pty = Pty {
        child: Some(child),
        handle,
        reader: TapReader {
            file: File::from(master),
            tap,
            eof_reads: 0,
        },
        signals,
        signal_id,
        poller: None,
        on_abandon: None,
    };
    // From here on dropping `pty` hangs up on the child and reaps it, so errors can just
    // return. The owner is only told about a child it actually got.
    let flags = fcntl_getfl(&pty.reader.file).map_err(io::Error::from)?;
    fcntl_setfl(&pty.reader.file, flags | OFlags::NONBLOCK).map_err(io::Error::from)?;
    pty.on_abandon = Some(on_abandon);
    Ok(pty)
}

/// The command to run, and its name for error messages.
fn command_for(config: &TermConfig) -> (Command, String) {
    match &config.program {
        Some(program) => {
            let mut command = Command::new(program);
            command.args(&config.args);
            (command, program.clone())
        }
        None => {
            let shell = login_shell(&config.env);
            let name = Path::new(&shell)
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or("sh");
            let mut command = Command::new(&shell);
            // A leading dash in argv[0] is how a shell is told it is a login shell.
            command.arg0(format!("-{name}"));
            command.args(&config.args);
            (command, shell)
        }
    }
}

/// The user's shell: `SHELL` from the child's environment or this one's, then the account's
/// shell, then `/bin/sh`.
fn login_shell(env: &[(String, String)]) -> String {
    let configured = env
        .iter()
        .rev()
        .find(|(name, _)| name == "SHELL")
        .map(|(_, value)| value.clone());
    configured
        .or_else(|| std::env::var("SHELL").ok())
        .filter(|shell| !shell.is_empty())
        .or_else(account_shell)
        .unwrap_or_else(|| "/bin/sh".to_string())
}

/// The shell in the user's account record.
fn account_shell() -> Option<String> {
    let mut strings = vec![0 as libc::c_char; 16 * 1024];
    let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: getpwuid_r fills `record` and stores its strings in `strings`; both are live
    // locals of the sizes passed, and `found` is a valid place for the result pointer.
    let status = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            record.as_mut_ptr(),
            strings.as_mut_ptr(),
            strings.len(),
            &mut found,
        )
    };
    if status != 0 || found.is_null() {
        return None;
    }
    // SAFETY: on success `found` points at `record`, now initialised, and `pw_shell` is
    // either null or a NUL-terminated string inside `strings`, which is still alive.
    let shell = unsafe {
        let shell = (*found).pw_shell;
        if shell.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr(shell).to_str().ok()?.to_string()
    };
    (!shell.is_empty()).then_some(shell)
}

fn winsize(size: WindowSize) -> Winsize {
    Winsize {
        ws_row: size.num_lines,
        ws_col: size.num_cols,
        ws_xpixel: size.num_cols.saturating_mul(size.cell_width),
        ws_ypixel: size.num_lines.saturating_mul(size.cell_height),
    }
}

impl Pty {
    /// The handle through which other threads may signal the child.
    pub(crate) fn child_handle(&self) -> Arc<ChildHandle> {
        self.handle.clone()
    }
}

impl EventedReadWrite for Pty {
    type Reader = TapReader;
    type Writer = File;

    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        mut interest: Event,
        mode: PollMode,
    ) -> io::Result<()> {
        interest.key = PTY_READ_WRITE_TOKEN;
        self.poller = Some(poll.clone());
        // SAFETY: a source must stay open for as long as it is registered. Both
        // descriptors are owned by `self`, and `self` takes them out of the poller again
        // in `deregister` or, failing that, in `drop`, before they are closed.
        unsafe {
            poll.add_with_mode(&self.reader.file, interest, mode)?;
            poll.add_with_mode(
                &self.signals,
                Event::readable(PTY_CHILD_EVENT_TOKEN),
                PollMode::Level,
            )
        }
    }

    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        mut interest: Event,
        mode: PollMode,
    ) -> io::Result<()> {
        interest.key = PTY_READ_WRITE_TOKEN;
        poll.modify_with_mode(&self.reader.file, interest, mode)?;
        poll.modify_with_mode(
            &self.signals,
            Event::readable(PTY_CHILD_EVENT_TOKEN),
            PollMode::Level,
        )
    }

    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        self.poller = None;
        poll.delete(&self.reader.file)?;
        poll.delete(&self.signals)
    }

    fn reader(&mut self) -> &mut TapReader {
        &mut self.reader
    }

    fn writer(&mut self) -> &mut File {
        &mut self.reader.file
    }
}

impl EventedPty for Pty {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        // Some child of this process changed state. Empty the pipe, then see if it was ours.
        let mut buffer = [0u8; 64];
        loop {
            match self.signals.read(&mut buffer) {
                Ok(0) => break,
                Ok(_) => {}
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(err) => {
                    if err.kind() != ErrorKind::WouldBlock {
                        log::error!("reading the terminal's signal pipe failed: {err}");
                    }
                    break;
                }
            }
        }
        let child = self.child.as_mut()?;
        let mut gone = lock(&self.handle.gone);
        match child.try_wait() {
            Ok(Some(status)) => {
                *gone = true;
                drop(gone);
                self.child = None;
                Some(ChildEvent::Exited(Some(status)))
            }
            Ok(None) => None,
            Err(err) => {
                log::error!("checking on the terminal's child process failed: {err}");
                None
            }
        }
    }
}

impl OnResize for Pty {
    fn on_resize(&mut self, size: WindowSize) {
        if let Err(err) = termios::tcsetwinsize(&self.reader.file, winsize(size)) {
            log::warn!("could not resize the pty: {err}");
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        signal_hook::low_level::unregister(self.signal_id);
        if let Some(poller) = self.poller.take() {
            let _ = poller.delete(&self.reader.file);
            let _ = poller.delete(&self.signals);
        }
        let Some(mut child) = self.child.take() else {
            return;
        };
        {
            // Still running: the terminal was closed, or the pty failed under it. Hang up,
            // and let nobody signal the pid after this, because it is about to be reaped
            // where no lock can be held.
            let mut gone = lock(&self.handle.gone);
            signal_group(self.handle.pid, libc::SIGHUP);
            *gone = true;
        }
        if let Some(abandon) = self.on_abandon.take() {
            abandon();
        }
        if !matches!(child.try_wait(), Ok(Some(_))) {
            // Waiting here could block for as long as the child ignores the hangup. A
            // thread of its own collects it whenever it does exit, so no zombie is left.
            let reaper = std::thread::Builder::new().name("bakedit-term-reaper".to_string());
            if let Err(err) = reaper.spawn(move || {
                let _ = child.wait();
            }) {
                log::warn!("could not start a thread to reap the terminal's child: {err}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_env(env: &[(&str, &str)]) -> Vec<(String, String)> {
        env.iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn the_shell_in_the_child_environment_wins() {
        assert_eq!(
            login_shell(&config_with_env(&[("SHELL", "/bin/dash")])),
            "/bin/dash"
        );
        assert_eq!(
            login_shell(&config_with_env(&[
                ("SHELL", "/bin/dash"),
                ("X", "1"),
                ("SHELL", "/bin/ksh")
            ])),
            "/bin/ksh"
        );
        // Otherwise it is whatever this process and this account say, and never empty.
        let fallback = login_shell(&[]);
        assert!(fallback.starts_with('/'), "{fallback}");
        assert!(!login_shell(&config_with_env(&[("SHELL", "")])).is_empty());
    }

    #[test]
    fn the_account_has_a_shell() {
        // Sandboxes without a passwd entry exist; when there is one it is a path.
        if let Some(shell) = account_shell() {
            assert!(shell.starts_with('/'), "{shell}");
        }
    }

    #[test]
    fn a_login_shell_is_marked_by_a_dash() {
        let config = TermConfig {
            env: config_with_env(&[("SHELL", "/bin/sh")]),
            args: vec!["-c".to_string(), "true".to_string()],
            ..TermConfig::default()
        };
        let (command, program) = command_for(&config);
        assert_eq!(program, "/bin/sh");
        assert_eq!(command.get_program(), "/bin/sh");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["-c", "true"]);

        let config = TermConfig {
            program: Some("cat".to_string()),
            args: vec!["-v".to_string()],
            ..TermConfig::default()
        };
        let (command, program) = command_for(&config);
        assert_eq!(program, "cat");
        assert_eq!(command.get_program(), "cat");
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["-v"]);
    }

    #[test]
    fn window_size_in_cells_and_pixels() {
        let size = winsize(WindowSize {
            num_lines: 24,
            num_cols: 80,
            cell_width: 8,
            cell_height: 16,
        });
        assert_eq!(
            (size.ws_row, size.ws_col, size.ws_xpixel, size.ws_ypixel),
            (24, 80, 640, 384)
        );
        let huge = winsize(WindowSize {
            num_lines: 60000,
            num_cols: 60000,
            cell_width: 20,
            cell_height: 40,
        });
        assert_eq!((huge.ws_xpixel, huge.ws_ypixel), (u16::MAX, u16::MAX));
    }

    #[test]
    fn dangerous_pids_are_never_signalled() {
        assert!(!signal_group(0, libc::SIGHUP));
        assert!(!signal_group(1, libc::SIGHUP));
        assert!(!signal_group(u32::MAX, libc::SIGHUP));
        let gone = ChildHandle {
            pid: std::process::id(),
            gone: Mutex::new(true),
        };
        assert!(!gone.hang_up());
    }
}
