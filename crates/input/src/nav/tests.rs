//! Nav layer tests: one or more for every rule of the layer.

mod odd_orders;

use std::time::{Duration, Instant};

use bakedit_core::{Key, KeyEvent, KeyKind, ModKey, Mods};

use super::{NavLayer, NavLayerConfig, MAX_TRACKED_KEYS};

/// A layer with a clock the test moves by hand.
struct Rig {
    layer: NavLayer,
    now: Instant,
}

impl Rig {
    fn new() -> Rig {
        Rig::with(NavLayerConfig::default())
    }

    fn with(config: NavLayerConfig) -> Rig {
        Rig {
            layer: NavLayer::new(config),
            now: Instant::now(),
        }
    }

    fn wait(&mut self, millis: u64) {
        self.now += Duration::from_millis(millis);
    }

    fn send(&mut self, ev: KeyEvent) -> Vec<KeyEvent> {
        self.wait(5);
        self.layer.process(ev, self.now)
    }

    /// The key that types `c` goes down.
    fn down(&mut self, c: char) -> Vec<KeyEvent> {
        self.send(press(c))
    }

    /// The key that types `c` auto-repeats.
    fn again(&mut self, c: char) -> Vec<KeyEvent> {
        self.send(repeat(c))
    }

    /// The key that types `c` comes up.
    fn up(&mut self, c: char) -> Vec<KeyEvent> {
        self.send(release(c))
    }
}

/// The events of the real keyboard, as the parser reports them in kitty mode.
fn press(c: char) -> KeyEvent {
    KeyEvent::typed(c)
}

fn repeat(c: char) -> KeyEvent {
    KeyEvent::typed(c).with_kind(KeyKind::Repeat)
}

fn release(c: char) -> KeyEvent {
    let mut ev = KeyEvent::typed(c).with_kind(KeyKind::Release);
    ev.shifted = None;
    ev
}

/// The events the layer is expected to send.
fn sent(key: Key, mods: Mods) -> KeyEvent {
    KeyEvent::new(key, mods)
}

fn sent_repeat(key: Key, mods: Mods) -> KeyEvent {
    KeyEvent::new(key, mods).with_kind(KeyKind::Repeat)
}

fn sent_release(key: Key, mods: Mods) -> KeyEvent {
    KeyEvent::new(key, mods).with_kind(KeyKind::Release)
}

const NONE: Mods = Mods::empty();

#[test]
fn the_default_mapping_is_the_karabiner_rule() {
    let config = NavLayerConfig::default();
    assert!(config.enabled);
    assert_eq!(config.trigger, ';');
    assert_eq!(
        (config.up, config.left, config.down, config.right),
        ('e', 's', 'd', 'f')
    );
    assert_eq!(
        (config.shift, config.cmd, config.alt, config.ctrl),
        ('j', 'k', 'l', 'h')
    );
    assert_eq!((config.tab, config.escape), ('a', 'c'));
    assert_eq!(config.tap_passthrough, Mods::SUPER | Mods::ALT);
    assert_eq!(config.alone_timeout, Duration::from_millis(1000));
    assert_eq!(NavLayer::REQUIRED_KITTY_FLAGS, 2 | 8);
    assert_eq!(NavLayer::new(config.clone()).config(), &config);
}

#[test]
fn disabled_everything_passes_through_untouched() {
    let mut rig = Rig::with(NavLayerConfig {
        enabled: false,
        ..NavLayerConfig::default()
    });
    assert!(!rig.layer.enabled());
    for c in [';', 'e', 'j', 'a', 'x'] {
        assert_eq!(rig.down(c), vec![press(c)]);
        assert_eq!(rig.again(c), vec![repeat(c)]);
        assert!(!rig.layer.active());
    }
    for c in ['x', 'a', 'j', 'e', ';'] {
        assert_eq!(rig.up(c), vec![release(c)]);
    }
}

#[test]
fn the_trigger_tapped_alone_types_itself_on_release() {
    let mut rig = Rig::new();
    assert_eq!(rig.down(';'), vec![]);
    assert!(rig.layer.active());
    assert_eq!(rig.up(';'), vec![press(';'), release(';')]);
    assert!(!rig.layer.active());
    assert_eq!(press(';').text.as_deref(), Some(";"));
}

