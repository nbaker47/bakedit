//! Every control string bakedit writes to the host terminal.
//!
//! Nothing here does I/O: these are the bytes to write. [`enter_sequence`] and
//! [`leave_sequence`] put the startup and shutdown strings together in the right order; the
//! rest are the individual pieces for use while the app runs. The replies to the queries
//! come back through [`Parser`](crate::Parser) as
//! [`InputEvent::Reply`](bakedit_core::InputEvent::Reply).

/// Kitty keyboard flag: report Escape, alt+key and ctrl+key as unambiguous `CSI u` codes.
pub const KITTY_DISAMBIGUATE: u8 = 1;
/// Kitty keyboard flag: report key repeats and key releases.
pub const KITTY_EVENT_TYPES: u8 = 2;
/// Kitty keyboard flag: report the shifted and base-layout keys along with the key.
pub const KITTY_ALTERNATE_KEYS: u8 = 4;
/// Kitty keyboard flag: report every key as an escape code, text keys and modifiers included.
pub const KITTY_ALL_KEYS: u8 = 8;
/// Kitty keyboard flag: report the text a key types inside its escape code. Only defined
/// together with [`KITTY_ALL_KEYS`].
pub const KITTY_TEXT: u8 = 16;
/// All five kitty keyboard flags.
pub const KITTY_ALL_FLAGS: u8 =
    KITTY_DISAMBIGUATE | KITTY_EVENT_TYPES | KITTY_ALTERNATE_KEYS | KITTY_ALL_KEYS | KITTY_TEXT;

/// `CSI > flags u`: push the current kitty keyboard flags on the terminal's stack and make
/// `flags` current. The stack is per screen, so push after entering the alternate screen.
pub fn kitty_push(flags: u8) -> String {
    format!("\x1b[>{flags}u")
}

/// `CSI < u`: pop one entry off the kitty keyboard stack, restoring the flags that were
/// active before the matching [`kitty_push`].
pub const KITTY_POP: &str = "\x1b[<u";

/// `CSI ? u`: ask for the active kitty keyboard flags. A terminal that implements the
/// protocol answers `CSI ? flags u` (`TerminalReply::KittyKeyboardFlags`); one that does
/// not stays silent, which is why a round of queries ends with
/// [`DEVICE_ATTRIBUTES_QUERY`].
pub const KITTY_QUERY: &str = "\x1b[?u";

/// `CSI c`: ask for the primary device attributes. Every terminal answers
/// (`TerminalReply::DeviceAttributes`), so the reply marks the end of a round of queries.
pub const DEVICE_ATTRIBUTES_QUERY: &str = "\x1b[c";

/// `CSI > 0 q` (XTVERSION): ask for the terminal's name and version
/// (`TerminalReply::Version`).
pub const VERSION_QUERY: &str = "\x1b[>0q";

/// Mode 1000 on: report mouse button presses, releases and the wheel.
pub const MOUSE_BUTTONS_ON: &str = "\x1b[?1000h";
/// Mode 1000 off.
pub const MOUSE_BUTTONS_OFF: &str = "\x1b[?1000l";
/// Mode 1002 on: also report motion while a button is held.
pub const MOUSE_DRAG_ON: &str = "\x1b[?1002h";
/// Mode 1002 off.
pub const MOUSE_DRAG_OFF: &str = "\x1b[?1002l";
/// Mode 1003 on: also report motion with no button held.
pub const MOUSE_MOTION_ON: &str = "\x1b[?1003h";
/// Mode 1003 off.
pub const MOUSE_MOTION_OFF: &str = "\x1b[?1003l";
/// Mode 1006 on: mouse reports in the SGR form, which has no coordinate limit and tells
/// releases apart.
pub const MOUSE_SGR_ON: &str = "\x1b[?1006h";
/// Mode 1006 off.
pub const MOUSE_SGR_OFF: &str = "\x1b[?1006l";
/// All four mouse modes off, whichever were on.
pub const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l";

/// Mode 1004 on: report the terminal gaining and losing focus.
pub const FOCUS_EVENTS_ON: &str = "\x1b[?1004h";
/// Mode 1004 off.
pub const FOCUS_EVENTS_OFF: &str = "\x1b[?1004l";

