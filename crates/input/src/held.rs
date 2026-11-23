//! Which modifier keys are physically down.

use bakedit_core::{Key, KeyEvent, ModKey, Mods};

/// The kitty key code of the first modifier key; the others follow it.
const FIRST_MOD_KEY: u32 = 57441;
/// How many modifier keys the protocol knows.
const MOD_KEY_COUNT: u32 = 14;

/// Tracks which modifiers are held on the real keyboard, from the key events.
///
/// Mouse reports can only say shift, alt and ctrl, so to know that cmd is down during a
/// click the app asks this tracker. Feed it every key event exactly as the parser produced
/// it, *before* the nav layer: the layer writes its own modifiers into the events it sends,
/// and those are not physically held (ask
/// [`NavLayer::virtual_mods`](crate::NavLayer::virtual_mods) for them).
///
/// A modifier counts as held from the press of its key to the release. Because presses and
/// releases can be missed (the terminal was not focused, say), the `mods` of every other
/// key event corrects the picture: a modifier the event does not carry is no longer held,
/// and one it carries is held even if its key was never seen going down.
///
/// That second correction is only made once the terminal has reported a modifier key on
/// its own. A terminal that never does (anything without the kitty protocol's all-keys
/// mode) does not report the releases either, so a modifier learned from one of its key
/// events could never be cleared; with such a terminal the tracker reports nothing held.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeldMods {
    /// One bit per modifier key that is down, in kitty key code order.
    keys: u16,
    /// Modifiers known to be held without knowing which key holds them.
    extra: Mods,
    /// The terminal has reported a modifier key on its own.
    reports_modifier_keys: bool,
}

impl HeldMods {
    /// A tracker with nothing held.
    pub fn new() -> HeldMods {
        HeldMods::default()
    }

    /// Take note of one key event.
    ///
    /// A modifier key's own press or release decides its own modifier; the event's `mods`
    /// are used for the others. An ordinary key event corrects all of them. Text that
    /// belongs to no key (`Key::Other(0)`) says nothing about modifiers and is ignored.
    pub fn observe(&mut self, ev: &KeyEvent) {
        match ev.key {
            Key::Modifier(modifier) => {
                self.reports_modifier_keys = true;
                let own = modifier.mods();
                if ev.is_down() {
                    self.keys |= bit(modifier);
                } else {
                    self.keys &= !bit(modifier);
                    // If this modifier was only known from another event's `mods`, this
                    // was the key behind it.
                    self.extra.remove(own);
                }
                self.correct(ev.mods, own);
            }
            Key::Other(0) => {}
            _ => self.correct(ev.mods, Mods::empty()),
        }
    }

    /// The modifiers that are held right now.
    pub fn mods(&self) -> Mods {
        mod_keys()
            .filter(|key| self.keys & bit(*key) != 0)
            .fold(self.extra, |mods, key| mods | key.mods())
    }

    /// Forget everything that is held. For the app to call when the terminal loses focus:
    /// the releases may never arrive.
    pub fn clear(&mut self) {
        self.keys = 0;
        self.extra = Mods::empty();
    }

    /// Bring the picture in line with the modifiers an event reports, leaving `skip` alone.
    fn correct(&mut self, reported: Mods, skip: Mods) {
        let held = self.mods();
        let gone = held - reported - skip;
        if !gone.is_empty() {
            self.extra.remove(gone);
            for key in mod_keys().filter(|key| key.mods().intersects(gone)) {
                self.keys &= !bit(key);
            }
        }
        if self.reports_modifier_keys {
            self.extra |= reported - held - skip;
        }
    }
}

/// Every modifier key.
fn mod_keys() -> impl Iterator<Item = ModKey> {
    (FIRST_MOD_KEY..FIRST_MOD_KEY + MOD_KEY_COUNT).filter_map(ModKey::from_kitty_code)
}

/// The bit that stands for `key` in [`HeldMods::keys`].
fn bit(key: ModKey) -> u16 {
    1 << (key.kitty_code() - FIRST_MOD_KEY)
}

#[cfg(test)]
mod tests {
    use bakedit_core::KeyKind;

