//! Keystrokes as a keymap spells them (`cmd-k`, `ctrl-alt--`, `G`, `{`) and how one is
//! matched against a key event from the host terminal.
//!
//! Parsing follows gpui's `Keystroke::parse`. Matching follows what gpui ends up doing once
//! its platform layer has named the key: a letter keeps its shift modifier (`shift-g`), any
//! other key that shift changes is named by the character it produces with shift dropped
//! (`{`, `?`, `"`), and the character a key press typed is tried as well. On top of that the
//! spelling with an explicit shift (`shift-[`) is accepted, which Zed itself never matches.

use std::fmt;
use std::str::FromStr;

use bakedit_core::{us_shifted, us_unshifted, Key, KeyEvent, Mods};

/// One key press as a keymap names it: the modifiers that must be held and the key.
///
/// The fields are public so a keystroke can be taken apart, but build one with
/// [`Keystroke::parse`] or [`Keystroke::new`]: both keep the one invariant matching relies
/// on, that an upper-case ASCII letter is stored as shift plus the lower-case letter.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Keystroke {
    /// The modifiers that must be held, exactly.
    pub mods: Mods,
    /// The key. A character is stored as written, so `{` stays `{` and is matched through
    /// the character shift produces.
    pub key: Key,
    /// `fn-` was written. A terminal cannot report the fn key, so such a keystroke parses
    /// (Zed's default keymap has `fn-f`) but never matches an event.
    pub function: bool,
}

/// Why a keystroke could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid keystroke \"{keystroke}\": {reason}")]
pub struct KeystrokeError {
    /// The text that failed, a single keystroke or (when there was none at all) the whole
    /// sequence.
    pub keystroke: String,
    /// What is wrong with it.
    pub reason: String,
}

/// What a `-` terminated name in front of the key can be.
enum Prefix {
    Mods(Mods),
    Function,
}

impl Prefix {
    /// Modifier names are matched without regard to ASCII case, as gpui does.
    fn from_name(name: &str) -> Option<Prefix> {
        Some(match name.to_ascii_lowercase().as_str() {
            "ctrl" => Prefix::Mods(Mods::CTRL),
            "alt" | "option" | "opt" => Prefix::Mods(Mods::ALT),
            "shift" => Prefix::Mods(Mods::SHIFT),
            "cmd" | "super" | "win" => Prefix::Mods(Mods::SUPER),
            "secondary" => Prefix::Mods(secondary()),
            "hyper" => Prefix::Mods(Mods::HYPER),
            "meta" => Prefix::Mods(Mods::META),
            "fn" => Prefix::Function,
            _ => return None,
        })
    }
}

/// The modifier `secondary-` stands for: command on a Mac, control everywhere else.
fn secondary() -> Mods {
    if cfg!(target_os = "macos") {
        Mods::SUPER
    } else {
        Mods::CTRL
    }
}

impl Keystroke {
    /// A keystroke from its parts. An upper-case ASCII letter becomes shift plus the
    /// lower-case letter, the same as parsing `G` does.
    pub fn new(key: Key, mods: Mods) -> Keystroke {
        let (key, mods) = unshift_letter(key, mods);
        Keystroke {
            mods,
            key,
            function: false,
        }
    }

    /// Parse one keystroke: modifier names joined with `-`, then the key.
    ///
    /// Modifiers are `ctrl`, `alt` (`option`, `opt`), `shift`, `cmd` (`super`, `win`),
    /// `secondary` (cmd on macOS, ctrl elsewhere), `hyper`, `meta` and `fn`, in any order and
    /// any case. The key is whatever follows them: a key name (`enter`, `space`, `f5`, any
    /// case) or a single character, which may be `-` itself (`cmd--`, `-`), punctuation or a
    /// shifted form (`{`, `?`, `"`). A single upper-case ASCII letter means shift plus that
    /// letter, so `G` and `shift-g` are the same keystroke.
    ///
    /// Errors: no key (`""`, `cmd-`), a modifier on its own (`shift`, which Zed can bind and
    /// a terminal cannot report reliably), an unknown key name (`cmd-nope`), or anything
    /// after the key (`a-b`).
    pub fn parse(text: &str) -> Result<Keystroke, KeystrokeError> {
        let fail = |reason: String| KeystrokeError {
            keystroke: text.to_string(),
            reason,
        };
        let mut mods = Mods::empty();
        let mut function = false;
        // The modifiers are the `-` terminated names at the front. Whatever follows them is
        // the key, which is how `cmd--` and a bare `-` come to name the dash key.
        let mut rest = text;
        while let Some((name, after)) = rest.split_once('-') {
            match Prefix::from_name(name) {
                Some(Prefix::Mods(modifier)) => mods |= modifier,
                Some(Prefix::Function) => function = true,
                None => break,
            }
            rest = after;
        }
        let key = match single_char(rest) {
            Some(c) if c.is_ascii_uppercase() => {
                mods |= Mods::SHIFT;
                Key::Char(c.to_ascii_lowercase())
            }
            Some(c) if c.is_whitespace() || c.is_control() => {
                return Err(fail("the key is not a printable character".to_string()));
            }
            _ if rest.is_empty() => return Err(fail("there is no key".to_string())),
            _ if Prefix::from_name(rest).is_some() => {
                return Err(fail(format!(
                    "\"{rest}\" is a modifier, and a modifier on its own cannot be bound"
                )));
            }
            _ => Key::from_name(rest).ok_or_else(|| fail(format!("unknown key \"{rest}\"")))?,
        };
        Ok(Keystroke {
            mods,
            key,
            function,
        })
    }