/// Mode 2004 on: wrap pasted text in markers so it arrives as one paste, not as keys.
pub const BRACKETED_PASTE_ON: &str = "\x1b[?2004h";
/// Mode 2004 off.
pub const BRACKETED_PASTE_OFF: &str = "\x1b[?2004l";

/// Mode 1049 on: save the cursor, switch to the alternate screen and clear it.
pub const ALTERNATE_SCREEN_ON: &str = "\x1b[?1049h";
/// Mode 1049 off: back to the main screen with the cursor restored.
pub const ALTERNATE_SCREEN_OFF: &str = "\x1b[?1049l";

/// Mode 2026 on: begin a synchronized update. The terminal holds back drawing until
/// [`SYNC_END`], so a frame appears at once. Terminals without the mode ignore it.
pub const SYNC_BEGIN: &str = "\x1b[?2026h";
/// Mode 2026 off: end the synchronized update and draw.
pub const SYNC_END: &str = "\x1b[?2026l";

/// Show the cursor.
pub const CURSOR_SHOW: &str = "\x1b[?25h";
/// Hide the cursor.
pub const CURSOR_HIDE: &str = "\x1b[?25l";

/// `OSC 10 ; ?`: ask for the default foreground colour (`TerminalReply::Color`).
pub const FOREGROUND_QUERY: &str = "\x1b]10;?\x1b\\";
/// `OSC 11 ; ?`: ask for the default background colour.
pub const BACKGROUND_QUERY: &str = "\x1b]11;?\x1b\\";
/// `OSC 12 ; ?`: ask for the cursor colour.
pub const CURSOR_COLOR_QUERY: &str = "\x1b]12;?\x1b\\";

/// `CSI ? 996 n`: ask whether the terminal is in dark or light mode
/// (`TerminalReply::DarkMode`).
pub const COLOR_SCHEME_QUERY: &str = "\x1b[?996n";
/// Mode 2031 on: have the terminal send the [`COLOR_SCHEME_QUERY`] reply by itself whenever
/// its colour scheme changes.
pub const COLOR_SCHEME_UPDATES_ON: &str = "\x1b[?2031h";
/// Mode 2031 off.
pub const COLOR_SCHEME_UPDATES_OFF: &str = "\x1b[?2031l";

/// `OSC 4 ; index ; ?`: ask for one of the 256 palette colours.
pub fn palette_query(index: u8) -> String {
    format!("\x1b]4;{index};?\x1b\\")
}

/// `CSI ? mode h` or `l`: set or reset a DEC private mode not covered by a constant.
pub fn private_mode(mode: u16, on: bool) -> String {
    format!("\x1b[?{mode}{}", if on { 'h' } else { 'l' })
}

/// `CSI ? mode $ p` (DECRQM): ask whether a DEC private mode is set
/// (`TerminalReply::Mode`). A terminal that does not know DECRQM stays silent.
pub fn mode_query(mode: u16) -> String {
    format!("\x1b[?{mode}$p")
}

/// How much of the mouse the host terminal reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MouseTracking {
    /// Nothing: the terminal keeps the mouse for its own text selection.
    Off,
    /// Presses, releases and the wheel (mode 1000).
    Buttons,
    /// Also motion while a button is held (mode 1002).
    #[default]
    Drag,
    /// Also motion with no button held (mode 1003).
    AnyMotion,
}

/// The string that switches mouse reporting to `tracking` from any other level: the modes
/// above it are reset first, then its own are set, with SGR coordinates (1006).
pub fn mouse_tracking(tracking: MouseTracking) -> String {
    let parts: &[&str] = match tracking {
        MouseTracking::Off => &[MOUSE_OFF],
        MouseTracking::Buttons => &[
            MOUSE_MOTION_OFF,
            MOUSE_DRAG_OFF,
            MOUSE_BUTTONS_ON,
            MOUSE_SGR_ON,
        ],
        MouseTracking::Drag => &[
            MOUSE_MOTION_OFF,
            MOUSE_BUTTONS_ON,
            MOUSE_DRAG_ON,
            MOUSE_SGR_ON,
        ],
        MouseTracking::AnyMotion => &[
            MOUSE_BUTTONS_ON,
            MOUSE_DRAG_ON,
            MOUSE_MOTION_ON,
            MOUSE_SGR_ON,
        ],
    };
    parts.concat()
}

