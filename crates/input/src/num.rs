//! Hold-Space number layer: hold `Space` and the home row (`a s d f g h j k l ;`)
//! becomes `1 2 3 4 5 6 7 8 9 0` (or `! @ # $ % ^ & * ( )` with Shift held).
//!
//! Fast typing rollover (`Space` followed by a letter within `hold_threshold`) flushes
//! `Space` and the letter normally so everyday typing never turns into digits.

use std::time::{Duration, Instant};

use bakedit_core::{Key, KeyEvent, KeyKind, Mods};

use crate::NavLayer;

const MAX_TRACKED_KEYS: usize = 64;

const HOME_ROW: [(char, char, char); 10] = [
    ('a', '1', '!'),
    ('s', '2', '@'),
    ('d', '3', '#'),
    ('f', '4', '$'),
    ('g', '5', '%'),
    ('h', '6', '^'),
    ('j', '7', '&'),
    ('k', '8', '*'),
    ('l', '9', '('),
    (';', '0', ')'),
];

/// Configuration for [`NumLayer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumLayerConfig {
    /// Whether the layer is active.
    pub enabled: bool,
    /// Minimum time `Space` must be held before home-row keys become numbers.
    /// Shorter overlaps are treated as fast typing rollover.
    pub hold_threshold: Duration,
    /// Maximum hold duration for a standalone `Space` tap to still emit `Space` on release.
    pub alone_timeout: Duration,
}

impl Default for NumLayerConfig {
    fn default() -> NumLayerConfig {
        NumLayerConfig {
            enabled: true,
            hold_threshold: Duration::from_millis(160),
            alone_timeout: Duration::from_millis(1000),
        }
    }
}

#[derive(Debug, Clone)]
struct Hold {
    since: Instant,
    press: KeyEvent,
    used: bool,
}

#[derive(Debug, Clone)]
enum Phase {
    Idle,
    Held(Hold),
    Cancelled,
}

#[derive(Debug, Clone)]
struct Mapped {
    physical: Key,
    emitted: KeyEvent,
}

/// Hold `Space` to turn the home row into digits `1..0`.
#[derive(Debug, Clone)]
pub struct NumLayer {
    config: NumLayerConfig,
    phase: Phase,
    mapped: Vec<Mapped>,
    ghosts: Vec<Key>,
}

impl NumLayer {
    /// Create a new number layer with `config`.
    pub fn new(config: NumLayerConfig) -> NumLayer {
        NumLayer {
            config,
            phase: Phase::Idle,
            mapped: Vec::new(),
            ghosts: Vec::new(),
        }
    }

    /// Whether the layer can work with these kitty keyboard flags active on the terminal.
    pub fn works_with(kitty_flags: u8) -> bool {
        NavLayer::works_with(kitty_flags)
    }

    /// Whether the layer is enabled.
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// Switch the layer on or off, releasing any held mapped keys when turning off.
    pub fn set_enabled(&mut self, on: bool) -> Vec<KeyEvent> {
        let out = if on { Vec::new() } else { self.reset() };
        self.config.enabled = on;
        out
    }

    /// Release any held mapped keys and return to idle (called on focus loss).
    pub fn reset(&mut self) -> Vec<KeyEvent> {
        let out = self.release_all();
        self.ghosts.clear();
        self.phase = Phase::Idle;
        out
    }

    /// Feed one key event through the hold-Space number layer.
    pub fn process(&mut self, ev: KeyEvent, now: Instant) -> Vec<KeyEvent> {
        if !self.config.enabled || matches!(ev.key, Key::Modifier(_) | Key::Other(0)) {
            return vec![ev];
        }
        if let Some(index) = self.ghosts.iter().position(|key| *key == ev.key) {
            if ev.kind == KeyKind::Release {
                self.ghosts.remove(index);
            }
            return Vec::new();
        }
        let is_space = matches!(ev.key, Key::Char(' ')) && !ev.keypad;
        match self.phase {
            Phase::Idle if is_space && ev.kind == KeyKind::Press && ev.mods.is_empty() => {
                self.phase = Phase::Held(Hold { since: now, press: ev, used: false });
                Vec::new()
            }
            Phase::Idle => vec![ev],
            Phase::Cancelled => {
                if is_space && ev.kind == KeyKind::Release {
                    self.phase = Phase::Idle;
                }
                vec![ev]
            }
            Phase::Held(_) if is_space => match ev.kind {
                KeyKind::Press | KeyKind::Repeat => Vec::new(),
                KeyKind::Release => self.end_hold(ev, now),
            },
            Phase::Held(_) => match ev.kind {
                KeyKind::Press => self.press_in_layer(ev, now),
                KeyKind::Repeat => self.repeat_in_layer(ev),
                KeyKind::Release => self.release_in_layer(ev),
            },
        }
    }

    fn end_hold(&mut self, release: KeyEvent, now: Instant) -> Vec<KeyEvent> {
        let mut out = self.release_all();
        if let Phase::Held(hold) = std::mem::replace(&mut self.phase, Phase::Idle) {
            let held_for = now.saturating_duration_since(hold.since);
            if !hold.used && held_for < self.config.alone_timeout {
                out.push(hold.press);
                out.push(release);
            }
        }
        out
    }