    /// Parse the keystrokes of one binding, separated by whitespace (`cmd-k cmd-s`, `g g`).
    /// Fails on the first keystroke that does not parse, or when there is none.
    pub fn parse_sequence(text: &str) -> Result<Vec<Keystroke>, KeystrokeError> {
        let keystrokes = text
            .split_whitespace()
            .map(Keystroke::parse)
            .collect::<Result<Vec<Keystroke>, KeystrokeError>>()?;
        if keystrokes.is_empty() {
            return Err(KeystrokeError {
                keystroke: text.to_string(),
                reason: "there is no keystroke".to_string(),
            });
        }
        Ok(keystrokes)
    }

    /// Whether `event` is this keystroke being typed.
    ///
    /// Releases, modifier keys pressed on their own and keystrokes written with `fn-` never
    /// match. Otherwise an event matches when any of these holds (an upper-case ASCII letter
    /// as the event's key counts as shift plus that letter throughout):
    ///
    /// 1. Direct: the same modifiers and the same key.
    /// 2. Shifted form: shift is held, it turns the key into the keystroke's character, and
    ///    the other modifiers are the same. `cmd-{` matches cmd+shift+`[`. The character is
    ///    the one the terminal reported, else the one the event typed, else the US layout's,
    ///    so a JIS shift+`2` is `"` and not `@`.
    /// 3. Legacy: the key is itself a US shifted symbol with no shift bit, which is how a
    ///    terminal without the kitty protocol sends `{`. It also matches the keystroke
    ///    written with the unshifted key, `shift-[`.
    /// 4. Typed character: the event typed one character other than its key (option+`s`
    ///    typing `ß`), the keystroke names that character, and the modifiers are the same
    ///    once shift and alt, which only chose the character, are left out. This is gpui's
    ///    `key_char` rule.
    /// 5. Base layout: the key is not ASCII and the terminal reported the key at the same
    ///    position on a US layout, which is then tried in its place by rules 1 and 2, so
    ///    `ctrl-c` works on a Cyrillic layout. An ASCII key never falls back this way: on a
    ///    JIS keyboard `[` sits where a US keyboard has `]`.
    pub fn matches(&self, event: &KeyEvent) -> bool {
        if self.function || !event.is_down() || matches!(event.key, Key::Modifier(_)) {
            return false;
        }
        let (key, mods) = unshift_letter(event.key, event.mods);
        if self.is(mods, key) {
            return true;
        }
        let Key::Char(c) = key else {
            return false;
        };
        if mods.contains(Mods::SHIFT) {
            let shifted = shifted_char(event, c, mods);
            if shifted.is_some_and(|s| s != c && self.is(mods - Mods::SHIFT, Key::Char(s))) {
                return true;
            }
        } else if us_unshifted(c).is_some_and(|u| self.is(mods | Mods::SHIFT, Key::Char(u))) {
            return true;
        }
        let character_mods = Mods::SHIFT | Mods::ALT;
        if typed_char(event).is_some_and(|t| t != c && self.is(mods - character_mods, Key::Char(t)))
        {
            return true;
        }
        match event.base {
            Some(base) if !c.is_ascii() && base.is_ascii() => self.matches_base(base, mods),
            _ => false,
        }
    }

    /// Rules 1 and 2 of [`Keystroke::matches`] for the US-layout key `base`.
    fn matches_base(&self, base: char, mods: Mods) -> bool {
        let base = base.to_ascii_lowercase();
        if self.is(mods, Key::Char(base)) {
            return true;
        }
        mods.contains(Mods::SHIFT)
            && us_shift(base).is_some_and(|s| self.is(mods - Mods::SHIFT, Key::Char(s)))
    }

    fn is(&self, mods: Mods, key: Key) -> bool {
        self.mods == mods && self.key == key
    }

    /// The key press that types this keystroke on a US layout, for simulating keys (a
    /// `SendKeystrokes` style action) and for asking the keymap what a binding's own keys
    /// resolve to.
    ///
    /// A character that needs shift on a US layout becomes its unshifted key plus shift, with
    /// the character reported as the shifted form, so `{` gives `[` with shift held. `text`
    /// is set when the press would type a character: a character key with no modifier other
    /// than shift. The event matches this keystroke, except for the keystrokes nothing can
    /// type: one written with `fn-` (an event cannot carry fn) or with shift on a character
    /// that is already shifted (`shift-{`).
    pub fn to_event(&self) -> KeyEvent {
        let Key::Char(c) = self.key else {
            return KeyEvent::new(self.key, self.mods);
        };
        let (key, mods, shifted) = match unshifted_key(c) {
            Some(unshifted) => (unshifted, self.mods | Mods::SHIFT, Some(c)),
            None if self.mods.contains(Mods::SHIFT) => (c, self.mods, us_shift(c)),
            None => (c, self.mods, None),
        };
        let mut event = KeyEvent::new(Key::Char(key), mods);
        event.shifted = shifted;
        if (mods - Mods::SHIFT).is_empty() {
            event.text = Some(shifted.unwrap_or(key).to_string());
        }
        event
    }

    /// The keystroke Zed would name `event`: a letter keeps shift (`shift-g`), any other key
    /// that shift changes becomes the character it produces without shift (`{`, `cmd-"`).
    /// Meant for showing keys (the pending chord in the status bar, a key recorder); the
    /// result matches the event it came from when that event is a press.
    pub fn from_event(event: &KeyEvent) -> Keystroke {
        let (key, mods) = unshift_letter(event.key, event.mods);
        if let Key::Char(c) = key {
            if mods.contains(Mods::SHIFT) && !c.is_ascii_lowercase() {
                if let Some(shifted) = shifted_char(event, c, mods).filter(|s| *s != c) {
                    return Keystroke {
                        mods: mods - Mods::SHIFT,
                        key: Key::Char(shifted),
                        function: false,
                    };
                }
            }
        }
        Keystroke {
            mods,
            key,
            function: false,
        }
    }