/// The modes that turn `tracking` on from nothing, in the order they are set.
fn mouse_modes_on(tracking: MouseTracking) -> &'static [&'static str] {
    match tracking {
        MouseTracking::Off => &[],
        MouseTracking::Buttons => &[MOUSE_BUTTONS_ON, MOUSE_SGR_ON],
        MouseTracking::Drag => &[MOUSE_BUTTONS_ON, MOUSE_DRAG_ON, MOUSE_SGR_ON],
        MouseTracking::AnyMotion => &[
            MOUSE_BUTTONS_ON,
            MOUSE_DRAG_ON,
            MOUSE_MOTION_ON,
            MOUSE_SGR_ON,
        ],
    }
}

/// The cursor shapes of DECSCUSR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CursorShape {
    /// Whatever the user configured in the terminal.
    #[default]
    Default,
    /// A blinking block.
    BlinkingBlock,
    /// A block that does not blink.
    SteadyBlock,
    /// A blinking underline.
    BlinkingUnderline,
    /// An underline that does not blink.
    SteadyUnderline,
    /// A blinking vertical bar.
    BlinkingBar,
    /// A vertical bar that does not blink.
    SteadyBar,
}

/// `CSI n SP q` (DECSCUSR): set the cursor shape.
pub fn cursor_shape(shape: CursorShape) -> &'static str {
    match shape {
        CursorShape::Default => "\x1b[0 q",
        CursorShape::BlinkingBlock => "\x1b[1 q",
        CursorShape::SteadyBlock => "\x1b[2 q",
        CursorShape::BlinkingUnderline => "\x1b[3 q",
        CursorShape::SteadyUnderline => "\x1b[4 q",
        CursorShape::BlinkingBar => "\x1b[5 q",
        CursorShape::SteadyBar => "\x1b[6 q",
    }
}

/// `OSC 2 ; title ST`: set the window title. Control characters are left out of the title,
/// so it cannot end the sequence early or smuggle in another one.
pub fn set_title(title: &str) -> String {
    let clean: String = title.chars().filter(|c| !c.is_control()).collect();
    format!("\x1b]2;{clean}\x1b\\")
}

/// `OSC 52 ; c ; base64 ST`: put `text` on the system clipboard. Terminals may refuse
/// (many ask the user first) and some limit the size; there is no reply either way.
pub fn clipboard_write(text: &str) -> String {
    format!("\x1b]52;c;{}\x1b\\", base64_encode(text.as_bytes()))
}

/// Standard base64 (RFC 4648) with `=` padding.
pub fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let symbol = |n: u32, shift: u32| char::from(ALPHABET[((n >> shift) & 63) as usize]);
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let byte = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let n = (byte(0) << 16) | (byte(1) << 8) | byte(2);
        out.push(symbol(n, 18));
        out.push(symbol(n, 12));
        out.push(if chunk.len() > 1 { symbol(n, 6) } else { '=' });
        out.push(if chunk.len() > 2 { symbol(n, 0) } else { '=' });
    }
    out
}

/// What [`enter_sequence`] sets up and [`leave_sequence`] takes down. The default turns
/// everything on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostOptions {
    /// Run on the alternate screen, leaving the user's scrollback as it was.
    pub alternate_screen: bool,
    /// The kitty keyboard flags to push; 0 leaves the keyboard in legacy mode.
    pub kitty_flags: u8,
    /// How much of the mouse to ask for.
    pub mouse: MouseTracking,
    /// Report focus gained and lost.
    pub focus_events: bool,
    /// Deliver pastes as one event.
    pub bracketed_paste: bool,
    /// Report colour scheme (dark/light) changes.
    pub color_scheme_updates: bool,
    /// Hide the cursor on entry; the app shows it where it wants it.
    pub hide_cursor: bool,
    /// End the enter sequence with [`query_sequence`].
    pub queries: bool,
}

