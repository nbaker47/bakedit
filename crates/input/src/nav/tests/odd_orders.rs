//! Releases in odd orders, holds cut short by `reset` and `set_enabled`, lost releases, and
//! the promise that the layer never leaves a key stuck down.

use bakedit_core::{Key, KeyEvent, KeyKind, Mods};

use super::{press, release, repeat, sent, sent_release, sent_repeat, Rig, NONE};

#[test]
fn releasing_the_trigger_first_releases_what_the_layer_holds() {
    let mut rig = Rig::new();
    rig.down(';');
    assert_eq!(rig.down('f'), vec![sent(Key::Right, NONE)]);
    assert_eq!(rig.up(';'), vec![sent_release(Key::Right, NONE)]);
    assert!(!rig.layer.active());
    // `f` is still down. What it sends now belongs to the layer that is gone: no stray
    // "f" is typed by its repeats, and its release is not seen twice.
    assert_eq!(rig.again('f'), vec![]);
    assert_eq!(rig.again('f'), vec![]);
    assert_eq!(rig.up('f'), vec![]);
    // After that `f` is an ordinary key again.
    assert_eq!(rig.down('f'), vec![press('f')]);
    assert_eq!(rig.again('f'), vec![repeat('f')]);
    assert_eq!(rig.up('f'), vec![release('f')]);
}

#[test]
fn several_held_keys_are_released_in_the_order_they_went_down() {
    let mut rig = Rig::new();
    rig.down(';');
    rig.down('j');
    rig.down('e');
    rig.down('f');
    rig.down('a');
    assert_eq!(
        rig.up(';'),
        vec![
            sent_release(Key::Up, Mods::SHIFT),
            sent_release(Key::Right, Mods::SHIFT),
            sent_release(Key::Tab, Mods::SHIFT),
        ]
    );
    assert_eq!(
        rig.layer.virtual_mods(),
        NONE,
        "the layer's modifiers end with it"
    );
    for c in ['a', 'j', 'f', 'e'] {
        assert_eq!(rig.again(c), vec![], "{c}");
        assert_eq!(rig.up(c), vec![], "{c}");
    }
    assert_eq!(rig.down('j'), vec![press('j')]);
}

#[test]
fn a_layer_modifier_released_before_its_key_keeps_the_press_mods_on_the_release() {
    let mut rig = Rig::new();
    rig.down(';');
    rig.down('j');
    assert_eq!(rig.down('e'), vec![sent(Key::Up, Mods::SHIFT)]);
    assert_eq!(rig.up('j'), vec![]);
    assert_eq!(rig.again('e'), vec![sent_repeat(Key::Up, NONE)]);
    assert_eq!(rig.up('e'), vec![sent_release(Key::Up, Mods::SHIFT)]);
    // And the other way round: the modifier arrives after the press.
    assert_eq!(rig.down('e'), vec![sent(Key::Up, NONE)]);
    rig.down('j');
    assert_eq!(rig.up('e'), vec![sent_release(Key::Up, NONE)]);
    rig.up('j');
    assert_eq!(rig.up(';'), vec![]);
}

#[test]
fn a_layer_modifier_still_down_when_the_layer_ends_is_swallowed() {
    let mut rig = Rig::new();
    rig.down(';');
    rig.down('k');
    assert_eq!(rig.up(';'), vec![]);
    assert_eq!(rig.layer.virtual_mods(), NONE);
    assert_eq!(rig.again('k'), vec![], "no stray k is typed");
    assert_eq!(rig.up('k'), vec![]);
    assert_eq!(rig.down('k'), vec![press('k')]);
}

#[test]
fn a_shortcut_key_still_down_when_the_layer_ends_is_released_once() {
    let mut rig = Rig::new();
    rig.down(';');
    rig.down('k');
    assert_eq!(rig.down('t'), vec![sent(Key::Char('t'), Mods::SUPER)]);
    assert_eq!(rig.up(';'), vec![sent_release(Key::Char('t'), Mods::SUPER)]);
    assert_eq!(rig.again('t'), vec![]);
    assert_eq!(rig.up('t'), vec![]);
    assert_eq!(rig.up('k'), vec![]);
}