    fn press_in_layer(&mut self, ev: KeyEvent, now: Instant) -> Vec<KeyEvent> {
        let (used, held_for) = match &self.phase {
            Phase::Held(hold) => (hold.used, now.saturating_duration_since(hold.since)),
            _ => return vec![ev],
        };
        let layer_char = match ev.key {
            Key::Char(c) if !ev.keypad => Some(ev.base.unwrap_or(c).to_ascii_lowercase()),
            _ => None,
        };
        let mapped_digit = layer_char.and_then(|c| HOME_ROW.iter().find(|(k, _, _)| *k == c).copied());
        if let Some((_, digit, shifted)) = mapped_digit {
            if used || held_for >= self.config.hold_threshold {
                let non_shift = ev.mods.intersects(Mods::CTRL | Mods::ALT | Mods::SUPER);
                let out_char = if !non_shift && ev.mods.contains(Mods::SHIFT) { shifted } else { digit };
                let mut mapped = KeyEvent::new(Key::Char(out_char), ev.mods);
                mapped.caps_lock = ev.caps_lock;
                mapped.num_lock = ev.num_lock;
                if !non_shift {
                    mapped.text = Some(out_char.to_string());
                }
                self.mapped.retain(|down| down.physical != ev.key);
                self.mapped.push(Mapped { physical: ev.key, emitted: mapped.clone() });
                if self.mapped.len() > MAX_TRACKED_KEYS {
                    self.mapped.drain(..self.mapped.len() - MAX_TRACKED_KEYS);
                }
                if let Phase::Held(hold) = &mut self.phase {
                    hold.used = true;
                }
                return vec![mapped];
            }
        }
        if !used && held_for < self.config.alone_timeout {
            if let Phase::Held(hold) = std::mem::replace(&mut self.phase, Phase::Cancelled) {
                return vec![hold.press, ev];
            }
        }
        vec![ev]
    }

    fn repeat_in_layer(&mut self, ev: KeyEvent) -> Vec<KeyEvent> {
        if let Some(down) = self.mapped.iter().find(|down| down.physical == ev.key) {
            let mut repeat = down.emitted.clone();
            repeat.kind = KeyKind::Repeat;
            return vec![repeat];
        }
        vec![ev]
    }

    fn release_in_layer(&mut self, ev: KeyEvent) -> Vec<KeyEvent> {
        if let Some(index) = self.mapped.iter().position(|down| down.physical == ev.key) {
            let down = self.mapped.remove(index);
            return vec![down.emitted.with_kind(KeyKind::Release)];
        }
        vec![ev]
    }

    fn release_all(&mut self) -> Vec<KeyEvent> {
        let mut out = Vec::with_capacity(self.mapped.len());
        for down in self.mapped.drain(..) {
            self.ghosts.push(down.physical);
            out.push(down.emitted.with_kind(KeyKind::Release));
        }
        if self.ghosts.len() > MAX_TRACKED_KEYS {
            self.ghosts.drain(..self.ghosts.len() - MAX_TRACKED_KEYS);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(c: char) -> KeyEvent {
        KeyEvent::new(Key::Char(c), Mods::empty()).with_kind(KeyKind::Release)
    }

    #[test]
    fn space_tap_emits_space() {
        let mut layer = NumLayer::new(NumLayerConfig::default());
        let t0 = Instant::now();
        assert!(layer.process(KeyEvent::typed(' '), t0).is_empty());
        let out = layer.process(release(' '), t0 + Duration::from_millis(60));
        assert_eq!(out, vec![KeyEvent::typed(' '), release(' ')]);
    }

    #[test]
    fn fast_rollover_types_space_then_letter() {
        let mut layer = NumLayer::new(NumLayerConfig::default());
        let t0 = Instant::now();
        assert!(layer.process(KeyEvent::typed(' '), t0).is_empty());
        let out = layer.process(KeyEvent::typed('a'), t0 + Duration::from_millis(50));
        assert_eq!(out, vec![KeyEvent::typed(' '), KeyEvent::typed('a')]);
        assert_eq!(layer.process(release(' '), t0 + Duration::from_millis(90)), vec![release(' ')]);
    }

    #[test]
    fn hold_space_maps_homerow_to_digits_1_through_0() {
        let mut layer = NumLayer::new(NumLayerConfig::default());
        let t0 = Instant::now();
        let held = t0 + Duration::from_millis(200);
        assert!(layer.process(KeyEvent::typed(' '), t0).is_empty());
        for (src, digit, _) in HOME_ROW {
            let out = layer.process(KeyEvent::typed(src), held);
            assert_eq!(out, vec![KeyEvent::typed(digit)]);
            let rel = layer.process(release(src), held + Duration::from_millis(10));
            assert_eq!(rel, vec![KeyEvent::typed(digit).with_kind(KeyKind::Release)]);
        }
        assert!(layer.process(release(' '), held + Duration::from_millis(30)).is_empty());
    }

    #[test]
    fn hold_space_with_shift_maps_to_symbols() {
        let mut layer = NumLayer::new(NumLayerConfig::default());
        let t0 = Instant::now();
        let held = t0 + Duration::from_millis(200);
        assert!(layer.process(KeyEvent::typed(' '), t0).is_empty());
        let shift_a = KeyEvent::new(Key::Char('A'), Mods::SHIFT).with_text("A");
        let out = layer.process(shift_a, held);
        assert_eq!(out[0].key, Key::Char('!'));
        assert_eq!(out[0].text.as_deref(), Some("!"));
    }

    #[test]
    fn modified_space_passes_through_untouched() {
        let mut layer = NumLayer::new(NumLayerConfig::default());
        let t0 = Instant::now();
        let ctrl_space = KeyEvent::new(Key::Char(' '), Mods::CTRL);
        assert_eq!(layer.process(ctrl_space.clone(), t0), vec![ctrl_space]);
    }
}
