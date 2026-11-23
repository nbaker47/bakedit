//! The key press state machine: one per window, fed one key event at a time.
//!
//! A port of gpui's `DispatchTree::dispatch_key`, `flush_dispatch` and `replay_prefix`, and of
//! the part of `Window::dispatch_key_event` that decides when pending keys time out.

use std::time::Duration;

use bakedit_core::{Action, Key, KeyEvent};

use crate::context::KeyContext;
use crate::keymap::{Binding, Keymap};
use crate::keystroke::{sequence_text, Keystroke};

/// How long pending keys wait for the next one when their wait is bounded at all, see
/// [`PendingTimer`]. Zed's value.
pub const PENDING_TIMEOUT: Duration = Duration::from_secs(1);

/// One thing for the app to do, in order, after a key press or a timeout.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Bindings matched. Try the candidates in order and stop at the first one something
    /// handles. Each candidate is one binding's actions, to run in order: one action, or the
    /// several of an `action::Sequence`. A candidate with several actions, or none, counts as
    /// handled whether or not its actions find a handler, which is what Zed's workspace does
    /// with a sequence. If no candidate is handled, treat `event` as ordinary input.
    Dispatch {
        /// One entry per matching binding, best first.
        candidates: Vec<Vec<Action>>,
        /// The key press that completed the bindings.
        event: KeyEvent,
    },
    /// No binding matched this key press: handle it as ordinary input (insert its text, send
    /// it to the terminal, move the cursor).
    Input(KeyEvent),
}

/// What the app should do with the timer that ends in [`Matcher::timeout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PendingTimer {
    /// Leave the timer as it is. The event was not a key press the matcher takes.
    #[default]
    Keep,
    /// Start the timer afresh, to fire [`PENDING_TIMEOUT`] from now.
    Start,
    /// Cancel the timer. Either nothing is pending, or the pending keys wait for the next
    /// key however long it takes.
    Stop,
}

/// What [`Matcher::push`] and [`Matcher::timeout`] return.
#[derive(Debug, Clone, PartialEq, Default)]
#[must_use = "the steps are the key presses: dropping them loses input"]
pub struct PushResult {
    /// What to do now, in order. Empty when the key was held back or not taken.
    pub steps: Vec<Step>,
    /// Whether keys are held back waiting for more, after this call.
    pub pending: bool,
    /// What to do with the pending timer.
    pub timer: PendingTimer,
}

/// Turns key events into [`Step`]s, holding keys back while they could still become a longer
/// binding (`cmd-k` before `cmd-s`, `j` before `k`).
///
/// It owns nothing but the keys it is holding. The keymap and the context stack are passed
/// in with every call, so they may change between keys; clear the matcher when focus moves.
#[derive(Debug, Clone, Default)]
pub struct Matcher {
    pending: Vec<KeyEvent>,
    /// Whether the keys held so far are waiting on a timer. Once a pending sequence has one
    /// it keeps one, restarted by every key.
    timer_running: bool,
}

impl Matcher {
    /// A matcher holding no keys.
    pub fn new() -> Matcher {
        Matcher::default()
    }

