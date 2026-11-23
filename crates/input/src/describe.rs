//! One readable line per input event, for the app's `--dump-input` diagnostic.

use bakedit_core::{
    ColorSlot, InputEvent, KeyEvent, KeyKind, Mods, MouseButton, MouseEvent, MouseKind,
    TerminalReply,
};

use crate::host;

/// The most characters of a paste shown in its description.
const PASTE_PREVIEW_CHARS: usize = 60;

/// Describe `event` on one line.
///
/// A key shows its kind, its keymap spelling (`cmd-shift-p`), and then whatever else the
/// terminal said about it: the text it types, the shifted and base-layout keys, and whether
/// it is a keypad key or caps lock or num lock is on. Text is quoted and escaped, so the
/// line never contains a control character or a line break. For example:
///
/// ```text
/// key press   shift-a text="A" shifted='A'
/// key release ctrl-alt-up
/// mouse press left at 12,3 mods=shift
/// paste 11 chars "hello\nworld"
/// reply kitty-keyboard flags=11 (disambiguate, event-types, all-keys)
/// ```
pub fn describe(event: &InputEvent) -> String {
    match event {
        InputEvent::Key(ev) => describe_key(ev),
        InputEvent::Mouse(ev) => describe_mouse(ev),
        InputEvent::Paste(text) => describe_paste(text),
        InputEvent::FocusGained => "focus gained".to_string(),
        InputEvent::FocusLost => "focus lost".to_string(),
        InputEvent::Reply(reply) => format!("reply {}", describe_reply(reply)),
    }
}

fn describe_key(ev: &KeyEvent) -> String {
    let kind = match ev.kind {
        KeyKind::Press => "press",
        KeyKind::Repeat => "repeat",
        KeyKind::Release => "release",
    };
    let mut line = format!("key {kind:<7} {}", ev.display());
    if let Some(text) = &ev.text {
        line.push_str(&format!(" text={text:?}"));
    }
    if let Some(shifted) = ev.shifted {
        line.push_str(&format!(" shifted={shifted:?}"));
    }
    if let Some(base) = ev.base {
        line.push_str(&format!(" base={base:?}"));
    }
    for (set, name) in [
        (ev.keypad, " keypad"),
        (ev.caps_lock, " caps-lock"),
        (ev.num_lock, " num-lock"),
    ] {
        if set {
            line.push_str(name);
        }
    }
    line
}

fn describe_mouse(ev: &MouseEvent) -> String {
    let button = |button: MouseButton| match button {
        MouseButton::Left => "left",
        MouseButton::Middle => "middle",
        MouseButton::Right => "right",
        MouseButton::Back => "back",
        MouseButton::Forward => "forward",
    };
    let what = match ev.kind {
        MouseKind::Press(b) => format!("press {}", button(b)),
        MouseKind::Release(b) => format!("release {}", button(b)),
        MouseKind::Drag(b) => format!("drag {}", button(b)),
        MouseKind::Move => "move".to_string(),
        MouseKind::ScrollUp => "scroll up".to_string(),
        MouseKind::ScrollDown => "scroll down".to_string(),
        MouseKind::ScrollLeft => "scroll left".to_string(),
        MouseKind::ScrollRight => "scroll right".to_string(),
    };
    let mut line = format!("mouse {what} at {},{}", ev.col, ev.row);
    if !ev.mods.is_empty() {
        line.push_str(&format!(" mods={}", mods_name(ev.mods)));
    }
    line
}

/// `ctrl-shift` style: the keymap prefix without its trailing dash.
fn mods_name(mods: Mods) -> String {
    mods.prefix().trim_end_matches('-').to_string()
}

fn describe_paste(text: &str) -> String {
    let count = text.chars().count();
    let unit = if count == 1 { "char" } else { "chars" };
    if count <= PASTE_PREVIEW_CHARS {
        return format!("paste {count} {unit} {text:?}");
    }
    let preview: String = text.chars().take(PASTE_PREVIEW_CHARS).collect();
    format!("paste {count} {unit} {preview:?}...")
}

fn describe_reply(reply: &TerminalReply) -> String {
    match reply {
        TerminalReply::KittyKeyboardFlags(flags) => {
            format!(
                "kitty-keyboard flags={flags} ({})",
                kitty_flag_names(*flags)
            )
        }
        TerminalReply::DeviceAttributes(attributes) => format!("device-attributes {attributes:?}"),
        TerminalReply::DarkMode(dark) => {
            format!("color-scheme {}", if *dark { "dark" } else { "light" })
        }
        TerminalReply::Color { slot, rgb } => {
            let slot = match slot {
                ColorSlot::Foreground => "foreground".to_string(),
                ColorSlot::Background => "background".to_string(),
                ColorSlot::Cursor => "cursor".to_string(),
                ColorSlot::Palette(index) => format!("palette {index}"),
            };
            format!("color {slot} {}", rgb.to_hex())
        }
        TerminalReply::Version(version) => format!("version {version:?}"),
        TerminalReply::Mode { mode, state } => {
            let meaning = match state {
                0 => "not recognised",
                1 => "set",
                2 => "reset",
                3 => "permanently set",
                4 => "permanently reset",
                _ => "unknown state",
            };
            format!("mode {mode} state={state} ({meaning})")
        }
    }
}

