//! The table of input sequences and the events they must produce, and the tests that run
//! it whole, cut at every byte boundary, byte by byte and as one long stream.
//!
//! The case lists are kept one case to a line (`rustfmt::skip`): they are a table.

use bakedit_core::{
    ColorSlot, InputEvent, Key, KeyKind, ModKey, Mods, MouseButton, MouseKind, Rgb, TerminalReply,
};

use super::{alt_typed, csi, key, mouse, parse, reply, typed, Expect, Parser};

type Case = (Vec<u8>, Vec<InputEvent>);

fn case(bytes: impl AsRef<[u8]>, events: Vec<InputEvent>) -> Case {
    (bytes.as_ref().to_vec(), events)
}

/// Every complete sequence the parser understands, with what it means.
fn table() -> Vec<Case> {
    let mut cases = Vec::new();
    cases.extend(text_cases());
    cases.extend(control_cases());
    cases.extend(escape_prefix_cases());
    cases.extend(legacy_key_cases());
    cases.extend(tilde_cases());
    cases.extend(ss3_cases());
    cases.extend(modify_other_keys_cases());
    cases.extend(kitty_text_key_cases());
    cases.extend(kitty_functional_cases());
    cases.extend(sgr_mouse_cases());
    cases.extend(x10_mouse_cases());
    cases.extend(focus_and_paste_cases());
    cases.extend(reply_cases());
    cases.extend(ignored_cases());
    cases
}

#[rustfmt::skip]
fn text_cases() -> Vec<Case> {
    vec![
        case("a", vec![typed('a')]),
        case("A", vec![Expect::ch('a').mods(Mods::SHIFT).shifted('A').text("A").event()]),
        case("{", vec![Expect::ch('[').mods(Mods::SHIFT).shifted('{').text("{").event()]),
        case(":", vec![Expect::ch(';').mods(Mods::SHIFT).shifted(':').text(":").event()]),
        case(" ", vec![Expect::ch(' ').text(" ").event()]),
        case("~", vec![typed('~')]),
        case("ab", vec![typed('a'), typed('b')]),
        case("é", vec![Expect::ch('é').text("é").event()]),
        case("€", vec![typed('€')]),
        case("😀", vec![typed('😀')]),
        case("日本", vec![typed('日'), typed('本')]),
        case("É", vec![Expect::ch('É').text("É").event()]),
    ]
}

#[rustfmt::skip]
fn control_cases() -> Vec<Case> {
    let ctrl = |c: char| key(Key::Char(c), Mods::CTRL);
    let mut cases = vec![
        case([0x0du8], vec![key(Key::Enter, Mods::empty())]),
        case([0x09u8], vec![key(Key::Tab, Mods::empty())]),
        case([0x7fu8], vec![key(Key::Backspace, Mods::empty())]),
        case([0x00u8], vec![ctrl(' ')]),
        case([0x08u8], vec![ctrl('h')]),
        case([0x0au8], vec![ctrl('j')]),
        case([0x1cu8], vec![ctrl('\\')]),
        case([0x1du8], vec![ctrl(']')]),
        case([0x1eu8], vec![ctrl('6')]),
        case([0x1fu8], vec![ctrl('-')]),
    ];
    for byte in 0x01..=0x1au8 {
        if byte != 0x09 && byte != 0x0d {
            cases.push(case([byte], vec![ctrl(char::from(b'a' + byte - 1))]));
        }
    }
    cases
}

#[rustfmt::skip]
fn escape_prefix_cases() -> Vec<Case> {
    let alt = Mods::ALT;
    let none = Mods::empty();
    let escape = || key(Key::Escape, Mods::empty());
    let left_press = mouse(MouseKind::Press(MouseButton::Left), 2, 3, none);
    let black = reply(TerminalReply::Color { slot: ColorSlot::Background, rgb: Rgb::new(0, 0, 0) });
    vec![
        case("\x1ba", vec![key(Key::Char('a'), alt)]),
        case("\x1bA", vec![Expect::ch('a').mods(alt | Mods::SHIFT).shifted('A').event()]),
        case("\x1b ", vec![key(Key::Char(' '), alt)]),
        case("\x1b\r", vec![key(Key::Enter, alt)]),
        case("\x1b\t", vec![key(Key::Tab, alt)]),
        case("\x1b\x7f", vec![key(Key::Backspace, alt)]),
        case("\x1b\x01", vec![key(Key::Char('a'), alt | Mods::CTRL)]),
        case("\x1b\x00", vec![key(Key::Char(' '), alt | Mods::CTRL)]),
        case("\x1b\x08", vec![key(Key::Char('h'), alt | Mods::CTRL)]),
        case("\x1bé", vec![key(Key::Char('é'), alt)]),
        case("\x1b\\", vec![key(Key::Char('\\'), alt)]),
        // ESC in front of a key sequence adds alt to the key.
        case("\x1b\x1b[A", vec![key(Key::Up, alt)]),
        case("\x1b\x1bOP", vec![key(Key::F(1), alt)]),
        case("\x1b\x1b[1;5C", vec![key(Key::Right, alt | Mods::CTRL)]),
        case("\x1b\x1b[3~", vec![key(Key::Delete, alt)]),
        case("\x1b\x1b[97;5u", vec![key(Key::Char('a'), alt | Mods::CTRL)]),
        case("\x1b\x1b[Z", vec![key(Key::Tab, alt | Mods::SHIFT)]),
        // ESC in front of anything else was an Escape press of its own.
        case("\x1b\x1ba", vec![escape(), key(Key::Char('a'), alt)]),
        case("\x1b\x1b\x1ba", vec![escape(), escape(), key(Key::Char('a'), alt)]),
        case("\x1b\x1b[I", vec![escape(), InputEvent::FocusGained]),
        case("\x1b\x1b[<0;3;4M", vec![escape(), left_press]),
        case("\x1b\x1b[?1u", vec![escape(), reply(TerminalReply::KittyKeyboardFlags(1))]),
        case("\x1b\x1b[5n", vec![escape()]),
        case("\x1b\x1b]11;rgb:00/00/00\x07", vec![escape(), black]),
        case("\x1b\x1b[200~x\x1b[201~", vec![escape(), InputEvent::Paste("x".into())]),
        // An SS3 or CSI that is cut short by something else was alt+O or alt+[.
        case("\x1bOz", vec![alt_typed('O'), typed('z')]),
        case("\x1bO\x1b[A", vec![alt_typed('O'), key(Key::Up, none)]),
        case("\x1b[\x1b[A", vec![alt_typed('['), key(Key::Up, none)]),
        case("\x1b[\r", vec![alt_typed('['), key(Key::Enter, none)]),
        case("\x1b[é", vec![alt_typed('['), typed('é')]),
        case("\x1b]\x1b[A", vec![alt_typed(']'), key(Key::Up, none)]),
        case("\x1bP\x1ba", vec![alt_typed('P'), key(Key::Char('a'), alt)]),
        case("\x1b\x1b[\x1b[B", vec![escape(), alt_typed('['), key(Key::Down, none)]),
    ]
}