    /// Feed one key event.
    ///
    /// `contexts` is the context stack of the focused element, root first. `accepts_text`
    /// says whether that element would insert the event's text if no binding took the key (an
    /// editor in edit mode, a terminal, a search field): only then does a typed character
    /// that starts a longer binding get a timeout, so a `j` that could become `j k` shows up
    /// after a second while a `g` in a list waits for the second `g`.
    ///
    /// A release, or a modifier key pressed on its own, is not taken: the result has no
    /// steps, `pending` as it was and [`PendingTimer::Keep`], and what else to do with the
    /// event is up to the app. Any other event, a repeat included, is resolved together with
    /// the keys held so far:
    ///
    /// - A longer binding could still follow: the key is held, `pending` is true and there
    ///   are no steps for it. The timer starts if the keys held are also a complete binding,
    ///   if the key types text and `accepts_text`, or if the sequence already had a timer.
    /// - Otherwise, if the keys complete bindings, a [`Step::Dispatch`] with them.
    /// - Otherwise the keys held are given up, longest bound prefix first: a
    ///   [`Step::Dispatch`] for the longest run of them that is a complete binding, or a
    ///   [`Step::Input`] for the first key when no run is. Then the rest is resolved again
    ///   with the new key, which may itself dispatch, become input or start a new wait.
    pub fn push(
        &mut self,
        keymap: &Keymap,
        event: &KeyEvent,
        contexts: &[KeyContext],
        accepts_text: bool,
    ) -> PushResult {
        if !event.is_down() || matches!(event.key, Key::Modifier(_)) {
            return PushResult {
                steps: Vec::new(),
                pending: self.has_pending(),
                timer: PendingTimer::Keep,
            };
        }
        let mut steps = Vec::new();
        let mut input = std::mem::take(&mut self.pending);
        loop {
            input.push(event.clone());
            let (bindings, pending) = keymap.bindings_for_input(&input, contexts);
            if pending {
                let ambiguous = !bindings.is_empty();
                let would_type = accepts_text && types_text(event);
                self.timer_running = self.timer_running || ambiguous || would_type;
                self.pending = input;
                let timer = if self.timer_running {
                    PendingTimer::Start
                } else {
                    PendingTimer::Stop
                };
                return PushResult {
                    steps,
                    pending: true,
                    timer,
                };
            }
            if !bindings.is_empty() {
                steps.push(dispatch(&bindings, event.clone()));
                break;
            }
            if input.len() == 1 {
                steps.push(Step::Input(event.clone()));
                break;
            }
            // The keys held do not go with the new one. Give up the front of them and try
            // what is left with the new key again.
            input.pop();
            steps.extend(replay_prefix(keymap, &mut input, contexts));
        }
        self.timer_running = false;
        PushResult {
            steps,
            pending: false,
            timer: PendingTimer::Stop,
        }
    }

    /// Give up waiting. Call it when the timer started by [`PendingTimer::Start`] fires.
    ///
    /// The keys held are flushed as in [`Matcher::push`]: a [`Step::Dispatch`] for the longest
    /// run of them that is a complete binding, else a [`Step::Input`] for the first key, and
    /// so on until none is left. Nothing is pending afterwards. With no keys held the result
    /// has no steps.
    pub fn timeout(&mut self, keymap: &Keymap, contexts: &[KeyContext]) -> PushResult {
        let mut input = std::mem::take(&mut self.pending);
        self.timer_running = false;
        let mut steps = Vec::new();
        while let Some(step) = replay_prefix(keymap, &mut input, contexts) {
            steps.push(step);
        }
        PushResult {
            steps,
            pending: false,
            timer: PendingTimer::Stop,
        }
    }

    /// Whether keys are held back waiting for more.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The key events held back, oldest first. Pass them to
    /// [`Keymap::pending_completions`] for a which-key popup.
    pub fn pending(&self) -> &[KeyEvent] {
        &self.pending
    }

    /// The keys held back in keymap spelling, for the status bar: `cmd-k`, `g`, `space w`.
    /// Empty when nothing is pending.
    pub fn pending_display(&self) -> String {
        let keystrokes: Vec<Keystroke> = self.pending.iter().map(Keystroke::from_event).collect();
        sequence_text(&keystrokes)
    }

    /// Drop the keys held back without running or typing them. Do this when focus moves to
    /// another element, as gpui does; the app's timer should be cancelled with it.
    pub fn clear(&mut self) {
        self.pending.clear();
        self.timer_running = false;
    }
}

/// Whether a key press types into a text field. Enter and tab count as well as the keys
/// that carry text: gpui gives both a `key_char` on a Mac.
fn types_text(event: &KeyEvent) -> bool {
    event.text.is_some() || matches!(event.key, Key::Enter | Key::Tab)
}

fn dispatch(bindings: &[&Binding], event: KeyEvent) -> Step {
    Step::Dispatch {
        candidates: bindings
            .iter()
            .map(|binding| binding.actions().to_vec())
            .collect(),
        event,
    }
}

