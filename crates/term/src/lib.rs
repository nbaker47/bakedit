//! Embedded terminal: pty, VT emulation, key and mouse encoding, link detection.
//!
//! This is the terminal in bakedit's left dock. It runs herdr, a shell or any other command
//! on a pseudo-terminal, emulates the screen with `alacritty_terminal` (the library Zed's
//! terminal is built on) and hands the result over as data. Nothing in here draws.
//!
//! - [`Terminal`] is a running terminal: spawn it from a [`TermConfig`], read the screen
//!   with [`Terminal::render`], type into it with [`Terminal::send_key`], and hear from it
//!   through [`TermEvent`]s.
//! - [`keys`] and [`input`] turn key, mouse, paste and focus events into the bytes a
//!   terminal program expects: xterm's legacy encoding and the kitty keyboard protocol.
//!   They are pure functions, usable without a terminal.
//! - [`links`] finds file references and URLs in text, so that a path printed by a
//!   compiler or an agent can be clicked open, and resolves them against the disk.
//!
//! ```no_run
//! use bakedit_term::{TermConfig, TermEvent, Terminal};
//!
//! // herdr by default, the user's shell when it is not installed.
//! let on_event = |event: TermEvent| println!("{event:?}");
//! let terminal = match Terminal::spawn(TermConfig::herdr(), Box::new(on_event)) {
//!     Err(error) if error.is_program_missing() => Terminal::spawn(TermConfig::shell(), Box::new(on_event)),
//!     other => other,
//! }?;
//! terminal.send_text("echo hello\r");
//! # Ok::<(), bakedit_term::TermError>(())
//! ```

#![cfg(unix)]
#![warn(missing_docs)]

pub mod config;
pub mod error;
pub mod event;
pub mod input;
pub mod keys;
pub mod links;
mod osc;
mod pty;
pub mod screen;
pub mod terminal;

pub use config::{
    find_program, inherited_env_names, split_command_line, TermColors, TermConfig, DEFAULT_COMMAND,
};
pub use error::{Result, TermError};
pub use event::TermEvent;
pub use input::{encode_alt_scroll, encode_focus, encode_mouse, encode_paste, MouseModes};
pub use keys::{encode_key, KeyModes};
pub use links::{
    default_search_bases, detect_links, link_at, resolve_path, resolve_reference, LinkSpan,
    LinkTarget, ResolvedPath,
};
pub use screen::{Cell, Cursor, CursorShape, ScreenLink, SelectionKind, TermModes, Underline};
pub use terminal::{Scroll, Terminal};