#[rustfmt::skip]
fn legacy_key_cases() -> Vec<Case> {
    let none = Mods::empty();
    let kp_begin = || Expect::key(Key::Other(57427)).keypad();
    vec![
        case(csi("A"), vec![key(Key::Up, none)]),
        case(csi("B"), vec![key(Key::Down, none)]),
        case(csi("C"), vec![key(Key::Right, none)]),
        case(csi("D"), vec![key(Key::Left, none)]),
        case(csi("H"), vec![key(Key::Home, none)]),
        case(csi("F"), vec![key(Key::End, none)]),
        case(csi("E"), vec![kp_begin().event()]),
        case(csi("P"), vec![key(Key::F(1), none)]),
        case(csi("Q"), vec![key(Key::F(2), none)]),
        case(csi("R"), vec![key(Key::F(3), none)]),
        case(csi("S"), vec![key(Key::F(4), none)]),
        case(csi("1A"), vec![key(Key::Up, none)]),
        case(csi("1;1A"), vec![key(Key::Up, none)]),
        case(csi("1;2A"), vec![key(Key::Up, Mods::SHIFT)]),
        case(csi("1;3D"), vec![key(Key::Left, Mods::ALT)]),
        case(csi("1;5C"), vec![key(Key::Right, Mods::CTRL)]),
        case(csi("1;9H"), vec![key(Key::Home, Mods::SUPER)]),
        case(csi("1;6F"), vec![key(Key::End, Mods::CTRL | Mods::SHIFT)]),
        case(csi("1;17B"), vec![key(Key::Down, Mods::HYPER)]),
        case(csi("1;33B"), vec![key(Key::Down, Mods::META)]),
        case(csi(";5B"), vec![key(Key::Down, Mods::CTRL)]),
        case(csi("1;2P"), vec![key(Key::F(1), Mods::SHIFT)]),
        case(csi("1;1R"), vec![key(Key::F(3), none)]),
        case(csi("1;5R"), vec![key(Key::F(3), Mods::CTRL)]),
        case(csi("1;3S"), vec![key(Key::F(4), Mods::ALT)]),
        case(csi("1;5E"), vec![kp_begin().mods(Mods::CTRL).event()]),
        // Event types on the legacy forms, as kitty sends them with flag 2.
        case(csi("1;2:3A"), vec![Expect::key(Key::Up).mods(Mods::SHIFT).kind(KeyKind::Release).event()]),
        case(csi("1;1:2B"), vec![Expect::key(Key::Down).kind(KeyKind::Repeat).event()]),
        case(csi("1;1:1C"), vec![key(Key::Right, none)]),
        case(csi("1;1:3P"), vec![Expect::key(Key::F(1)).kind(KeyKind::Release).event()]),
        // Lock states ride along in the modifier value.
        case(csi("1;65A"), vec![Expect::key(Key::Up).caps_lock().event()]),
        case(csi("1;130A"), vec![Expect::key(Key::Up).mods(Mods::SHIFT).num_lock().event()]),
        // Old xterms put the modifiers alone.
        case(csi("5C"), vec![key(Key::Right, Mods::CTRL)]),
        case(csi("2D"), vec![key(Key::Left, Mods::SHIFT)]),
        // Shift-tab.
        case(csi("Z"), vec![key(Key::Tab, Mods::SHIFT)]),
        case(csi("1;5Z"), vec![key(Key::Tab, Mods::SHIFT | Mods::CTRL)]),
        case(csi("1;2Z"), vec![key(Key::Tab, Mods::SHIFT)]),
    ]
}

fn tilde_cases() -> Vec<Case> {
    let none = Mods::empty();
    #[rustfmt::skip]
    let numbers: [(u32, Key); 28] = [
        (1, Key::Home), (2, Key::Insert), (3, Key::Delete), (4, Key::End),
        (5, Key::PageUp), (6, Key::PageDown), (7, Key::Home), (8, Key::End),
        (11, Key::F(1)), (12, Key::F(2)), (13, Key::F(3)), (14, Key::F(4)), (15, Key::F(5)),
        (17, Key::F(6)), (18, Key::F(7)), (19, Key::F(8)), (20, Key::F(9)), (21, Key::F(10)),
        (23, Key::F(11)), (24, Key::F(12)),
        (25, Key::F(13)), (26, Key::F(14)), (28, Key::F(15)), (29, Key::F(16)),
        (31, Key::F(17)), (32, Key::F(18)), (33, Key::F(19)), (34, Key::F(20)),
    ];
    let mut cases = Vec::new();
    for (number, which) in numbers {
        let shift_release = Expect::key(which).mods(Mods::SHIFT).kind(KeyKind::Release);
        cases.push(case(csi(&format!("{number}~")), vec![key(which, none)]));
        cases.push(case(
            csi(&format!("{number};5~")),
            vec![key(which, Mods::CTRL)],
        ));
        cases.push(case(
            csi(&format!("{number};2:3~")),
            vec![shift_release.event()],
        ));
    }
    let kp_begin = || Expect::key(Key::Other(57427)).keypad();
    let f5_repeat = Expect::key(Key::F(5)).kind(KeyKind::Repeat);
    cases.push(case(csi("15;1:2~"), vec![f5_repeat.event()]));
    cases.push(case(
        csi("3;10~"),
        vec![key(Key::Delete, Mods::SUPER | Mods::SHIFT)],
    ));
    cases.push(case(csi("3;~"), vec![key(Key::Delete, none)]));
    cases.push(case(csi("57427~"), vec![kp_begin().event()]));
    cases.push(case(
        csi("57427;5~"),
        vec![kp_begin().mods(Mods::CTRL).event()],
    ));
    cases
}