#[test]
fn the_trigger_keeps_its_modifiers_and_text_when_tapped() {
    // shift+; types a colon.
    let mut rig = Rig::new();
    let colon = KeyEvent::typed(':');
    assert_eq!((colon.key, colon.mods), (Key::Char(';'), Mods::SHIFT));
    assert_eq!(rig.send(colon.clone()), vec![]);
    assert!(
        rig.layer.active(),
        "the trigger works whatever modifiers are down"
    );
    let up = KeyEvent::new(Key::Char(';'), Mods::SHIFT).with_kind(KeyKind::Release);
    assert_eq!(rig.send(up.clone()), vec![colon, up]);

    // cmd+; is a shortcut and still reaches the app, on release.
    let shortcut = KeyEvent::new(Key::Char(';'), Mods::SUPER);
    assert_eq!(rig.send(shortcut.clone()), vec![]);
    assert_eq!(rig.up(';'), vec![shortcut, release(';')]);
}

#[test]
fn the_trigger_held_past_the_timeout_types_nothing() {
    let mut rig = Rig::new();
    rig.down(';');
    rig.wait(1000);
    assert_eq!(rig.up(';'), vec![]);
    assert!(!rig.layer.active());

    // Just inside the timeout it still types. `send` itself advances the clock by 5 ms.
    rig.down(';');
    rig.wait(994);
    assert_eq!(rig.up(';'), vec![press(';'), release(';')]);
    rig.down(';');
    rig.wait(995);
    assert_eq!(rig.up(';'), vec![]);
}

#[test]
fn the_timeout_is_configurable() {
    let mut rig = Rig::with(NavLayerConfig {
        alone_timeout: Duration::from_millis(200),
        ..NavLayerConfig::default()
    });
    rig.down(';');
    rig.wait(150);
    assert_eq!(rig.up(';').len(), 2);
    rig.down(';');
    rig.wait(250);
    assert_eq!(rig.up(';'), vec![]);

    // A timeout of zero never types.
    let mut rig = Rig::with(NavLayerConfig {
        alone_timeout: Duration::ZERO,
        ..NavLayerConfig::default()
    });
    rig.down(';');
    assert_eq!(rig.layer.process(release(';'), rig.now), vec![]);
}

#[test]
fn a_clock_that_goes_backwards_is_no_trouble() {
    let mut rig = Rig::new();
    rig.wait(10_000);
    rig.down(';');
    let earlier = rig.now - Duration::from_secs(5);
    assert_eq!(
        rig.layer.process(release(';'), earlier),
        vec![press(';'), release(';')]
    );
}

#[test]
fn e_s_d_f_are_the_arrows() {
    let mut rig = Rig::new();
    assert_eq!(rig.down(';'), vec![]);
    for (c, key) in [
        ('e', Key::Up),
        ('s', Key::Left),
        ('d', Key::Down),
        ('f', Key::Right),
    ] {
        assert_eq!(rig.down(c), vec![sent(key, NONE)], "{c}");
        assert_eq!(rig.up(c), vec![sent_release(key, NONE)], "{c}");
    }
    // The layer was used, so the trigger types nothing.
    assert_eq!(rig.up(';'), vec![]);
    // And outside the layer the keys are themselves again.
    assert_eq!(rig.down('e'), vec![press('e')]);
    assert_eq!(rig.up('e'), vec![release('e')]);
}

#[test]
fn arrows_carry_no_text_and_keep_the_lock_states() {
    let mut rig = Rig::new();
    rig.down(';');
    let mut e = press('e');
    e.caps_lock = true;
    e.num_lock = true;
    let out = rig.send(e);
    assert_eq!(out.len(), 1);
    assert_eq!(
        (out[0].key, out[0].text.clone(), out[0].shifted, out[0].base),
        (Key::Up, None, None, None)
    );
    assert!(out[0].caps_lock && out[0].num_lock && !out[0].keypad);
}

#[test]
fn j_k_l_h_are_modifiers_that_send_nothing() {
    let mut rig = Rig::new();
    rig.down(';');
    assert_eq!(rig.layer.virtual_mods(), NONE);
    for (c, bit) in [
        ('j', Mods::SHIFT),
        ('k', Mods::SUPER),
        ('l', Mods::ALT),
        ('h', Mods::CTRL),
    ] {
        assert_eq!(rig.down(c), vec![], "{c}");
        assert_eq!(rig.layer.virtual_mods(), bit, "{c}");
        assert_eq!(rig.down('f'), vec![sent(Key::Right, bit)], "{c}");
        assert_eq!(rig.up('f'), vec![sent_release(Key::Right, bit)], "{c}");
        assert_eq!(rig.up(c), vec![], "{c}");
        assert_eq!(rig.layer.virtual_mods(), NONE, "{c}");
    }
    assert_eq!(rig.down('f'), vec![sent(Key::Right, NONE)]);
    assert_eq!(rig.up('f'), vec![sent_release(Key::Right, NONE)]);
    assert_eq!(rig.up(';'), vec![]);
}

