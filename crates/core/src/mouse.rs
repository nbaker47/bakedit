//! Mouse events in terminal cells.

use crate::keys::Mods;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Back,
    Forward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseKind {
    Press(MouseButton),
    Release(MouseButton),
    /// Motion with a button held.
    Drag(MouseButton),
    /// Motion with no button held (only when the host reports any-motion).
    Move,
    ScrollUp,
    ScrollDown,
    ScrollLeft,
    ScrollRight,
}

/// A mouse event. `col` and `row` are 0-based cells from the top-left of whatever surface the
/// event is addressed to: the whole screen as parsed from the host, or a panel after the app
/// has translated it.
///
/// `mods` carries only what the mouse protocol can say (shift, alt, ctrl). Terminals do not
/// report the command key on mouse events; the app tracks it from key events instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MouseEvent {
    pub kind: MouseKind,
    pub col: u16,
    pub row: u16,
    pub mods: Mods,
}

impl MouseEvent {
    pub fn new(kind: MouseKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent { kind, col, row, mods: Mods::empty() }
    }

    /// The same event with its origin moved to `(left, top)`, or `None` if it falls above or
    /// left of that origin.
    pub fn relative_to(self, left: u16, top: u16) -> Option<MouseEvent> {
        Some(MouseEvent {
            col: self.col.checked_sub(left)?,
            row: self.row.checked_sub(top)?,
            ..self
        })
    }
}
