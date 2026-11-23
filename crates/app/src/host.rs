//! The host terminal: switching it into the modes bakedit needs and back, reading the bytes
//! it sends, and noticing resizes and signals.
//!
//! The control strings come from `bakedit_input::host`. Everything that changes the
//! terminal's state goes through [`Host`], and leaving undoes it in reverse order. A panic
//! or a kill signal must never leave the user's terminal in raw mode with the mouse
//! captured, so the same restore runs from the panic hook and the signal path too.

use std::io::{self, Read, Write};
use std::sync::Mutex;

use bakedit_input::host::{self as seq, HostOptions, MouseTracking};

/// Save the window title on the terminal's title stack (XTWINOPS 22), so leaving puts back
/// the one the user had.
const TITLE_PUSH: &str = "\x1b[22;0t";
/// Take the saved title back (XTWINOPS 23).
const TITLE_POP: &str = "\x1b[23;0t";

/// What to write to undo the current [`Host`]; `None` when the terminal is as it was.
static LEAVE: Mutex<Option<String>> = Mutex::new(None);

/// The modes the editor runs in: the alternate screen, every kitty keyboard flag (key
/// releases and bare modifier keys are what make the nav layer and cmd-click work, and the
/// text report keeps typing exact), the mouse with motion (for hover), focus events,
/// bracketed paste, colour scheme reports, and the capability queries.
pub fn editor_options() -> HostOptions {
    HostOptions { mouse: MouseTracking::AnyMotion, ..HostOptions::default() }
}

/// The modes `--dump-input` and `--probe` use: the same input reporting without taking over
/// the screen, so what they print stays in the scrollback.
pub fn inline_options() -> HostOptions {
    HostOptions { alternate_screen: false, hide_cursor: false, color_scheme_updates: false, ..editor_options() }
}

/// Everything written on entering, title push first.
fn enter_string(options: &HostOptions) -> String {
    format!("{TITLE_PUSH}{}", seq::enter_sequence(options))
}

/// Everything written on leaving: the reverse, the title last.
fn leave_string(options: &HostOptions) -> String {
    format!("{}{TITLE_POP}", seq::leave_sequence(options))
}

/// Put the terminal back the way it was. Safe to call more than once, from a panic hook and
/// from a signal path.
pub fn restore() {
    let leave = match LEAVE.lock() {
        Ok(mut leave) => leave.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    };
    if let Some(leave) = leave {
        let mut stdout = io::stdout();
        let _ = stdout.write_all(leave.as_bytes());
        let _ = stdout.flush();
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Whether bakedit has the terminal in its modes right now.
pub fn is_active() -> bool {
    LEAVE.lock().map(|leave| leave.is_some()).unwrap_or(true)
}

/// Holds the terminal in bakedit's modes; dropping it restores the terminal.
pub struct Host {
    options: HostOptions,
}

impl Host {
    /// Enter raw mode and the modes in `options`, ending with the capability queries.
    pub fn enter(options: HostOptions) -> io::Result<Host> {
        crossterm::terminal::enable_raw_mode()?;
        if let Ok(mut leave) = LEAVE.lock() {
            *leave = Some(leave_string(&options));
        }
        let mut stdout = io::stdout();
        stdout.write_all(enter_string(&options).as_bytes())?;
        stdout.flush()?;
        Ok(Host { options })
    }

    /// The modes this host was entered with.
    pub fn options(&self) -> &HostOptions {
        &self.options
    }

    /// Size in cells, with a sane fallback when the terminal will not say.
    pub fn size() -> (u16, u16) {
        match crossterm::terminal::size() {
            Ok((cols, rows)) if cols > 0 && rows > 0 => (cols, rows),
            _ => (80, 24),
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        restore();
    }
}

/// Restore the terminal before a panic on the main thread is printed, so the message is
/// readable and the shell usable afterwards. A panic on another thread while the UI is up
/// goes to the log instead of the screen: that thread is gone, the editor keeps running,
/// and text printed over the frame would wreck it.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let main = std::thread::current().name() == Some("main");
        if main || !is_active() {
            restore();
            previous(info);
        } else {
            let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
            log::error!("thread {thread} panicked: {info}");
        }
    }));
}