#[rustfmt::skip]
fn ss3_cases() -> Vec<Case> {
    let none = Mods::empty();
    let pad = |c: char| Expect::ch(c).keypad().text(&c.to_string()).event();
    let mut cases = vec![
        case("\x1bOA", vec![key(Key::Up, none)]),
        case("\x1bOB", vec![key(Key::Down, none)]),
        case("\x1bOC", vec![key(Key::Right, none)]),
        case("\x1bOD", vec![key(Key::Left, none)]),
        case("\x1bOH", vec![key(Key::Home, none)]),
        case("\x1bOF", vec![key(Key::End, none)]),
        case("\x1bOP", vec![key(Key::F(1), none)]),
        case("\x1bOQ", vec![key(Key::F(2), none)]),
        case("\x1bOR", vec![key(Key::F(3), none)]),
        case("\x1bOS", vec![key(Key::F(4), none)]),
        case("\x1bOE", vec![Expect::key(Key::Other(57427)).keypad().event()]),
        // Application keypad mode, should the terminal have been left in it.
        case("\x1bOM", vec![Expect::key(Key::Enter).keypad().event()]),
        case("\x1bOI", vec![Expect::key(Key::Tab).keypad().event()]),
        case("\x1bO ", vec![pad(' ')]),
        case("\x1bOX", vec![pad('=')]),
        case("\x1bOj", vec![pad('*')]),
        case("\x1bOk", vec![pad('+')]),
        case("\x1bOl", vec![pad(',')]),
        case("\x1bOm", vec![pad('-')]),
        case("\x1bOn", vec![pad('.')]),
        case("\x1bOo", vec![pad('/')]),
        case("\x1b\x1bOp", vec![Expect::ch('0').keypad().mods(Mods::ALT).event()]),
    ];
    for digit in 0..=9u8 {
        let bytes = [0x1b, b'O', b'p' + digit];
        cases.push(case(bytes, vec![pad(char::from(b'0' + digit))]));
    }
    cases
}

#[rustfmt::skip]
fn modify_other_keys_cases() -> Vec<Case> {
    let ctrl_shift = Mods::CTRL | Mods::SHIFT;
    vec![
        case(csi("27;5;97~"), vec![key(Key::Char('a'), Mods::CTRL)]),
        case(csi("27;6;65~"), vec![Expect::ch('a').mods(ctrl_shift).shifted('A').event()]),
        case(csi("27;6;33~"), vec![Expect::ch('1').mods(ctrl_shift).shifted('!').event()]),
        case(csi("27;2;13~"), vec![key(Key::Enter, Mods::SHIFT)]),
        case(csi("27;5;13~"), vec![key(Key::Enter, Mods::CTRL)]),
        case(csi("27;5;9~"), vec![key(Key::Tab, Mods::CTRL)]),
        case(csi("27;3;27~"), vec![key(Key::Escape, Mods::ALT)]),
        case(csi("27;5;127~"), vec![key(Key::Backspace, Mods::CTRL)]),
        case(csi("27;5;8~"), vec![key(Key::Backspace, Mods::CTRL)]),
        case(csi("27;2;32~"), vec![Expect::ch(' ').mods(Mods::SHIFT).text(" ").event()]),
        case(csi("27;2;65~"), vec![Expect::ch('a').mods(Mods::SHIFT).shifted('A').text("A").event()]),
        case(csi("27;2;97~"), vec![Expect::ch('a').mods(Mods::SHIFT).text("A").event()]),
        case(csi("27;;97~"), vec![Expect::ch('a').text("a").event()]),
        case(csi("27;5;44~"), vec![key(Key::Char(','), Mods::CTRL)]),
    ]
}

#[rustfmt::skip]
fn kitty_text_key_cases() -> Vec<Case> {
    let shift = Mods::SHIFT;
    let nothing = Expect::key(Key::Other(0));
    vec![
        // Plain and shifted text keys; the text is worked out when the terminal sends none.
        case(csi("97u"), vec![Expect::ch('a').text("a").event()]),
        case(csi("97;1u"), vec![Expect::ch('a').text("a").event()]),
        case(csi("97;2u"), vec![Expect::ch('a').mods(shift).text("A").event()]),
        case(csi("97:65;2u"), vec![Expect::ch('a').mods(shift).shifted('A').text("A").event()]),
        case(csi("97:65:97;2u"), vec![Expect::ch('a').mods(shift).shifted('A').base('a').text("A").event()]),
        case(csi("97:65u"), vec![Expect::ch('a').shifted('A').text("a").event()]),
        case(csi("97::97u"), vec![Expect::ch('a').base('a').text("a").event()]),
        case(csi("49;2u"), vec![Expect::ch('1').mods(shift).text("!").event()]),
        case(csi("50:34;2u"), vec![Expect::ch('2').mods(shift).shifted('"').text("\"").event()]),
        case(csi("59;2u"), vec![Expect::ch(';').mods(shift).text(":").event()]),
        case(csi("32u"), vec![Expect::ch(' ').text(" ").event()]),
        case(csi("32;2u"), vec![Expect::ch(' ').mods(shift).text(" ").event()]),
        case(csi("233u"), vec![Expect::ch('é').text("é").event()]),
        case(csi("233;2u"), vec![Expect::ch('é').mods(shift).text("É").event()]),
        // A base-layout key without a shifted key.
        case(csi("1089::99;5u"), vec![Expect::ch('с').mods(Mods::CTRL).base('c').event()]),
        case(csi("1089::99u"), vec![Expect::ch('с').base('c').text("с").event()]),
        // Shortcuts type nothing.
        case(csi("97;5u"), vec![key(Key::Char('a'), Mods::CTRL)]),
        case(csi("97;3u"), vec![key(Key::Char('a'), Mods::ALT)]),
        case(csi("97;9u"), vec![key(Key::Char('a'), Mods::SUPER)]),
        case(csi("97;17u"), vec![key(Key::Char('a'), Mods::HYPER)]),
        case(csi("97;33u"), vec![key(Key::Char('a'), Mods::META)]),
        case(csi("97;64u"), vec![key(Key::Char('a'), Mods::all())]),
        case(csi("112;10u"), vec![key(Key::Char('p'), Mods::SUPER | shift)]),
        case(csi("59;9u"), vec![key(Key::Char(';'), Mods::SUPER)]),
        // Event types.
        case(csi("97;1:1u"), vec![Expect::ch('a').text("a").event()]),
        case(csi("97;1:2u"), vec![Expect::ch('a').kind(KeyKind::Repeat).text("a").event()]),
        case(csi("97;1:3u"), vec![Expect::ch('a').kind(KeyKind::Release).event()]),
        case(csi("97;2:3u"), vec![Expect::ch('a').mods(shift).kind(KeyKind::Release).event()]),
        case(csi("97;5:2u"), vec![Expect::ch('a').mods(Mods::CTRL).kind(KeyKind::Repeat).event()]),
        case(csi("97;1:9u"), vec![Expect::ch('a').text("a").event()]),
        case(csi("59;1:3u"), vec![Expect::ch(';').kind(KeyKind::Release).event()]),
        // Text sent by the terminal wins.
        case(csi("97;;97u"), vec![Expect::ch('a').text("a").event()]),
        case(csi("97;1;97u"), vec![Expect::ch('a').text("a").event()]),
        case(csi("97;2;65u"), vec![Expect::ch('a').mods(shift).text("A").event()]),
        case(csi("97:65;2;65u"), vec![Expect::ch('a').mods(shift).shifted('A').text("A").event()]),
        case(csi("97;3;229u"), vec![Expect::ch('a').mods(Mods::ALT).text("å").event()]),
        case(csi("97;1:2;97u"), vec![Expect::ch('a').kind(KeyKind::Repeat).text("a").event()]),
        case(csi("97;1:3;97u"), vec![Expect::ch('a').kind(KeyKind::Release).event()]),
        case(csi("101;;233:769u"), vec![Expect::ch('e').text("\u{e9}\u{301}").event()]),
        case(csi("32;;32u"), vec![Expect::ch(' ').text(" ").event()]),
        // Control characters are not text; without anything else the text is worked out.
        case(csi("97;;7u"), vec![Expect::ch('a').text("a").event()]),
        case(csi("97;;97:7:98u"), vec![Expect::ch('a').text("ab").event()]),
        case(csi("13;;13u"), vec![key(Key::Enter, Mods::empty())]),
        // Text that belongs to no key.
        case(csi("0;;229u"), vec![nothing.clone().text("å").event()]),
        case(csi(";;229u"), vec![nothing.clone().text("å").event()]),
        case(csi("0;;12371:12435u"), vec![nothing.clone().text("こん").event()]),
        case(csi("0;1:2;12371u"), vec![nothing.clone().kind(KeyKind::Repeat).text("こ").event()]),
        // An upper-case key code is shift + the lower-case key.
        case(csi("65;2u"), vec![Expect::ch('a').mods(shift).shifted('A').text("A").event()]),
        case(csi("65u"), vec![Expect::ch('a').mods(shift).shifted('A').text("A").event()]),
        case(csi("65;6u"), vec![Expect::ch('a').mods(shift | Mods::CTRL).shifted('A').event()]),
        case(csi("201u"), vec![Expect::ch('é').mods(shift).shifted('É').text("É").event()]),
        // Lock states are not modifiers.
        case(csi("97;65u"), vec![Expect::ch('a').caps_lock().text("A").event()]),
        case(csi("97;66u"), vec![Expect::ch('a').mods(shift).caps_lock().text("A").event()]),
        case(csi("97;129u"), vec![Expect::ch('a').num_lock().text("a").event()]),
        case(csi("49;65u"), vec![Expect::ch('1').caps_lock().text("1").event()]),
        case(csi("97;69u"), vec![Expect::ch('a').mods(Mods::CTRL).caps_lock().event()]),
        case(csi("97;193u"), vec![Expect::ch('a').caps_lock().num_lock().text("A").event()]),
    ]
}