#[test]
fn a_key_held_from_before_the_layer_stays_itself() {
    // "a;" typed with rollover: `a` is released while `;` is already down.
    let mut rig = Rig::new();
    assert_eq!(rig.down('a'), vec![press('a')]);
    assert_eq!(rig.down(';'), vec![]);
    assert_eq!(rig.again('a'), vec![repeat('a')]);
    assert_eq!(rig.up('a'), vec![release('a')]);
    assert_eq!(
        rig.up(';'),
        vec![press(';'), release(';')],
        "a release is not another key"
    );

    // The same with one of the layer's modifier keys and one of its arrows.
    for c in ['j', 'e'] {
        assert_eq!(rig.down(c), vec![press(c)]);
        assert_eq!(rig.down(';'), vec![]);
        assert_eq!(rig.layer.virtual_mods(), NONE);
        assert_eq!(rig.up(c), vec![release(c)]);
        assert_eq!(rig.up(';'), vec![press(';'), release(';')]);
    }
}

#[test]
fn a_key_released_inside_the_layer_can_be_pressed_into_it() {
    // `e` went down before the layer, came up inside it, and goes down again as Up.
    let mut rig = Rig::new();
    rig.down('e');
    rig.down(';');
    assert_eq!(rig.up('e'), vec![release('e')]);
    assert_eq!(rig.down('e'), vec![sent(Key::Up, NONE)]);
    assert_eq!(rig.up(';'), vec![sent_release(Key::Up, NONE)]);
    assert_eq!(rig.up('e'), vec![]);
}

#[test]
fn disabling_in_the_middle_of_a_hold_releases_and_forgets() {
    let mut rig = Rig::new();
    rig.down(';');
    rig.down('j');
    rig.down('e');
    rig.down('a');
    assert!(rig.layer.active());
    assert_eq!(
        rig.layer.set_enabled(false),
        vec![
            sent_release(Key::Up, Mods::SHIFT),
            sent_release(Key::Tab, Mods::SHIFT)
        ]
    );
    assert!(!rig.layer.enabled());
    assert!(!rig.layer.active());
    assert_eq!(rig.layer.virtual_mods(), NONE);
    // From here every event passes through, the late releases included.
    assert_eq!(rig.again('e'), vec![repeat('e')]);
    assert_eq!(rig.up('e'), vec![release('e')]);
    assert_eq!(rig.up('j'), vec![release('j')]);
    assert_eq!(rig.up(';'), vec![release(';')]);
    assert_eq!(rig.down(';'), vec![press(';')]);
    assert_eq!(rig.up(';'), vec![release(';')]);
    // Disabling again is nothing; enabling starts clean.
    assert_eq!(rig.layer.set_enabled(false), vec![]);
    assert_eq!(rig.layer.set_enabled(true), vec![]);
    assert!(rig.layer.enabled());
    assert_eq!(rig.down(';'), vec![]);
    assert_eq!(rig.down('e'), vec![sent(Key::Up, NONE)]);
}

#[test]
fn enabling_while_enabled_keeps_the_hold() {
    let mut rig = Rig::new();
    rig.down(';');
    rig.down('e');
    assert_eq!(rig.layer.set_enabled(true), vec![]);
    assert!(rig.layer.active());
    assert_eq!(rig.up('e'), vec![sent_release(Key::Up, NONE)]);
}

#[test]
fn reset_in_the_middle_of_a_hold_releases_and_forgets() {
    // The terminal lost focus with `;`, `k` and `f` down.
    let mut rig = Rig::new();
    rig.down(';');
    rig.down('k');
    rig.down('f');
    assert_eq!(
        rig.layer.reset(),
        vec![sent_release(Key::Right, Mods::SUPER)]
    );
    assert!(rig.layer.enabled());
    assert!(!rig.layer.active());
    assert_eq!(rig.layer.virtual_mods(), NONE);
    assert_eq!(rig.layer.reset(), vec![], "nothing is released twice");
    // Releases that arrive after all pass through; nothing is left to swallow them.
    assert_eq!(rig.up('f'), vec![release('f')]);
    assert_eq!(rig.up('k'), vec![release('k')]);
    assert_eq!(rig.up(';'), vec![release(';')]);
    // The layer works as before.
    assert_eq!(rig.down(';'), vec![]);
    assert_eq!(rig.down('f'), vec![sent(Key::Right, NONE)]);
}

#[test]
fn reset_drops_a_trigger_press_that_was_being_kept_back() {
    let mut rig = Rig::new();
    rig.down(';');
    assert_eq!(rig.layer.reset(), vec![]);
    assert_eq!(
        rig.up(';'),
        vec![release(';')],
        "the tap is not typed after a reset"
    );
    assert_eq!(rig.layer.reset(), vec![]);
}