impl Default for HostOptions {
    fn default() -> HostOptions {
        HostOptions {
            alternate_screen: true,
            kitty_flags: KITTY_ALL_FLAGS,
            mouse: MouseTracking::default(),
            focus_events: true,
            bracketed_paste: true,
            color_scheme_updates: true,
            hide_cursor: true,
            queries: true,
        }
    }
}

/// The round of startup queries: kitty keyboard flags, foreground and background colour,
/// colour scheme, terminal version and, last, the primary device attributes.
///
/// Terminals answer in order and every terminal answers the last one, so the
/// `DeviceAttributes` reply means the round is over: whatever has not been answered by then
/// is not supported. Send it after the kitty flags are pushed and the reported flags are
/// the ones that took effect.
pub fn query_sequence() -> String {
    [
        KITTY_QUERY,
        FOREGROUND_QUERY,
        BACKGROUND_QUERY,
        COLOR_SCHEME_QUERY,
        VERSION_QUERY,
        DEVICE_ATTRIBUTES_QUERY,
    ]
    .concat()
}

/// Everything to write at startup, in order: alternate screen, hide the cursor, kitty
/// keyboard flags (after the screen switch, because the flag stack is per screen), mouse,
/// focus events, bracketed paste, colour scheme updates and finally the queries.
pub fn enter_sequence(opts: &HostOptions) -> String {
    let mut out = String::new();
    if opts.alternate_screen {
        out.push_str(ALTERNATE_SCREEN_ON);
    }
    if opts.hide_cursor {
        out.push_str(CURSOR_HIDE);
    }
    if opts.kitty_flags != 0 {
        out.push_str(&kitty_push(opts.kitty_flags));
    }
    for mode in mouse_modes_on(opts.mouse) {
        out.push_str(mode);
    }
    if opts.focus_events {
        out.push_str(FOCUS_EVENTS_ON);
    }
    if opts.bracketed_paste {
        out.push_str(BRACKETED_PASTE_ON);
    }
    if opts.color_scheme_updates {
        out.push_str(COLOR_SCHEME_UPDATES_ON);
    }
    if opts.queries {
        out.push_str(&query_sequence());
    }
    out
}

