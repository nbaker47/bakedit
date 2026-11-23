//! What a terminal tells the application.

use std::path::PathBuf;

/// Something the application has to hear about, delivered to the callback given to
/// [`Terminal::spawn`](crate::Terminal::spawn).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermEvent {
    /// The screen changed: redraw. Bursts of output are coalesced, so this arrives at most
    /// once per [`TermConfig::wakeup_interval`](crate::TermConfig::wakeup_interval), and
    /// always once more after the last change.
    Wakeup,
    /// The child set the window title (OSC 0 or 2).
    Title(String),
    /// The child went back to having no title of its own.
    ResetTitle,
    /// The child rang the bell. At most one of these is delivered per 100 ms.
    Bell,
    /// The child process ended. `Some(code)` is its exit status; `None` means a signal
    /// killed it (which is what [`Terminal::kill`](crate::Terminal::kill) does), or that
    /// the pty failed underneath it. Sent exactly once, after the child's last output has
    /// been drawn.
    Exit(Option<i32>),
    /// The child copied text to the clipboard (OSC 52).
    ClipboardStore(String),
    /// The child asks for the clipboard's contents (OSC 52 with `?`). Answer with
    /// [`Terminal::respond_clipboard`](crate::Terminal::respond_clipboard), or do not
    /// answer at all to refuse.
    ClipboardLoad,
    /// The shell reported its working directory on this machine (OSC 7).
    Cwd(PathBuf),
    /// The child posted a desktop notification (OSC 9, OSC 777 or OSC 99). Either field
    /// may be empty, never both.
    Notification {
        /// The headline, when the protocol has one.
        title: String,
        /// The message.
        body: String,
    },
}