    /// Mac style glyphs: `⌃` ctrl, `⌥` alt, `⇧` shift, `⌘` cmd in that order, then the key as
    /// its symbol (`↑ ↓ ← → ⏎ ⇥ ⌫ ⌦ ⎋ ␣ ↖ ↘ ⇞ ⇟`) or upper-cased (`K`, `F5`). `cmd-shift-p`
    /// is `⇧⌘P`. The modifiers without a Mac glyph are spelled out in front (`fn-`, `hyper-`,
    /// `meta-`).
    pub fn display_mac(&self) -> String {
        let mut out = String::new();
        if self.function {
            out.push_str("fn-");
        }
        for (flag, name) in [(Mods::HYPER, "hyper-"), (Mods::META, "meta-")] {
            if self.mods.contains(flag) {
                out.push_str(name);
            }
        }
        for (flag, glyph) in [
            (Mods::CTRL, '⌃'),
            (Mods::ALT, '⌥'),
            (Mods::SHIFT, '⇧'),
            (Mods::SUPER, '⌘'),
        ] {
            if self.mods.contains(flag) {
                out.push(glyph);
            }
        }
        out.push_str(&mac_key_label(self.key));
        out
    }
}

/// The keymap spelling, modifiers in the order `bakedit_core::Mods::prefix` prints them:
/// `ctrl-alt-shift-cmd-p`, `cmd--`, `shift-g`, `space`. `fn-` comes first when present.
/// Parsing the result gives the keystroke back.
impl fmt::Display for Keystroke {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.function {
            f.write_str("fn-")?;
        }
        write!(f, "{}{}", self.mods.prefix(), self.key.name())
    }
}

impl FromStr for Keystroke {
    type Err = KeystrokeError;

    /// The same as [`Keystroke::parse`].
    fn from_str(text: &str) -> Result<Keystroke, KeystrokeError> {
        Keystroke::parse(text)
    }
}

/// The only character of `text`, if it has exactly one.
fn single_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// An upper-case ASCII letter as a key stands for shift plus that letter. Keymaps write `G`
/// for it, and a terminal without the kitty protocol sends `G` as text with no shift bit.
fn unshift_letter(key: Key, mods: Mods) -> (Key, Mods) {
    match key {
        Key::Char(c) if c.is_ascii_uppercase() => {
            (Key::Char(c.to_ascii_lowercase()), mods | Mods::SHIFT)
        }
        _ => (key, mods),
    }
}

/// The single printable character an event typed, if it typed exactly one.
fn typed_char(event: &KeyEvent) -> Option<char> {
    single_char(event.text.as_deref()?).filter(|c| !c.is_control())
}

/// What shift turns the key `c` into for this event: what the terminal reported, else what
/// the event typed (alt would have changed that, so not with alt held), else the US layout's
/// answer. The US table comes last because it is a guess on any other layout.
fn shifted_char(event: &KeyEvent, c: char, mods: Mods) -> Option<char> {
    if event.shifted.is_some() {
        return event.shifted;
    }
    if !mods.contains(Mods::ALT) {
        if let Some(typed) = typed_char(event) {
            return Some(typed);
        }
    }
    us_shift(c)
}

/// What shift turns `c` into on a US layout: the upper-case letter or the shifted symbol.
fn us_shift(c: char) -> Option<char> {
    if c.is_ascii_lowercase() {
        Some(c.to_ascii_uppercase())
    } else {
        us_shifted(c)
    }
}

/// The key that types `c` together with shift, when `c` needs shift: the US key under a
/// shifted symbol, or the lower-case form of an upper-case letter.
fn unshifted_key(c: char) -> Option<char> {
    if let Some(key) = us_unshifted(c) {
        return Some(key);
    }
    if !c.is_uppercase() {
        return None;
    }
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) if l != c => Some(l),
        _ => None,
    }
}