#[rustfmt::skip]
fn kitty_functional_cases() -> Vec<Case> {
    let none = Mods::empty();
    let release = KeyKind::Release;
    let modifier = |key: ModKey| Expect::key(Key::Modifier(key));
    let mut cases = vec![
        case(csi("27u"), vec![key(Key::Escape, none)]),
        case(csi("13u"), vec![key(Key::Enter, none)]),
        case(csi("9u"), vec![key(Key::Tab, none)]),
        case(csi("127u"), vec![key(Key::Backspace, none)]),
        case(csi("27;1:3u"), vec![Expect::key(Key::Escape).kind(release).event()]),
        case(csi("13;2u"), vec![key(Key::Enter, Mods::SHIFT)]),
        case(csi("13;1:2u"), vec![Expect::key(Key::Enter).kind(KeyKind::Repeat).event()]),
        case(csi("9;5u"), vec![key(Key::Tab, Mods::CTRL)]),
        case(csi("9;6u"), vec![key(Key::Tab, Mods::CTRL | Mods::SHIFT)]),
        case(csi("127;3u"), vec![key(Key::Backspace, Mods::ALT)]),
        case(csi("127;9u"), vec![key(Key::Backspace, Mods::SUPER)]),
        case(csi("57358u"), vec![key(Key::CapsLock, none)]),
        case(csi("57358;65u"), vec![Expect::key(Key::CapsLock).caps_lock().event()]),
        case(csi("57359u"), vec![key(Key::ScrollLock, none)]),
        case(csi("57360u"), vec![key(Key::NumLock, none)]),
        case(csi("57361u"), vec![key(Key::PrintScreen, none)]),
        case(csi("57362u"), vec![key(Key::Pause, none)]),
        case(csi("57363u"), vec![key(Key::Menu, none)]),
        // The keypad maps to the ordinary keys with `keypad` set.
        case(csi("57409u"), vec![Expect::ch('.').keypad().text(".").event()]),
        case(csi("57410u"), vec![Expect::ch('/').keypad().text("/").event()]),
        case(csi("57411u"), vec![Expect::ch('*').keypad().text("*").event()]),
        case(csi("57412u"), vec![Expect::ch('-').keypad().text("-").event()]),
        case(csi("57413u"), vec![Expect::ch('+').keypad().text("+").event()]),
        case(csi("57414u"), vec![Expect::key(Key::Enter).keypad().event()]),
        case(csi("57415u"), vec![Expect::ch('=').keypad().text("=").event()]),
        case(csi("57416u"), vec![Expect::ch(',').keypad().text(",").event()]),
        case(csi("57417u"), vec![Expect::key(Key::Left).keypad().event()]),
        case(csi("57418u"), vec![Expect::key(Key::Right).keypad().event()]),
        case(csi("57419u"), vec![Expect::key(Key::Up).keypad().event()]),
        case(csi("57420u"), vec![Expect::key(Key::Down).keypad().event()]),
        case(csi("57421u"), vec![Expect::key(Key::PageUp).keypad().event()]),
        case(csi("57422u"), vec![Expect::key(Key::PageDown).keypad().event()]),
        case(csi("57423u"), vec![Expect::key(Key::Home).keypad().event()]),
        case(csi("57424u"), vec![Expect::key(Key::End).keypad().event()]),
        case(csi("57425u"), vec![Expect::key(Key::Insert).keypad().event()]),
        case(csi("57426u"), vec![Expect::key(Key::Delete).keypad().event()]),
        case(csi("57427u"), vec![Expect::key(Key::Other(57427)).keypad().event()]),
        case(csi("57400;2u"), vec![Expect::ch('1').keypad().mods(Mods::SHIFT).text("1").event()]),
        case(csi("57400;5u"), vec![Expect::ch('1').keypad().mods(Mods::CTRL).event()]),
        case(csi("57400;129u"), vec![Expect::ch('1').keypad().num_lock().text("1").event()]),
        case(csi("57400;;49u"), vec![Expect::ch('1').keypad().text("1").event()]),
        case(csi("57414;1:3u"), vec![Expect::key(Key::Enter).keypad().kind(release).event()]),
        // Media keys and anything else in the private use area keep their code.
        case(csi("57428u"), vec![key(Key::Other(57428), none)]),
        case(csi("57440u"), vec![key(Key::Other(57440), none)]),
        case(csi("57364u"), vec![key(Key::Other(57364), none)]),
        case(csi("63743;5u"), vec![key(Key::Other(63743), Mods::CTRL)]),
        // Codes that are no character at all.
        case(csi("55296u"), vec![key(Key::Other(55296), none)]),
        case(csi("99999999999u"), vec![key(Key::Other(u32::MAX), none)]),
        case(csi("1u"), vec![key(Key::Other(1), none)]),
        case(csi("8u"), vec![key(Key::Backspace, none)]),
        // Modifier keys on their own.
        case(csi("57441;2u"), vec![modifier(ModKey::LeftShift).mods(Mods::SHIFT).event()]),
        case(csi("57441;1:3u"), vec![modifier(ModKey::LeftShift).kind(release).event()]),
        case(csi("57442;5u"), vec![modifier(ModKey::LeftCtrl).mods(Mods::CTRL).event()]),
        case(csi("57443;3u"), vec![modifier(ModKey::LeftAlt).mods(Mods::ALT).event()]),
        case(csi("57444;9u"), vec![modifier(ModKey::LeftSuper).mods(Mods::SUPER).event()]),
        case(csi("57444;9:2u"), vec![modifier(ModKey::LeftSuper).mods(Mods::SUPER).kind(KeyKind::Repeat).event()]),
        case(csi("57450;9u"), vec![modifier(ModKey::RightSuper).mods(Mods::SUPER).event()]),
        case(csi("57453u"), vec![modifier(ModKey::IsoLevel3Shift).event()]),
        case(csi("57454u"), vec![modifier(ModKey::IsoLevel5Shift).event()]),
    ];
    for n in 13..=35u32 {
        cases.push(case(csi(&format!("{}u", 57376 + n - 13)), vec![key(Key::F(n as u8), none)]));
    }
    for digit in 0..=9u32 {
        let c = char::from(b'0' + digit as u8);
        let expected = Expect::ch(c).keypad().text(&c.to_string());
        cases.push(case(csi(&format!("{}u", 57399 + digit)), vec![expected.event()]));
    }
    for code in 57441..=57454u32 {
        let expected = modifier(ModKey::from_kitty_code(code).unwrap()).kind(release);
        cases.push(case(csi(&format!("{code};1:3u")), vec![expected.event()]));
    }
    cases
}