/// gpui's `replay_prefix`: take the longest front run of `input` that is a complete binding
/// and turn it into a dispatch that carries the run's last key press, or failing that turn
/// the first key into ordinary input. `None` when `input` is empty.
fn replay_prefix(
    keymap: &Keymap,
    input: &mut Vec<KeyEvent>,
    contexts: &[KeyContext],
) -> Option<Step> {
    for last in (0..input.len()).rev() {
        let (bindings, _) = keymap.bindings_for_input(&input[..=last], contexts);
        if !bindings.is_empty() {
            let event = input[last].clone();
            input.drain(..=last);
            return Some(dispatch(&bindings, event));
        }
    }
    if input.is_empty() {
        None
    } else {
        Some(Step::Input(input.remove(0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::KeymapSource;
    use bakedit_core::{KeyKind, ModKey, Mods};

    fn keymap(text: &str) -> Keymap {
        let mut keymap = Keymap::new();
        let warnings = keymap.load(text, KeymapSource::User);
        assert!(warnings.is_empty(), "{warnings:?}");
        keymap
    }

    fn stack(text: &str) -> Vec<KeyContext> {
        text.split('/')
            .map(|part| KeyContext::parse(part).unwrap())
            .collect()
    }

    fn key(text: &str) -> KeyEvent {
        Keystroke::parse(text).unwrap().to_event()
    }

    /// A step in a line: `Alpha|Beta@cmd-s` for a dispatch, `input:j` for input. The actions
    /// of one candidate are joined with `+`.
    fn show(step: &Step) -> String {
        match step {
            Step::Dispatch { candidates, event } => {
                let candidates: Vec<String> = candidates
                    .iter()
                    .map(|actions| {
                        let names: Vec<&str> = actions.iter().map(|a| a.name.as_str()).collect();
                        names.join("+")
                    })
                    .collect();
                format!("{}@{}", candidates.join("|"), Keystroke::from_event(event))
            }
            Step::Input(event) => format!("input:{}", Keystroke::from_event(event)),
        }
    }

    /// A matcher with its keymap and context stack, for feeding keys by name.
    struct Session {
        matcher: Matcher,
        keymap: Keymap,
        contexts: Vec<KeyContext>,
        accepts_text: bool,
    }

    impl Session {
        fn new(keymap_text: &str, contexts: &str) -> Session {
            Session {
                matcher: Matcher::new(),
                keymap: keymap(keymap_text),
                contexts: stack(contexts),
                accepts_text: true,
            }
        }

        /// Push one key and describe the result: the steps, then `pending` and the timer.
        fn push(&mut self, text: &str) -> (Vec<String>, bool, PendingTimer) {
            let event = key(text);
            self.push_event(&event)
        }

        fn push_event(&mut self, event: &KeyEvent) -> (Vec<String>, bool, PendingTimer) {
            let result = self
                .matcher
                .push(&self.keymap, event, &self.contexts, self.accepts_text);
            assert_eq!(result.pending, self.matcher.has_pending());
            (
                result.steps.iter().map(show).collect(),
                result.pending,
                result.timer,
            )
        }

        fn timeout(&mut self) -> Vec<String> {
            let result = self.matcher.timeout(&self.keymap, &self.contexts);
            assert!(!result.pending);
            assert!(!self.matcher.has_pending());
            assert_eq!(result.timer, PendingTimer::Stop);
            result.steps.iter().map(show).collect()
        }
    }

    fn steps(expected: &[&str]) -> Vec<String> {
        expected.iter().map(|step| step.to_string()).collect()
    }

    use PendingTimer::{Keep, Start, Stop};

    const EDITOR: &str = r#"[
        {"context": "Workspace", "bindings": {"cmd-k cmd-s": "zed::OpenKeymap", "cmd-s": "workspace::Save"}},
        {"context": "Editor", "bindings": {
            "cmd-k cmd-s": ["action::Sequence", ["editor::Format", "workspace::Save"]],
            "cmd-k cmd-0": "editor::FoldAll",
            "enter": "editor::Newline"
        }},
        {"context": "Editor && vim_mode == insert", "bindings": {"j k": "vim::NormalBefore"}}
    ]"#;

    #[test]
    fn a_single_key_binding_dispatches_at_once() {
        let mut session = Session::new(EDITOR, "Workspace/Editor");
        assert_eq!(
            session.push("cmd-s"),
            (steps(&["workspace::Save@cmd-s"]), false, Stop)
        );
        assert!(!session.matcher.has_pending());
        assert_eq!(session.matcher.pending_display(), "");
        // The event that completed the binding comes back with the candidates.
        let result = session
            .matcher
            .push(&session.keymap, &key("enter"), &session.contexts, true);
        assert_eq!(
            result.steps,
            [Step::Dispatch {
                candidates: vec![vec![Action::new("editor::Newline")]],
                event: key("enter"),
            }]
        );
    }

    #[test]
    fn an_unbound_key_is_input() {
        let mut session = Session::new(EDITOR, "Workspace/Editor");
        assert_eq!(session.push("x"), (steps(&["input:x"]), false, Stop));
        assert_eq!(
            session.push("cmd-x"),
            (steps(&["input:cmd-x"]), false, Stop)
        );
        // The event comes back as it went in, text and all.
        let typed = KeyEvent::typed('é');
        let result = session
            .matcher
            .push(&session.keymap, &typed, &session.contexts, true);
        assert_eq!(result.steps, [Step::Input(typed)]);
        // Out of context a binding is not a binding.
        let mut session = Session::new(EDITOR, "Workspace/Terminal");
        assert_eq!(
            session.push("enter"),
            (steps(&["input:enter"]), false, Stop)
        );
    }

    #[test]
    fn a_chord_waits_for_its_second_key() {
        let mut session = Session::new(EDITOR, "Workspace/Editor");
        // Nothing is bound to cmd-k alone and it types nothing: it waits without a timer.
        assert_eq!(session.push("cmd-k"), (steps(&[]), true, Stop));
        assert!(session.matcher.has_pending());
        assert_eq!(session.matcher.pending_display(), "cmd-k");
        assert_eq!(session.matcher.pending(), [key("cmd-k")]);
        // Both bindings for the chord are candidates, the deeper one first.
        assert_eq!(
            session.push("cmd-s"),
            (
                steps(&["editor::Format+workspace::Save|zed::OpenKeymap@cmd-s"]),
                false,
                Stop
            )
        );
        assert!(!session.matcher.has_pending());
        assert_eq!(session.matcher.pending_display(), "");
        // The matcher starts afresh.
        assert_eq!(
            session.push("cmd-s"),
            (steps(&["workspace::Save@cmd-s"]), false, Stop)
        );
    }

    #[test]
    fn a_chord_broken_by_an_unbound_key_gives_both_keys_back() {
        let mut session = Session::new(EDITOR, "Workspace/Editor");
        assert_eq!(session.push("cmd-k"), (steps(&[]), true, Stop));
        assert_eq!(
            session.push("x"),
            (steps(&["input:cmd-k", "input:x"]), false, Stop)
        );
        assert!(!session.matcher.has_pending());
    }

    #[test]
    fn a_chord_broken_by_a_bound_key_dispatches_that_key() {
        let mut session = Session::new(EDITOR, "Workspace/Editor");
        assert_eq!(session.push("cmd-k"), (steps(&[]), true, Stop));
        assert_eq!(
            session.push("enter"),
            (
                steps(&["input:cmd-k", "editor::Newline@enter"]),
                false,
                Stop
            )
        );
    }

    #[test]
    fn a_chord_broken_by_its_own_first_key_starts_over() {
        let mut session = Session::new(EDITOR, "Workspace/Editor");
        assert_eq!(session.push("cmd-k"), (steps(&[]), true, Stop));
        assert_eq!(session.push("cmd-k"), (steps(&["input:cmd-k"]), true, Stop));
        assert_eq!(session.matcher.pending_display(), "cmd-k");
        assert_eq!(
            session.push("cmd-0"),
            (steps(&["editor::FoldAll@cmd-0"]), false, Stop)
        );
    }

    #[test]
    fn typed_keys_that_start_a_binding_wait_with_a_timer() {
        let mut session = Session::new(EDITOR, "Workspace/Editor vim_mode=insert");
        // `j` alone types text, so it must not be held for ever.
        assert_eq!(session.push("j"), (steps(&[]), true, Start));
        assert_eq!(session.matcher.pending_display(), "j");
        assert_eq!(
            session.push("k"),
            (steps(&["vim::NormalBefore@k"]), false, Stop)
        );

        // Another letter: both are typed, in order.
        assert_eq!(session.push("j"), (steps(&[]), true, Start));
        assert_eq!(
            session.push("a"),
            (steps(&["input:j", "input:a"]), false, Stop)
        );

        // `j j k`: the first j is typed, the second starts the binding again.
        assert_eq!(session.push("j"), (steps(&[]), true, Start));
        assert_eq!(session.push("j"), (steps(&["input:j"]), true, Start));
        assert_eq!(
            session.push("k"),
            (steps(&["vim::NormalBefore@k"]), false, Stop)
        );

        // Nothing follows: the timeout types the j.
        assert_eq!(session.push("j"), (steps(&[]), true, Start));
        assert_eq!(session.timeout(), steps(&["input:j"]));
        assert_eq!(session.matcher.pending_display(), "");

        // The replayed event is the one that was pushed.
        let typed_j = KeyEvent::typed('j');
        session.push_event(&typed_j);
        let result = session.matcher.timeout(&session.keymap, &session.contexts);
        assert_eq!(result.steps, [Step::Input(typed_j)]);
    }

    #[test]
    fn where_the_binding_is_out_of_context_the_key_is_plain_input() {
        let mut session = Session::new(EDITOR, "Workspace/Editor vim_mode=normal");
        assert_eq!(session.push("j"), (steps(&["input:j"]), false, Stop));
    }

    #[test]
    fn a_typed_key_only_gets_a_timer_where_text_is_accepted() {
        let list = r#"[{"context": "ProjectPanel", "bindings": {"g g": "menu::SelectFirst"}}]"#;
        let mut session = Session::new(list, "Workspace/ProjectPanel");
        session.accepts_text = false;
        assert_eq!(session.push("g"), (steps(&[]), true, Stop));
        assert_eq!(
            session.push("g"),
            (steps(&["menu::SelectFirst@g"]), false, Stop)
        );
        session.accepts_text = true;
        assert_eq!(session.push("g"), (steps(&[]), true, Start));
        session.matcher.clear();

        // Enter and tab type into a text field as well; other keys do not.
        let keys = r#"[{"bindings": {"enter x": "A", "tab x": "A", "escape x": "A", "cmd-k x": "A", "space x": "A"}}]"#;
        let mut session = Session::new(keys, "Editor");
        for (key, timer) in [
            ("enter", Start),
            ("tab", Start),
            ("space", Start),
            ("escape", Stop),
            ("cmd-k", Stop),
        ] {
            assert_eq!(session.push(key), (steps(&[]), true, timer), "{key}");
            session.matcher.clear();
        }
        session.accepts_text = false;
        for key in ["enter", "tab", "space", "escape", "cmd-k"] {
            assert_eq!(session.push(key), (steps(&[]), true, Stop), "{key}");
            session.matcher.clear();
        }
    }

    const TERMINAL: &str = r#"[{"context": "Terminal", "bindings": {
        "ctrl-b": "Short",
        "ctrl-b h": "Longer",
        "ctrl-b h j": "Longest"
    }}]"#;

    #[test]
    fn timeout_dispatches_the_shorter_binding() {
        let mut session = Session::new(TERMINAL, "Terminal");
        // ctrl-b is a binding and the start of longer ones: it waits, but not for ever.
        assert_eq!(session.push("ctrl-b"), (steps(&[]), true, Start));
        assert_eq!(session.timeout(), steps(&["Short@ctrl-b"]));
        // A second timeout has nothing to flush.
        assert_eq!(session.timeout(), steps(&[]));
    }

    #[test]
    fn the_timer_restarts_when_the_binding_advances() {
        let mut session = Session::new(TERMINAL, "Terminal");
        assert_eq!(session.push("ctrl-b"), (steps(&[]), true, Start));
        assert_eq!(session.push("h"), (steps(&[]), true, Start));
        assert_eq!(session.matcher.pending_display(), "ctrl-b h");
        // The timeout runs the binding the two keys complete, on the second key.
        assert_eq!(session.timeout(), steps(&["Longer@h"]));

        assert_eq!(session.push("ctrl-b"), (steps(&[]), true, Start));
        assert_eq!(session.push("h"), (steps(&[]), true, Start));
        assert_eq!(session.push("j"), (steps(&["Longest@j"]), false, Stop));
    }

    #[test]
    fn the_timer_starts_when_the_keys_become_ambiguous() {
        let bindings = r#"[{"context": "Terminal", "bindings": {
            "ctrl-b h": "Longer",
            "ctrl-b h j": "Longest"
        }}]"#;
        let mut session = Session::new(bindings, "Terminal");
        // ctrl-b alone does nothing and types nothing: no timer.
        assert_eq!(session.push("ctrl-b"), (steps(&[]), true, Stop));
        // ctrl-b h is a binding and the start of another: now there is one.
        assert_eq!(session.push("h"), (steps(&[]), true, Start));
        assert_eq!(session.timeout(), steps(&["Longer@h"]));
    }

    #[test]
    fn once_started_the_timer_stays_for_the_sequence() {
        let bindings = r#"[{"bindings": {"j k l": "Triple", "cmd-k cmd-j cmd-l": "Chord"}}]"#;
        let mut session = Session::new(bindings, "Editor");
        assert_eq!(session.push("j"), (steps(&[]), true, Start));
        // `k` is pushed where text is no longer accepted, yet the sequence keeps its timer.
        session.accepts_text = false;
        assert_eq!(session.push("k"), (steps(&[]), true, Start));
        assert_eq!(session.push("l"), (steps(&["Triple@l"]), false, Stop));
        // A new sequence starts without one.
        assert_eq!(session.push("cmd-k"), (steps(&[]), true, Stop));
        assert_eq!(session.push("cmd-j"), (steps(&[]), true, Stop));
        assert_eq!(session.timeout(), steps(&["input:cmd-k", "input:cmd-j"]));
        // A timer carries over into the sequence the same key press starts by replaying.
        session.accepts_text = true;
        assert_eq!(session.push("j"), (steps(&[]), true, Start));
        session.accepts_text = false;
        assert_eq!(session.push("cmd-k"), (steps(&["input:j"]), true, Start));
        session.matcher.clear();
        assert_eq!(session.push("cmd-k"), (steps(&[]), true, Stop));
    }

    #[test]
    fn an_invalid_continuation_replays_the_shorter_binding_first() {
        let bindings = r#"[{"context": "Terminal", "bindings": {
            "ctrl-b": "Short",
            "ctrl-b h": "Longer",
            "x": "Other"
        }}]"#;
        let mut session = Session::new(bindings, "Terminal");
        assert_eq!(session.push("ctrl-b"), (steps(&[]), true, Start));
        assert_eq!(
            session.push("x"),
            (steps(&["Short@ctrl-b", "Other@x"]), false, Stop)
        );
    }

    #[test]
    fn the_longest_bound_prefix_is_replayed_then_the_rest_is_tried_again() {
        let bindings = r#"[{"bindings": {
            "a": "A",
            "a b c": "ABC",
            "b z": "BZ",
            "c": "C"
        }}]"#;
        let mut session = Session::new(bindings, "Editor");
        assert_eq!(session.push("a"), (steps(&[]), true, Start));
        assert_eq!(session.push("b"), (steps(&[]), true, Start));
        // `a b z` is nothing. `a` is the longest prefix that is a binding, so it runs; what
        // is left, `b`, goes with the new key and makes `b z`.
        assert_eq!(session.push("z"), (steps(&["A@a", "BZ@z"]), false, Stop));

        // `a b x`: `a` runs, `b` with `x` is nothing so `b` is typed, then `x` is typed.
        session.push("a");
        session.push("b");
        assert_eq!(
            session.push("x"),
            (steps(&["A@a", "input:b", "input:x"]), false, Stop)
        );

        // `a b` then the timeout: `a` runs and `b`, a prefix of nothing complete, is typed.
        session.push("a");
        session.push("b");
        assert_eq!(session.timeout(), steps(&["A@a", "input:b"]));

        // `a b a`: `a` runs, `b` is typed, and the new `a` waits in its turn.
        session.push("a");
        session.push("b");
        assert_eq!(session.push("a"), (steps(&["A@a", "input:b"]), true, Start));
        assert_eq!(session.matcher.pending_display(), "a");
        assert_eq!(session.timeout(), steps(&["A@a"]));

        // A prefix run of two keys keeps only its last key press.
        let bindings = r#"[{"bindings": {"a b": "AB", "a b c d": "ABCD"}}]"#;
        let mut session = Session::new(bindings, "Editor");
        session.push("a");
        session.push("b");
        session.push("c");
        assert_eq!(session.matcher.pending_display(), "a b c");
        assert_eq!(
            session.push("x"),
            (steps(&["AB@b", "input:c", "input:x"]), false, Stop)
        );
    }

    #[test]
    fn pending_state_follows_the_context() {
        let bindings = r#"[
            {"bindings": {"ctrl-b h": "Global"}},
            {"context": "ContextA", "bindings": {"space": "Space"}},
            {"context": "ContextB", "bindings": {"space f g": "Deep"}}
        ]"#;
        let keymap = keymap(bindings);
        let mut matcher = Matcher::new();
        // With no context at all the global chord still works.
        let result = matcher.push(&keymap, &key("ctrl-b"), &[], false);
        assert_eq!(
            (result.steps.len(), result.pending, result.timer),
            (0, true, Stop)
        );
        let result = matcher.push(&keymap, &key("h"), &[], false);
        assert_eq!(
            result.steps.iter().map(show).collect::<Vec<_>>(),
            ["Global@h"]
        );
        // In ContextB space only starts a longer binding; in ContextA it is one.
        let result = matcher.push(&keymap, &key("space"), &stack("ContextB"), false);
        assert_eq!(
            (result.steps.len(), result.pending, result.timer),
            (0, true, Stop)
        );
        matcher.clear();
        let result = matcher.push(&keymap, &key("space"), &stack("ContextA"), false);
        assert_eq!(
            result.steps.iter().map(show).collect::<Vec<_>>(),
            ["Space@space"]
        );
        // The context may change while keys are held: they are judged by the current one.
        let result = matcher.push(&keymap, &key("space"), &stack("ContextB"), false);
        assert!(result.pending);
        let result = matcher.push(&keymap, &key("f"), &stack("ContextA"), false);
        assert_eq!(
            result.steps.iter().map(show).collect::<Vec<_>>(),
            ["Space@space", "input:f"]
        );
    }

    #[test]
    fn releases_and_modifier_keys_do_not_disturb_pending_keys() {
        let mut session = Session::new(EDITOR, "Workspace/Editor vim_mode=insert");
        let ignored = [
            KeyEvent::typed('j').with_kind(KeyKind::Release),
            KeyEvent::new(Key::Modifier(ModKey::LeftShift), Mods::SHIFT),
            KeyEvent::new(Key::Modifier(ModKey::LeftShift), Mods::empty())
                .with_kind(KeyKind::Release),
            KeyEvent::new(Key::Modifier(ModKey::RightSuper), Mods::SUPER),
        ];
        // With nothing pending they are simply not taken.
        for event in &ignored {
            assert_eq!(session.push_event(event), (steps(&[]), false, Keep));
        }
        assert_eq!(session.push("j"), (steps(&[]), true, Start));
        for event in &ignored {
            assert_eq!(session.push_event(event), (steps(&[]), true, Keep));
            assert_eq!(session.matcher.pending_display(), "j");
        }
        assert_eq!(
            session.push("k"),
            (steps(&["vim::NormalBefore@k"]), false, Stop)
        );
        // Releasing the key that completed the binding changes nothing either.
        let release = KeyEvent::typed('k').with_kind(KeyKind::Release);
        assert_eq!(session.push_event(&release), (steps(&[]), false, Keep));
    }

    #[test]
    fn a_repeat_is_a_key_press() {
        let mut session = Session::new(EDITOR, "Workspace/Editor");
        let repeat = key("cmd-s").with_kind(KeyKind::Repeat);
        assert_eq!(
            session.push_event(&repeat),
            (steps(&["workspace::Save@cmd-s"]), false, Stop)
        );
    }

    #[test]
    fn clear_drops_the_keys_held() {
        let mut session = Session::new(EDITOR, "Workspace/Editor vim_mode=insert");
        assert_eq!(session.push("j"), (steps(&[]), true, Start));
        session.matcher.clear();
        assert!(!session.matcher.has_pending());
        assert_eq!(session.matcher.pending(), []);
        assert_eq!(session.timeout(), steps(&[]));
        // The j is gone: k is just a k, and the timer state went with it.
        assert_eq!(session.push("k"), (steps(&["input:k"]), false, Stop));
        assert_eq!(session.push("cmd-k"), (steps(&[]), true, Stop));
    }

    #[test]
    fn pending_keys_are_shown_the_way_they_are_bound() {
        let bindings = r#"[{"bindings": {"cmd-k { x": "A", "G g x": "B", "space w x": "C"}}]"#;
        let mut session = Session::new(bindings, "Editor");
        session.push("cmd-k");
        session.push_event(&KeyEvent::typed('{'));
        assert_eq!(session.matcher.pending_display(), "cmd-k {");
        session.matcher.clear();
        session.push_event(&KeyEvent::typed('G'));
        session.push("g");
        assert_eq!(session.matcher.pending_display(), "shift-g g");
        session.matcher.clear();
        session.push("space");
        session.push("w");
        assert_eq!(session.matcher.pending_display(), "space w");
        assert_eq!(session.push("x"), (steps(&["C@x"]), false, Stop));
    }

    #[test]
    fn a_disabled_chord_is_not_waited_for() {
        let bindings = r#"[
            {"context": "Workspace", "bindings": {"space w w": "Alpha"}},
            {"context": "Editor", "bindings": {"space w w": null}}
        ]"#;
        let mut session = Session::new(bindings, "Workspace/Editor");
        assert_eq!(
            session.push("space"),
            (steps(&["input:space"]), false, Stop)
        );
        let mut session = Session::new(bindings, "Workspace");
        assert_eq!(session.push("space"), (steps(&[]), true, Start));
        assert_eq!(session.push("w"), (steps(&[]), true, Start));
        assert_eq!(session.push("w"), (steps(&["Alpha@w"]), false, Stop));
    }

    #[test]
    fn an_empty_sequence_is_still_a_candidate() {
        let bindings = r#"[
            {"context": "Workspace", "bindings": {"cmd-e": "Fallback"}},
            {"context": "Editor", "bindings": {"cmd-e": ["action::Sequence", []]}}
        ]"#;
        let mut session = Session::new(bindings, "Workspace/Editor");
        let result = session
            .matcher
            .push(&session.keymap, &key("cmd-e"), &session.contexts, true);
        assert_eq!(
            result.steps,
            [Step::Dispatch {
                candidates: vec![vec![], vec![Action::new("Fallback")]],
                event: key("cmd-e"),
            }]
        );
    }
}