#[test]
fn layer_modifiers_stack() {
    let mut rig = Rig::new();
    rig.down(';');
    rig.down('j');
    rig.down('l');
    assert_eq!(rig.layer.virtual_mods(), Mods::SHIFT | Mods::ALT);
    assert_eq!(
        rig.down('s'),
        vec![sent(Key::Left, Mods::SHIFT | Mods::ALT)]
    );
    rig.up('s');
    rig.down('h');
    rig.down('k');
    assert_eq!(
        rig.layer.virtual_mods(),
        Mods::SHIFT | Mods::ALT | Mods::CTRL | Mods::SUPER
    );
    rig.up('l');
    assert_eq!(
        rig.down('e'),
        vec![sent(Key::Up, Mods::SHIFT | Mods::CTRL | Mods::SUPER)]
    );
}

#[test]
fn a_modifier_key_used_alone_still_stops_the_trigger_from_typing() {
    let mut rig = Rig::new();
    rig.down(';');
    assert_eq!(rig.down('j'), vec![]);
    assert_eq!(rig.up('j'), vec![]);
    assert_eq!(rig.up(';'), vec![]);
}

#[test]
fn a_is_tab_and_c_is_escape() {
    let mut rig = Rig::new();
    rig.down(';');
    assert_eq!(rig.down('a'), vec![sent(Key::Tab, NONE)]);
    assert_eq!(rig.up('a'), vec![sent_release(Key::Tab, NONE)]);
    assert_eq!(rig.down('c'), vec![sent(Key::Escape, NONE)]);
    assert_eq!(rig.up('c'), vec![sent_release(Key::Escape, NONE)]);
    // With one layer modifier they are still Tab and Escape: shift-tab, cmd-tab, alt-escape.
    for (c, bit) in [
        ('j', Mods::SHIFT),
        ('k', Mods::SUPER),
        ('l', Mods::ALT),
        ('h', Mods::CTRL),
    ] {
        rig.down(c);
        assert_eq!(rig.down('a'), vec![sent(Key::Tab, bit)]);
        assert_eq!(rig.up('a'), vec![sent_release(Key::Tab, bit)]);
        assert_eq!(rig.down('c'), vec![sent(Key::Escape, bit)]);
        assert_eq!(rig.up('c'), vec![sent_release(Key::Escape, bit)]);
        rig.up(c);
    }
    assert_eq!(rig.up(';'), vec![]);
}

#[test]
fn with_cmd_and_alt_held_a_and_c_stay_themselves() {
    let mut rig = Rig::new();
    rig.down(';');
    rig.down('k');
    rig.down('l');
    let cmd_alt = Mods::SUPER | Mods::ALT;
    // cmd-alt-a and cmd-alt-c reach the app as shortcuts, so they type nothing.
    assert_eq!(rig.down('a'), vec![sent(Key::Char('a'), cmd_alt)]);
    assert_eq!(rig.again('a'), vec![sent_repeat(Key::Char('a'), cmd_alt)]);
    assert_eq!(rig.up('a'), vec![sent_release(Key::Char('a'), cmd_alt)]);
    assert_eq!(rig.down('c'), vec![sent(Key::Char('c'), cmd_alt)]);
    assert_eq!(rig.up('c'), vec![sent_release(Key::Char('c'), cmd_alt)]);
    // A third modifier does not change that.
    rig.down('j');
    assert_eq!(
        rig.down('a'),
        vec![sent(Key::Char('a'), cmd_alt | Mods::SHIFT)]
    );
    rig.up('a');
    rig.up('j');
    // The arrows are not affected.
    assert_eq!(rig.down('e'), vec![sent(Key::Up, cmd_alt)]);
    rig.up('e');
    // With alt released it is cmd-tab again.
    rig.up('l');
    assert_eq!(rig.down('a'), vec![sent(Key::Tab, Mods::SUPER)]);
}