    use super::*;

    fn down(key: ModKey, mods: Mods) -> KeyEvent {
        KeyEvent::new(Key::Modifier(key), mods)
    }

    fn up(key: ModKey, mods: Mods) -> KeyEvent {
        KeyEvent::new(Key::Modifier(key), mods).with_kind(KeyKind::Release)
    }

    fn letter(c: char, mods: Mods) -> KeyEvent {
        KeyEvent::new(Key::Char(c), mods)
    }

    /// A tracker that has seen the terminal report a modifier key.
    fn primed() -> HeldMods {
        let mut held = HeldMods::new();
        held.observe(&down(ModKey::LeftShift, Mods::SHIFT));
        held.observe(&up(ModKey::LeftShift, Mods::empty()));
        assert_eq!(held.mods(), Mods::empty());
        held
    }

    #[test]
    fn nothing_is_held_at_first() {
        assert_eq!(HeldMods::new().mods(), Mods::empty());
        assert_eq!(HeldMods::default(), HeldMods::new());
    }

    #[test]
    fn a_modifier_is_held_from_press_to_release() {
        let mut held = HeldMods::new();
        held.observe(&down(ModKey::LeftSuper, Mods::SUPER));
        assert_eq!(held.mods(), Mods::SUPER);
        held.observe(&down(ModKey::LeftSuper, Mods::SUPER).with_kind(KeyKind::Repeat));
        assert_eq!(held.mods(), Mods::SUPER);
        held.observe(&up(ModKey::LeftSuper, Mods::empty()));
        assert_eq!(held.mods(), Mods::empty());
    }

    #[test]
    fn every_modifier_key_holds_its_modifier() {
        for key in mod_keys() {
            let mut held = HeldMods::new();
            held.observe(&down(key, key.mods()));
            assert_eq!(held.mods(), key.mods(), "{key:?}");
            held.observe(&up(key, Mods::empty()));
            assert_eq!(held.mods(), Mods::empty(), "{key:?}");
        }
        assert_eq!(mod_keys().count(), 14);
    }

    #[test]
    fn several_modifiers_combine() {
        let mut held = HeldMods::new();
        held.observe(&down(ModKey::LeftCtrl, Mods::CTRL));
        held.observe(&down(ModKey::LeftShift, Mods::CTRL | Mods::SHIFT));
        held.observe(&down(
            ModKey::RightAlt,
            Mods::CTRL | Mods::SHIFT | Mods::ALT,
        ));
        assert_eq!(held.mods(), Mods::CTRL | Mods::SHIFT | Mods::ALT);
        held.observe(&up(ModKey::LeftShift, Mods::CTRL | Mods::ALT));
        assert_eq!(held.mods(), Mods::CTRL | Mods::ALT);
        held.observe(&up(ModKey::LeftCtrl, Mods::ALT));
        held.observe(&up(ModKey::RightAlt, Mods::empty()));
        assert_eq!(held.mods(), Mods::empty());
    }

    #[test]
    fn left_and_right_keys_are_tracked_apart() {
        let mut held = HeldMods::new();
        held.observe(&down(ModKey::LeftSuper, Mods::SUPER));
        held.observe(&down(ModKey::RightSuper, Mods::SUPER));
        held.observe(&up(ModKey::LeftSuper, Mods::SUPER));
        assert_eq!(held.mods(), Mods::SUPER, "the right one is still down");
        held.observe(&up(ModKey::RightSuper, Mods::empty()));
        assert_eq!(held.mods(), Mods::empty());
    }

    #[test]
    fn the_own_modifier_follows_the_key_whatever_the_event_reports() {
        // Terminals disagree on whether a modifier key's own event carries its own bit.
        let mut held = HeldMods::new();
        held.observe(&down(ModKey::LeftSuper, Mods::empty()));
        assert_eq!(held.mods(), Mods::SUPER);
        held.observe(&up(ModKey::LeftSuper, Mods::SUPER));
        assert_eq!(held.mods(), Mods::empty());
    }

