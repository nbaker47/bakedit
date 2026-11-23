//! Keys to actions: the keymap, the chord matcher, and the loop that offers each candidate
//! action to the shell until one is taken.

use std::time::Instant;

use bakedit_core::{Action, KeyEvent};
use bakedit_keymap::{KeyContext, Keymap, KeymapSource, Matcher, PendingTimer, PushResult, Step, PENDING_TIMEOUT};

use crate::shell::Shell;
use crate::view::Context;

/// The built-in keymap, compiled in.
pub const DEFAULT_KEYMAP: &str = include_str!("../../../assets/keymaps/default.json");

/// How long after a chord's first key the which-key popup appears.
const WHICH_KEY_DELAY: std::time::Duration = std::time::Duration::from_millis(600);

pub struct KeyRouter {
    pub keymap: Keymap,
    matcher: Matcher,
    /// When the pending chord gives up (Zed's one second).
    pub chord_deadline: Option<Instant>,
    /// When the which-key popup should appear for the pending chord.
    pub hint_deadline: Option<Instant>,
}

fn key_contexts(contexts: &[Context]) -> Vec<KeyContext> {
    contexts
        .iter()
        .map(|context| {
            let mut key_context = KeyContext::new(context.name);
            for flag in &context.flags {
                key_context = key_context.add(*flag);
            }
            for (key, value) in &context.values {
                key_context = key_context.set(*key, value.clone());
            }
            key_context
        })
        .collect()
}

/// Whether a plain key would type something where the focus is: an editor that is not in
/// nav mode, a one-line input, the terminal.
fn accepts_text(contexts: &[Context]) -> bool {
    contexts.last().is_some_and(|context| match context.name {
        "Terminal" => true,
        "Editor" => !context.values.iter().any(|(key, value)| *key == "vim_mode" && value == "normal"),
        _ => false,
    })
}

impl KeyRouter {
    /// Load the built-in keymap and then the user's on top. Returns the warnings from the
    /// user's file (the built-in one is covered by a test).
    pub fn new(user_keymap: Option<&str>) -> (KeyRouter, Vec<String>) {
        let mut keymap = Keymap::new();
        keymap.load(DEFAULT_KEYMAP, KeymapSource::Default);
        let warnings = match user_keymap {
            Some(text) => keymap.load(text, KeymapSource::User).iter().map(|w| w.to_string()).collect(),
            None => Vec::new(),
        };
        (KeyRouter { keymap, matcher: Matcher::new(), chord_deadline: None, hint_deadline: None }, warnings)
    }

    /// Give a key event to the keymap and run what it resolves to. Releases and bare
    /// modifier keys are not the keymap's business and return false.
    pub fn key(&mut self, shell: &mut Shell, event: &KeyEvent) -> bool {
        let contexts = shell.contexts();
        let result = self.matcher.push(&self.keymap, event, &key_contexts(&contexts), accepts_text(&contexts));
        let taken = event.is_down() && !matches!(event.key, bakedit_core::Key::Modifier(_));
        self.finish(shell, result);
        taken
    }

    /// The chord timer fired: give up waiting and act on the keys held so far.
    pub fn chord_timeout(&mut self, shell: &mut Shell) {
        let contexts = shell.contexts();
        let result = self.matcher.timeout(&self.keymap, &key_contexts(&contexts));
        self.chord_deadline = None;
        self.finish(shell, result);
    }

    /// The which-key delay passed with a chord still pending: list its completions.
    pub fn show_hints(&mut self, shell: &mut Shell) {
        self.hint_deadline = None;
        if !self.matcher.has_pending() {
            return;
        }
        let contexts = key_contexts(&shell.contexts());
        let mut seen = std::collections::HashSet::new();
        shell.which_key = self
            .keymap
            .pending_completions(self.matcher.pending(), &contexts)
            .into_iter()
            .filter(|(keys, _)| seen.insert(keys.clone()))
            .map(|(keys, action)| {
                let title = crate::actions::title(&action.name);
                (keys, title.split_once(": ").map(|(_, rest)| rest.to_string()).unwrap_or(title))
            })
            .collect();
    }

    /// Forget a half-typed chord (the focus moved, a modal opened).
    pub fn clear(&mut self, shell: &mut Shell) {
        self.matcher.clear();
        self.chord_deadline = None;
        self.hint_deadline = None;
        shell.pending_keys = None;
        shell.which_key.clear();
    }

    fn finish(&mut self, shell: &mut Shell, result: PushResult) {
        match result.timer {
            PendingTimer::Start => self.chord_deadline = Some(Instant::now() + PENDING_TIMEOUT),
            PendingTimer::Stop => self.chord_deadline = None,
            PendingTimer::Keep => {}
        }
        for step in result.steps {
            match step {
                Step::Dispatch { candidates, event } => {
                    if !candidates.iter().any(|actions| run_candidate(shell, actions)) {
                        shell.handle_key(&event);
                    }
                }
                Step::Input(event) => {
                    shell.handle_key(&event);
                }
            }
        }
        if result.pending {
            if shell.pending_keys.is_none() {
                self.hint_deadline = Some(Instant::now() + WHICH_KEY_DELAY);
            }
            shell.pending_keys = Some(self.matcher.pending_display());
        } else {
            self.hint_deadline = None;
            shell.pending_keys = None;
            shell.which_key.clear();
        }
    }

    /// The keys to show next to a command, for where the focus is now.
    pub fn hint_for(&self, action: &Action, contexts: &[Context]) -> Option<String> {
        // Of the bindings that work here, show the handiest: fewest keys, cmd before others.
        self.keymap
            .bindings_for_action(&action.name, action.args.as_ref(), Some(&key_contexts(contexts)))
            .into_iter()
            .min_by_key(|binding| (binding.keystrokes().len(), !binding.keystroke_text().contains("cmd")))
            .map(|binding| binding.display_mac())
    }
}

/// Run one binding's actions. A single action reports whether anything took it; a sequence
/// always counts as taken, as in Zed.
fn run_candidate(shell: &mut Shell, actions: &[Action]) -> bool {
    match actions {
        [single] => shell.dispatch(single),
        many => {
            for action in many {
                shell.dispatch(action);
            }
            true
        }
    }
}
