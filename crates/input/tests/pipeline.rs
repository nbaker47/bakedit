//! The crate as the app uses it, through its public API only: bytes into the parser, key
//! events through the modifier tracker and the nav layer, and what the keymap would see
//! out of the other end.

use std::time::{Duration, Instant};

use bakedit_core::{
    InputEvent, Key, KeyEvent, KeyKind, Mods, MouseButton, MouseKind, TerminalReply,
};
use bakedit_input::{describe, host, HeldMods, NavLayer, NavLayerConfig, Parser};

/// A miniature of the app's input loop.
struct App {
    parser: Parser,
    held: HeldMods,
    nav: NavLayer,
    now: Instant,
    /// What reaches the keymap.
    keys: Vec<KeyEvent>,
    /// Left clicks, with every modifier the app can know about.
    clicks: Vec<(u16, u16, Mods)>,
    /// Everything else, described.
    log: Vec<String>,
}

impl App {
    /// An app that has just started: the nav layer stays off until the terminal has shown
    /// that it reports key releases.
    fn new() -> App {
        App {
            parser: Parser::new(),
            held: HeldMods::new(),
            nav: NavLayer::new(NavLayerConfig {
                enabled: false,
                ..NavLayerConfig::default()
            }),
            now: Instant::now(),
            keys: Vec::new(),
            clicks: Vec::new(),
            log: Vec::new(),
        }
    }

    /// An app talking to a terminal that confirmed every kitty keyboard flag.
    fn in_kitty_mode() -> App {
        let mut app = App::new();
        app.read(b"\x1b[?31u\x1b[?62;4c");
        assert!(app.nav.enabled());
        app.log.clear();
        app
    }

    /// Bytes arrived from the terminal.
    fn read(&mut self, bytes: &[u8]) {
        self.now += Duration::from_millis(10);
        let events = self.parser.feed(bytes);
        self.handle(events);
    }

    /// Nothing arrived for 25 ms.
    fn idle(&mut self) {
        self.now += Duration::from_millis(25);
        if self.parser.has_pending() {
            let events = self.parser.flush();
            self.handle(events);
            assert!(!self.parser.has_pending());
        }
    }

    fn handle(&mut self, events: Vec<InputEvent>) {
        for event in events {
            let line = describe(&event);
            assert!(!line.contains('\n'));
            match event {
                InputEvent::Key(ev) => {
                    self.held.observe(&ev);
                    self.keys.extend(self.nav.process(ev, self.now));
                }
                InputEvent::Mouse(mouse) => {
                    if mouse.kind == MouseKind::Press(MouseButton::Left) {
                        let mods = mouse.mods | self.held.mods() | self.nav.virtual_mods();
                        self.clicks.push((mouse.col, mouse.row, mods));
                    }
                }
                InputEvent::FocusLost => {
                    self.held.clear();
                    self.keys.extend(self.nav.reset());
                    self.log.push(line);
                }
                InputEvent::Reply(TerminalReply::KittyKeyboardFlags(flags)) => {
                    self.parser.set_kitty_flags(flags);
                    let on = NavLayer::works_with(flags);
                    self.keys.extend(self.nav.set_enabled(on));
                    self.log.push(line);
                }
                _ => self.log.push(line),
            }
        }
    }

    /// The text the keys seen so far would have typed.
    fn typed(&self) -> String {
        self.keys
            .iter()
            .filter(|ev| ev.is_down())
            .filter_map(|ev| ev.text.as_deref())
            .collect()
    }

    /// The keys seen so far that are not typing, as `kind spelling`.
    fn shortcuts(&self) -> Vec<String> {
        self.keys
            .iter()
            .filter(|ev| ev.text.is_none())
            .map(|ev| {
                let kind = match ev.kind {
                    KeyKind::Press => "press",
                    KeyKind::Repeat => "repeat",
                    KeyKind::Release => "release",
                };
                format!("{kind} {}", ev.display())
            })
            .collect()
    }
}

/// The bytes a kitty-mode terminal sends for a text key going down, repeating or coming up.
fn down(c: char) -> Vec<u8> {
    format!("\x1b[{0};;{0}u", c as u32).into_bytes()
}

fn again(c: char) -> Vec<u8> {
    format!("\x1b[{0};1:2;{0}u", c as u32).into_bytes()
}

fn up(c: char) -> Vec<u8> {
    format!("\x1b[{};1:3u", c as u32).into_bytes()
}

