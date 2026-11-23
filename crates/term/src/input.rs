//! Encoding of everything the user sends that is not a key: mouse reports, pastes and focus
//! changes. Pure functions of the event and the modes the child has set.

use bakedit_core::{Mods, MouseButton, MouseEvent, MouseKind};

/// The mouse reporting state of the child (xterm private modes 1000 to 1006).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MouseModes {
    /// Mode 1000: button presses, releases and the wheel.
    pub click: bool,
    /// Mode 1002: the above, plus motion while a button is held.
    pub drag: bool,
    /// Mode 1003: the above, plus motion with no button held.
    pub motion: bool,
    /// Mode 1006: reports as `CSI < button ; col ; row M` (or `m` for a release).
    pub sgr: bool,
    /// Mode 1005: the legacy report with coordinates as UTF-8 characters.
    pub utf8: bool,
}

impl MouseModes {
    /// Whether the child wants mouse events at all.
    pub fn reporting(self) -> bool {
        self.click || self.drag || self.motion
    }
}

/// Encode a mouse event for the child, or `None` when the child should not get it.
///
/// `event.col` and `event.row` are 0-based cells from the terminal's top-left corner. The
/// result is `None` when reporting is off, when the event is a kind of motion the child did
/// not ask for, and when the position does not fit the encoding: the legacy report cannot
/// say a column or row past 223 (2015 with the UTF-8 extension), and SGR has no limit.
pub fn encode_mouse(event: &MouseEvent, modes: MouseModes) -> Option<Vec<u8>> {
    if !modes.reporting() {
        return None;
    }
    let (mut code, release) = match event.kind {
        MouseKind::Press(button) => (button_code(button), false),
        MouseKind::Release(button) => (button_code(button), true),
        MouseKind::Drag(button) if modes.drag || modes.motion => (button_code(button) + 32, false),
        MouseKind::Move if modes.motion => (3 + 32, false),
        MouseKind::Drag(_) | MouseKind::Move => return None,
        MouseKind::ScrollUp => (64, false),
        MouseKind::ScrollDown => (65, false),
        MouseKind::ScrollLeft => (66, false),
        MouseKind::ScrollRight => (67, false),
    };
    // The legacy report has one code for "a button went up".
    if release && !modes.sgr {
        code = 3;
    }
    if event.mods.contains(Mods::SHIFT) {
        code += 4;
    }
    if event.mods.contains(Mods::ALT) {
        code += 8;
    }
    if event.mods.contains(Mods::CTRL) {
        code += 16;
    }
    let col = u32::from(event.col) + 1;
    let row = u32::from(event.row) + 1;

    if modes.sgr {
        let last = if release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{col};{row}{last}").into_bytes());
    }
    let mut out = b"\x1b[M".to_vec();
    for value in [code + 32, col + 32, row + 32] {
        if modes.utf8 {
            // Two UTF-8 bytes at most, as xterm does.
            let c = char::from_u32(value).filter(|_| value <= 0x7ff)?;
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        } else {
            out.push(u8::try_from(value).ok()?);
        }
    }
    Some(out)
}

fn button_code(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::Back => 128,
        MouseButton::Forward => 129,
    }
}

/// The arrow keys a wheel event turns into under alternate scroll mode (1007), which is how
/// the wheel scrolls a pager or an editor on the alternate screen that has not asked for
/// the mouse.
///
/// `lines` arrows are sent, as `SS3` sequences in application cursor mode. Only vertical
/// wheel events translate; anything else is `None`.
pub fn encode_alt_scroll(kind: MouseKind, app_cursor: bool, lines: u16) -> Option<Vec<u8>> {
    let last = match kind {
        MouseKind::ScrollUp => b'A',
        MouseKind::ScrollDown => b'B',
        _ => return None,
    };
    let intro = if app_cursor { b'O' } else { b'[' };
    let mut out = Vec::with_capacity(usize::from(lines) * 3);
    for _ in 0..lines {
        out.extend_from_slice(&[0x1b, intro, last]);
    }
    Some(out)
}

const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

/// Encode pasted text.
///
/// With `bracketed` (the child set mode 2004) the text is wrapped in `CSI 200 ~` and
/// `CSI 201 ~`. The end marker, every remaining `ESC` and every `ctrl-c` byte are removed
/// from the text first, so that what is pasted cannot end the paste early and run as
/// keystrokes. Without it the child cannot tell a paste from typing, so line breaks become
/// carriage returns, which is what the Enter key sends.
///
/// Empty text encodes to nothing.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if text.is_empty() {
        return Vec::new();
    }
    if !bracketed {
        return text.replace("\r\n", "\r").replace('\n', "\r").into_bytes();
    }
    let mut clean = text.to_string();
    // Removing one marker can splice the halves of another together.
    while clean.contains(PASTE_END) {
        clean = clean.replace(PASTE_END, "");
    }
    clean.retain(|c| c != '\x1b' && c != '\x03');
    let mut out = Vec::with_capacity(clean.len() + PASTE_START.len() + PASTE_END.len());
    out.extend_from_slice(PASTE_START.as_bytes());
    out.extend_from_slice(clean.as_bytes());
    out.extend_from_slice(PASTE_END.as_bytes());
    out
}

