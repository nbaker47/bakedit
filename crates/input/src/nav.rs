//! The built-in nav layer: hold one key and the home row becomes arrows and modifiers.
//!
//! This is the user's Karabiner-Elements rule, done inside the IDE so it works wherever
//! bakedit runs. While the trigger key (`;`) is held:
//!
//! - `e` `s` `d` `f` are Up, Left, Down, Right;
//! - `j` `k` `l` `h` are shift, cmd, alt, ctrl: they send nothing themselves and add their
//!   modifier to whatever the layer sends while they are down;
//! - `a` is Tab and `c` is Escape, except with cmd and alt both held, when they stay `a`
//!   and `c`;
//! - any other key goes through with the held layer modifiers added.
//!
//! Tapped on its own, the trigger key types what it always typed.

#[cfg(test)]
mod tests;

use std::time::{Duration, Instant};

use bakedit_core::{Key, KeyEvent, KeyKind, Mods};

use crate::host;

/// The most keys the layer tracks at once. Far above what hands can hold down; it only
/// keeps the state bounded when releases go missing.
const MAX_TRACKED_KEYS: usize = 64;

/// Which keys the [`NavLayer`] uses and how long a tap may last. `Default` is the mapping
/// in the module documentation.
///
/// Every key is given as the unshifted character it types, the way [`Key::Char`] holds it.
/// If one character is given two jobs, the trigger wins, then the modifiers, then the
/// arrows, then the tap keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavLayerConfig {
    /// Whether the layer does anything. Off, every event passes through untouched.
    pub enabled: bool,
    /// The key that turns the layer on while it is held.
    pub trigger: char,
    /// The key that becomes Up.
    pub up: char,
    /// The key that becomes Left.
    pub left: char,
    /// The key that becomes Down.
    pub down: char,
    /// The key that becomes Right.
    pub right: char,
    /// The key that acts as shift.
    pub shift: char,
    /// The key that acts as cmd (super).
    pub cmd: char,
    /// The key that acts as alt (option).
    pub alt: char,
    /// The key that acts as ctrl.
    pub ctrl: char,
    /// The key that becomes Tab.
    pub tab: char,
    /// The key that becomes Escape.
    pub escape: char,
    /// The tap keys stay themselves while all of these modifiers are held, whether by the
    /// layer's modifier keys or by real ones. Empty means they never do.
    pub tap_passthrough: Mods,
    /// How long the trigger may be held and still type its own character on release.
    pub alone_timeout: Duration,
}

impl Default for NavLayerConfig {
    fn default() -> NavLayerConfig {
        NavLayerConfig {
            enabled: true,
            trigger: ';',
            up: 'e',
            left: 's',
            down: 'd',
            right: 'f',
            shift: 'j',
            cmd: 'k',
            alt: 'l',
            ctrl: 'h',
            tab: 'a',
            escape: 'c',
            tap_passthrough: Mods::SUPER.union(Mods::ALT),
            alone_timeout: Duration::from_millis(1000),
        }
    }
}

/// What a key does while the layer is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Acts as a modifier and sends nothing.
    Modifier(Mods),
    /// Becomes this key.
    Arrow(Key),
    /// Becomes this key unless the passthrough modifiers are held.
    Tap(Key),
}

impl NavLayerConfig {
    fn role(&self, ev: &KeyEvent) -> Option<Role> {
        let Key::Char(c) = ev.key else {
            return None;
        };
        if ev.keypad {
            return None;
        }
        let roles = [
            (self.shift, Role::Modifier(Mods::SHIFT)),
            (self.cmd, Role::Modifier(Mods::SUPER)),
            (self.alt, Role::Modifier(Mods::ALT)),
            (self.ctrl, Role::Modifier(Mods::CTRL)),
            (self.up, Role::Arrow(Key::Up)),
            (self.left, Role::Arrow(Key::Left)),
            (self.down, Role::Arrow(Key::Down)),
            (self.right, Role::Arrow(Key::Right)),
            (self.tab, Role::Tap(Key::Tab)),
            (self.escape, Role::Tap(Key::Escape)),
        ];
        roles
            .into_iter()
            .find(|(key, _)| *key == c)
            .map(|(_, role)| role)
    }