fn mac_key_label(key: Key) -> String {
    let symbol = match key {
        Key::Up => '↑',
        Key::Down => '↓',
        Key::Left => '←',
        Key::Right => '→',
        Key::Enter => '⏎',
        Key::Tab => '⇥',
        Key::Backspace => '⌫',
        Key::Delete => '⌦',
        Key::Escape => '⎋',
        Key::Home => '↖',
        Key::End => '↘',
        Key::PageUp => '⇞',
        Key::PageDown => '⇟',
        Key::Char(' ') => '␣',
        Key::Char(c) => c.to_ascii_uppercase(),
        other => return other.name().to_uppercase(),
    };
    symbol.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bakedit_core::{KeyKind, ModKey};

    fn ks(text: &str) -> Keystroke {
        Keystroke::parse(text).unwrap_or_else(|e| panic!("{e}"))
    }

    fn press(key: Key, mods: Mods) -> KeyEvent {
        KeyEvent::new(key, mods)
    }

    fn ch(c: char, mods: Mods) -> KeyEvent {
        KeyEvent::new(Key::Char(c), mods)
    }

    const NONE: Mods = Mods::empty();

    #[test]
    fn parses_modifiers_and_keys() {
        let cases: &[(&str, Mods, Key)] = &[
            ("a", NONE, Key::Char('a')),
            ("cmd-k", Mods::SUPER, Key::Char('k')),
            ("ctrl-g", Mods::CTRL, Key::Char('g')),
            ("alt-/", Mods::ALT, Key::Char('/')),
            (
                "ctrl-alt-shift-cmd-p",
                Mods::CTRL | Mods::ALT | Mods::SHIFT | Mods::SUPER,
                Key::Char('p'),
            ),
            // Any order, any case, and the aliases.
            ("shift-cmd-p", Mods::SHIFT | Mods::SUPER, Key::Char('p')),
            ("cmd-shift-p", Mods::SHIFT | Mods::SUPER, Key::Char('p')),
            ("CMD-Shift-p", Mods::SHIFT | Mods::SUPER, Key::Char('p')),
            ("option-x", Mods::ALT, Key::Char('x')),
            ("opt-x", Mods::ALT, Key::Char('x')),
            ("super-x", Mods::SUPER, Key::Char('x')),
            ("win-x", Mods::SUPER, Key::Char('x')),
            ("hyper-a", Mods::HYPER, Key::Char('a')),
            ("meta-a", Mods::META, Key::Char('a')),
            ("cmd-cmd-a", Mods::SUPER, Key::Char('a')),
            // Named keys, in any case.
            ("enter", NONE, Key::Enter),
            ("Enter", NONE, Key::Enter),
            ("ESCAPE", NONE, Key::Escape),
            ("space", NONE, Key::Char(' ')),
            ("shift-tab", Mods::SHIFT, Key::Tab),
            ("cmd-backspace", Mods::SUPER, Key::Backspace),
            ("alt-delete", Mods::ALT, Key::Delete),
            ("cmd-alt-left", Mods::SUPER | Mods::ALT, Key::Left),
            (
                "ctrl-alt-cmd-up",
                Mods::CTRL | Mods::ALT | Mods::SUPER,
                Key::Up,
            ),
            ("pageup", NONE, Key::PageUp),
            ("shift-pagedown", Mods::SHIFT, Key::PageDown),
            ("home", NONE, Key::Home),
            ("end", NONE, Key::End),
            ("insert", NONE, Key::Insert),
            ("f4", NONE, Key::F(4)),
            ("cmd-f6", Mods::SUPER, Key::F(6)),
            ("shift-F12", Mods::SHIFT, Key::F(12)),
            // The dash key.
            ("-", NONE, Key::Char('-')),
            ("cmd--", Mods::SUPER, Key::Char('-')),
            ("ctrl-alt--", Mods::CTRL | Mods::ALT, Key::Char('-')),
            ("shift--", Mods::SHIFT, Key::Char('-')),
            ("ctrl-_", Mods::CTRL, Key::Char('_')),
            // Punctuation and shifted forms are kept as written.
            ("{", NONE, Key::Char('{')),
            ("cmd-{", Mods::SUPER, Key::Char('{')),
            ("cmd-}", Mods::SUPER, Key::Char('}')),
            ("?", NONE, Key::Char('?')),
            ("~", NONE, Key::Char('~')),
            ("+", NONE, Key::Char('+')),
            ("cmd-+", Mods::SUPER, Key::Char('+')),
            ("cmd-=", Mods::SUPER, Key::Char('=')),
            ("|", NONE, Key::Char('|')),
            (">", NONE, Key::Char('>')),
            ("<", NONE, Key::Char('<')),
            ("cmd->", Mods::SUPER, Key::Char('>')),
            ("\"", NONE, Key::Char('"')),
            ("'", NONE, Key::Char('\'')),
            ("`", NONE, Key::Char('`')),
            ("ctrl-`", Mods::CTRL, Key::Char('`')),
            ("cmd-alt-,", Mods::SUPER | Mods::ALT, Key::Char(',')),
            ("cmd-\\", Mods::SUPER, Key::Char('\\')),
            ("(", NONE, Key::Char('(')),
            ("@", NONE, Key::Char('@')),
            ("^", NONE, Key::Char('^')),
            ("0", NONE, Key::Char('0')),
            ("cmd-shift-0", Mods::SUPER | Mods::SHIFT, Key::Char('0')),
            // An upper-case ASCII letter is shift plus the letter.
            ("G", Mods::SHIFT, Key::Char('g')),
            ("shift-g", Mods::SHIFT, Key::Char('g')),
            ("shift-G", Mods::SHIFT, Key::Char('g')),
            ("cmd-K", Mods::SUPER | Mods::SHIFT, Key::Char('k')),
            // Other alphabets are taken as written.
            ("ä", NONE, Key::Char('ä')),
            ("Ä", NONE, Key::Char('Ä')),
            ("¥", NONE, Key::Char('¥')),
            ("ctrl-с", Mods::CTRL, Key::Char('с')),
        ];
        for (text, mods, key) in cases {
            let keystroke = ks(text);
            assert_eq!((keystroke.mods, keystroke.key), (*mods, *key), "{text}");
            assert!(!keystroke.function, "{text}");
        }
    }

    #[test]
    fn secondary_is_the_platform_shortcut_modifier() {
        let expected = if cfg!(target_os = "macos") {
            Mods::SUPER
        } else {
            Mods::CTRL
        };
        assert_eq!(ks("secondary-s").mods, expected);
        assert_eq!(ks("secondary-shift-s").mods, expected | Mods::SHIFT);
    }

    #[test]
    fn fn_is_accepted_and_remembered() {
        let keystroke = ks("fn-f");
        assert!(keystroke.function);
        assert_eq!((keystroke.mods, keystroke.key), (NONE, Key::Char('f')));
        let keystroke = ks("ctrl-fn-f");
        assert!(keystroke.function);
        assert_eq!(keystroke.mods, Mods::CTRL);
    }

    #[test]
    fn rejects_what_is_not_a_keystroke() {
        let cases = [
            ("", "there is no key"),
            ("cmd-", "there is no key"),
            ("ctrl-alt-", "there is no key"),
            ("shift", "a modifier on its own"),
            ("ctrl-shift", "a modifier on its own"),
            ("fn", "a modifier on its own"),
            ("nonsense", "unknown key \"nonsense\""),
            ("cmd-nonsense", "unknown key \"nonsense\""),
            ("a-b", "unknown key \"a-b\""),
            ("cmd-a-shift", "unknown key \"a-shift\""),
            ("a--", "unknown key \"a--\""),
            ("cmd---", "unknown key \"--\""),
            ("alt-s->ß", "unknown key \"s->ß\""),
            ("f0", "unknown key \"f0\""),
            ("f36", "unknown key \"f36\""),
            (" ", "not a printable character"),
            ("cmd-\t", "not a printable character"),
            ("\u{7}", "not a printable character"),
        ];
        for (text, reason) in cases {
            let error = Keystroke::parse(text).expect_err(text);
            assert_eq!(error.keystroke, text);
            assert!(error.reason.contains(reason), "{text:?}: {}", error.reason);
        }
        let error = Keystroke::parse("cmd-nope").unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid keystroke \"cmd-nope\": unknown key \"nope\""
        );
    }

    #[test]
    fn parses_sequences() {
        assert_eq!(
            Keystroke::parse_sequence("cmd-k cmd-s").unwrap(),
            vec![ks("cmd-k"), ks("cmd-s")]
        );
        assert_eq!(
            Keystroke::parse_sequence("  g   g ").unwrap(),
            vec![ks("g"), ks("g")]
        );
        assert_eq!(
            Keystroke::parse_sequence("cmd-k \"").unwrap(),
            vec![ks("cmd-k"), ks("\"")]
        );
        assert_eq!(
            Keystroke::parse_sequence("space").unwrap(),
            vec![ks("space")]
        );
        let error = Keystroke::parse_sequence("cmd-k nope").unwrap_err();
        assert_eq!(error.keystroke, "nope");
        for empty in ["", "   "] {
            let error = Keystroke::parse_sequence(empty).unwrap_err();
            assert!(error.reason.contains("no keystroke"), "{}", error.reason);
        }
    }

    #[test]
    fn canonical_spellings_round_trip_unchanged() {
        for text in [
            "a",
            "cmd-k",
            "ctrl-alt-shift-cmd-p",
            "cmd--",
            "ctrl-alt--",
            "-",
            "ctrl-_",
            "{",
            "cmd-{",
            "?",
            "~",
            "+",
            "|",
            ">",
            "\"",
            "'",
            "`",
            "shift-g",
            "space",
            "shift-space",
            "enter",
            "escape",
            "shift-tab",
            "alt-cmd-left",
            "f12",
            "cmd-f6",
            "fn-f",
            "fn-ctrl-f",
            "cmd-hyper-meta-a",
            "ä",
            "Ä",
            "¥",
        ] {
            assert_eq!(ks(text).to_string(), text);
            assert_eq!(text.parse::<Keystroke>().unwrap(), ks(text));
        }
    }

    #[test]
    fn every_spelling_parses_back_to_the_same_keystroke() {
        for (text, canonical) in [
            ("G", "shift-g"),
            ("cmd-K", "shift-cmd-k"),
            ("cmd-shift-p", "shift-cmd-p"),
            ("cmd-alt-t", "alt-cmd-t"),
            ("option-x", "alt-x"),
            ("win-x", "cmd-x"),
            ("ESC", "escape"),
            ("Return", "enter"),
            ("cmd-ctrl-fn-f", "fn-ctrl-cmd-f"),
            ("meta-hyper-a", "hyper-meta-a"),
        ] {
            let keystroke = ks(text);
            assert_eq!(keystroke.to_string(), canonical, "{text}");
            assert_eq!(ks(&keystroke.to_string()), keystroke, "{text}");
        }
    }

    #[test]
    fn new_normalises_upper_case_letters() {
        assert_eq!(Keystroke::new(Key::Char('G'), NONE), ks("G"));
        assert_eq!(Keystroke::new(Key::Char('g'), Mods::SHIFT), ks("G"));
        assert_eq!(Keystroke::new(Key::Enter, Mods::SUPER), ks("cmd-enter"));
        assert_eq!(Keystroke::new(Key::Char('Ä'), NONE), ks("Ä"));
    }

    #[test]
    fn displays_mac_glyphs() {
        for (text, glyphs) in [
            ("cmd-k", "⌘K"),
            ("cmd-shift-p", "⇧⌘P"),
            ("ctrl-alt-shift-cmd-p", "⌃⌥⇧⌘P"),
            ("G", "⇧G"),
            ("ctrl-`", "⌃`"),
            ("cmd--", "⌘-"),
            ("cmd-{", "⌘{"),
            ("cmd-alt-left", "⌥⌘←"),
            ("up", "↑"),
            ("down", "↓"),
            ("right", "→"),
            ("shift-enter", "⇧⏎"),
            ("shift-tab", "⇧⇥"),
            ("cmd-backspace", "⌘⌫"),
            ("alt-delete", "⌥⌦"),
            ("escape", "⎋"),
            ("space", "␣"),
            ("cmd-home", "⌘↖"),
            ("cmd-end", "⌘↘"),
            ("pageup", "⇞"),
            ("pagedown", "⇟"),
            ("f5", "F5"),
            ("cmd-f12", "⌘F12"),
            ("insert", "INSERT"),
            ("fn-f", "fn-F"),
            ("hyper-cmd-a", "hyper-⌘A"),
            ("ä", "ä"),
        ] {
            assert_eq!(ks(text).display_mac(), glyphs, "{text}");
        }
    }

    #[test]
    fn direct_match_needs_the_same_key_and_exactly_the_same_modifiers() {
        assert!(ks("cmd-k").matches(&ch('k', Mods::SUPER)));
        assert!(!ks("cmd-k").matches(&ch('k', NONE)));
        assert!(!ks("cmd-k").matches(&ch('k', Mods::SUPER | Mods::ALT)));
        assert!(!ks("cmd-k").matches(&ch('k', Mods::CTRL)));
        assert!(!ks("cmd-k").matches(&ch('j', Mods::SUPER)));
        assert!(!ks("k").matches(&ch('k', Mods::SUPER)));
        assert!(ks("cmd-alt-left").matches(&press(Key::Left, Mods::SUPER | Mods::ALT)));
        assert!(!ks("cmd-alt-left").matches(&press(Key::Right, Mods::SUPER | Mods::ALT)));
        assert!(ks("enter").matches(&press(Key::Enter, NONE)));
        assert!(!ks("enter").matches(&press(Key::Enter, Mods::SHIFT)));
        assert!(ks("f5").matches(&press(Key::F(5), NONE)));
        assert!(ks("space").matches(&KeyEvent::typed(' ')));
        assert!(ks("cmd--").matches(&ch('-', Mods::SUPER)));
        assert!(ks("ctrl-alt--").matches(&ch('-', Mods::CTRL | Mods::ALT)));
        assert!(ks("hyper-a").matches(&ch('a', Mods::HYPER)));
        // A repeat acts like a press.
        assert!(ks("j").matches(&KeyEvent::typed('j').with_kind(KeyKind::Repeat)));
        // Lock keys and the keypad flag do not get in the way.
        let mut keypad = ch('5', NONE);
        keypad.keypad = true;
        keypad.num_lock = true;
        assert!(ks("5").matches(&keypad));
    }

    #[test]
    fn shift_stays_on_named_keys_and_letters() {
        assert!(ks("shift-tab").matches(&press(Key::Tab, Mods::SHIFT)));
        assert!(!ks("tab").matches(&press(Key::Tab, Mods::SHIFT)));
        assert!(!ks("shift-tab").matches(&press(Key::Tab, NONE)));
        assert!(ks("shift-up").matches(&press(Key::Up, Mods::SHIFT)));
        assert!(ks("cmd-shift-e").matches(&ch('e', Mods::SUPER | Mods::SHIFT)));
        assert!(!ks("cmd-e").matches(&ch('e', Mods::SUPER | Mods::SHIFT)));
        assert!(!ks("cmd-shift-e").matches(&ch('e', Mods::SUPER)));
        // Shift+space is not space, even if a terminal reports the space as its shifted form.
        let shift_space = ch(' ', Mods::SHIFT).with_shifted(' ').with_text(" ");
        assert!(ks("shift-space").matches(&shift_space));
        assert!(!ks("space").matches(&shift_space));
    }

    #[test]
    fn upper_case_letter_is_shift_plus_the_letter() {
        let shift_g = KeyEvent::typed('G');
        assert!(ks("G").matches(&shift_g));
        assert!(ks("shift-g").matches(&shift_g));
        assert!(!ks("g").matches(&shift_g));
        assert!(!ks("G").matches(&KeyEvent::typed('g')));
        assert!(ks("g").matches(&KeyEvent::typed('g')));
        // Without the terminal's shifted report.
        assert!(ks("G").matches(&ch('g', Mods::SHIFT)));
        assert!(ks("cmd-K").matches(&ch('k', Mods::SUPER | Mods::SHIFT)));
        // A sloppy event that carries the upper-case letter as its key.
        assert!(ks("G").matches(&ch('G', Mods::SHIFT)));
        // A legacy terminal sends "G" as text and cannot report shift at all.
        assert!(ks("G").matches(&ch('G', NONE).with_text("G")));
        assert!(!ks("g").matches(&ch('G', NONE).with_text("G")));
        // Caps lock types an upper-case letter without making it shift+letter.
        let mut caps = KeyEvent::typed('g').with_text("G");
        caps.caps_lock = true;
        assert!(ks("g").matches(&caps));
        assert!(!ks("G").matches(&caps));
    }

    #[test]
    fn shifted_form_matches_the_character_shift_produces() {
        // What a kitty-protocol terminal sends for cmd+shift+[ on a US layout.
        let cmd_shift_bracket = ch('[', Mods::SUPER | Mods::SHIFT).with_shifted('{');
        assert!(ks("cmd-{").matches(&cmd_shift_bracket));
        assert!(ks("cmd-shift-[").matches(&cmd_shift_bracket));
        assert!(!ks("cmd-[").matches(&cmd_shift_bracket));
        assert!(!ks("{").matches(&cmd_shift_bracket));
        assert!(!ks("cmd-}").matches(&cmd_shift_bracket));
        // Shift on top of the shifted character is not a keystroke anything types.
        assert!(!ks("cmd-shift-{").matches(&cmd_shift_bracket));

        for (text, key) in [
            ("{", '['),
            ("}", ']'),
            ("?", '/'),
            ("~", '`'),
            ("+", '='),
            ("|", '\\'),
            (">", '.'),
            ("<", ','),
            ("\"", '\''),
            (":", ';'),
            ("_", '-'),
            ("!", '1'),
            ("(", '9'),
        ] {
            let typed = KeyEvent::typed(ks(text).key_char());
            assert_eq!(typed.key, Key::Char(key), "{text}");
            assert!(ks(text).matches(&typed), "{text} typed");
            // The same press when the terminal reports neither the shifted form nor the text.
            assert!(ks(text).matches(&ch(key, Mods::SHIFT)), "{text} bare");
            assert!(ks(&format!("shift-{key}")).matches(&typed), "shift-{key}");
            assert!(!ks(&key.to_string()).matches(&typed), "{key} unshifted");
            assert!(!ks(text).matches(&ch(key, NONE)), "{text} without shift");
        }
    }

    #[test]
    fn the_terminals_shifted_character_wins_over_the_us_table() {
        // A JIS keyboard: shift+2 is a double quote, shift+7 an apostrophe, shift+@ a
        // backtick, shift+- an equals sign and shift+8 an opening parenthesis.
        let shift_2 = ch('2', Mods::SHIFT).with_shifted('"').with_text("\"");
        assert!(ks("\"").matches(&shift_2));
        assert!(ks("shift-2").matches(&shift_2));
        assert!(!ks("@").matches(&shift_2));
        assert!(!ks("2").matches(&shift_2));

        let cmd_shift_2 = ch('2', Mods::SUPER | Mods::SHIFT).with_shifted('"');
        assert!(ks("cmd-\"").matches(&cmd_shift_2));
        assert!(ks("cmd-shift-2").matches(&cmd_shift_2));
        assert!(!ks("cmd-@").matches(&cmd_shift_2));
        assert!(!ks("\"").matches(&cmd_shift_2));

        // Characters that are not shifted symbols on a US layout at all.
        assert!(ks("'").matches(&ch('7', Mods::SHIFT).with_shifted('\'')));
        assert!(!ks("&").matches(&ch('7', Mods::SHIFT).with_shifted('\'')));
        assert!(ks("`").matches(&ch('@', Mods::SHIFT).with_shifted('`')));
        assert!(ks("ctrl-`").matches(&ch('@', Mods::CTRL | Mods::SHIFT).with_shifted('`')));
        assert!(ks("cmd-=").matches(&ch('-', Mods::SUPER | Mods::SHIFT).with_shifted('=')));
        assert!(!ks("cmd-_").matches(&ch('-', Mods::SUPER | Mods::SHIFT).with_shifted('=')));
        assert!(ks("(").matches(&ch('8', Mods::SHIFT).with_shifted('(')));
        assert!(!ks("*").matches(&ch('8', Mods::SHIFT).with_shifted('(')));

        // The unshifted JIS keys are plain direct matches.
        assert!(ks("@").matches(&ch('@', NONE).with_text("@")));
        assert!(ks("cmd-alt-2").matches(&ch('2', Mods::SUPER | Mods::ALT)));
    }

    #[test]
    fn the_typed_text_stands_in_when_no_shifted_form_is_reported() {
        // The same JIS shift+2 from a terminal that reports the text but not the alternates.
        let shift_2 = ch('2', Mods::SHIFT).with_text("\"");
        assert!(ks("\"").matches(&shift_2));
        assert!(!ks("@").matches(&shift_2));
        assert!(ks("shift-2").matches(&shift_2));
        // With nothing reported the US layout is the only guide left.
        let bare = ch('2', Mods::SHIFT);
        assert!(ks("@").matches(&bare));
        assert!(!ks("\"").matches(&bare));
        assert!(ks("cmd-@").matches(&ch('2', Mods::SUPER | Mods::SHIFT)));
        assert!(ks("alt-@").matches(&ch('2', Mods::ALT | Mods::SHIFT)));
    }

    #[test]
    fn legacy_shifted_symbol_without_a_shift_bit() {
        // An old terminal sends "{" as plain text.
        let brace = ch('{', NONE).with_text("{");
        assert!(ks("{").matches(&brace));
        assert!(ks("shift-[").matches(&brace));
        assert!(!ks("[").matches(&brace));
        assert!(!ks("}").matches(&brace));
        assert!(!ks("shift-]").matches(&brace));
        let cmd_brace = ch('{', Mods::SUPER);
        assert!(ks("cmd-{").matches(&cmd_brace));
        assert!(ks("cmd-shift-[").matches(&cmd_brace));
        assert!(!ks("cmd-[").matches(&cmd_brace));
        assert!(ks("?").matches(&ch('?', NONE)));
        assert!(ks("shift-/").matches(&ch('?', NONE)));
        assert!(ks("shift-2").matches(&ch('@', NONE)));
        // An unshifted key is never rewritten.
        assert!(!ks("shift-[").matches(&ch('[', NONE)));
    }

    #[test]
    fn typed_character_matches_with_shift_and_alt_left_out() {
        // Option+s on a Mac layout types a sharp s.
        let alt_s = ch('s', Mods::ALT).with_text("ß");
        assert!(ks("ß").matches(&alt_s));
        assert!(ks("alt-s").matches(&alt_s));
        assert!(!ks("s").matches(&alt_s));
        assert!(!ks("alt-ß").matches(&alt_s));
        // A German layout types an at sign with option+l and a brace with option+8.
        assert!(ks("@").matches(&ch('l', Mods::ALT).with_text("@")));
        assert!(ks("{").matches(&ch('8', Mods::ALT).with_text("{")));
        assert!(ks("alt-8").matches(&ch('8', Mods::ALT).with_text("{")));
        // Text that is just the key adds nothing.
        assert!(!ks("alt-a").matches(&KeyEvent::typed('a')));
        // Control characters and longer text are not characters to bind.
        assert!(!ks("a").matches(&ch('c', Mods::CTRL).with_text("\u{3}")));
        assert!(ks("ctrl-c").matches(&ch('c', Mods::CTRL).with_text("\u{3}")));
        assert!(!ks("a").matches(&ch('x', NONE).with_text("ab")));
        // A dead key: the press of `e` typed an accented letter.
        let accented = ch('e', NONE).with_text("é");
        assert!(ks("é").matches(&accented));
        assert!(ks("e").matches(&accented));
    }

    #[test]
    fn non_ascii_upper_case_letters_go_through_the_shifted_form() {
        let shift_a_umlaut = ch('ä', Mods::SHIFT).with_shifted('Ä');
        assert!(ks("Ä").matches(&shift_a_umlaut));
        assert!(ks("shift-ä").matches(&shift_a_umlaut));
        assert!(!ks("ä").matches(&shift_a_umlaut));
        assert!(!ks("Ä").matches(&ch('ä', NONE)));
        assert!(ks("cmd-Ö").matches(&ch('ö', Mods::SUPER | Mods::SHIFT).with_shifted('Ö')));
    }

    #[test]
    fn base_layout_key_is_used_for_non_ascii_keys_only() {
        // A Cyrillic layout: the key that types "с" sits where a US keyboard has "c".
        let mut ctrl_es = ch('с', Mods::CTRL);
        ctrl_es.base = Some('c');
        assert!(ks("ctrl-c").matches(&ctrl_es));
        assert!(ks("ctrl-с").matches(&ctrl_es));
        assert!(!ks("ctrl-x").matches(&ctrl_es));
        let mut typed_es = KeyEvent::typed('с');
        typed_es.base = Some('c');
        assert!(ks("c").matches(&typed_es));
        assert!(ks("с").matches(&typed_es));
        let mut shift_zhe = ch('ж', Mods::SUPER | Mods::SHIFT).with_shifted('Ж');
        shift_zhe.base = Some(';');
        assert!(ks("cmd-shift-;").matches(&shift_zhe));
        assert!(ks("cmd-:").matches(&shift_zhe));
        assert!(ks("cmd-Ж").matches(&shift_zhe));
        let mut shift_es = ch('с', Mods::SHIFT).with_shifted('С');
        shift_es.base = Some('c');
        assert!(ks("C").matches(&shift_es));
        // Without a reported base there is nothing to fall back to.
        assert!(!ks("ctrl-c").matches(&ch('с', Mods::CTRL)));
        // On a JIS keyboard "[" sits where a US keyboard has "]": the layout's own key wins.
        let mut cmd_bracket = ch('[', Mods::SUPER);
        cmd_bracket.base = Some(']');
        assert!(ks("cmd-[").matches(&cmd_bracket));
        assert!(!ks("cmd-]").matches(&cmd_bracket));
    }

    #[test]
    fn releases_modifier_keys_and_fn_keystrokes_never_match() {
        let release = KeyEvent::typed('k').with_kind(KeyKind::Release);
        assert!(!ks("k").matches(&release));
        let cmd_release = ch('k', Mods::SUPER).with_kind(KeyKind::Release);
        assert!(!ks("cmd-k").matches(&cmd_release));
        let shift_down = press(Key::Modifier(ModKey::LeftShift), Mods::SHIFT);
        assert!(
            !Keystroke::new(Key::Modifier(ModKey::LeftShift), Mods::SHIFT).matches(&shift_down)
        );
        assert!(!ks("fn-f").matches(&KeyEvent::typed('f')));
        assert!(!ks("ctrl-fn-f").matches(&ch('f', Mods::CTRL)));
    }

    #[test]
    fn to_event_types_the_keystroke_on_a_us_layout() {
        let event = ks("cmd-k").to_event();
        assert_eq!((event.key, event.mods), (Key::Char('k'), Mods::SUPER));
        assert_eq!((event.shifted, event.text.as_deref()), (None, None));
        assert!(event.is_press());

        let event = ks("j").to_event();
        assert_eq!(event, KeyEvent::typed('j'));
        assert_eq!(ks("G").to_event(), KeyEvent::typed('G'));
        assert_eq!(ks("{").to_event(), KeyEvent::typed('{'));
        assert_eq!(ks("space").to_event(), KeyEvent::typed(' '));

        let event = ks("cmd-{").to_event();
        assert_eq!(event.key, Key::Char('['));
        assert_eq!(event.mods, Mods::SUPER | Mods::SHIFT);
        assert_eq!((event.shifted, event.text.as_deref()), (Some('{'), None));

        let event = ks("shift-2").to_event();
        assert_eq!((event.key, event.mods), (Key::Char('2'), Mods::SHIFT));
        assert_eq!(
            (event.shifted, event.text.as_deref()),
            (Some('@'), Some("@"))
        );

        let event = ks("Ä").to_event();
        assert_eq!((event.key, event.mods), (Key::Char('ä'), Mods::SHIFT));
        assert_eq!(
            (event.shifted, event.text.as_deref()),
            (Some('Ä'), Some("Ä"))
        );

        let event = ks("shift-enter").to_event();
        assert_eq!((event.key, event.mods), (Key::Enter, Mods::SHIFT));
        assert_eq!(event.text, None);
    }

    #[test]
    fn a_keystroke_matches_its_own_event() {
        for text in [
            "a",
            "G",
            "cmd-k",
            "cmd-K",
            "ctrl-alt-shift-cmd-p",
            "cmd--",
            "-",
            "ctrl-_",
            "{",
            "cmd-{",
            "cmd-shift-[",
            "shift-2",
            "cmd-shift-0",
            "?",
            "\"",
            "'",
            "`",
            "ctrl-`",
            "space",
            "shift-space",
            "enter",
            "shift-tab",
            "cmd-alt-left",
            "f5",
            "hyper-meta-a",
            "ä",
            "Ä",
            "¥",
        ] {
            let keystroke = ks(text);
            let event = keystroke.to_event();
            assert!(keystroke.matches(&event), "{text}");
        }
        // The two spellings of one press match each other's events.
        assert!(ks("cmd-{").matches(&ks("cmd-shift-[").to_event()));
        assert!(ks("cmd-shift-[").matches(&ks("cmd-{").to_event()));
        // Nothing types these, so not even their own event matches.
        assert!(!ks("fn-f").matches(&ks("fn-f").to_event()));
        assert!(!ks("shift-{").matches(&ks("shift-{").to_event()));
    }

    #[test]
    fn from_event_names_a_press_the_way_zed_does() {
        for (event, text) in [
            (ch('k', Mods::SUPER), "cmd-k"),
            (KeyEvent::typed('j'), "j"),
            (KeyEvent::typed('G'), "shift-g"),
            (ch('G', NONE), "shift-g"),
            (KeyEvent::typed('{'), "{"),
            (ch('[', Mods::SUPER | Mods::SHIFT), "cmd-{"),
            (ch('{', NONE), "{"),
            (
                ch('2', Mods::SUPER | Mods::SHIFT).with_shifted('"'),
                "cmd-\"",
            ),
            (ch('ä', Mods::SHIFT).with_shifted('Ä'), "Ä"),
            (press(Key::Tab, Mods::SHIFT), "shift-tab"),
            (press(Key::Enter, NONE), "enter"),
            (ch(' ', Mods::SHIFT).with_shifted(' '), "shift-space"),
            (KeyEvent::typed(' '), "space"),
        ] {
            let keystroke = Keystroke::from_event(&event);
            assert_eq!(keystroke.to_string(), text);
            assert!(keystroke.matches(&event), "{text}");
        }
        for text in [
            "cmd-k",
            "shift-g",
            "{",
            "cmd-{",
            "shift-tab",
            "space",
            "cmd--",
        ] {
            assert_eq!(
                Keystroke::from_event(&ks(text).to_event()),
                ks(text),
                "{text}"
            );
        }
    }

    impl Keystroke {
        /// The character of a character key, for building test events.
        fn key_char(&self) -> char {
            match self.key {
                Key::Char(c) => c,
                other => panic!("{other:?} is not a character key"),
            }
        }
    }
}