#[rustfmt::skip]
fn sgr_mouse_cases() -> Vec<Case> {
    use MouseButton::{Back, Forward, Left, Middle, Right};
    let none = Mods::empty();
    let all = Mods::SHIFT | Mods::ALT | Mods::CTRL;
    vec![
        case(csi("<0;1;1M"), vec![mouse(MouseKind::Press(Left), 0, 0, none)]),
        case(csi("<0;10;5m"), vec![mouse(MouseKind::Release(Left), 9, 4, none)]),
        case(csi("<1;10;5M"), vec![mouse(MouseKind::Press(Middle), 9, 4, none)]),
        case(csi("<1;10;5m"), vec![mouse(MouseKind::Release(Middle), 9, 4, none)]),
        case(csi("<2;10;5M"), vec![mouse(MouseKind::Press(Right), 9, 4, none)]),
        case(csi("<2;10;5m"), vec![mouse(MouseKind::Release(Right), 9, 4, none)]),
        case(csi("<4;2;3M"), vec![mouse(MouseKind::Press(Left), 1, 2, Mods::SHIFT)]),
        case(csi("<8;2;3M"), vec![mouse(MouseKind::Press(Left), 1, 2, Mods::ALT)]),
        case(csi("<16;2;3M"), vec![mouse(MouseKind::Press(Left), 1, 2, Mods::CTRL)]),
        case(csi("<30;2;3m"), vec![mouse(MouseKind::Release(Right), 1, 2, all)]),
        case(csi("<32;7;8M"), vec![mouse(MouseKind::Drag(Left), 6, 7, none)]),
        case(csi("<33;7;8M"), vec![mouse(MouseKind::Drag(Middle), 6, 7, none)]),
        case(csi("<34;7;8M"), vec![mouse(MouseKind::Drag(Right), 6, 7, none)]),
        case(csi("<36;7;8M"), vec![mouse(MouseKind::Drag(Left), 6, 7, Mods::SHIFT)]),
        case(csi("<35;7;8M"), vec![mouse(MouseKind::Move, 6, 7, none)]),
        case(csi("<51;7;8M"), vec![mouse(MouseKind::Move, 6, 7, Mods::CTRL)]),
        case(csi("<64;3;4M"), vec![mouse(MouseKind::ScrollUp, 2, 3, none)]),
        case(csi("<65;3;4M"), vec![mouse(MouseKind::ScrollDown, 2, 3, none)]),
        case(csi("<66;3;4M"), vec![mouse(MouseKind::ScrollLeft, 2, 3, none)]),
        case(csi("<67;3;4M"), vec![mouse(MouseKind::ScrollRight, 2, 3, none)]),
        case(csi("<68;3;4M"), vec![mouse(MouseKind::ScrollUp, 2, 3, Mods::SHIFT)]),
        case(csi("<81;3;4M"), vec![mouse(MouseKind::ScrollDown, 2, 3, Mods::CTRL)]),
        case(csi("<128;3;4M"), vec![mouse(MouseKind::Press(Back), 2, 3, none)]),
        case(csi("<129;3;4M"), vec![mouse(MouseKind::Press(Forward), 2, 3, none)]),
        case(csi("<128;3;4m"), vec![mouse(MouseKind::Release(Back), 2, 3, none)]),
        case(csi("<129;3;4m"), vec![mouse(MouseKind::Release(Forward), 2, 3, none)]),
        case(csi("<160;3;4M"), vec![mouse(MouseKind::Drag(Back), 2, 3, none)]),
        // Coordinates are 1-based on the wire; nonsense is clamped, not trusted.
        case(csi("<0;0;0M"), vec![mouse(MouseKind::Press(Left), 0, 0, none)]),
        case(csi("<0;300;200M"), vec![mouse(MouseKind::Press(Left), 299, 199, none)]),
        case(csi("<0;70000;99999999999M"), vec![mouse(MouseKind::Press(Left), u16::MAX, u16::MAX, none)]),
        // Reports that say nothing usable.
        case(csi("<3;3;4M"), vec![]),
        case(csi("<3;3;4m"), vec![]),
        case(csi("<64;3;4m"), vec![]),
        case(csi("<130;3;4M"), vec![]),
        case(csi("<256;3;4M"), vec![]),
        case(csi("<0;3M"), vec![]),
        case(csi("<0;3;4;5M"), vec![]),
        case(csi("<;3;4M"), vec![]),
        case(csi("<0:1;3;4M"), vec![]),
    ]
}