#[test]
fn reset_forgets_swallowed_keys_and_a_cancelled_hold() {
    let mut rig = Rig::new();
    rig.down(';');
    rig.down('f');
    rig.up(';');
    // `f` would be swallowed until its release; after a reset it is not.
    rig.layer.reset();
    assert_eq!(rig.again('f'), vec![repeat('f')]);

    rig.down(';');
    rig.down('x');
    assert!(!rig.layer.active(), "cancelled by typing");
    rig.layer.reset();
    assert_eq!(
        rig.down(';'),
        vec![],
        "the trigger works again without its release"
    );
    assert!(rig.layer.active());
}

#[test]
fn a_trigger_press_while_it_is_held_is_a_repeat() {
    // Some terminals report auto-repeat as presses. They must not restart the hold: the
    // layer stays as it is and the tap timer keeps running from the first press.
    let mut rig = Rig::new();
    rig.down(';');
    assert_eq!(rig.down('e'), vec![sent(Key::Up, NONE)]);
    assert_eq!(rig.down(';'), vec![]);
    assert!(rig.layer.active());
    assert_eq!(rig.up('e'), vec![sent_release(Key::Up, NONE)]);
    assert_eq!(
        rig.up(';'),
        vec![],
        "the layer was used, so nothing is typed"
    );

    rig.down(';');
    for _ in 0..20 {
        rig.wait(95);
        assert_eq!(rig.down(';'), vec![]);
    }
    assert_eq!(rig.up(';'), vec![], "held for two seconds: not a tap");

    rig.down(';');
    assert_eq!(rig.down(';'), vec![]);
    assert_eq!(
        rig.up(';'),
        vec![press(';'), release(';')],
        "typed once, not twice"
    );
}

#[test]
fn a_trigger_press_after_the_hold_was_cancelled_passes_through() {
    let mut rig = Rig::new();
    rig.down(';');
    assert_eq!(rig.down('x'), vec![press(';'), press('x')]);
    assert_eq!(rig.down(';'), vec![press(';')]);
    assert!(!rig.layer.active());
    assert_eq!(rig.down('e'), vec![press('e')]);
    assert_eq!(rig.up(';'), vec![release(';')]);
    // The release ended the cancelled hold; the layer is back.
    assert_eq!(rig.down(';'), vec![]);
    assert!(rig.layer.active());
}

#[test]
fn a_swallowed_key_stays_swallowed_until_it_comes_up() {
    // `;` was released first while `f` was repeating. Whether the terminal reports the
    // repeats as repeats or as presses, no "f" is typed.
    let mut rig = Rig::new();
    rig.down(';');
    rig.down('f');
    rig.up(';');
    assert_eq!(rig.down('f'), vec![]);
    assert_eq!(rig.again('f'), vec![]);
    assert_eq!(rig.down('f'), vec![]);
    assert_eq!(rig.up('f'), vec![]);
    assert_eq!(rig.down('f'), vec![press('f')]);
    assert_eq!(rig.again('f'), vec![repeat('f')]);
    assert_eq!(rig.up('f'), vec![release('f')]);
}

#[test]
fn a_mapped_key_pressed_twice_is_released_once() {
    let mut rig = Rig::new();
    rig.down(';');
    assert_eq!(rig.down('e'), vec![sent(Key::Up, NONE)]);
    rig.down('j');
    // No release in between, as from a terminal that reports repeats as presses.
    assert_eq!(rig.down('e'), vec![sent(Key::Up, Mods::SHIFT)]);
    assert_eq!(rig.down('j'), vec![]);
    assert_eq!(rig.up('e'), vec![sent_release(Key::Up, Mods::SHIFT)]);
    assert_eq!(
        rig.up('e'),
        vec![release('e')],
        "a second release is not the layer's"
    );
    assert_eq!(rig.up('j'), vec![]);
    assert_eq!(rig.layer.virtual_mods(), NONE);
    assert_eq!(rig.up(';'), vec![]);
}

#[test]
fn trigger_events_with_the_trigger_up_pass_through() {
    let mut rig = Rig::new();
    assert_eq!(rig.again(';'), vec![repeat(';')]);
    assert_eq!(rig.up(';'), vec![release(';')]);
    assert!(!rig.layer.active());
}

/// A small deterministic generator for the property test.
struct Rng(u64);