#[test]
fn the_startup_handshake_turns_the_nav_layer_on() {
    let opts = host::HostOptions::default();
    let enter = host::enter_sequence(&opts);
    assert!(enter.starts_with(host::ALTERNATE_SCREEN_ON));
    assert!(enter.contains(&host::kitty_push(host::KITTY_ALL_FLAGS)));
    assert!(enter.ends_with(host::DEVICE_ATTRIBUTES_QUERY));

    // The terminal answers the queries in the order they were asked.
    let mut app = App::new();
    assert!(!app.nav.enabled());
    app.read(b"\x1b[?31u\x1b]10;rgb:c0c0/caca/f5f5\x1b\\\x1b]11;rgb:1a1a/1b1b/2626\x1b\\");
    app.read(b"\x1b[?997;1n\x1bP>|ghostty 1.2.3\x1b\\\x1b[?62;22;52c");
    assert_eq!(
        app.log,
        [
            "reply kitty-keyboard flags=31 (disambiguate, event-types, alternate-keys, all-keys, text)",
            "reply color foreground #c0caf5",
            "reply color background #1a1b26",
            "reply color-scheme dark",
            "reply version \"ghostty 1.2.3\"",
            "reply device-attributes \"62;22;52\"",
        ]
    );
    assert!(app.nav.enabled());
    assert_eq!(app.parser.kitty_flags(), 31);
    assert!(app.keys.is_empty());

    let leave = host::leave_sequence(&opts);
    assert!(leave.ends_with(host::ALTERNATE_SCREEN_OFF));
    assert!(leave.find(host::KITTY_POP) < leave.find(host::ALTERNATE_SCREEN_OFF));
}

#[test]
fn a_terminal_without_the_protocol_leaves_the_nav_layer_off() {
    // Only the device attributes come back: the round is over and nothing was confirmed.
    let mut app = App::new();
    app.read(b"\x1b[?1;2c");
    assert!(!app.nav.enabled());
    // Its input is plain bytes. `;` is just a character and Escape needs the timeout.
    app.read(b"x;e\r");
    app.read(b"\x1b");
    assert_eq!(app.keys.len(), 4);
    app.idle();
    assert_eq!(app.typed(), "x;e");
    assert_eq!(app.shortcuts(), ["press enter", "press escape"]);
    // An arrow key arrives in one piece or in two; either way it is one key.
    app.read(b"\x1b[");
    app.read(b"1;5C");
    assert_eq!(
        app.shortcuts().last().map(String::as_str),
        Some("press ctrl-right")
    );
    // A terminal that reports only some of the flags is not enough either.
    app.read(b"\x1b[?1u");
    assert!(!app.nav.enabled());
}

#[test]
fn typing_and_navigating_in_kitty_mode() {
    let mut app = App::in_kitty_mode();
    // "hi;" with every key released before the next.
    for c in ['h', 'i', ';'] {
        app.read(&down(c));
        app.read(&up(c));
    }
    assert_eq!(app.typed(), "hi;");

    // Hold `;`, tap `e` twice, hold `f` until it repeats.
    app.keys.clear();
    app.read(&down(';'));
    assert!(app.nav.active());
    app.read(&[down('e'), up('e'), down('e'), up('e')].concat());
    app.read(&[down('f'), again('f'), again('f'), up('f')].concat());
    app.read(&up(';'));
    assert!(!app.nav.active());
    assert_eq!(app.typed(), "", "nothing was typed, the `;` included");
    assert_eq!(
        app.shortcuts(),
        [
            "press up",
            "release up",
            "press up",
            "release up",
            "press right",
            "repeat right",
            "repeat right",
            "release right",
        ]
    );
}

#[test]
fn layer_shortcuts_and_selection() {
    let mut app = App::in_kitty_mode();
    // ; + k + t is cmd-t; ; + j + f is shift-right; ; + k + l + a is cmd-alt-a.
    app.read(&[down(';'), down('k'), down('t'), up('t'), up('k')].concat());
    app.read(&[down('j'), down('f'), up('f'), up('j')].concat());
    app.read(&[down('k'), down('l'), down('a'), up('a'), up('l'), up('k')].concat());
    app.read(&[down('a'), up('a')].concat());
    app.read(&up(';'));
    assert_eq!(app.typed(), "");
    assert_eq!(
        app.shortcuts(),
        [
            "press cmd-t",
            "release cmd-t",
            "press shift-right",
            "release shift-right",
            "press alt-cmd-a",
            "release alt-cmd-a",
            "press tab",
            "release tab",
        ]
    );
}