    fn is_trigger(&self, ev: &KeyEvent) -> bool {
        !ev.keypad && ev.key == Key::Char(self.trigger)
    }

    fn taps_pass_through(&self, mods: Mods) -> bool {
        !self.tap_passthrough.is_empty() && mods.contains(self.tap_passthrough)
    }
}

/// One hold of the trigger key.
#[derive(Debug, Clone)]
struct Hold {
    since: Instant,
    /// The trigger's own press, kept back in case the hold turns out to be typing.
    press: KeyEvent,
    /// A layer key has been used during this hold.
    used: bool,
}

#[derive(Debug, Clone)]
enum Phase {
    /// The trigger is up.
    Idle,
    /// The trigger is down and the layer is on.
    Held(Hold),
    /// The trigger is down but the hold was typing: the layer stays off until it is released.
    Cancelled,
}

/// A key that went down through the layer and has not come up yet.
#[derive(Debug, Clone)]
struct Mapped {
    physical: Key,
    /// The press the layer sent for it.
    emitted: KeyEvent,
}

/// The nav layer. Feed it every key event; it returns the events to act on instead.
///
/// It is a pure function of the events and the times passed to it: it never reads a clock
/// and has no timers. See the module documentation for the mapping, and
/// [`process`](NavLayer::process) for the rules.
///
/// The layer needs key releases, so it only works when the host terminal reports them:
/// the kitty keyboard protocol with [`REQUIRED_KITTY_FLAGS`](NavLayer::REQUIRED_KITTY_FLAGS).
/// Without releases the first press of the trigger would leave the layer on for good, so
/// the app must keep the layer disabled until the terminal has confirmed those flags.
///
/// # Example
///
/// ```
/// use std::time::Instant;
///
/// use bakedit_core::{Key, KeyEvent, KeyKind, Mods};
/// use bakedit_input::{NavLayer, NavLayerConfig};
///
/// let mut nav = NavLayer::new(NavLayerConfig::default());
/// let now = Instant::now();
/// let release = |c| KeyEvent::new(Key::Char(c), Mods::empty()).with_kind(KeyKind::Release);
///
/// // Hold `;`, then `j` (shift) and `f` (Right): shift-right.
/// assert!(nav.process(KeyEvent::typed(';'), now).is_empty());
/// assert!(nav.process(KeyEvent::typed('j'), now).is_empty());
/// let sent = nav.process(KeyEvent::typed('f'), now);
/// assert_eq!(sent, [KeyEvent::new(Key::Right, Mods::SHIFT)]);
///
/// // Letting go of `;` releases what the layer still holds down, and types no `;`.
/// let sent = nav.process(release(';'), now);
/// assert_eq!(sent, [KeyEvent::new(Key::Right, Mods::SHIFT).with_kind(KeyKind::Release)]);
///
/// // Tapped on its own, `;` types itself when it comes up.
/// assert!(nav.process(release('f'), now).is_empty());
/// assert!(nav.process(release('j'), now).is_empty());
/// assert!(nav.process(KeyEvent::typed(';'), now).is_empty());
/// assert_eq!(nav.process(release(';'), now), [KeyEvent::typed(';'), release(';')]);
/// ```
#[derive(Debug, Clone)]
pub struct NavLayer {
    config: NavLayerConfig,
    phase: Phase,
    /// Keys sent through the layer that are still down, oldest first.
    mapped: Vec<Mapped>,
    /// The layer's modifier keys that are down, with the modifier each one holds.
    virtual_down: Vec<(Key, Mods)>,
    /// Keys the layer swallowed that were still down when the layer ended. Their repeats
    /// and their release belong to the layer too and are swallowed when they come.
    ghosts: Vec<Key>,
}