#[test]
fn real_cmd_and_alt_count_for_the_exception_too() {
    let mut rig = Rig::new();
    rig.down(';');
    let cmd_alt = Mods::SUPER | Mods::ALT;
    let real = KeyEvent::new(Key::Char('a'), cmd_alt);
    assert_eq!(rig.send(real.clone()), vec![real]);
    assert_eq!(
        rig.send(release('a')),
        vec![sent_release(Key::Char('a'), cmd_alt)]
    );
    // One real and one from the layer.
    rig.down('k');
    let real_alt = KeyEvent::new(Key::Char('c'), Mods::ALT);
    assert_eq!(rig.send(real_alt), vec![sent(Key::Char('c'), cmd_alt)]);
    assert_eq!(rig.up(';'), vec![sent_release(Key::Char('c'), cmd_alt)]);
}

#[test]
fn the_exception_is_configurable() {
    // No passthrough modifiers: the tap keys are always Tab and Escape.
    let mut rig = Rig::with(NavLayerConfig {
        tap_passthrough: Mods::empty(),
        ..NavLayerConfig::default()
    });
    rig.down(';');
    rig.down('k');
    rig.down('l');
    assert_eq!(rig.down('a'), vec![sent(Key::Tab, Mods::SUPER | Mods::ALT)]);

    let mut rig = Rig::with(NavLayerConfig {
        tap_passthrough: Mods::CTRL,
        ..NavLayerConfig::default()
    });
    rig.down(';');
    rig.down('h');
    assert_eq!(rig.down('c'), vec![sent(Key::Char('c'), Mods::CTRL)]);
}

#[test]
fn another_key_with_a_layer_modifier_becomes_a_shortcut() {
    // ; + k + t is cmd-t.
    let mut rig = Rig::new();
    rig.down(';');
    rig.down('k');
    assert_eq!(press('t').text.as_deref(), Some("t"));
    assert_eq!(rig.down('t'), vec![sent(Key::Char('t'), Mods::SUPER)]);
    assert_eq!(
        rig.again('t'),
        vec![sent_repeat(Key::Char('t'), Mods::SUPER)]
    );
    assert_eq!(rig.up('t'), vec![sent_release(Key::Char('t'), Mods::SUPER)]);
    // Keys that are not characters too: cmd-enter, cmd-backspace.
    assert_eq!(
        rig.send(sent(Key::Enter, NONE)),
        vec![sent(Key::Enter, Mods::SUPER)]
    );
    assert_eq!(
        rig.send(sent_release(Key::Enter, NONE)),
        vec![sent_release(Key::Enter, Mods::SUPER)]
    );
    rig.up('k');
    assert_eq!(rig.up(';'), vec![]);
}

#[test]
fn a_shortcut_keeps_what_the_terminal_said_about_the_key() {
    let mut rig = Rig::new();
    rig.down(';');
    rig.down('k');
    let mut real = KeyEvent::typed('/');
    real.base = Some('/');
    real.shifted = Some('?');
    real.mods = Mods::SHIFT;
    let out = rig.send(real);
    assert_eq!(out.len(), 1);
    assert_eq!(
        (out[0].key, out[0].mods),
        (Key::Char('/'), Mods::SUPER | Mods::SHIFT)
    );
    assert_eq!(
        (out[0].shifted, out[0].base, out[0].text.clone()),
        (Some('?'), Some('/'), None)
    );
}

#[test]
fn typing_with_rollover_cancels_the_layer() {
    // `;` then Enter, with Enter going down before `;` comes up.
    let mut rig = Rig::new();
    let enter = sent(Key::Enter, NONE);
    assert_eq!(rig.down(';'), vec![]);
    assert_eq!(rig.send(enter.clone()), vec![press(';'), enter]);
    assert!(!rig.layer.active());
    // Until `;` is released nothing is mapped, not even the layer's own keys.
    assert_eq!(rig.down('e'), vec![press('e')]);
    assert_eq!(rig.down('j'), vec![press('j')]);
    assert_eq!(rig.again('j'), vec![repeat('j')]);
    assert_eq!(rig.again(';'), vec![repeat(';')]);
    assert_eq!(rig.layer.virtual_mods(), NONE);
    // The release of `;` passes through: its press was sent.
    assert_eq!(rig.up(';'), vec![release(';')]);
    assert_eq!(rig.up('e'), vec![release('e')]);
    assert_eq!(rig.up('j'), vec![release('j')]);
    assert_eq!(
        rig.send(sent_release(Key::Enter, NONE)),
        vec![sent_release(Key::Enter, NONE)]
    );
    // The next hold is a fresh start.
    assert_eq!(rig.down(';'), vec![]);
    assert_eq!(rig.down('e'), vec![sent(Key::Up, NONE)]);
}

