//! Key events as bakedit understands them.
//!
//! The model follows the kitty keyboard protocol because that is the richest thing a host
//! terminal can report: the unshifted key, the shifted and base-layout alternates, the
//! modifiers, whether the key went down, repeated or came up, and the text it produces.
//! Legacy terminals fill in what they can and leave the rest `None`.

use bitflags::bitflags;

bitflags! {
    /// Keyboard modifiers. The bit layout is the kitty protocol's (its wire value minus 1).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
    pub struct Mods: u8 {
        const SHIFT = 0b0000_0001;
        const ALT   = 0b0000_0010;
        const CTRL  = 0b0000_0100;
        /// The command key on a Mac, the super/windows key elsewhere. Written `cmd` in keymaps.
        const SUPER = 0b0000_1000;
        const HYPER = 0b0001_0000;
        const META  = 0b0010_0000;
    }
}

impl Mods {
    /// Alias so call sites can say what the user says.
    pub const CMD: Mods = Mods::SUPER;

    /// `ctrl-alt-cmd-shift-` style prefix, in the order Zed prints it.
    pub fn prefix(self) -> String {
        let mut out = String::new();
        for (flag, name) in [
            (Mods::CTRL, "ctrl-"),
            (Mods::ALT, "alt-"),
            (Mods::SUPER, "cmd-"),
            (Mods::SHIFT, "shift-"),
            (Mods::HYPER, "hyper-"),
            (Mods::META, "meta-"),
        ] {
            if self.contains(flag) {
                out.push_str(name);
            }
        }
        out
    }
}

/// A modifier key reported on its own (kitty functional codes 57441..=57454).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModKey {
    LeftShift,
    LeftCtrl,
    LeftAlt,
    LeftSuper,
    LeftHyper,
    LeftMeta,
    RightShift,
    RightCtrl,
    RightAlt,
    RightSuper,
    RightHyper,
    RightMeta,
    IsoLevel3Shift,
    IsoLevel5Shift,
}

impl ModKey {
    /// The modifier bit this key contributes while it is held.
    pub fn mods(self) -> Mods {
        match self {
            ModKey::LeftShift | ModKey::RightShift => Mods::SHIFT,
            ModKey::LeftCtrl | ModKey::RightCtrl => Mods::CTRL,
            ModKey::LeftAlt | ModKey::RightAlt => Mods::ALT,
            ModKey::LeftSuper | ModKey::RightSuper => Mods::SUPER,
            ModKey::LeftHyper | ModKey::RightHyper => Mods::HYPER,
            ModKey::LeftMeta | ModKey::RightMeta => Mods::META,
            ModKey::IsoLevel3Shift | ModKey::IsoLevel5Shift => Mods::empty(),
        }
    }

    /// Kitty protocol functional key code.
    pub fn kitty_code(self) -> u32 {
        match self {
            ModKey::LeftShift => 57441,
            ModKey::LeftCtrl => 57442,
            ModKey::LeftAlt => 57443,
            ModKey::LeftSuper => 57444,
            ModKey::LeftHyper => 57445,
            ModKey::LeftMeta => 57446,
            ModKey::RightShift => 57447,
            ModKey::RightCtrl => 57448,
            ModKey::RightAlt => 57449,
            ModKey::RightSuper => 57450,
            ModKey::RightHyper => 57451,
            ModKey::RightMeta => 57452,
            ModKey::IsoLevel3Shift => 57453,
            ModKey::IsoLevel5Shift => 57454,
        }
    }

    pub fn from_kitty_code(code: u32) -> Option<ModKey> {
        Some(match code {
            57441 => ModKey::LeftShift,
            57442 => ModKey::LeftCtrl,
            57443 => ModKey::LeftAlt,
            57444 => ModKey::LeftSuper,
            57445 => ModKey::LeftHyper,
            57446 => ModKey::LeftMeta,
            57447 => ModKey::RightShift,
            57448 => ModKey::RightCtrl,
            57449 => ModKey::RightAlt,
            57450 => ModKey::RightSuper,
            57451 => ModKey::RightHyper,
            57452 => ModKey::RightMeta,
            57453 => ModKey::IsoLevel3Shift,
            57454 => ModKey::IsoLevel5Shift,
            _ => return None,
        })
    }
}

/// Which key. `Char` always holds the *unshifted* key: lower case for letters, `[` not `{`.
/// The space bar is `Char(' ')`. Keypad keys are reported as their ordinary equivalents with
/// [`KeyEvent::keypad`] set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    Enter,
    Escape,
    Tab,
    Backspace,
    Delete,
    Insert,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    /// Function key, 1-based (`F(1)` is F1).
    F(u8),
    CapsLock,
    ScrollLock,
    NumLock,
    PrintScreen,
    Pause,
    Menu,
    /// A modifier key pressed or released on its own.
    Modifier(ModKey),
    /// Any other kitty functional key (media keys and the like), by protocol code.
    Other(u32),
}