impl NavLayer {
    /// The kitty keyboard flags the host terminal must have active for the layer to work:
    /// event types (so releases are reported) and all keys as escape codes (so releases of
    /// text keys are too).
    pub const REQUIRED_KITTY_FLAGS: u8 = host::KITTY_EVENT_TYPES | host::KITTY_ALL_KEYS;

    /// A layer with this configuration, with the trigger up.
    pub fn new(config: NavLayerConfig) -> NavLayer {
        NavLayer {
            config,
            phase: Phase::Idle,
            mapped: Vec::new(),
            virtual_down: Vec::new(),
            ghosts: Vec::new(),
        }
    }

    /// The configuration in use.
    pub fn config(&self) -> &NavLayerConfig {
        &self.config
    }

    /// Replace the configuration. Whatever the layer holds down is released first, as
    /// [`reset`](NavLayer::reset) does; the releases are returned.
    pub fn set_config(&mut self, config: NavLayerConfig) -> Vec<KeyEvent> {
        let out = self.reset();
        self.config = config;
        out
    }

    /// Whether the layer is switched on.
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// Switch the layer on or off. Switching it off in the middle of a hold returns the
    /// releases of the keys the layer still holds down and forgets the hold, as
    /// [`reset`](NavLayer::reset) does. Switching to the state it is already in does
    /// nothing.
    pub fn set_enabled(&mut self, on: bool) -> Vec<KeyEvent> {
        let out = if on { Vec::new() } else { self.reset() };
        self.config.enabled = on;
        out
    }

    /// Forget the current hold: return the releases of the keys the layer still holds down
    /// and go back to the trigger being up. A trigger press that was being kept back is
    /// dropped, not typed.
    ///
    /// The app calls this when the terminal loses focus, because the releases that would
    /// end the hold may never arrive. Releases that do arrive later pass through.
    pub fn reset(&mut self) -> Vec<KeyEvent> {
        let out = self.release_all();
        self.ghosts.clear();
        self.phase = Phase::Idle;
        out
    }

    /// True while the trigger is held and the layer is mapping keys. False while it is
    /// held but the hold was cancelled by typing.
    pub fn active(&self) -> bool {
        matches!(self.phase, Phase::Held(_))
    }

    /// The modifiers held through the layer's modifier keys right now. Empty when the
    /// layer is not active. Mouse reports cannot carry cmd, so the app adds these (and
    /// [`HeldMods`](crate::HeldMods)) to a click.
    pub fn virtual_mods(&self) -> Mods {
        self.virtual_down
            .iter()
            .fold(Mods::empty(), |mods, (_, bit)| mods | *bit)
    }