/// The names of the kitty keyboard flags that are set, or `none`.
fn kitty_flag_names(flags: u8) -> String {
    let known = [
        (host::KITTY_DISAMBIGUATE, "disambiguate"),
        (host::KITTY_EVENT_TYPES, "event-types"),
        (host::KITTY_ALTERNATE_KEYS, "alternate-keys"),
        (host::KITTY_ALL_KEYS, "all-keys"),
        (host::KITTY_TEXT, "text"),
    ];
    let mut names: Vec<String> = known
        .iter()
        .filter(|(flag, _)| flags & flag != 0)
        .map(|(_, name)| name.to_string())
        .collect();
    let unknown = flags & !host::KITTY_ALL_FLAGS;
    if unknown != 0 {
        names.push(format!("unknown {unknown:#x}"));
    }
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use bakedit_core::{Key, ModKey, Rgb};

    use super::*;
    use crate::Parser;

    fn key(ev: KeyEvent) -> String {
        describe(&InputEvent::Key(ev))
    }

    #[test]
    fn keys_show_kind_spelling_and_text() {
        assert_eq!(key(KeyEvent::typed('a')), "key press   a text=\"a\"");
        assert_eq!(
            key(KeyEvent::typed('A')),
            "key press   shift-a text=\"A\" shifted='A'"
        );
        assert_eq!(key(KeyEvent::typed(' ')), "key press   space text=\" \"");
        assert_eq!(
            key(KeyEvent::typed('"')),
            "key press   shift-' text=\"\\\"\" shifted='\"'"
        );
        assert_eq!(
            key(KeyEvent::new(Key::Char('p'), Mods::SUPER | Mods::SHIFT)),
            "key press   cmd-shift-p"
        );
        assert_eq!(
            key(KeyEvent::new(Key::Up, Mods::CTRL | Mods::ALT).with_kind(KeyKind::Release)),
            "key release ctrl-alt-up"
        );
        assert_eq!(
            key(KeyEvent::new(Key::Enter, Mods::empty()).with_kind(KeyKind::Repeat)),
            "key repeat  enter"
        );
        assert_eq!(
            key(KeyEvent::new(Key::F(12), Mods::HYPER | Mods::META)),
            "key press   hyper-meta-f12"
        );
        assert_eq!(
            key(KeyEvent::new(Key::Modifier(ModKey::LeftSuper), Mods::SUPER)),
            "key press   cmd-leftsuper"
        );
        assert_eq!(
            key(KeyEvent::new(Key::Other(57428), Mods::empty())),
            "key press   key57428"
        );
    }

    #[test]
    fn keys_show_everything_else_the_terminal_reported() {
        let mut ev = KeyEvent::new(Key::Char('с'), Mods::CTRL);
        ev.base = Some('c');
        assert_eq!(key(ev), "key press   ctrl-с base='c'");

        let mut ev = KeyEvent::new(Key::Char('1'), Mods::empty()).with_text("1");
        ev.keypad = true;
        ev.num_lock = true;
        assert_eq!(key(ev), "key press   1 text=\"1\" keypad num-lock");

        let mut ev = KeyEvent::new(Key::Char('a'), Mods::SHIFT)
            .with_text("A")
            .with_shifted('A');
        ev.base = Some('a');
        ev.caps_lock = true;
        assert_eq!(
            key(ev),
            "key press   shift-a text=\"A\" shifted='A' base='a' caps-lock"
        );

        let ev = KeyEvent::new(Key::Other(0), Mods::empty()).with_text("こん");
        assert_eq!(key(ev), "key press   key0 text=\"こん\"");
    }

    #[test]
    fn mouse_events() {
        let mouse = |kind, mods| {
            describe(&InputEvent::Mouse(MouseEvent {
                kind,
                col: 12,
                row: 3,
                mods,
            }))
        };
        let none = Mods::empty();
        assert_eq!(
            mouse(MouseKind::Press(MouseButton::Left), none),
            "mouse press left at 12,3"
        );
        assert_eq!(
            mouse(MouseKind::Release(MouseButton::Right), none),
            "mouse release right at 12,3"
        );
        assert_eq!(
            mouse(MouseKind::Drag(MouseButton::Middle), none),
            "mouse drag middle at 12,3"
        );
        assert_eq!(
            mouse(MouseKind::Press(MouseButton::Back), none),
            "mouse press back at 12,3"
        );
        assert_eq!(
            mouse(MouseKind::Press(MouseButton::Forward), none),
            "mouse press forward at 12,3"
        );
        assert_eq!(mouse(MouseKind::Move, none), "mouse move at 12,3");
        assert_eq!(mouse(MouseKind::ScrollUp, none), "mouse scroll up at 12,3");
        assert_eq!(
            mouse(MouseKind::ScrollDown, Mods::SHIFT),
            "mouse scroll down at 12,3 mods=shift"
        );
        assert_eq!(
            mouse(MouseKind::ScrollLeft, none),
            "mouse scroll left at 12,3"
        );
        assert_eq!(
            mouse(MouseKind::ScrollRight, none),
            "mouse scroll right at 12,3"
        );
        assert_eq!(
            mouse(
                MouseKind::Press(MouseButton::Left),
                Mods::CTRL | Mods::ALT | Mods::SHIFT
            ),
            "mouse press left at 12,3 mods=ctrl-alt-shift"
        );
    }

    #[test]
    fn pastes_are_quoted_escaped_and_cut_short() {
        let paste = |text: &str| describe(&InputEvent::Paste(text.to_string()));
        assert_eq!(paste(""), "paste 0 chars \"\"");
        assert_eq!(paste("x"), "paste 1 char \"x\"");
        assert_eq!(paste("hello\nworld"), "paste 11 chars \"hello\\nworld\"");
        assert_eq!(
            paste("tab\there \u{1b}[31m"),
            "paste 14 chars \"tab\\there \\u{1b}[31m\""
        );
        let exactly = "é".repeat(60);
        assert_eq!(paste(&exactly), format!("paste 60 chars \"{exactly}\""));
        let long = "日".repeat(61);
        assert_eq!(
            paste(&long),
            format!("paste 61 chars \"{}\"...", "日".repeat(60))
        );
        assert_eq!(
            paste(&"a".repeat(100_000)),
            format!("paste 100000 chars \"{}\"...", "a".repeat(60))
        );
    }

    #[test]
    fn focus_changes() {
        assert_eq!(describe(&InputEvent::FocusGained), "focus gained");
        assert_eq!(describe(&InputEvent::FocusLost), "focus lost");
    }

    #[test]
    fn replies() {
        let reply = |reply| describe(&InputEvent::Reply(reply));
        assert_eq!(
            reply(TerminalReply::KittyKeyboardFlags(0)),
            "reply kitty-keyboard flags=0 (none)"
        );
        assert_eq!(
            reply(TerminalReply::KittyKeyboardFlags(11)),
            "reply kitty-keyboard flags=11 (disambiguate, event-types, all-keys)"
        );
        assert_eq!(
            reply(TerminalReply::KittyKeyboardFlags(31)),
            "reply kitty-keyboard flags=31 (disambiguate, event-types, alternate-keys, all-keys, text)"
        );
        assert_eq!(
            reply(TerminalReply::KittyKeyboardFlags(0x41)),
            "reply kitty-keyboard flags=65 (disambiguate, unknown 0x40)"
        );
        assert_eq!(
            reply(TerminalReply::DeviceAttributes("62;4;22".into())),
            "reply device-attributes \"62;4;22\""
        );
        assert_eq!(
            reply(TerminalReply::DarkMode(true)),
            "reply color-scheme dark"
        );
        assert_eq!(
            reply(TerminalReply::DarkMode(false)),
            "reply color-scheme light"
        );
        let rgb = Rgb::new(0x1a, 0x1b, 0x26);
        let color = |slot| reply(TerminalReply::Color { slot, rgb });
        assert_eq!(
            color(ColorSlot::Foreground),
            "reply color foreground #1a1b26"
        );
        assert_eq!(
            color(ColorSlot::Background),
            "reply color background #1a1b26"
        );
        assert_eq!(color(ColorSlot::Cursor), "reply color cursor #1a1b26");
        assert_eq!(
            color(ColorSlot::Palette(12)),
            "reply color palette 12 #1a1b26"
        );
        assert_eq!(
            reply(TerminalReply::Version("ghostty 1.2.3".into())),
            "reply version \"ghostty 1.2.3\""
        );
        assert_eq!(
            reply(TerminalReply::Version("a\x07b".into())),
            "reply version \"a\\u{7}b\""
        );
        let mode = |state| reply(TerminalReply::Mode { mode: 2026, state });
        assert_eq!(mode(0), "reply mode 2026 state=0 (not recognised)");
        assert_eq!(mode(1), "reply mode 2026 state=1 (set)");
        assert_eq!(mode(2), "reply mode 2026 state=2 (reset)");
        assert_eq!(mode(3), "reply mode 2026 state=3 (permanently set)");
        assert_eq!(mode(4), "reply mode 2026 state=4 (permanently reset)");
        assert_eq!(mode(9), "reply mode 2026 state=9 (unknown state)");
    }

    #[test]
    fn a_description_is_always_one_printable_line() {
        // Whatever the terminal sends, the dump stays one clean line per event.
        let input: &[u8] = b"a\x1b[97;5u\x1b[200~line one\r\nline\ttwo \x1b[31m\x07\x1b[201~\x1bP>|v\x07\x1b[0m\x1b\\\
            \x1b[<0;1;1M\x1b[I\x1b[?62;4c\x1b]11;rgb:00/00/00\x07\x1b[0;;10:233u\x1b[57441;2u";
        let events = Parser::new().feed(input);
        assert_eq!(events.len(), 10);
        for event in &events {
            let line = describe(event);
            assert!(!line.is_empty());
            assert!(!line.chars().any(char::is_control), "{line:?}");
        }
    }
}
