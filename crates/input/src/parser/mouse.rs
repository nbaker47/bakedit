//! Mouse reports: the SGR form (`CSI < b ; x ; y M/m`) and the legacy X10 form (`CSI M` and
//! three bytes). Both use the same button code.

use bakedit_core::{Mods, MouseButton, MouseEvent, MouseKind};

/// A 1-based coordinate from a mouse report as a 0-based cell, clamped to what fits.
pub(super) fn cell(coordinate: u32) -> u16 {
    u16::try_from(coordinate.saturating_sub(1)).unwrap_or(u16::MAX)
}

/// Decode an SGR mouse report. `release` is true for the `m` final byte.
///
/// The low two bits of `code` pick the button (0 left, 1 middle, 2 right, 3 none), 4 is
/// shift, 8 alt, 16 ctrl, 32 marks motion, 64 turns the low bits into a wheel direction and
/// 128 into the back and forward buttons. Reports that make no sense (a press of no button,
/// a release of the wheel, an unknown button) are `None`.
pub(super) fn decode(code: u32, col: u16, row: u16, release: bool) -> Option<MouseEvent> {
    if code > 0xff {
        return None;
    }
    let low = code & 0b11;
    let motion = code & 32 != 0;
    let kind = if code & 64 != 0 {
        if release {
            return None;
        }
        match low {
            0 => MouseKind::ScrollUp,
            1 => MouseKind::ScrollDown,
            2 => MouseKind::ScrollLeft,
            _ => MouseKind::ScrollRight,
        }
    } else {
        let button = if code & 128 != 0 {
            match low {
                0 => Some(MouseButton::Back),
                1 => Some(MouseButton::Forward),
                _ => return None,
            }
        } else {
            match low {
                0 => Some(MouseButton::Left),
                1 => Some(MouseButton::Middle),
                2 => Some(MouseButton::Right),
                _ => None,
            }
        };
        match (button, motion, release) {
            (Some(button), true, _) => MouseKind::Drag(button),
            (None, true, _) => MouseKind::Move,
            (Some(button), false, true) => MouseKind::Release(button),
            (Some(button), false, false) => MouseKind::Press(button),
            (None, false, _) => return None,
        }
    };
    Some(MouseEvent {
        kind,
        col,
        row,
        mods: mods(code),
    })
}

/// The modifiers a mouse report can carry.
fn mods(code: u32) -> Mods {
    let mut mods = Mods::empty();
    mods.set(Mods::SHIFT, code & 4 != 0);
    mods.set(Mods::ALT, code & 8 != 0);
    mods.set(Mods::CTRL, code & 16 != 0);
    mods
}

/// Decode the three bytes of a legacy `CSI M cb cx cy` report, each of which is its value
/// plus 32 (the caller has checked that none is below 32).
///
/// This form reports a release as "button 3" without saying which button it was, so the
/// caller passes the button of the last press. Returns the event and the button to remember
/// for the next release.
pub(super) fn decode_x10(
    cb: u8,
    cx: u8,
    cy: u8,
    last: MouseButton,
) -> Option<(MouseEvent, MouseButton)> {
    let code = u32::from(cb.saturating_sub(32));
    let col = cell(u32::from(cx.saturating_sub(32)));
    let row = cell(u32::from(cy.saturating_sub(32)));
    let is_release = code & (32 | 64 | 128) == 0 && code & 0b11 == 3;
    if is_release {
        let event = MouseEvent {
            kind: MouseKind::Release(last),
            col,
            row,
            mods: mods(code),
        };
        return Some((event, last));
    }
    let event = decode(code, col, row, false)?;
    let pressed = match event.kind {
        MouseKind::Press(button) => button,
        _ => last,
    };
    Some((event, pressed))
}