impl Key {
    /// The name used in keymap files (`enter`, `pageup`, `space`, `f5`, `a`, `/` ...).
    pub fn name(self) -> String {
        match self {
            Key::Char(' ') => "space".into(),
            Key::Char(c) => c.to_string(),
            Key::Enter => "enter".into(),
            Key::Escape => "escape".into(),
            Key::Tab => "tab".into(),
            Key::Backspace => "backspace".into(),
            Key::Delete => "delete".into(),
            Key::Insert => "insert".into(),
            Key::Up => "up".into(),
            Key::Down => "down".into(),
            Key::Left => "left".into(),
            Key::Right => "right".into(),
            Key::Home => "home".into(),
            Key::End => "end".into(),
            Key::PageUp => "pageup".into(),
            Key::PageDown => "pagedown".into(),
            Key::F(n) => format!("f{n}"),
            Key::CapsLock => "capslock".into(),
            Key::ScrollLock => "scrolllock".into(),
            Key::NumLock => "numlock".into(),
            Key::PrintScreen => "printscreen".into(),
            Key::Pause => "pause".into(),
            Key::Menu => "menu".into(),
            Key::Modifier(m) => format!("{m:?}").to_lowercase(),
            Key::Other(code) => format!("key{code}"),
        }
    }

    /// Parse a keymap key name. Single characters are taken literally, so `A` and `{` come
    /// back as `Char('A')` and `Char('{')`; the keymap decides what a shifted form means.
    pub fn from_name(name: &str) -> Option<Key> {
        let lower = name.to_ascii_lowercase();
        Some(match lower.as_str() {
            "space" => Key::Char(' '),
            "enter" | "return" => Key::Enter,
            "escape" | "esc" => Key::Escape,
            "tab" => Key::Tab,
            "backspace" => Key::Backspace,
            "delete" | "del" => Key::Delete,
            "insert" => Key::Insert,
            "up" => Key::Up,
            "down" => Key::Down,
            "left" => Key::Left,
            "right" => Key::Right,
            "home" => Key::Home,
            "end" => Key::End,
            "pageup" => Key::PageUp,
            "pagedown" => Key::PageDown,
            "capslock" => Key::CapsLock,
            "scrolllock" => Key::ScrollLock,
            "numlock" => Key::NumLock,
            "printscreen" => Key::PrintScreen,
            "pause" => Key::Pause,
            "menu" => Key::Menu,
            _ => {
                if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                    if (1..=35).contains(&n) {
                        return Some(Key::F(n));
                    }
                }
                let mut chars = name.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => Key::Char(c),
                    _ => return None,
                }
            }
        })
    }
}

/// Did the key go down, auto-repeat, or come up. Release (and often repeat) is only reported
/// by terminals that implement the kitty keyboard protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum KeyKind {
    #[default]
    Press,
    Repeat,
    Release,
}

/// One key event from the host terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub mods: Mods,
    pub kind: KeyKind,
    /// The character this key produces with shift on the active layout, when the terminal says.
    pub shifted: Option<char>,
    /// The key at the same position on a US layout, when the terminal says.
    pub base: Option<char>,
    /// Text this event types, if it types any. Set for plain text keys and when the terminal
    /// sends associated text; `None` for shortcuts, navigation keys and releases.
    pub text: Option<String>,
    pub keypad: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
}

impl KeyEvent {
    /// A key press with no text.
    pub fn new(key: Key, mods: Mods) -> KeyEvent {
        KeyEvent {
            key,
            mods,
            kind: KeyKind::Press,
            shifted: None,
            base: None,
            text: None,
            keypad: false,
            caps_lock: false,
            num_lock: false,
        }
    }

    /// The press produced by typing `c`. Upper-case ASCII letters and US shifted symbols are
    /// normalised to their unshifted key plus `SHIFT`, with `shifted` and `text` holding `c`.
    pub fn typed(c: char) -> KeyEvent {
        let mut ev = if c.is_ascii_uppercase() {
            let mut ev = KeyEvent::new(Key::Char(c.to_ascii_lowercase()), Mods::SHIFT);
            ev.shifted = Some(c);
            ev
        } else if let Some(unshifted) = us_unshifted(c) {
            let mut ev = KeyEvent::new(Key::Char(unshifted), Mods::SHIFT);
            ev.shifted = Some(c);
            ev
        } else {
            KeyEvent::new(Key::Char(c), Mods::empty())
        };
        ev.text = Some(c.to_string());
        ev
    }

    pub fn with_kind(mut self, kind: KeyKind) -> KeyEvent {
        self.kind = kind;
        if kind == KeyKind::Release {
            self.text = None;
        }
        self
    }

    pub fn with_text(mut self, text: impl Into<String>) -> KeyEvent {
        self.text = Some(text.into());
        self
    }