#[test]
fn fast_typing_through_the_trigger_loses_nothing() {
    let mut app = App::in_kitty_mode();
    // `x;` Enter `y`, each key down before the previous one is up.
    let enter_down = b"\x1b[13u".to_vec();
    let enter_up = b"\x1b[13;1:3u".to_vec();
    app.read(
        &[
            down('x'),
            down(';'),
            up('x'),
            enter_down,
            up(';'),
            down('y'),
            enter_up,
            up('y'),
        ]
        .concat(),
    );
    let pressed: Vec<String> = app
        .keys
        .iter()
        .filter(|ev| ev.is_press())
        .map(KeyEvent::display)
        .collect();
    assert_eq!(pressed, ["x", ";", "enter", "y"]);
    assert_eq!(app.typed(), "x;y");

    // And released just before the other key: no stray character.
    app.keys.clear();
    app.read(
        &[
            down(';'),
            down('f'),
            again('f'),
            up(';'),
            again('f'),
            again('f'),
            up('f'),
        ]
        .concat(),
    );
    assert_eq!(app.typed(), "");
    assert_eq!(
        app.shortcuts(),
        ["press right", "repeat right", "release right"]
    );
}

#[test]
fn cmd_is_known_during_a_click() {
    let mut app = App::in_kitty_mode();
    let click = b"\x1b[<0;11;6M\x1b[<0;11;6m";
    app.read(click);
    // Real cmd held: the mouse report cannot say so, the modifier key's own events do.
    app.read(b"\x1b[57444;9u");
    app.read(click);
    app.read(b"\x1b[57444;1:3u");
    app.read(click);
    // Shift comes with the report itself.
    app.read(b"\x1b[<4;11;6M");
    // The layer's cmd: `;` and `k` held.
    app.read(&[down(';'), down('k')].concat());
    app.read(click);
    app.read(&[up('k'), up(';')].concat());
    app.read(click);
    assert_eq!(
        app.clicks,
        [
            (10, 5, Mods::empty()),
            (10, 5, Mods::SUPER),
            (10, 5, Mods::empty()),
            (10, 5, Mods::SHIFT),
            (10, 5, Mods::SUPER),
            (10, 5, Mods::empty()),
        ]
    );
    assert_eq!(app.typed(), "");
}

#[test]
fn losing_focus_in_the_middle_of_a_hold_lets_go_of_everything() {
    let mut app = App::in_kitty_mode();
    // Real cmd is held, so the terminal reports it on every key that follows.
    app.read(b"\x1b[57444;9u");
    app.read(b"\x1b[59;9u\x1b[106;9u\x1b[102;9u");
    assert_eq!(app.held.mods(), Mods::SUPER);
    assert_eq!(app.nav.virtual_mods(), Mods::SHIFT);
    assert_eq!(
        app.shortcuts(),
        ["press cmd-leftsuper", "press shift-cmd-right"]
    );
    // The window loses focus; none of the releases will ever arrive.
    app.read(b"\x1b[O");
    assert_eq!(
        app.shortcuts().last().map(String::as_str),
        Some("release shift-cmd-right")
    );
    assert_eq!(app.log, ["focus lost"]);
    assert!(!app.nav.active());
    assert_eq!(app.held.mods(), Mods::empty());
    assert_eq!(app.nav.virtual_mods(), Mods::empty());
    // Back again: everything is as if nothing had been held.
    app.keys.clear();
    app.read(b"\x1b[I");
    app.read(b"\x1b[<0;1;1M");
    assert_eq!(app.clicks, [(0, 0, Mods::empty())]);
    app.read(&[down('f'), up('f')].concat());
    assert_eq!(app.typed(), "f");
}

#[test]
fn text_from_elsewhere_does_not_hold_the_layer_on() {
    // Another program writes text into the terminal, or an input method commits it. It
    // contains the trigger and layer keys, but nobody pressed them and no release follows.
    let mut app = App::in_kitty_mode();
    app.read("let x = 1; else 日本".as_bytes());
    assert!(!app.nav.active());
    assert_eq!(app.typed(), "let x = 1; else 日本");
    assert!(app.keys.iter().all(|ev| ev.key == Key::Other(0)));
    // The layer still works for real keys.
    app.keys.clear();
    app.read(&[down(';'), down('e'), up('e'), up(';')].concat());
    assert_eq!(app.shortcuts(), ["press up", "release up"]);
    // A paste is one event and never keys.
    app.read(b"\x1b[200~a;e\r\nb\x1b[201~");
    assert_eq!(app.log, ["paste 5 chars \"a;e\\nb\""]);
}

#[test]
fn input_cut_at_awkward_places_still_makes_sense() {
    let mut app = App::in_kitty_mode();
    // A key event split across three reads with the escape timer firing in between.
    app.read(b"\x1b");
    app.read(b"[97;");
    app.idle();
    app.read(b";97u");
    assert_eq!(app.typed(), "a");
    // The first flush resolved nothing and typed nothing.
    assert_eq!(app.keys.len(), 1);
    // A character split in the middle.
    let snowman = "☃".as_bytes();
    app.read(&snowman[..1]);
    app.idle();
    app.read(&snowman[1..]);
    assert_eq!(app.typed(), "a☃");
}