/// The focus report for mode 1004: `CSI I` when the terminal gains focus, `CSI O` when it
/// loses it.
pub fn encode_focus(focused: bool) -> &'static [u8] {
    if focused {
        b"\x1b[I"
    } else {
        b"\x1b[O"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLICK: MouseModes = MouseModes {
        click: true,
        drag: false,
        motion: false,
        sgr: false,
        utf8: false,
    };
    const DRAG: MouseModes = MouseModes {
        click: false,
        drag: true,
        motion: false,
        sgr: false,
        utf8: false,
    };
    const MOTION: MouseModes = MouseModes {
        click: false,
        drag: false,
        motion: true,
        sgr: false,
        utf8: false,
    };

    fn sgr(modes: MouseModes) -> MouseModes {
        MouseModes { sgr: true, ..modes }
    }

    fn utf8(modes: MouseModes) -> MouseModes {
        MouseModes {
            utf8: true,
            ..modes
        }
    }

    fn at(kind: MouseKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent::new(kind, col, row)
    }

    fn with_mods(kind: MouseKind, mods: Mods) -> MouseEvent {
        MouseEvent {
            kind,
            col: 0,
            row: 0,
            mods,
        }
    }

    fn text(event: MouseEvent, modes: MouseModes) -> Option<String> {
        encode_mouse(&event, modes).map(|bytes| String::from_utf8(bytes).unwrap())
    }

    #[test]
    fn nothing_is_reported_when_the_child_did_not_ask() {
        let off = MouseModes::default();
        assert!(!off.reporting());
        for kind in [
            MouseKind::Press(MouseButton::Left),
            MouseKind::Release(MouseButton::Left),
            MouseKind::Drag(MouseButton::Left),
            MouseKind::Move,
            MouseKind::ScrollUp,
        ] {
            assert_eq!(encode_mouse(&at(kind, 1, 1), off), None);
            // An encoding without a tracking mode is still off.
            assert_eq!(
                encode_mouse(&at(kind, 1, 1), MouseModes { sgr: true, ..off }),
                None
            );
        }
    }

    #[test]
    fn sgr_reports_match_the_kitty_test_suite() {
        let m = sgr(MOTION);
        let left = MouseButton::Left;
        let middle = MouseButton::Middle;
        let right = MouseButton::Right;
        let cases: &[(MouseEvent, &str)] = &[
            (at(MouseKind::Press(left), 0, 0), "\x1b[<0;1;1M"),
            (at(MouseKind::Release(left), 0, 0), "\x1b[<0;1;1m"),
            (at(MouseKind::Move, 0, 0), "\x1b[<35;1;1M"),
            (at(MouseKind::Drag(left), 0, 0), "\x1b[<32;1;1M"),
            (at(MouseKind::Press(right), 0, 0), "\x1b[<2;1;1M"),
            (at(MouseKind::Release(right), 0, 0), "\x1b[<2;1;1m"),
            (at(MouseKind::Drag(right), 0, 0), "\x1b[<34;1;1M"),
            (at(MouseKind::Press(middle), 0, 0), "\x1b[<1;1;1M"),
            (at(MouseKind::Release(middle), 0, 0), "\x1b[<1;1;1m"),
            (at(MouseKind::Drag(middle), 0, 0), "\x1b[<33;1;1M"),
            (at(MouseKind::Press(left), 1233, 5677), "\x1b[<0;1234;5678M"),
            (
                with_mods(MouseKind::Press(left), Mods::SHIFT),
                "\x1b[<4;1;1M",
            ),
            (with_mods(MouseKind::Press(left), Mods::ALT), "\x1b[<8;1;1M"),
            (
                with_mods(MouseKind::Press(left), Mods::CTRL),
                "\x1b[<16;1;1M",
            ),
            (
                with_mods(MouseKind::Press(left), Mods::SHIFT | Mods::ALT | Mods::CTRL),
                "\x1b[<28;1;1M",
            ),
        ];
        for (event, want) in cases {
            assert_eq!(text(*event, m).as_deref(), Some(*want), "{event:?}");
        }
    }

    #[test]
    fn wheel_and_extra_buttons() {
        let m = sgr(CLICK);
        assert_eq!(
            text(at(MouseKind::ScrollUp, 4, 6), m).as_deref(),
            Some("\x1b[<64;5;7M")
        );
        assert_eq!(
            text(at(MouseKind::ScrollDown, 4, 6), m).as_deref(),
            Some("\x1b[<65;5;7M")
        );
        assert_eq!(
            text(at(MouseKind::ScrollLeft, 4, 6), m).as_deref(),
            Some("\x1b[<66;5;7M")
        );
        assert_eq!(
            text(at(MouseKind::ScrollRight, 4, 6), m).as_deref(),
            Some("\x1b[<67;5;7M")
        );
        assert_eq!(
            text(with_mods(MouseKind::ScrollDown, Mods::SHIFT), m).as_deref(),
            Some("\x1b[<69;1;1M")
        );
        assert_eq!(
            text(at(MouseKind::Press(MouseButton::Back), 0, 0), m).as_deref(),
            Some("\x1b[<128;1;1M")
        );
        assert_eq!(
            text(at(MouseKind::Release(MouseButton::Forward), 0, 0), m).as_deref(),
            Some("\x1b[<129;1;1m")
        );
        // The command key is not part of the mouse protocol.
        assert_eq!(
            text(
                with_mods(MouseKind::Press(MouseButton::Left), Mods::SUPER),
                m
            )
            .as_deref(),
            Some("\x1b[<0;1;1M")
        );
    }

    #[test]
    fn motion_is_reported_only_in_the_mode_that_asks_for_it() {
        let left = MouseButton::Left;
        // 1000: clicks and wheel.
        assert!(encode_mouse(&at(MouseKind::Press(left), 0, 0), sgr(CLICK)).is_some());
        assert!(encode_mouse(&at(MouseKind::Release(left), 0, 0), sgr(CLICK)).is_some());
        assert!(encode_mouse(&at(MouseKind::ScrollUp, 0, 0), sgr(CLICK)).is_some());
        assert_eq!(
            encode_mouse(&at(MouseKind::Drag(left), 0, 0), sgr(CLICK)),
            None
        );
        assert_eq!(encode_mouse(&at(MouseKind::Move, 0, 0), sgr(CLICK)), None);
        // 1002: plus drags.
        assert!(encode_mouse(&at(MouseKind::Press(left), 0, 0), sgr(DRAG)).is_some());
        assert!(encode_mouse(&at(MouseKind::Drag(left), 0, 0), sgr(DRAG)).is_some());
        assert_eq!(encode_mouse(&at(MouseKind::Move, 0, 0), sgr(DRAG)), None);
        // 1003: everything.
        assert!(encode_mouse(&at(MouseKind::Drag(left), 0, 0), sgr(MOTION)).is_some());
        assert!(encode_mouse(&at(MouseKind::Move, 0, 0), sgr(MOTION)).is_some());
    }

    #[test]
    fn legacy_reports_are_three_offset_bytes() {
        let left = MouseButton::Left;
        assert_eq!(
            encode_mouse(&at(MouseKind::Press(left), 0, 0), CLICK).unwrap(),
            b"\x1b[M\x20\x21\x21"
        );
        assert_eq!(
            encode_mouse(&at(MouseKind::Press(MouseButton::Right), 9, 4), CLICK).unwrap(),
            b"\x1b[M\x22\x2a\x25"
        );
        // Every release is button 3.
        assert_eq!(
            encode_mouse(&at(MouseKind::Release(left), 0, 0), CLICK).unwrap(),
            b"\x1b[M\x23\x21\x21"
        );
        assert_eq!(
            encode_mouse(&at(MouseKind::Release(MouseButton::Right), 0, 0), CLICK).unwrap(),
            b"\x1b[M\x23\x21\x21"
        );
        assert_eq!(
            encode_mouse(&with_mods(MouseKind::Release(left), Mods::CTRL), CLICK).unwrap(),
            b"\x1b[M\x33\x21\x21"
        );
        assert_eq!(
            encode_mouse(&at(MouseKind::Drag(left), 0, 0), DRAG).unwrap(),
            b"\x1b[M\x40\x21\x21"
        );
        assert_eq!(
            encode_mouse(&at(MouseKind::Move, 0, 0), MOTION).unwrap(),
            b"\x1b[M\x43\x21\x21"
        );
        assert_eq!(
            encode_mouse(&at(MouseKind::ScrollUp, 0, 0), CLICK).unwrap(),
            b"\x1b[M\x60\x21\x21"
        );
        assert_eq!(
            encode_mouse(&at(MouseKind::ScrollDown, 0, 0), CLICK).unwrap(),
            b"\x1b[M\x61\x21\x21"
        );
    }

    #[test]
    fn legacy_reports_stop_at_column_223() {
        let press = MouseKind::Press(MouseButton::Left);
        assert_eq!(
            encode_mouse(&at(press, 222, 222), CLICK).unwrap(),
            b"\x1b[M\x20\xff\xff"
        );
        assert_eq!(encode_mouse(&at(press, 223, 0), CLICK), None);
        assert_eq!(encode_mouse(&at(press, 0, 223), CLICK), None);
        assert_eq!(encode_mouse(&at(press, u16::MAX, u16::MAX), CLICK), None);
    }

    #[test]
    fn utf8_reports_encode_large_values_as_two_bytes() {
        let press = MouseKind::Press(MouseButton::Left);
        // Small values are the same bytes as the legacy report.
        assert_eq!(
            encode_mouse(&at(press, 0, 0), utf8(CLICK)).unwrap(),
            b"\x1b[M\x20\x21\x21"
        );
        // Column 100 is value 133, U+0085.
        assert_eq!(
            encode_mouse(&at(press, 100, 0), utf8(CLICK)).unwrap(),
            b"\x1b[M\x20\xc2\x85\x21"
        );
        assert_eq!(
            encode_mouse(&at(press, 2014, 2014), utf8(CLICK)).unwrap(),
            "\x1b[M \u{7ff}\u{7ff}".as_bytes()
        );
        assert_eq!(encode_mouse(&at(press, 2015, 0), utf8(CLICK)), None);
        // A button code past 127 is two bytes too.
        let back =
            encode_mouse(&at(MouseKind::Press(MouseButton::Back), 0, 0), utf8(CLICK)).unwrap();
        assert_eq!(back, "\x1b[M\u{a0}!!".as_bytes());
        // SGR wins when both are set.
        let both = MouseModes {
            sgr: true,
            utf8: true,
            ..CLICK
        };
        assert_eq!(
            text(at(press, 300, 2), both).as_deref(),
            Some("\x1b[<0;301;3M")
        );
    }

    #[test]
    fn alt_scroll_sends_arrow_keys() {
        assert_eq!(
            encode_alt_scroll(MouseKind::ScrollUp, false, 3).unwrap(),
            b"\x1b[A\x1b[A\x1b[A"
        );
        assert_eq!(
            encode_alt_scroll(MouseKind::ScrollDown, false, 1).unwrap(),
            b"\x1b[B"
        );
        assert_eq!(
            encode_alt_scroll(MouseKind::ScrollUp, true, 2).unwrap(),
            b"\x1bOA\x1bOA"
        );
        assert_eq!(
            encode_alt_scroll(MouseKind::ScrollDown, true, 1).unwrap(),
            b"\x1bOB"
        );
        assert_eq!(
            encode_alt_scroll(MouseKind::ScrollDown, false, 0).unwrap(),
            b""
        );
        assert_eq!(encode_alt_scroll(MouseKind::ScrollLeft, false, 3), None);
        assert_eq!(
            encode_alt_scroll(MouseKind::Press(MouseButton::Left), false, 3),
            None
        );
        assert_eq!(encode_alt_scroll(MouseKind::Move, false, 3), None);
    }

    #[test]
    fn plain_paste_turns_line_breaks_into_enter() {
        assert_eq!(encode_paste("hello", false), b"hello");
        assert_eq!(encode_paste("a\nb\r\nc\rd", false), b"a\rb\rc\rd");
        assert_eq!(encode_paste("héllo 日本", false), "héllo 日本".as_bytes());
        assert_eq!(encode_paste("", false), b"");
    }

    #[test]
    fn bracketed_paste_wraps_the_text() {
        assert_eq!(encode_paste("hello", true), b"\x1b[200~hello\x1b[201~");
        // Line breaks are the child's business now.
        assert_eq!(
            encode_paste("a\nb\r\nc", true),
            b"\x1b[200~a\nb\r\nc\x1b[201~"
        );
        assert_eq!(encode_paste("", true), b"");
    }

    #[test]
    fn bracketed_paste_cannot_be_ended_from_inside() {
        assert_eq!(
            encode_paste("a\x1b[201~rm -rf /\n", true),
            b"\x1b[200~arm -rf /\n\x1b[201~"
        );
        // Markers hidden inside markers.
        assert_eq!(
            encode_paste("a\x1b[20\x1b[201~1~b", true),
            b"\x1b[200~ab\x1b[201~"
        );
        // Any other escape sequence loses its ESC, and ctrl-c is dropped.
        assert_eq!(
            encode_paste("x\x1b[31my\x03z", true),
            b"\x1b[200~x[31myz\x1b[201~"
        );
        assert_eq!(encode_paste("\x1b", true), b"\x1b[200~\x1b[201~");
    }

    #[test]
    fn focus_reports() {
        assert_eq!(encode_focus(true), b"\x1b[I");
        assert_eq!(encode_focus(false), b"\x1b[O");
    }
}