#[rustfmt::skip]
fn x10_mouse_cases() -> Vec<Case> {
    use MouseButton::{Left, Middle, Right};
    let none = Mods::empty();
    let report = |cb: u8, cx: u8, cy: u8| vec![0x1b, b'[', b'M', cb, cx, cy];
    let then = |a: Vec<u8>, b: Vec<u8>| [a, b].concat();
    let press = |button, col, row, mods| mouse(MouseKind::Press(button), col, row, mods);
    let release = |button, col, row, mods| mouse(MouseKind::Release(button), col, row, mods);
    vec![
        // A press and its release. The release does not name the button; the press did.
        case(then(report(32, 33, 33), report(35, 34, 35)), vec![press(Left, 0, 0, none), release(Left, 1, 2, none)]),
        case(then(report(33, 40, 41), report(35, 40, 41)), vec![press(Middle, 7, 8, none), release(Middle, 7, 8, none)]),
        case(then(report(34, 40, 41), report(35, 40, 41)), vec![press(Right, 7, 8, none), release(Right, 7, 8, none)]),
        case(then(report(36, 40, 41), report(39, 40, 41)), vec![press(Left, 7, 8, Mods::SHIFT), release(Left, 7, 8, Mods::SHIFT)]),
        case(then(report(48, 40, 41), report(35, 40, 41)), vec![press(Left, 7, 8, Mods::CTRL), release(Left, 7, 8, none)]),
        case(then(report(40, 40, 41), report(35, 40, 41)), vec![press(Left, 7, 8, Mods::ALT), release(Left, 7, 8, none)]),
        // A drag keeps the button for the release that ends it.
        case(
            [report(34, 40, 41), report(32 + 34, 41, 41), report(35, 41, 41)].concat(),
            vec![press(Right, 7, 8, none), mouse(MouseKind::Drag(Right), 8, 8, none), release(Right, 8, 8, none)],
        ),
        case(report(32 + 32, 40, 41), vec![mouse(MouseKind::Drag(Left), 7, 8, none)]),
        case(report(32 + 35, 40, 41), vec![mouse(MouseKind::Move, 7, 8, none)]),
        case(report(32 + 64, 40, 41), vec![mouse(MouseKind::ScrollUp, 7, 8, none)]),
        case(report(32 + 65, 40, 41), vec![mouse(MouseKind::ScrollDown, 7, 8, none)]),
        case(report(32 + 66, 40, 41), vec![mouse(MouseKind::ScrollLeft, 7, 8, none)]),
        case(report(32 + 67, 40, 41), vec![mouse(MouseKind::ScrollRight, 7, 8, none)]),
        // The largest coordinate a byte can carry, and bytes that look like other things.
        case(then(report(32, 255, 255), report(35, 255, 255)), vec![press(Left, 222, 222, none), release(Left, 222, 222, none)]),
        case(then(report(32, b'[', b'~'), report(35, b'M', b'u')), vec![press(Left, 58, 93, none), release(Left, 44, 84, none)]),
        case(then(report(32, 32, 32), report(35, 32, 32)), vec![press(Left, 0, 0, none), release(Left, 0, 0, none)]),
        case(
            then(vec![0x1b], then(report(32, 33, 33), report(35, 33, 33))),
            vec![key(Key::Escape, none), press(Left, 0, 0, none), release(Left, 0, 0, none)],
        ),
    ]
}

#[rustfmt::skip]
fn focus_and_paste_cases() -> Vec<Case> {
    let paste = |text: &str| vec![InputEvent::Paste(text.to_string())];
    vec![
        case(csi("I"), vec![InputEvent::FocusGained]),
        case(csi("O"), vec![InputEvent::FocusLost]),
        case("\x1b[200~hello\x1b[201~", paste("hello")),
        case("\x1b[200~\x1b[201~", paste("")),
        case("\x1b[200~one\r\ntwo\rthree\nfour\r\r\n\x1b[201~", paste("one\ntwo\nthree\nfour\n\n")),
        case("\x1b[200~日本語 😀\x1b[201~", paste("日本語 😀")),
        case(b"\x1b[200~a\xffb\xc3\x1b[201~", paste("a\u{fffd}b\u{fffd}")),
        // Nothing inside a paste is a key, a mouse report or another sequence.
        case(
            "\x1b[200~\x1b[A\x1b[97u\x03\x1b]0;t\x07\x1b[<0;1;1M\x1b\x1b[201~",
            paste("\x1b[A\x1b[97u\x03\x1b]0;t\x07\x1b[<0;1;1M\x1b"),
        ),
        case("\x1b[200~\x1b[200~x\x1b[201~", paste("\x1b[200~x")),
        // Beginnings of the end marker that turn out to be content.
        case("\x1b[200~a\x1b[20b\x1b[201x\x1b[2\x1b[201~", paste("a\x1b[20b\x1b[201x\x1b[2")),
        case("\x1b[200~\x1b\x1b\x1b[201~", paste("\x1b\x1b")),
        case("\x1b[200~x\x1b[201~y", vec![InputEvent::Paste("x".into()), typed('y')]),
    ]
}

