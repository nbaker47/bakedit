//! The host terminal: switching it into the modes bakedit needs and back, reading the bytes
//! it sends, and noticing resizes and signals.
//!
//! Everything that changes the terminal's state goes through [`Host`], and leaving undoes
//! it in reverse order. A panic or a kill signal must never leave the user's terminal in
//! raw mode with the mouse captured, so the same restore runs from a panic hook too.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU8, Ordering};

/// Kitty keyboard flags: tell apart keys that legacy encoding merges (ctrl-i and tab...).
pub const KITTY_DISAMBIGUATE: u8 = 1;
/// Report key repeats and releases.
pub const KITTY_EVENT_TYPES: u8 = 2;
/// Report the shifted and base-layout forms of a key.
pub const KITTY_ALTERNATE_KEYS: u8 = 4;
/// Report every key as an escape code, including plain text keys and bare modifiers.
pub const KITTY_ALL_KEYS: u8 = 8;
/// Report the text a key produces alongside it.
pub const KITTY_TEXT: u8 = 16;

/// Flags bakedit asks for. All of them: releases and bare modifier keys are what make the
/// nav layer (hold `;`) and cmd-click possible, and the text report keeps typing exact when
/// every key arrives as an escape code.
pub const KITTY_FULL: u8 = KITTY_DISAMBIGUATE | KITTY_EVENT_TYPES | KITTY_ALTERNATE_KEYS | KITTY_ALL_KEYS | KITTY_TEXT;
/// A gentler set for setups where full reporting gets in the way of an input method:
/// plain text keys stay plain text. The nav layer and cmd-click do not work with it.
pub const KITTY_BASIC: u8 = KITTY_DISAMBIGUATE | KITTY_EVENT_TYPES | KITTY_ALTERNATE_KEYS;

/// The flags currently pushed, so the panic hook knows whether there is anything to pop.
static ACTIVE: AtomicU8 = AtomicU8::new(0);
const ACTIVE_BIT: u8 = 0x80;

fn enter_sequence(kitty_flags: u8) -> String {
    let mut out = String::new();
    out.push_str("\x1b[?1049h"); // alternate screen
    out.push_str(&format!("\x1b[>{kitty_flags}u")); // push keyboard flags
    out.push_str("\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h"); // mouse: clicks, drags, motion, SGR coordinates
    out.push_str("\x1b[?1004h"); // focus in/out
    out.push_str("\x1b[?2004h"); // bracketed paste
    // Ask what the terminal supports. The device-attributes reply always comes, and comes
    // last, so its arrival means "any keyboard-flags reply that was coming has come".
    out.push_str("\x1b[?u\x1b[c");
    out
}

fn leave_sequence() -> &'static str {
    concat!(
        "\x1b[?2004l",
        "\x1b[?1004l",
        "\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l",
        "\x1b[<u",    // pop keyboard flags
        "\x1b[0 q",   // cursor shape back to the terminal's default
        "\x1b[?25h",  // cursor visible
        "\x1b[?1049l" // leave the alternate screen
    )
}

/// Put the terminal back the way it was. Safe to call more than once and from a panic hook.
pub fn restore() {
    if ACTIVE.swap(0, Ordering::SeqCst) & ACTIVE_BIT == 0 {
        return;
    }
    let mut stdout = io::stdout();
    let _ = stdout.write_all(leave_sequence().as_bytes());
    let _ = stdout.flush();
    let _ = crossterm::terminal::disable_raw_mode();
}

/// Holds the terminal in bakedit's modes; dropping it restores the terminal.
pub struct Host {
    kitty_flags: u8,
}

impl Host {
    /// Enter raw mode and the alternate screen, and turn on mouse, focus, paste and keyboard
    /// reporting with the given kitty flags.
    pub fn enter(kitty_flags: u8) -> io::Result<Host> {
        crossterm::terminal::enable_raw_mode()?;
        ACTIVE.store(ACTIVE_BIT | (kitty_flags & 0x1f), Ordering::SeqCst);
        let mut stdout = io::stdout();
        stdout.write_all(enter_sequence(kitty_flags).as_bytes())?;
        stdout.flush()?;
        Ok(Host { kitty_flags })
    }

    pub fn kitty_flags(&self) -> u8 {
        self.kitty_flags
    }

    /// Change the keyboard flags (the nav layer toggle moves between the full and basic
    /// sets). Sets the top of the terminal's flag stack in place.
    pub fn set_kitty_flags(&mut self, flags: u8) -> io::Result<()> {
        if flags == self.kitty_flags {
            return Ok(());
        }
        self.kitty_flags = flags;
        ACTIVE.store(ACTIVE_BIT | (flags & 0x1f), Ordering::SeqCst);
        let mut stdout = io::stdout();
        stdout.write_all(format!("\x1b[={flags};1u").as_bytes())?;
        stdout.flush()
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

/// Restore the terminal before a panic message is printed, so the message is readable and
/// the shell is usable afterwards.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
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

    #[test]
    fn leaving_undoes_entering_in_reverse() {
        let enter = enter_sequence(KITTY_FULL);
        let leave = leave_sequence();
        assert!(enter.starts_with("\x1b[?1049h"), "the alternate screen is entered first");
        assert!(leave.ends_with("\x1b[?1049l"), "and left last");
        assert!(enter.contains("\x1b[>31u"));
        assert!(leave.contains("\x1b[<u"));
        // Every private mode that is set is reset.
        for mode in ["1000", "1002", "1003", "1004", "1006", "2004", "1049"] {
            assert!(enter.contains(&format!("\x1b[?{mode}h")), "{mode} set");
            assert!(leave.contains(&format!("\x1b[?{mode}l")), "{mode} reset");
        }
        let order = |text: &str, a: &str, b: &str| text.find(a).unwrap() < text.find(b).unwrap();
        assert!(order(&enter, "\x1b[?1000h", "\x1b[?2004h"));
        assert!(order(leave, "\x1b[?2004l", "\x1b[?1000l"));
        assert!(enter.ends_with("\x1b[?u\x1b[c"), "the capability queries go out last");
    }

    #[test]
    fn flag_sets() {
        assert_eq!(KITTY_FULL, 31);
        assert_eq!(KITTY_BASIC, 7);
        assert_eq!(KITTY_BASIC & KITTY_ALL_KEYS, 0);
    }

    #[test]
    fn restore_without_enter_does_nothing() {
        // Must not write escape codes into the test output or touch the terminal mode.
        ACTIVE.store(0, Ordering::SeqCst);
        restore();
        assert_eq!(ACTIVE.load(Ordering::SeqCst), 0);
    }
}