#[test]
fn a_key_pressed_after_the_tap_timeout_is_not_rollover_typing() {
    // `;` held for a while and then Enter: nobody types that slowly. Enter is just Enter,
    // no `;` appears, and the layer stays on.
    let enter = sent(Key::Enter, NONE);
    let mut rig = Rig::new();
    rig.down(';');
    rig.wait(1500);
    assert_eq!(rig.send(enter.clone()), vec![enter.clone()]);
    assert!(rig.layer.active());
    assert_eq!(rig.down('e'), vec![sent(Key::Up, NONE)]);
    assert_eq!(rig.up('e'), vec![sent_release(Key::Up, NONE)]);
    assert_eq!(rig.up(';'), vec![]);

    // Just inside the timeout it is still typing.
    rig.down(';');
    rig.wait(990);
    assert_eq!(rig.send(enter.clone()), vec![press(';'), enter.clone()]);
    assert_eq!(rig.up(';'), vec![release(';')]);

    // Passing a key through late does not use the layer or type the trigger afterwards.
    rig.down(';');
    rig.wait(1500);
    assert_eq!(rig.send(enter.clone()), vec![enter]);
    assert_eq!(rig.up(';'), vec![]);
}

#[test]
fn rollover_typing_keeps_the_order_of_the_characters() {
    // "x; y" typed fast: every key goes down before the previous one is up.
    let mut rig = Rig::new();
    let mut typed = String::new();
    let steps = [
        press('x'),
        press(';'),
        release('x'),
        press(' '),
        release(';'),
        press('y'),
        release(' '),
        release('y'),
    ];
    for step in steps {
        for ev in rig.send(step) {
            if ev.is_press() {
                typed.push_str(ev.text.as_deref().unwrap_or(""));
            }
        }
    }
    assert_eq!(typed, "x; y");
}

#[test]
fn a_shifted_trigger_rolls_over_with_its_text() {
    // ":" then space, as in `key: value`.
    let mut rig = Rig::new();
    let colon = KeyEvent::typed(':');
    assert_eq!(rig.send(colon.clone()), vec![]);
    assert_eq!(rig.down(' '), vec![colon, press(' ')]);
}

#[test]
fn after_the_layer_was_used_other_keys_pass_through_and_it_stays_on() {
    let mut rig = Rig::new();
    rig.down(';');
    rig.down('e');
    rig.up('e');
    // No layer modifier is held and the layer was used: `x` is just `x`, text and all.
    assert_eq!(rig.down('x'), vec![press('x')]);
    assert_eq!(rig.again('x'), vec![repeat('x')]);
    assert_eq!(rig.up('x'), vec![release('x')]);
    assert!(rig.layer.active());
    assert_eq!(rig.down('d'), vec![sent(Key::Down, NONE)]);
    assert_eq!(rig.up('d'), vec![sent_release(Key::Down, NONE)]);
    assert_eq!(rig.up(';'), vec![]);
}

#[test]
fn a_mapped_key_repeats_as_what_it_was_mapped_to() {
    let mut rig = Rig::new();
    rig.down(';');
    assert_eq!(rig.down('f'), vec![sent(Key::Right, NONE)]);
    assert_eq!(rig.again('f'), vec![sent_repeat(Key::Right, NONE)]);
    assert_eq!(rig.again('f'), vec![sent_repeat(Key::Right, NONE)]);
    // A modifier pressed while it repeats applies from then on: the selection grows.
    rig.down('j');
    assert_eq!(rig.again('f'), vec![sent_repeat(Key::Right, Mods::SHIFT)]);
    rig.up('j');
    assert_eq!(rig.again('f'), vec![sent_repeat(Key::Right, NONE)]);
    assert_eq!(rig.up('f'), vec![sent_release(Key::Right, NONE)]);
    // Tab repeats as Tab.
    rig.down('a');
    assert_eq!(rig.again('a'), vec![sent_repeat(Key::Tab, NONE)]);
}