/// What arrives from outside the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEvent {
    /// Bytes typed, clicked or pasted.
    Bytes(Vec<u8>),
    /// The window changed size.
    Resized,
    /// SIGTERM or SIGHUP: save and leave.
    Terminate,
    /// Standard input closed (the terminal went away).
    Closed,
}

/// Start the thread that reads standard input. `send` is called with every chunk read.
pub fn spawn_reader(send: impl Fn(HostEvent) + Send + 'static) -> io::Result<()> {
    std::thread::Builder::new().name("bakedit-input".into()).spawn(move || {
        let stdin = io::stdin();
        let mut stdin = stdin.lock();
        let mut buffer = [0u8; 16 * 1024];
        loop {
            match stdin.read(&mut buffer) {
                Ok(0) => {
                    send(HostEvent::Closed);
                    return;
                }
                Ok(n) => send(HostEvent::Bytes(buffer[..n].to_vec())),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    send(HostEvent::Closed);
                    return;
                }
            }
        }
    })?;
    Ok(())
}

/// Start the thread that turns signals into events: window resizes, and requests to quit.
pub fn spawn_signal_watcher(send: impl Fn(HostEvent) + Send + 'static) -> io::Result<()> {
    use signal_hook::consts::{SIGHUP, SIGTERM, SIGWINCH};
    let mut signals = signal_hook::iterator::Signals::new([SIGWINCH, SIGTERM, SIGHUP])?;
    std::thread::Builder::new().name("bakedit-signals".into()).spawn(move || {
        for signal in signals.forever() {
            match signal {
                SIGWINCH => send(HostEvent::Resized),
                _ => send(HostEvent::Terminate),
            }
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bakedit_input::host::KITTY_ALL_FLAGS;

    #[test]
    fn leaving_undoes_entering_in_reverse() {
        let options = editor_options();
        let enter = enter_string(&options);
        let leave = leave_string(&options);
        assert!(enter.starts_with(TITLE_PUSH), "the user's title is saved first");
        assert!(leave.ends_with(TITLE_POP), "and put back last");
        let after_push = &enter[TITLE_PUSH.len()..];
        assert!(after_push.starts_with(seq::ALTERNATE_SCREEN_ON), "then the alternate screen");
        assert!(enter.contains(&seq::kitty_push(KITTY_ALL_FLAGS)), "every kitty flag is asked for");
        assert!(enter.contains("\x1b[?1003h"), "mouse motion, for hover");
        assert!(enter.ends_with(seq::DEVICE_ATTRIBUTES_QUERY), "the queries end with the sentinel");
        let order = |text: &str, a: &str, b: &str| text.find(a).unwrap() < text.find(b).unwrap();
        assert!(order(&enter, seq::ALTERNATE_SCREEN_ON, &seq::kitty_push(KITTY_ALL_FLAGS)), "flags are per screen: push after switching");
        assert!(order(&leave, seq::KITTY_POP, seq::ALTERNATE_SCREEN_OFF), "pop before leaving the screen");
        for mode in ["1003", "1004", "1006", "2004", "1049"] {
            assert!(enter.contains(&format!("\x1b[?{mode}h")), "{mode} set");
            assert!(leave.contains(&format!("\x1b[?{mode}l")), "{mode} reset");
        }
        assert!(leave.starts_with(seq::SYNC_END), "a frame cut off mid-update is ended");
    }

    #[test]
    fn the_inline_modes_leave_the_screen_alone() {
        let options = inline_options();
        let enter = enter_string(&options);
        assert!(!enter.contains(seq::ALTERNATE_SCREEN_ON));
        assert!(!enter.contains(seq::CURSOR_HIDE));
        assert!(enter.contains(&seq::kitty_push(KITTY_ALL_FLAGS)));
    }

    #[test]
    fn restore_without_enter_does_nothing() {
        // Must not write escape codes into the test output or touch the terminal mode.
        if let Ok(mut leave) = LEAVE.lock() {
            *leave = None;
        }
        restore();
        assert!(!is_active());
    }
}