impl Rng {
    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) % bound as u64) as usize
    }
}

/// Follows a stream of key events and checks that it is one a real keyboard could have
/// produced: presses and releases alternate for every key.
#[derive(Default)]
struct Balance {
    down: Vec<Key>,
}

impl Balance {
    fn see(&mut self, ev: &KeyEvent, context: &str) {
        let held = self.down.iter().position(|key| *key == ev.key);
        match (ev.kind, held) {
            (KeyKind::Press, None) => self.down.push(ev.key),
            (KeyKind::Press, Some(_)) => panic!("{:?} pressed twice: {context}", ev.key),
            (KeyKind::Repeat, Some(_)) => {}
            (KeyKind::Repeat, None) => panic!("{:?} repeats without being down: {context}", ev.key),
            (KeyKind::Release, Some(index)) => {
                self.down.remove(index);
            }
            (KeyKind::Release, None) => {
                panic!("{:?} released without being down: {context}", ev.key)
            }
        }
    }
}

#[test]
fn the_layer_never_leaves_a_key_stuck_or_releases_one_it_did_not_press() {
    // Random but physically possible input: a key is pressed only when it is up, and
    // repeats or is released only when it is down. Whatever the order, what the layer
    // sends must be just as well-formed, and when every real key is up again nothing the
    // layer sent may still be down.
    let keys = [
        ';', 'e', 's', 'd', 'f', 'j', 'k', 'l', 'h', 'a', 'c', 'x', ' ', 't',
    ];
    let mut rng = Rng(0x5eed_1234_abcd_ef01);
    for round in 0..300 {
        let mut rig = Rig::new();
        let mut real: Vec<char> = Vec::new();
        let mut sent = Balance::default();
        let mut log = Vec::new();
        for _ in 0..60 {
            let c = keys[rng.below(keys.len())];
            let ev = match (real.iter().position(|held| *held == c), rng.below(3)) {
                (None, _) => {
                    real.push(c);
                    press(c)
                }
                (Some(_), 0) => repeat(c),
                (Some(index), _) => {
                    real.remove(index);
                    release(c)
                }
            };
            if rng.below(4) == 0 {
                rig.wait(600);
            }
            log.push(format!("{:?} {:?}", ev.kind, ev.key));
            for out in rig.send(ev) {
                sent.see(&out, &format!("round {round}: {}", log.join(", ")));
            }
        }
        // Let go of everything, in a random order.
        while !real.is_empty() {
            let c = real.remove(rng.below(real.len()));
            log.push(format!("Release {:?}", Key::Char(c)));
            for out in rig.send(release(c)) {
                sent.see(&out, &format!("round {round}: {}", log.join(", ")));
            }
        }
        assert!(
            sent.down.is_empty(),
            "round {round}: {:?} left down after {}",
            sent.down,
            log.join(", ")
        );
        assert!(!rig.layer.active());
        assert_eq!(rig.layer.virtual_mods(), NONE);
        assert!(
            rig.layer.ghosts.is_empty() && rig.layer.mapped.is_empty(),
            "round {round}"
        );
    }
}

#[test]
fn a_reset_at_any_point_releases_everything_the_layer_sent() {
    // The same random input, but interrupted by a reset (the terminal lost focus). At that
    // moment everything the layer itself pressed must come up; only keys that passed
    // through untouched may still be down, because their own releases are still to come.
    let keys = [';', 'e', 'f', 'j', 'k', 'a', 'x'];
    let layer_made = [Key::Up, Key::Right, Key::Tab];
    let mut rng = Rng(0x0dd_ba11_5eed);
    for round in 0..300 {
        let mut rig = Rig::new();
        let mut real: Vec<char> = Vec::new();
        let mut sent = Balance::default();
        for _ in 0..rng.below(25) {
            let c = keys[rng.below(keys.len())];
            let ev = match real.iter().position(|held| *held == c) {
                None => {
                    real.push(c);
                    press(c)
                }
                Some(index) => {
                    real.remove(index);
                    release(c)
                }
            };
            for out in rig.send(ev) {
                sent.see(&out, &format!("round {round}"));
            }
        }
        for out in rig.layer.reset() {
            assert!(out.is_release());
            sent.see(&out, &format!("round {round}, reset"));
        }
        assert!(!rig.layer.active());
        for key in &sent.down {
            assert!(
                !layer_made.contains(key),
                "round {round}: {key:?} left down by the layer"
            );
        }
    }
}