#[test]
fn the_trigger_and_the_layer_modifiers_do_not_repeat() {
    let mut rig = Rig::new();
    rig.down(';');
    assert_eq!(rig.again(';'), vec![]);
    assert_eq!(rig.again(';'), vec![]);
    rig.down('j');
    assert_eq!(rig.again('j'), vec![]);
    assert_eq!(rig.layer.virtual_mods(), Mods::SHIFT);
    rig.up('j');
    // Repeats alone did not use the layer... but `j` did.
    assert_eq!(rig.up(';'), vec![]);

    let mut rig = Rig::new();
    rig.down(';');
    rig.again(';');
    assert_eq!(
        rig.up(';'),
        vec![press(';'), release(';')],
        "its own repeats do not count as another key"
    );
}

#[test]
fn real_modifiers_combine_with_the_layers() {
    let mut rig = Rig::new();
    rig.down(';');
    // Real shift held: the terminal reports it on the key.
    let shifted_e = KeyEvent::typed('E');
    assert_eq!(shifted_e.mods, Mods::SHIFT);
    assert_eq!(rig.send(shifted_e), vec![sent(Key::Up, Mods::SHIFT)]);
    assert_eq!(rig.up('e'), vec![sent_release(Key::Up, Mods::SHIFT)]);
    // Real shift and the layer's ctrl.
    rig.down('h');
    assert_eq!(
        rig.send(KeyEvent::typed('F')),
        vec![sent(Key::Right, Mods::SHIFT | Mods::CTRL)]
    );
    // The repeat follows the real modifier as the terminal reports it now.
    assert_eq!(rig.again('f'), vec![sent_repeat(Key::Right, Mods::CTRL)]);
    // The release has the modifiers of the press.
    assert_eq!(
        rig.up('f'),
        vec![sent_release(Key::Right, Mods::SHIFT | Mods::CTRL)]
    );
}

#[test]
fn real_modifier_keys_pass_through_and_count_for_nothing() {
    let shift_down = KeyEvent::new(Key::Modifier(ModKey::LeftShift), Mods::SHIFT);
    let shift_up =
        KeyEvent::new(Key::Modifier(ModKey::LeftShift), NONE).with_kind(KeyKind::Release);

    // Held alone with the trigger: the trigger still types.
    let mut rig = Rig::new();
    rig.down(';');
    assert_eq!(rig.send(shift_down.clone()), vec![shift_down.clone()]);
    assert_eq!(rig.send(shift_up.clone()), vec![shift_up.clone()]);
    assert!(rig.layer.active(), "a modifier key is not typing");
    assert_eq!(rig.up(';'), vec![press(';'), release(';')]);

    // Pressed inside the layer: it does not cancel it.
    rig.down(';');
    assert_eq!(rig.send(shift_down.clone()), vec![shift_down.clone()]);
    assert_eq!(
        rig.send(KeyEvent::typed('E')),
        vec![sent(Key::Up, Mods::SHIFT)]
    );
    assert_eq!(rig.send(shift_up.clone()), vec![shift_up.clone()]);
    assert_eq!(rig.up('e'), vec![sent_release(Key::Up, Mods::SHIFT)]);
    assert_eq!(rig.up(';'), vec![]);

    // The usual way to type a colon: shift down, `;` tapped, shift up.
    assert_eq!(rig.send(shift_down.clone()), vec![shift_down]);
    let colon = KeyEvent::typed(':');
    assert_eq!(rig.send(colon.clone()), vec![]);
    let colon_up = KeyEvent::new(Key::Char(';'), Mods::SHIFT).with_kind(KeyKind::Release);
    assert_eq!(rig.send(colon_up.clone()), vec![colon, colon_up]);
    assert_eq!(rig.send(shift_up.clone()), vec![shift_up]);
}

#[test]
fn text_that_belongs_to_no_key_passes_through_and_counts_for_nothing() {
    // An input method commits text while the trigger is held: it is not a key press.
    let text = KeyEvent::new(Key::Other(0), NONE).with_text("日本; e");
    let mut rig = Rig::new();
    rig.down(';');
    assert_eq!(rig.send(text.clone()), vec![text.clone()]);
    assert!(rig.layer.active(), "text is not typing with rollover");
    assert_eq!(rig.up(';'), vec![press(';'), release(';')]);
    // With a layer modifier held it is still just text.
    rig.down(';');
    rig.down('k');
    assert_eq!(rig.send(text.clone()), vec![text]);
    rig.up('k');
    assert_eq!(rig.up(';'), vec![]);
}