/// Everything to write at shutdown: what [`enter_sequence`] did for the same options,
/// undone in reverse order, so the kitty flags are popped before the alternate screen is
/// left.
///
/// Three things are restored whatever the options say, because the app changes them while
/// it runs: a synchronized update is ended first, and the cursor gets its default shape
/// back and is shown. That makes the string safe to write from a panic hook too. All four
/// mouse modes are reset if any mouse tracking was on.
pub fn leave_sequence(opts: &HostOptions) -> String {
    let mut out = String::from(SYNC_END);
    if opts.color_scheme_updates {
        out.push_str(COLOR_SCHEME_UPDATES_OFF);
    }
    if opts.bracketed_paste {
        out.push_str(BRACKETED_PASTE_OFF);
    }
    if opts.focus_events {
        out.push_str(FOCUS_EVENTS_OFF);
    }
    if opts.mouse != MouseTracking::Off {
        out.push_str(MOUSE_OFF);
    }
    if opts.kitty_flags != 0 {
        out.push_str(KITTY_POP);
    }
    out.push_str(cursor_shape(CursorShape::Default));
    out.push_str(CURSOR_SHOW);
    if opts.alternate_screen {
        out.push_str(ALTERNATE_SCREEN_OFF);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every combination of the options.
    fn all_options() -> Vec<HostOptions> {
        let mut all = Vec::new();
        for bits in 0u32..(1 << 7) {
            let on = |bit: u32| bits & (1 << bit) != 0;
            for mouse in [
                MouseTracking::Off,
                MouseTracking::Buttons,
                MouseTracking::Drag,
                MouseTracking::AnyMotion,
            ] {
                all.push(HostOptions {
                    alternate_screen: on(0),
                    kitty_flags: if on(1) { KITTY_ALL_FLAGS } else { 0 },
                    mouse,
                    focus_events: on(2),
                    bracketed_paste: on(3),
                    color_scheme_updates: on(4),
                    hide_cursor: on(5),
                    queries: on(6),
                });
            }
        }
        all
    }

    /// Split a string of control sequences into its sequences, checking that it is nothing
    /// but well-formed CSI and OSC sequences.
    fn sequences(text: &str) -> Vec<&str> {
        let bytes = text.as_bytes();
        let mut out = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            let start = at;
            assert_eq!(bytes[at], 0x1b, "expected ESC at {at} in {text:?}");
            match bytes.get(at + 1) {
                Some(b'[') => {
                    at += 2;
                    while bytes.get(at).is_some_and(|b| (0x30..=0x3f).contains(b)) {
                        at += 1;
                    }
                    while bytes.get(at).is_some_and(|b| (0x20..=0x2f).contains(b)) {
                        at += 1;
                    }
                    assert!(
                        bytes.get(at).is_some_and(|b| (0x40..=0x7e).contains(b)),
                        "unfinished CSI in {text:?}"
                    );
                    at += 1;
                }
                Some(b']') => {
                    let end = text[at..].find("\x1b\\").expect("OSC without ST");
                    assert!(
                        !text[at + 2..at + end].chars().any(char::is_control),
                        "control in OSC: {text:?}"
                    );
                    at += end + 2;
                }
                other => panic!("unexpected {other:?} after ESC in {text:?}"),
            }
            out.push(&text[start..at]);
        }
        out
    }

    #[test]
    fn kitty_keyboard_strings() {
        assert_eq!(
            (
                KITTY_DISAMBIGUATE,
                KITTY_EVENT_TYPES,
                KITTY_ALTERNATE_KEYS,
                KITTY_ALL_KEYS,
                KITTY_TEXT
            ),
            (1, 2, 4, 8, 16)
        );
        assert_eq!(KITTY_ALL_FLAGS, 31);
        assert_eq!(kitty_push(0), "\x1b[>0u");
        assert_eq!(
            kitty_push(KITTY_DISAMBIGUATE | KITTY_EVENT_TYPES | KITTY_ALL_KEYS),
            "\x1b[>11u"
        );
        assert_eq!(kitty_push(KITTY_ALL_FLAGS), "\x1b[>31u");
        assert_eq!(KITTY_POP, "\x1b[<u");
        assert_eq!(KITTY_QUERY, "\x1b[?u");
    }

    #[test]
    fn queries() {
        assert_eq!(DEVICE_ATTRIBUTES_QUERY, "\x1b[c");
        assert_eq!(VERSION_QUERY, "\x1b[>0q");
        assert_eq!(FOREGROUND_QUERY, "\x1b]10;?\x1b\\");
        assert_eq!(BACKGROUND_QUERY, "\x1b]11;?\x1b\\");
        assert_eq!(CURSOR_COLOR_QUERY, "\x1b]12;?\x1b\\");
        assert_eq!(palette_query(0), "\x1b]4;0;?\x1b\\");
        assert_eq!(palette_query(255), "\x1b]4;255;?\x1b\\");
        assert_eq!(COLOR_SCHEME_QUERY, "\x1b[?996n");
        assert_eq!(mode_query(2026), "\x1b[?2026$p");
        assert_eq!(
            query_sequence(),
            "\x1b[?u\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[?996n\x1b[>0q\x1b[c",
            "the device attributes query goes last: its reply ends the round"
        );
    }

    #[test]
    fn modes() {
        assert_eq!(
            (MOUSE_BUTTONS_ON, MOUSE_BUTTONS_OFF),
            ("\x1b[?1000h", "\x1b[?1000l")
        );
        assert_eq!(
            (MOUSE_DRAG_ON, MOUSE_DRAG_OFF),
            ("\x1b[?1002h", "\x1b[?1002l")
        );
        assert_eq!(
            (MOUSE_MOTION_ON, MOUSE_MOTION_OFF),
            ("\x1b[?1003h", "\x1b[?1003l")
        );
        assert_eq!(
            (MOUSE_SGR_ON, MOUSE_SGR_OFF),
            ("\x1b[?1006h", "\x1b[?1006l")
        );
        assert_eq!(MOUSE_OFF, "\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l");
        assert_eq!(
            (FOCUS_EVENTS_ON, FOCUS_EVENTS_OFF),
            ("\x1b[?1004h", "\x1b[?1004l")
        );
        assert_eq!(
            (BRACKETED_PASTE_ON, BRACKETED_PASTE_OFF),
            ("\x1b[?2004h", "\x1b[?2004l")
        );
        assert_eq!(
            (ALTERNATE_SCREEN_ON, ALTERNATE_SCREEN_OFF),
            ("\x1b[?1049h", "\x1b[?1049l")
        );
        assert_eq!((SYNC_BEGIN, SYNC_END), ("\x1b[?2026h", "\x1b[?2026l"));
        assert_eq!((CURSOR_SHOW, CURSOR_HIDE), ("\x1b[?25h", "\x1b[?25l"));
        assert_eq!(
            (COLOR_SCHEME_UPDATES_ON, COLOR_SCHEME_UPDATES_OFF),
            ("\x1b[?2031h", "\x1b[?2031l")
        );
        assert_eq!(private_mode(2027, true), "\x1b[?2027h");
        assert_eq!(private_mode(2027, false), "\x1b[?2027l");
    }

    #[test]
    fn mouse_tracking_switches_from_any_level() {
        assert_eq!(mouse_tracking(MouseTracking::Off), MOUSE_OFF);
        assert_eq!(
            mouse_tracking(MouseTracking::Buttons),
            "\x1b[?1003l\x1b[?1002l\x1b[?1000h\x1b[?1006h"
        );
        assert_eq!(
            mouse_tracking(MouseTracking::Drag),
            "\x1b[?1003l\x1b[?1000h\x1b[?1002h\x1b[?1006h"
        );
        assert_eq!(
            mouse_tracking(MouseTracking::AnyMotion),
            "\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h"
        );
        assert_eq!(MouseTracking::default(), MouseTracking::Drag);
    }

    #[test]
    fn cursor_shapes() {
        let shapes = [
            (CursorShape::Default, "\x1b[0 q"),
            (CursorShape::BlinkingBlock, "\x1b[1 q"),
            (CursorShape::SteadyBlock, "\x1b[2 q"),
            (CursorShape::BlinkingUnderline, "\x1b[3 q"),
            (CursorShape::SteadyUnderline, "\x1b[4 q"),
            (CursorShape::BlinkingBar, "\x1b[5 q"),
            (CursorShape::SteadyBar, "\x1b[6 q"),
        ];
        for (shape, expected) in shapes {
            assert_eq!(cursor_shape(shape), expected);
        }
        assert_eq!(CursorShape::default(), CursorShape::Default);
    }

    #[test]
    fn the_title_cannot_break_out_of_its_sequence() {
        assert_eq!(set_title("bakedit"), "\x1b]2;bakedit\x1b\\");
        assert_eq!(set_title(""), "\x1b]2;\x1b\\");
        assert_eq!(
            set_title("src/main.rs — bakedit ✓ 日本"),
            "\x1b]2;src/main.rs — bakedit ✓ 日本\x1b\\"
        );
        // ESC, BEL, newline, tab, DEL and the C1 controls (among them the one-character ST).
        assert_eq!(
            set_title("a\x1b]52;c;eA==\x07b\x1b\\c\nd\te\x7ff\u{9c}g\u{9b}h\u{0}i"),
            "\x1b]2;a]52;c;eA==b\\cdefghi\x1b\\"
        );
        assert_eq!(sequences(&set_title("x\x1by\x07")).len(), 1);
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        let vectors = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (plain, encoded) in vectors {
            assert_eq!(base64_encode(plain.as_bytes()), encoded);
        }
        assert_eq!(base64_encode(&[0xfb, 0xff]), "+/8=");
        assert_eq!(base64_encode(&[0xfb, 0xef, 0xbe]), "++++");
        assert_eq!(base64_encode(&[0xff, 0xff, 0xff]), "////");
        assert_eq!(base64_encode(&[0, 0, 0, 0]), "AAAAAA==");
    }

    /// A straightforward decoder to check the encoder against.
    fn base64_decode(text: &str) -> Vec<u8> {
        let value = |c: u8| match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            other => panic!("not base64: {other}"),
        };
        let mut out = Vec::new();
        for group in text.as_bytes().chunks(4) {
            assert_eq!(group.len(), 4, "base64 comes in groups of four");
            let data: Vec<u8> = group.iter().copied().filter(|&c| c != b'=').collect();
            let n =
                data.iter().fold(0u32, |acc, &c| (acc << 6) | value(c)) << (6 * (4 - data.len()));
            let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
            out.extend_from_slice(&bytes[..data.len() - 1]);
        }
        out
    }

    #[test]
    fn base64_round_trips_every_length_and_byte() {
        let all: Vec<u8> = (0..=255u8).collect();
        for len in 0..=all.len() {
            let encoded = base64_encode(&all[..len]);
            assert_eq!(encoded.len(), len.div_ceil(3) * 4);
            assert_eq!(base64_decode(&encoded), &all[..len], "length {len}");
        }
        let reversed: Vec<u8> = all.iter().rev().copied().collect();
        assert_eq!(base64_decode(&base64_encode(&reversed)), reversed);
    }

    #[test]
    fn clipboard_write_is_osc_52() {
        assert_eq!(clipboard_write("hello"), "\x1b]52;c;aGVsbG8=\x1b\\");
        assert_eq!(clipboard_write(""), "\x1b]52;c;\x1b\\");
        assert_eq!(
            clipboard_write("日本\n\x1b[31m"),
            "\x1b]52;c;5pel5pysChtbMzFt\x1b\\"
        );
        assert_eq!(sequences(&clipboard_write("a\x1b\\b\x07c")).len(), 1);
    }

    #[test]
    fn the_default_enter_and_leave_strings() {
        let opts = HostOptions::default();
        assert_eq!(
            enter_sequence(&opts),
            concat!(
                "\x1b[?1049h",
                "\x1b[?25l",
                "\x1b[>31u",
                "\x1b[?1000h\x1b[?1002h\x1b[?1006h",
                "\x1b[?1004h",
                "\x1b[?2004h",
                "\x1b[?2031h",
                "\x1b[?u\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[?996n\x1b[>0q\x1b[c",
            )
        );
        assert_eq!(
            leave_sequence(&opts),
            concat!(
                "\x1b[?2026l",
                "\x1b[?2031l",
                "\x1b[?2004l",
                "\x1b[?1004l",
                "\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l",
                "\x1b[<u",
                "\x1b[0 q",
                "\x1b[?25h",
                "\x1b[?1049l",
            )
        );
    }

    #[test]
    fn with_everything_off_enter_is_empty_and_leave_only_restores_the_cursor() {
        let opts = HostOptions {
            alternate_screen: false,
            kitty_flags: 0,
            mouse: MouseTracking::Off,
            focus_events: false,
            bracketed_paste: false,
            color_scheme_updates: false,
            hide_cursor: false,
            queries: false,
        };
        assert_eq!(enter_sequence(&opts), "");
        assert_eq!(leave_sequence(&opts), "\x1b[?2026l\x1b[0 q\x1b[?25h");
    }

    /// `Some((mode, on))` for a `CSI ? mode h/l` sequence.
    fn private_mode_of(sequence: &str) -> Option<(u32, bool)> {
        let body = sequence.strip_prefix("\x1b[?")?;
        let on = match body.chars().last()? {
            'h' => true,
            'l' => false,
            _ => return None,
        };
        Some((body[..body.len() - 1].parse().ok()?, on))
    }

    #[test]
    fn leave_undoes_everything_enter_did_in_reverse() {
        for opts in all_options() {
            let enter = enter_sequence(&opts);
            let leave = leave_sequence(&opts);
            let entered = sequences(&enter);
            let left = sequences(&leave);

            // Every mode enter sets is reset by leave, and in the opposite order.
            let set: Vec<u32> = entered
                .iter()
                .filter_map(|s| private_mode_of(s))
                .filter(|(_, on)| *on)
                .map(|(mode, _)| mode)
                .collect();
            let position = |mode: u32| {
                left.iter()
                    .position(|s| private_mode_of(s) == Some((mode, mode == 25)))
            };
            let mut last = usize::MAX;
            for mode in &set {
                let at = position(*mode)
                    .unwrap_or_else(|| panic!("mode {mode} is not undone: {opts:?}"));
                assert!(at < last, "mode {mode} is undone out of order: {opts:?}");
                last = at;
            }
            // Enter never resets a mode, and hides the cursor only on request.
            let reset: Vec<u32> = entered
                .iter()
                .filter_map(|s| private_mode_of(s))
                .filter(|(_, on)| !*on)
                .map(|(mode, _)| mode)
                .collect();
            assert_eq!(
                reset,
                if opts.hide_cursor { vec![25] } else { vec![] },
                "{opts:?}"
            );
            // The cursor always comes back.
            assert!(
                left.contains(&CURSOR_SHOW) && left.contains(&"\x1b[0 q"),
                "{opts:?}"
            );

            // The keyboard flags are pushed and popped exactly once, inside the alternate
            // screen: pushed after entering it, popped before leaving it.
            let pushes = entered
                .iter()
                .filter(|s| s.starts_with("\x1b[>") && s.ends_with('u'))
                .count();
            let pops = left.iter().filter(|s| **s == KITTY_POP).count();
            assert_eq!(
                (pushes, pops),
                if opts.kitty_flags != 0 {
                    (1, 1)
                } else {
                    (0, 0)
                },
                "{opts:?}"
            );
            if opts.alternate_screen {
                assert_eq!(entered.first(), Some(&ALTERNATE_SCREEN_ON), "{opts:?}");
                assert_eq!(left.last(), Some(&ALTERNATE_SCREEN_OFF), "{opts:?}");
            } else {
                assert!(
                    !enter.contains("1049") && !leave.contains("1049"),
                    "{opts:?}"
                );
            }

            // Queries come last on the way in and never on the way out.
            assert_eq!(enter.ends_with(&query_sequence()), opts.queries, "{opts:?}");
            assert!(
                !leave.contains(DEVICE_ATTRIBUTES_QUERY) && !leave.contains(']'),
                "{opts:?}"
            );
        }
    }

    #[test]
    fn every_string_is_a_run_of_well_formed_sequences() {
        let constants = [
            KITTY_POP,
            KITTY_QUERY,
            DEVICE_ATTRIBUTES_QUERY,
            VERSION_QUERY,
            MOUSE_BUTTONS_ON,
            MOUSE_BUTTONS_OFF,
            MOUSE_DRAG_ON,
            MOUSE_DRAG_OFF,
            MOUSE_MOTION_ON,
            MOUSE_MOTION_OFF,
            MOUSE_SGR_ON,
            MOUSE_SGR_OFF,
            FOCUS_EVENTS_ON,
            FOCUS_EVENTS_OFF,
            BRACKETED_PASTE_ON,
            BRACKETED_PASTE_OFF,
            ALTERNATE_SCREEN_ON,
            ALTERNATE_SCREEN_OFF,
            SYNC_BEGIN,
            SYNC_END,
            CURSOR_SHOW,
            CURSOR_HIDE,
            FOREGROUND_QUERY,
            BACKGROUND_QUERY,
            CURSOR_COLOR_QUERY,
            COLOR_SCHEME_QUERY,
            COLOR_SCHEME_UPDATES_ON,
            COLOR_SCHEME_UPDATES_OFF,
        ];
        for constant in constants {
            assert_eq!(sequences(constant).len(), 1, "{constant:?}");
        }
        assert_eq!(sequences(MOUSE_OFF).len(), 4);
        assert_eq!(sequences(&query_sequence()).len(), 6);
        for opts in all_options() {
            sequences(&enter_sequence(&opts));
            sequences(&leave_sequence(&opts));
        }
    }
}