    #[test]
    fn an_ordinary_key_clears_a_modifier_whose_release_was_missed() {
        let mut held = HeldMods::new();
        held.observe(&down(ModKey::LeftSuper, Mods::SUPER));
        held.observe(&down(ModKey::LeftShift, Mods::SUPER | Mods::SHIFT));
        // Focus went elsewhere, cmd came up there, and now a plain shift+a arrives.
        held.observe(&letter('a', Mods::SHIFT));
        assert_eq!(held.mods(), Mods::SHIFT);
        held.observe(&letter('a', Mods::SHIFT).with_kind(KeyKind::Release));
        assert_eq!(held.mods(), Mods::SHIFT);
        held.observe(&letter('b', Mods::empty()));
        assert_eq!(held.mods(), Mods::empty());
    }

    #[test]
    fn an_ordinary_key_reveals_a_modifier_whose_press_was_missed() {
        let mut held = primed();
        // cmd went down while another window had the focus.
        held.observe(&letter('s', Mods::SUPER));
        assert_eq!(held.mods(), Mods::SUPER);
        held.observe(&letter('s', Mods::SUPER).with_kind(KeyKind::Release));
        assert_eq!(held.mods(), Mods::SUPER);
        // Its release, when it comes, ends it even though the press was never seen.
        held.observe(&up(ModKey::LeftSuper, Mods::empty()));
        assert_eq!(held.mods(), Mods::empty());
    }

    #[test]
    fn a_modifier_key_event_corrects_the_other_modifiers() {
        let mut held = primed();
        held.observe(&down(ModKey::LeftShift, Mods::SHIFT | Mods::CTRL));
        assert_eq!(
            held.mods(),
            Mods::SHIFT | Mods::CTRL,
            "ctrl was already down"
        );
        held.observe(&up(ModKey::LeftShift, Mods::empty()));
        assert_eq!(held.mods(), Mods::empty(), "and is not any more");
    }

    #[test]
    fn a_terminal_that_never_reports_modifier_keys_holds_nothing() {
        // Legacy input: shift, ctrl and alt show up on key presses but nothing ever says
        // they were released, so they must not stick.
        let mut held = HeldMods::new();
        for ev in [
            KeyEvent::typed('A'),
            letter('c', Mods::CTRL),
            letter('x', Mods::ALT),
            letter('s', Mods::SUPER),
        ] {
            held.observe(&ev);
            assert_eq!(held.mods(), Mods::empty(), "{ev:?}");
        }
    }

    #[test]
    fn text_without_a_key_changes_nothing() {
        let mut held = HeldMods::new();
        held.observe(&down(ModKey::LeftAlt, Mods::ALT));
        // Option composed a character; the terminal reports the text with no modifiers.
        held.observe(&KeyEvent::new(Key::Other(0), Mods::empty()).with_text("å"));
        assert_eq!(held.mods(), Mods::ALT);
    }

    #[test]
    fn keys_without_a_modifier_bit_hold_nothing() {
        let mut held = HeldMods::new();
        held.observe(&down(ModKey::IsoLevel3Shift, Mods::empty()));
        assert_eq!(held.mods(), Mods::empty());
        held.observe(&down(ModKey::LeftCtrl, Mods::CTRL));
        held.observe(&up(ModKey::IsoLevel3Shift, Mods::CTRL));
        assert_eq!(held.mods(), Mods::CTRL);
    }

    #[test]
    fn clear_forgets_what_is_held_but_not_what_the_terminal_can_do() {
        let mut held = HeldMods::new();
        held.observe(&down(ModKey::LeftSuper, Mods::SUPER));
        held.observe(&letter('a', Mods::SUPER | Mods::ALT));
        assert_eq!(held.mods(), Mods::SUPER | Mods::ALT);
        held.clear();
        assert_eq!(held.mods(), Mods::empty());
        // A release that arrives after all is harmless.
        held.observe(&up(ModKey::LeftSuper, Mods::empty()));
        assert_eq!(held.mods(), Mods::empty());
        // Still a terminal that reports modifier keys, so `mods` is still believed.
        held.observe(&letter('a', Mods::CTRL));
        assert_eq!(held.mods(), Mods::CTRL);
    }
}