    /// Run one key event through the layer and return the events to act on, in order:
    /// none, the event itself, or what the layer made of it. `now` is when the event
    /// arrived; it only matters for the trigger's tap timeout.
    ///
    /// Disabled, everything passes through. Modifier keys on their own, and text that
    /// belongs to no key (`Key::Other(0)`), always pass through and never count as
    /// "another key". Otherwise:
    ///
    /// - **Trigger down** turns the layer on and sends nothing. Its repeats are swallowed,
    ///   and so is a second press with no release in between: some terminals report
    ///   repeats as presses.
    /// - **Trigger up** turns the layer off. Every key the layer still holds down is
    ///   released first. If no other key was pressed during the hold and it lasted less
    ///   than `alone_timeout`, the trigger's own press (with its text, so shift+`;` still
    ///   types a colon) and its release follow.
    /// - **Arrow keys** send their arrow, **tap keys** send Tab or Escape, each with the
    ///   real modifiers of the event plus the layer's. With the passthrough modifiers held
    ///   a tap key is sent as itself instead, with those modifiers and no text.
    /// - **Layer modifier keys** send nothing.
    /// - **Any other key** with a layer modifier held is sent with the layer's modifiers
    ///   added and its text removed. With none held it passes through unchanged; and if no
    ///   layer key has been used in this hold and the hold is younger than `alone_timeout`,
    ///   the user was just typing fast, so the trigger's kept-back press is sent first and
    ///   the layer stays off until the trigger is released (that release then passes
    ///   through).
    /// - **Repeats** of a key the layer sent repeat what was sent, with the modifiers held
    ///   now. **Releases** release what was sent, with the modifiers its press had.
    ///
    /// A key the layer swallowed that is still down when the layer ends stays swallowed
    /// until it comes up: its repeats and its release produce nothing, so letting go of the
    /// trigger a moment before the other key never types a stray character.
    pub fn process(&mut self, ev: KeyEvent, now: Instant) -> Vec<KeyEvent> {
        if !self.config.enabled || matches!(ev.key, Key::Modifier(_) | Key::Other(0)) {
            return vec![ev];
        }
        if let Some(index) = self.ghosts.iter().position(|key| *key == ev.key) {
            // The layer's key until it comes up. A press here is a repeat from a terminal
            // that does not tell the two apart.
            if ev.kind == KeyKind::Release {
                self.ghosts.remove(index);
            }
            return Vec::new();
        }
        let is_trigger = self.config.is_trigger(&ev);
        match self.phase {
            Phase::Idle if is_trigger && ev.kind == KeyKind::Press => self.begin_hold(ev, now),
            Phase::Idle => vec![ev],
            Phase::Cancelled => {
                if is_trigger && ev.kind == KeyKind::Release {
                    self.phase = Phase::Idle;
                }
                vec![ev]
            }
            Phase::Held(_) if is_trigger => match ev.kind {
                // A press here is a repeat from a terminal that does not tell the two apart.
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

    fn begin_hold(&mut self, press: KeyEvent, now: Instant) -> Vec<KeyEvent> {
        self.phase = Phase::Held(Hold {
            since: now,
            press,
            used: false,
        });
        Vec::new()
    }

    fn end_hold(&mut self, release: KeyEvent, now: Instant) -> Vec<KeyEvent> {
        let mut out = self.release_all();
        let tapped = self.trigger_may_still_type(now);
        if let Phase::Held(hold) = std::mem::replace(&mut self.phase, Phase::Idle) {
            if tapped {
                out.push(hold.press);
                out.push(release);
            }
        }
        out
    }

    /// True while the trigger's own press can still turn out to be typing: the layer has
    /// not been used in this hold and the hold is younger than the tap timeout.
    fn trigger_may_still_type(&self, now: Instant) -> bool {
        match &self.phase {
            Phase::Held(hold) => {
                let held_for = now.saturating_duration_since(hold.since);
                !hold.used && held_for < self.config.alone_timeout
            }
            _ => false,
        }
    }

    fn press_in_layer(&mut self, ev: KeyEvent, now: Instant) -> Vec<KeyEvent> {
        let layer_mods = self.virtual_mods();
        match self.config.role(&ev) {
            Some(Role::Modifier(bit)) => {
                if !self.virtual_down.iter().any(|(key, _)| *key == ev.key) {
                    self.virtual_down.push((ev.key, bit));
                    trim_oldest(&mut self.virtual_down);
                }
                self.mark_used();
                Vec::new()
            }
            Some(Role::Arrow(key)) => self.send_mapped(&ev, key, layer_mods),
            Some(Role::Tap(key)) if !self.config.taps_pass_through(ev.mods | layer_mods) => {
                self.send_mapped(&ev, key, layer_mods)
            }
            Some(Role::Tap(_)) => self.send_with_layer_mods(ev, layer_mods),
            None if !layer_mods.is_empty() => self.send_with_layer_mods(ev, layer_mods),
            None => self.pass_or_cancel(ev, now),
        }
    }

    /// Send `key` in place of the key that was pressed.
    fn send_mapped(&mut self, ev: &KeyEvent, key: Key, layer_mods: Mods) -> Vec<KeyEvent> {
        let mut mapped = KeyEvent::new(key, ev.mods | layer_mods);
        mapped.caps_lock = ev.caps_lock;
        mapped.num_lock = ev.num_lock;
        self.remember(ev.key, mapped.clone());
        vec![mapped]
    }

    /// Send the key that was pressed, as a shortcut with the layer's modifiers.
    fn send_with_layer_mods(&mut self, mut ev: KeyEvent, layer_mods: Mods) -> Vec<KeyEvent> {
        ev.mods |= layer_mods;
        ev.text = None;
        self.remember(ev.key, ev.clone());
        vec![ev]
    }

    /// A key the layer has nothing to say about, pressed with no layer modifier held.
    fn pass_or_cancel(&mut self, ev: KeyEvent, now: Instant) -> Vec<KeyEvent> {
        if !self.trigger_may_still_type(now) {
            return vec![ev];
        }
        // Nothing in this young hold used the layer, so it was typing with rollover: the
        // trigger went down, and the next key before it came back up.
        match std::mem::replace(&mut self.phase, Phase::Cancelled) {
            Phase::Held(hold) => vec![hold.press, ev],
            _ => vec![ev],
        }
    }

    fn repeat_in_layer(&mut self, ev: KeyEvent) -> Vec<KeyEvent> {
        if let Some(down) = self.mapped.iter().find(|down| down.physical == ev.key) {
            let mut repeat = down.emitted.clone();
            repeat.kind = KeyKind::Repeat;
            repeat.mods = ev.mods | self.virtual_mods();
            repeat.caps_lock = ev.caps_lock;
            repeat.num_lock = ev.num_lock;
            return vec![repeat];
        }
        if self.virtual_down.iter().any(|(key, _)| *key == ev.key) {
            return Vec::new();
        }
        // A key that went down before the layer came on keeps being itself.
        vec![ev]
    }

    fn release_in_layer(&mut self, ev: KeyEvent) -> Vec<KeyEvent> {
        if let Some(index) = self.mapped.iter().position(|down| down.physical == ev.key) {
            let down = self.mapped.remove(index);
            return vec![down.emitted.with_kind(KeyKind::Release)];
        }
        if let Some(index) = self.virtual_down.iter().position(|(key, _)| *key == ev.key) {
            self.virtual_down.remove(index);
            return Vec::new();
        }
        vec![ev]
    }

    /// Note that `physical` is down and was sent as `emitted`, and that the hold used the
    /// layer.
    fn remember(&mut self, physical: Key, emitted: KeyEvent) {
        self.mapped.retain(|down| down.physical != physical);
        self.mapped.push(Mapped { physical, emitted });
        trim_oldest(&mut self.mapped);
        self.mark_used();
    }

    fn mark_used(&mut self) {
        if let Phase::Held(hold) = &mut self.phase {
            hold.used = true;
        }
    }

    /// Release everything the layer holds down. The physical keys are still down, so they
    /// become ghosts: what they send from here on is swallowed.
    fn release_all(&mut self) -> Vec<KeyEvent> {
        let mut out = Vec::with_capacity(self.mapped.len());
        for down in self.mapped.drain(..) {
            self.ghosts.push(down.physical);
            out.push(down.emitted.with_kind(KeyKind::Release));
        }
        for (key, _) in self.virtual_down.drain(..) {
            self.ghosts.push(key);
        }
        trim_oldest(&mut self.ghosts);
        out
    }
}

/// Keep a list of tracked keys within [`MAX_TRACKED_KEYS`] by forgetting the oldest.
fn trim_oldest<T>(keys: &mut Vec<T>) {
    if keys.len() > MAX_TRACKED_KEYS {
        keys.drain(..keys.len() - MAX_TRACKED_KEYS);
    }
}