    pub fn with_shifted(mut self, shifted: char) -> KeyEvent {
        self.shifted = Some(shifted);
        self
    }

    pub fn is_press(&self) -> bool {
        self.kind == KeyKind::Press
    }

    /// Press or auto-repeat: the events that should act.
    pub fn is_down(&self) -> bool {
        self.kind != KeyKind::Release
    }

    pub fn is_release(&self) -> bool {
        self.kind == KeyKind::Release
    }

    /// The shifted character for this key: what the terminal reported, else the US layout's.
    pub fn shifted_char(&self) -> Option<char> {
        if let Some(c) = self.shifted {
            return Some(c);
        }
        match self.key {
            Key::Char(c) if c.is_ascii_lowercase() => Some(c.to_ascii_uppercase()),
            Key::Char(c) => us_shifted(c),
            _ => None,
        }
    }

    /// Keymap-style spelling, e.g. `cmd-shift-p`. For display and `--dump-input`.
    pub fn display(&self) -> String {
        format!("{}{}", self.mods.prefix(), self.key.name())
    }
}

/// US ANSI layout: the character shift produces on a symbol or digit key.
pub fn us_shifted(c: char) -> Option<char> {
    Some(match c {
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        '`' => '~',
        _ => return None,
    })
}

/// Inverse of [`us_shifted`]: the key that produces `c` with shift on a US layout.
pub fn us_unshifted(c: char) -> Option<char> {
    Some(match c {
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        '~' => '`',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_upper_case_letter_is_shift_plus_lower_key() {
        let ev = KeyEvent::typed('G');
        assert_eq!(ev.key, Key::Char('g'));
        assert_eq!(ev.mods, Mods::SHIFT);
        assert_eq!(ev.shifted, Some('G'));
        assert_eq!(ev.text.as_deref(), Some("G"));
    }

    #[test]
    fn typed_shifted_symbol_is_shift_plus_base_key() {
        let ev = KeyEvent::typed('{');
        assert_eq!(ev.key, Key::Char('['));
        assert_eq!(ev.mods, Mods::SHIFT);
        assert_eq!(ev.shifted, Some('{'));
    }

    #[test]
    fn typed_plain_and_non_ascii() {
        let ev = KeyEvent::typed('a');
        assert_eq!((ev.key, ev.mods), (Key::Char('a'), Mods::empty()));
        let ev = KeyEvent::typed('é');
        assert_eq!((ev.key, ev.mods), (Key::Char('é'), Mods::empty()));
        assert_eq!(ev.text.as_deref(), Some("é"));
    }

    #[test]
    fn release_drops_text() {
        let ev = KeyEvent::typed('a').with_kind(KeyKind::Release);
        assert!(ev.is_release());
        assert_eq!(ev.text, None);
    }

    #[test]
    fn key_names_round_trip() {
        for key in [
            Key::Char(' '),
            Key::Char('a'),
            Key::Char('/'),
            Key::Enter,
            Key::Escape,
            Key::Tab,
            Key::Backspace,
            Key::Delete,
            Key::Up,
            Key::PageDown,
            Key::F(12),
        ] {
            assert_eq!(Key::from_name(&key.name()), Some(key), "{key:?}");
        }
        assert_eq!(Key::from_name("esc"), Some(Key::Escape));
        assert_eq!(Key::from_name("F5"), Some(Key::F(5)));
        assert_eq!(Key::from_name("{"), Some(Key::Char('{')));
        assert_eq!(Key::from_name("nonsense"), None);
    }

    #[test]
    fn display_orders_modifiers_like_zed() {
        let ev = KeyEvent::new(Key::Char('p'), Mods::SUPER | Mods::SHIFT);
        assert_eq!(ev.display(), "cmd-shift-p");
        let ev = KeyEvent::new(Key::Up, Mods::CTRL | Mods::ALT | Mods::SUPER);
        assert_eq!(ev.display(), "ctrl-alt-cmd-up");
    }

    #[test]
    fn shifted_char_prefers_the_terminal_report() {
        // On a JIS layout shift+2 is a double quote; the terminal's report wins over the US table.
        let ev = KeyEvent::new(Key::Char('2'), Mods::SHIFT).with_shifted('"');
        assert_eq!(ev.shifted_char(), Some('"'));
        let ev = KeyEvent::new(Key::Char('2'), Mods::SHIFT);
        assert_eq!(ev.shifted_char(), Some('@'));
    }

    #[test]
    fn modifier_key_codes_round_trip() {
        for code in 57441..=57454 {
            let key = ModKey::from_kitty_code(code).unwrap();
            assert_eq!(key.kitty_code(), code);
        }
        assert_eq!(ModKey::LeftSuper.mods(), Mods::SUPER);
        assert_eq!(ModKey::from_kitty_code(57440), None);
    }
}