#[test]
fn keypad_keys_are_never_layer_keys() {
    let mut rig = Rig::with(NavLayerConfig {
        trigger: '/',
        up: '8',
        ..NavLayerConfig::default()
    });
    let mut keypad_slash = press('/');
    keypad_slash.keypad = true;
    assert_eq!(
        rig.send(keypad_slash.clone()),
        vec![keypad_slash],
        "the keypad's / is not the trigger"
    );
    assert!(!rig.layer.active());

    rig.down('/');
    rig.down('8');
    rig.up('8');
    let mut keypad_8 = press('8');
    keypad_8.keypad = true;
    assert_eq!(
        rig.send(keypad_8.clone()),
        vec![keypad_8],
        "the keypad's 8 is not Up"
    );
}

#[test]
fn the_mapping_is_configurable() {
    let config = NavLayerConfig {
        trigger: ',',
        up: 'i',
        left: 'j',
        down: 'k',
        right: 'l',
        shift: 'f',
        cmd: 'd',
        alt: 's',
        ctrl: 'a',
        tab: 'q',
        escape: 'w',
        ..NavLayerConfig::default()
    };
    let mut rig = Rig::with(config);
    // `;` is an ordinary key now.
    assert_eq!(rig.down(';'), vec![press(';')]);
    assert_eq!(rig.up(';'), vec![release(';')]);
    rig.down(',');
    assert_eq!(rig.down('i'), vec![sent(Key::Up, NONE)]);
    rig.up('i');
    rig.down('f');
    assert_eq!(rig.down('l'), vec![sent(Key::Right, Mods::SHIFT)]);
    rig.up('l');
    rig.up('f');
    assert_eq!(rig.down('q'), vec![sent(Key::Tab, NONE)]);
    assert_eq!(rig.down('w'), vec![sent(Key::Escape, NONE)]);
    // The old layer keys mean nothing here: `e` passes through.
    assert_eq!(rig.down('e'), vec![press('e')]);
}

#[test]
fn a_key_with_two_jobs_takes_the_first_in_the_documented_order() {
    // `j` is both shift and Up: the modifier wins. `e` is both Up and Tab: the arrow wins.
    let config = NavLayerConfig {
        up: 'j',
        tab: 'e',
        left: 'e',
        ..NavLayerConfig::default()
    };
    let mut rig = Rig::with(config);
    rig.down(';');
    assert_eq!(rig.down('j'), vec![]);
    assert_eq!(rig.layer.virtual_mods(), Mods::SHIFT);
    assert_eq!(rig.down('e'), vec![sent(Key::Left, Mods::SHIFT)]);

    // The trigger wins over everything.
    let config = NavLayerConfig {
        up: ';',
        ..NavLayerConfig::default()
    };
    let mut rig = Rig::with(config);
    assert_eq!(rig.down(';'), vec![]);
    assert_eq!(rig.up(';'), vec![press(';'), release(';')]);
}

#[test]
fn set_config_releases_what_is_held_and_takes_effect() {
    let mut rig = Rig::new();
    rig.down(';');
    rig.down('e');
    let config = NavLayerConfig {
        trigger: ',',
        ..NavLayerConfig::default()
    };
    assert_eq!(
        rig.layer.set_config(config.clone()),
        vec![sent_release(Key::Up, NONE)]
    );
    assert_eq!(rig.layer.config(), &config);
    assert!(!rig.layer.active());
    assert_eq!(rig.down(','), vec![]);
    assert!(rig.layer.active());
}

#[test]
fn the_tracked_keys_stay_bounded() {
    // Presses whose releases never come, far more than a keyboard has keys.
    let mut rig = Rig::new();
    rig.down(';');
    rig.down('k');
    for code in 0x4e00..0x4e00 + 1000u32 {
        let c = char::from_u32(code).unwrap();
        assert_eq!(rig.down(c), vec![sent(Key::Char(c), Mods::SUPER)]);
        assert!(rig.layer.mapped.len() <= MAX_TRACKED_KEYS);
    }
    let released = rig.up(';');
    assert_eq!(released.len(), MAX_TRACKED_KEYS);
    assert!(released.iter().all(KeyEvent::is_release));
    assert!(rig.layer.ghosts.len() <= MAX_TRACKED_KEYS);
    assert!(rig.layer.mapped.is_empty() && rig.layer.virtual_down.is_empty());
}