#[rustfmt::skip]
fn reply_cases() -> Vec<Case> {
    let color = |slot: ColorSlot, r: u8, g: u8, b: u8| reply(TerminalReply::Color { slot, rgb: Rgb::new(r, g, b) });
    let da = |text: &str| vec![reply(TerminalReply::DeviceAttributes(text.to_string()))];
    let version = |text: &str| vec![reply(TerminalReply::Version(text.to_string()))];
    let mode = |mode: u16, state: u8| vec![reply(TerminalReply::Mode { mode, state })];
    let flags = |flags: u8| vec![reply(TerminalReply::KittyKeyboardFlags(flags))];
    vec![
        case(csi("?31u"), flags(31)),
        case(csi("?0u"), flags(0)),
        case(csi("?u"), flags(0)),
        case(csi("?11u"), flags(11)),
        case(csi("?62;4;22c"), da("62;4;22")),
        case(csi("?1;2c"), da("1;2")),
        case(csi("?c"), da("")),
        case(csi("?997;1n"), vec![reply(TerminalReply::DarkMode(true))]),
        case(csi("?997;2n"), vec![reply(TerminalReply::DarkMode(false))]),
        case("\x1b]10;rgb:ffff/ffff/ffff\x07", vec![color(ColorSlot::Foreground, 255, 255, 255)]),
        case("\x1b]11;rgb:1a1a/1b1b/2626\x1b\\", vec![color(ColorSlot::Background, 0x1a, 0x1b, 0x26)]),
        case("\x1b]12;rgb:ff/00/80\x1b\\", vec![color(ColorSlot::Cursor, 255, 0, 128)]),
        case("\x1b]11;rgb:f/0/8\x07", vec![color(ColorSlot::Background, 255, 0, 136)]),
        case("\x1b]11;rgb:fff/000/800\x07", vec![color(ColorSlot::Background, 255, 0, 128)]),
        case("\x1b]11;rgb:1/22/333\x07", vec![color(ColorSlot::Background, 17, 34, 51)]),
        case("\x1b]11;RGB:FFFF/0000/8080\x07", vec![color(ColorSlot::Background, 255, 0, 128)]),
        case("\x1b]11;rgba:1212/3434/5656/ffff\x07", vec![color(ColorSlot::Background, 0x12, 0x34, 0x56)]),
        case("\x1b]11;#123456\x07", vec![color(ColorSlot::Background, 0x12, 0x34, 0x56)]),
        case("\x1b]11;#fff\x07", vec![color(ColorSlot::Background, 255, 255, 255)]),
        case("\x1b]10;#ffff00008080\x1b\\", vec![color(ColorSlot::Foreground, 255, 0, 128)]),
        case("\x1b]4;1;rgb:cdcd/0000/0000\x07", vec![color(ColorSlot::Palette(1), 0xcd, 0, 0)]),
        case("\x1b]4;255;rgb:ee/ee/ee\x1b\\", vec![color(ColorSlot::Palette(255), 0xee, 0xee, 0xee)]),
        case(
            "\x1b]4;0;rgb:00/00/00;7;rgb:ff/ff/ff\x07",
            vec![color(ColorSlot::Palette(0), 0, 0, 0), color(ColorSlot::Palette(7), 255, 255, 255)],
        ),
        case("\x1bP>|ghostty 1.2.3\x1b\\", version("ghostty 1.2.3")),
        case("\x1bP>|kitty(0.42.1)\x1b\\", version("kitty(0.42.1)")),
        case("\x1bP>|\x1b\\", version("")),
        case("\x1bP>|t\x07x\x1b\\", version("t\x07x")),
        case(csi("?2026;1$y"), mode(2026, 1)),
        case(csi("?2026;2$y"), mode(2026, 2)),
        case(csi("?2031;0$y"), mode(2031, 0)),
        case(csi("?2004;4$y"), mode(2004, 4)),
    ]
}

/// Complete sequences that are not input for bakedit: they produce nothing and, above all,
/// no text.
#[rustfmt::skip]
fn ignored_cases() -> Vec<Case> {
    let silent = |bytes: &[u8]| case(bytes, vec![]);
    vec![
        // CSI: a cursor position report, other device attributes, window reports, the rest.
        silent(b"\x1b[24;80R"),
        silent(b"\x1b[5R"),
        silent(b"\x1b[>1;4000;29c"),
        silent(b"\x1b[=5u"),
        silent(b"\x1b[>4;2m"),
        silent(b"\x1b[8;24;80t"),
        silent(b"\x1b[0n"),
        silent(b"\x1b[?6n"),
        silent(b"\x1b[?997;3n"),
        silent(b"\x1b[?997n"),
        silent(b"\x1b[1;2;3x"),
        silent(b"\x1b[?1;2;3x"),
        silent(b"\x1b[2026;1$y"),
        silent(b"\x1b[?70000;1$y"),
        silent(b"\x1b[?2026;300$y"),
        silent(b"\x1b[?2026$y"),
        silent(b"\x1b[1 q"),
        silent(b"\x1b[!p"),
        silent(b"\x1b[1$2y"),
        silent(b"\x1b[1?2u"),
        silent(b"\x1b[u"),
        silent(b"\x1b[0u"),
        silent(b"\x1b[0;1:3;229u"),
        silent(b"\x1b[201~"),
        silent(b"\x1b[27~"),
        silent(b"\x1b[27;5~"),
        silent(b"\x1b[27;5;1~"),
        silent(b"\x1b[9~"),
        silent(b"\x1b[10~"),
        silent(b"\x1b[16~"),
        silent(b"\x1b[22~"),
        silent(b"\x1b[27;;~"),
        silent(b"\x1b[30~"),
        silent(b"\x1b[35~"),
        silent(b"\x1b[~"),
        silent(b"\x1b[3;5;7~"),
        silent(b"\x1b[1;2;3A"),
        silent(b"\x1b[3;5A"),
        silent(b"\x1b[1I"),
        silent(b"\x1b[1O"),
        silent(b"\x1b[a"),
        silent(b"\x1b[1;5a"),
        silent(b"\x1b[0;5;7M"),
        // Strings: other OSCs, malformed colours, DCS, APC, PM, SOS.
        silent(b"\x1b]0;window title\x07"),
        silent(b"\x1b]52;c;aGVsbG8=\x1b\\"),
        silent(b"\x1b]11;garbage\x07"),
        silent(b"\x1b]11;rgb:12/34\x07"),
        silent(b"\x1b]11;rgb:12/34/56/78\x07"),
        silent(b"\x1b]11;rgb:12345/00/00\x07"),
        silent(b"\x1b]11;rgb:zz/00/00\x07"),
        silent(b"\x1b]11;rgb://\x07"),
        silent(b"\x1b]11;rgb:+f/00/00\x07"),
        silent(b"\x1b]11;#12345\x07"),
        silent(b"\x1b]11;#\x07"),
        silent(b"\x1b]11;?\x07"),
        silent(b"\x1b]11\x07"),
        silent(b"\x1b]4;256;rgb:00/00/00\x07"),
        silent(b"\x1b]4;x;rgb:00/00/00\x07"),
        silent(b"\x1b]4;1\x07"),
        silent(b"\x1b]\x07"),
        silent(b"\x1b]\x1b\\"),
        silent(b"\x1b]10;\xff\xfe\x07"),
        silent(b"\x1b]some text \xe6\x97\xa5\xe6\x9c\xac with utf-8\x1b\\"),
        silent(b"\x1bP1$r0m\x1b\\"),
        silent(b"\x1bP1+r544e\x1b\\"),
        silent(b"\x1bP\x1b\\"),
        silent(b"\x1bP>x\x1b\\"),
        silent(b"\x1b_Gi=31;OK\x1b\\"),
        silent(b"\x1b_\x07still inside\x1b\\"),
        silent(b"\x1b^private message\x1b\\"),
        silent(b"\x1bXstart of string\x1b\\"),
        // Bytes that are not UTF-8, or are control characters dressed up as UTF-8.
        silent(b"\x80"),
        silent(b"\xbf"),
        silent(b"\xff"),
        silent(b"\xc0\x80"),
        silent(b"\xc1\xbf"),
        silent(b"\xed\xa0\x80"),
        silent(b"\xf4\x90\x80\x80"),
        silent(b"\xf5\x80\x80\x80"),
        silent(b"\xe0\x80\x80"),
        silent(b"\xc2\x85"),
        silent(b"\xc2\x9b"),
    ]
}

