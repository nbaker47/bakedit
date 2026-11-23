//! Everything the host terminal can send us.

use crate::color::Rgb;
use crate::keys::KeyEvent;
use crate::mouse::MouseEvent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// Bracketed paste, already unwrapped.
    Paste(String),
    FocusGained,
    FocusLost,
    /// An answer to something we asked the terminal.
    Reply(TerminalReply),
}

/// Which colour a colour report is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorSlot {
    Foreground,
    Background,
    Cursor,
    Palette(u8),
}

/// Answers to capability and state queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalReply {
    /// `CSI ? flags u`: the kitty keyboard protocol is supported and these flags are active.
    KittyKeyboardFlags(u8),
    /// `CSI ? ... c`: primary device attributes. Every terminal answers this, so it is the
    /// sentinel that ends a round of queries.
    DeviceAttributes(String),
    /// `CSI ? 997 ; 1|2 n`: the terminal is in dark (true) or light (false) mode.
    DarkMode(bool),
    /// An OSC 10/11/12/4 colour report.
    Color { slot: ColorSlot, rgb: Rgb },
    /// `DCS > | text ST`: terminal name and version (XTVERSION).
    Version(String),
    /// `CSI ? mode ; state $ y`: DECRPM. State 1 or 3 means set, 2 or 4 reset, 0 unknown.
    Mode { mode: u16, state: u8 },
}