/// Bytes that are complete but where a broken piece is followed by something valid.
#[rustfmt::skip]
fn recovery_cases() -> Vec<Case> {
    let none = Mods::empty();
    let da = reply(TerminalReply::DeviceAttributes("62".into()));
    vec![
        case(b"\xc3(", vec![typed('(')]),
        case(b"\xe6\x97x", vec![typed('x')]),
        case(b"\xf0\x9f\x98\x1b[A", vec![key(Key::Up, none)]),
        case(b"\xc3\xc3\xa9", vec![typed('é')]),
        case(b"\x1b\xc3x", vec![typed('x')]),
        // A CSI cut short by a byte it cannot hold is dropped; the byte stands on its own.
        case(b"\x1b[1;5\x1b[A", vec![key(Key::Up, none)]),
        case(b"\x1b[1;5\rx", vec![key(Key::Enter, none), typed('x')]),
        case(b"\x1b[1;5\xc3\xa9", vec![typed('é')]),
        case(b"\x1b[<0;1\x7f", vec![key(Key::Backspace, none)]),
        // A string cut short by a new sequence is dropped with its content.
        case(b"\x1b]11;rgb:00\x1b[A", vec![key(Key::Up, none)]),
        case(b"\x1b]0;title\x1ba", vec![key(Key::Char('a'), Mods::ALT)]),
        case(b"\x1bPabc\x1b\x1b[A", vec![key(Key::Up, Mods::ALT)]),
        // A legacy mouse report cut short by a control byte.
        case(b"\x1b[M \x1b[A", vec![key(Key::Up, none)]),
        case(b"\x1b[M\r", vec![key(Key::Enter, none)]),
        // Sequences back to back with text around them.
        case(b"a\x1b[Ab", vec![typed('a'), key(Key::Up, none), typed('b')]),
        case(b"\x1b[24;80Rx", vec![typed('x')]),
        case(b"\x1b]0;t\x07x", vec![typed('x')]),
        case(b"\x1b[?62c\x1b[?1u", vec![da, reply(TerminalReply::KittyKeyboardFlags(1))]),
    ]
}

#[test]
fn every_sequence_in_the_table_parses_to_its_events() {
    for (bytes, expected) in table().into_iter().chain(recovery_cases()) {
        assert_eq!(
            parse(&bytes),
            expected,
            "input {:?}",
            String::from_utf8_lossy(&bytes)
        );
    }
}

#[test]
fn a_sequence_cut_at_any_byte_boundary_parses_the_same() {
    for (bytes, expected) in table().into_iter().chain(recovery_cases()) {
        for cut in 1..bytes.len() {
            let mut parser = Parser::new();
            let mut events = parser.feed(&bytes[..cut]);
            events.extend(parser.feed(&bytes[cut..]));
            let input = String::from_utf8_lossy(&bytes);
            assert_eq!(events, expected, "input {input:?} cut at {cut}");
            assert!(!parser.has_pending());
        }
    }
}

#[test]
fn a_sequence_fed_one_byte_at_a_time_parses_the_same() {
    for (bytes, expected) in table().into_iter().chain(recovery_cases()) {
        let mut parser = Parser::new();
        let mut events = Vec::new();
        for byte in &bytes {
            events.extend(parser.feed(std::slice::from_ref(byte)));
        }
        assert_eq!(
            events,
            expected,
            "input {:?}",
            String::from_utf8_lossy(&bytes)
        );
        assert!(!parser.has_pending());
    }
}

#[test]
fn the_whole_table_as_one_stream_parses_to_all_its_events() {
    let mut stream = Vec::new();
    let mut expected = Vec::new();
    for (bytes, events) in table() {
        stream.extend(bytes);
        expected.extend(events);
    }
    assert_eq!(parse(&stream), expected);

    // The same stream in chunks of every small size.
    for size in 1..=17 {
        let mut parser = Parser::new();
        let mut events = Vec::new();
        for chunk in stream.chunks(size) {
            events.extend(parser.feed(chunk));
        }
        assert_eq!(events, expected, "chunks of {size}");
    }
}

#[test]
fn the_table_is_as_big_as_it_looks() {
    // A guard against a case list silently going missing from `table`.
    let count = table().len();
    assert!(count > 500, "only {count} cases");
}

#[test]
fn the_table_covers_every_kind_of_event() {
    let events: Vec<InputEvent> = table().into_iter().flat_map(|(_, events)| events).collect();
    let has = |test: &dyn Fn(&InputEvent) -> bool| events.iter().any(test);
    assert!(has(
        &|e| matches!(e, InputEvent::Key(k) if k.kind == KeyKind::Repeat)
    ));
    assert!(has(
        &|e| matches!(e, InputEvent::Key(k) if k.kind == KeyKind::Release)
    ));
    assert!(has(&|e| matches!(e, InputEvent::Mouse(_))));
    assert!(has(&|e| matches!(e, InputEvent::Paste(_))));
    assert!(has(&|e| matches!(e, InputEvent::FocusGained)));
    assert!(has(&|e| matches!(e, InputEvent::FocusLost)));
    assert!(has(&|e| matches!(
        e,
        InputEvent::Reply(TerminalReply::KittyKeyboardFlags(_))
    )));
    assert!(has(&|e| matches!(
        e,
        InputEvent::Reply(TerminalReply::DeviceAttributes(_))
    )));
    assert!(has(&|e| matches!(
        e,
        InputEvent::Reply(TerminalReply::DarkMode(_))
    )));
    assert!(has(&|e| matches!(
        e,
        InputEvent::Reply(TerminalReply::Color { .. })
    )));
    assert!(has(&|e| matches!(
        e,
        InputEvent::Reply(TerminalReply::Version(_))
    )));
    assert!(has(&|e| matches!(
        e,
        InputEvent::Reply(TerminalReply::Mode { .. })
    )));
}
