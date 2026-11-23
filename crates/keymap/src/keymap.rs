//! The keymap: an ordered list of bindings and gpui's rules for which of them a key press
//! triggers.
//!
//! A port of gpui's `keymap.rs`. Loading the bindings from a keymap file is in `load.rs`.

use std::collections::HashSet;
use std::sync::Arc;

use bakedit_core::{Action, KeyEvent};
use serde_json::Value;

use crate::context::{ContextPredicate, KeyContext};
use crate::keystroke::{sequence_display_mac, sequence_text, Keystroke};

/// The name Zed gives the action a `null` binding stands for.
pub(crate) const NO_ACTION: &str = "zed::NoAction";
/// The action that disables the bindings of one named action.
pub(crate) const UNBIND: &str = "zed::Unbind";
/// The action that runs a list of actions.
pub(crate) const SEQUENCE: &str = "action::Sequence";

/// Which keymap a binding was loaded from.
///
/// Bindings loaded later win ties whatever their source, so load the built-in keymap first
/// and the user's after it. The source decides one thing only: a `null` binding disables the
/// bindings it outranks from its own source and from weaker ones, so a `null` in the
/// built-in keymap never switches off something the user bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeymapSource {
    /// The keymap that ships with the app. The weaker source.
    Default,
    /// The user's own keymap file. The stronger source.
    User,
}

impl KeymapSource {
    /// Smaller is stronger, like gpui's binding meta index.
    fn rank(self) -> u8 {
        match self {
            KeymapSource::User => 0,
            KeymapSource::Default => 1,
        }
    }
}

/// What a binding does when its keystrokes are typed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Target {
    /// `null`: disable the bindings these keystrokes would otherwise reach.
    Null,
    /// `["zed::Unbind", "name"]`: disable the bindings of that action for these keystrokes.
    Unbind(String),
    /// Run one action.
    Action(Action),
    /// `["action::Sequence", [...]]`: run these actions in order.
    Sequence(Vec<Action>),
}

/// One entry of a keymap: keystrokes, what they do, and the context they do it in.
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    pub(crate) keystrokes: Vec<Keystroke>,
    pub(crate) target: Target,
    pub(crate) context: Option<Arc<ContextPredicate>>,
    pub(crate) source: KeymapSource,
}

impl Binding {
    /// The keystrokes to type, in order. Never empty.
    pub fn keystrokes(&self) -> &[Keystroke] {
        &self.keystrokes
    }

    /// The keystrokes in keymap spelling: `cmd-k cmd-s`.
    pub fn keystroke_text(&self) -> String {
        sequence_text(&self.keystrokes)
    }

    /// The keystrokes as Mac glyphs, for a command palette row or a menu: `⌘K ⌘S`.
    pub fn display_mac(&self) -> String {
        sequence_display_mac(&self.keystrokes)
    }

    /// The actions to run, in order: one, or the flattened list of an `action::Sequence`
    /// (which may be empty). Empty for a `null` or `zed::Unbind` binding.
    pub fn actions(&self) -> &[Action] {
        match &self.target {
            Target::Action(action) => std::slice::from_ref(action),
            Target::Sequence(actions) => actions,
            Target::Null | Target::Unbind(_) => &[],
        }
    }

    /// The binding's action as a keymap writes it: the action itself, or for a sequence one
    /// `action::Sequence` action whose argument is the array of its actions. `None` for a
    /// `null` or `zed::Unbind` binding.
    pub fn action(&self) -> Option<Action> {
        match &self.target {
            Target::Action(action) => Some(action.clone()),
            Target::Sequence(actions) => Some(Action::with_args(
                SEQUENCE,
                Value::Array(actions.iter().map(action_json).collect()),
            )),
            Target::Null | Target::Unbind(_) => None,
        }
    }

    /// The section's `"context"`, or `None` for a binding that applies everywhere.
    pub fn context(&self) -> Option<&ContextPredicate> {
        self.context.as_deref()
    }

    /// Which keymap the binding was loaded from.
    pub fn source(&self) -> KeymapSource {
        self.source
    }

    /// Whether this is a `null` binding (also written `"zed::NoAction"`), which disables
    /// other bindings instead of running anything.
    pub fn is_null(&self) -> bool {
        self.target == Target::Null
    }

    /// The action name a `zed::Unbind` binding disables, if this is one.
    pub fn unbinds(&self) -> Option<&str> {
        match &self.target {
            Target::Unbind(name) => Some(name),
            _ => None,
        }
    }

    /// The name `zed::Unbind` knows this binding's action by.
    fn action_name(&self) -> Option<&str> {
        match &self.target {
            Target::Action(action) => Some(&action.name),
            Target::Sequence(_) => Some(SEQUENCE),
            Target::Null | Target::Unbind(_) => None,
        }
    }

    /// gpui's `match_keystrokes`: `None` when `input` is not the start of this binding's
    /// keystrokes, `Some(false)` when it is all of them, `Some(true)` when more must follow.
    fn match_keystrokes(&self, input: &[KeyEvent]) -> Option<bool> {
        if self.keystrokes.len() < input.len() {
            return None;
        }
        let matches = self
            .keystrokes
            .iter()
            .zip(input)
            .all(|(keystroke, event)| keystroke.matches(event));
        matches.then_some(self.keystrokes.len() > input.len())
    }

    /// The depth at which the binding is enabled in `contexts`, `None` when it is not. A
    /// binding without a context is enabled at the deepest level.
    fn depth(&self, contexts: &[KeyContext]) -> Option<usize> {
        match &self.context {
            Some(predicate) => predicate.depth_of(contexts),
            None => Some(contexts.len()),
        }
    }

    /// The key presses that type this binding on a US layout.
    fn events(&self) -> Vec<KeyEvent> {
        self.keystrokes.iter().map(Keystroke::to_event).collect()
    }

    /// Whether any key press can type the binding at all. Not so for `fn-` keystrokes.
    fn is_typable(&self) -> bool {
        self.match_keystrokes(&self.events()) == Some(false)
    }

    /// Whether `self`, a `null` or `zed::Unbind` binding, is written for the keys of `other`:
    /// the same keystrokes, or another spelling of them (`cmd-shift-[` for `cmd-{`).
    fn has_keys_of(&self, other: &Binding) -> bool {
        self.keystrokes == other.keystrokes || self.match_keystrokes(&other.events()) == Some(false)
    }

    /// Whether `self` is a `zed::Unbind` that removes `other`.
    fn unbinds_binding(&self, other: &Binding) -> bool {
        self.unbinds().is_some() && self.unbinds() == other.action_name() && self.has_keys_of(other)
    }

    /// gpui's `disabled_binding_matches_context`: whether this disabling binding's context
    /// certainly covers the context of `other`.
    fn context_covers(&self, other: &Binding) -> bool {
        match (&self.context, &other.context) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(mine), Some(theirs)) => mine.is_superset(theirs),
        }
    }
}

/// `"name"` or `["name", args]`, the way a keymap writes one action.
fn action_json(action: &Action) -> Value {
    let name = Value::String(action.name.clone());
    match &action.args {
        Some(args) => Value::Array(vec![name, args.clone()]),
        None => name,
    }
}

/// A binding with its position in the keymap. Later positions win at equal depth.
type Indexed<'a> = (usize, &'a Binding);

/// A set of key bindings. Load one or more keymap files into it with [`Keymap::load`], then
/// ask it what a key press does, directly or through a [`crate::Matcher`].
///
/// A keymap is plain data: once loaded it can be shared between threads and read freely.
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    pub(crate) bindings: Vec<Binding>,
}

impl Keymap {
    /// An empty keymap.
    pub fn new() -> Keymap {
        Keymap::default()
    }

    /// Every binding in load order, `null` and `zed::Unbind` entries included. Useful for a
    /// keymap viewer and for checking action names against the actions the app has.
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// How many bindings are loaded.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// Whether nothing is loaded.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Forget every binding, before loading the keymap files again.
    pub fn clear(&mut self) {
        self.bindings.clear();
    }

    fn indexed(&self) -> impl Iterator<Item = Indexed<'_>> {
        self.bindings.iter().enumerate()
    }

    /// gpui's `bindings_for_input`: what the keys typed so far (`input`, oldest first) do in
    /// the context stack `contexts` (root first, focused element last).
    ///
    /// Returns the bindings those keys complete, in the order to try them, and whether the
    /// keys are also the start of a longer binding.
    ///
    /// Only bindings enabled in the stack count; one without a context is enabled at the
    /// deepest level. The completed bindings are ordered deepest context first and, at equal
    /// depth, last loaded first. A `null` binding in that order removes the bindings after
    /// it that come from its own source or a weaker one ([`KeymapSource`]), and a
    /// `zed::Unbind` removes the bindings after it of the action it names. gpui dispatches
    /// the remaining bindings in order until an element handles one, so the caller must try
    /// them in order too, and treat the key as ordinary input if none is handled.
    ///
    /// A longer binding only counts as pending when it was loaded after the first completed
    /// binding (or nothing completed), and a `null` or `zed::Unbind` for the same longer
    /// keystrokes loaded after it cancels it.
    pub fn bindings_for_input(
        &self,
        input: &[KeyEvent],
        contexts: &[KeyContext],
    ) -> (Vec<&Binding>, bool) {
        let (bindings, pending) = resolve(input, contexts, self.indexed());
        let bindings = bindings.into_iter().map(|(_, binding)| binding).collect();
        (bindings, pending)
    }

    /// The bindings that trigger an action, best first, for showing its shortcut.
    ///
    /// `name` and `args` must equal the binding's action as written: `args` is compared as
    /// JSON, with `None` (or JSON `null`) meaning the binding has no argument. For a sequence
    /// pass `action::Sequence` and its array of actions. The best binding is the one loaded
    /// last, which is the one Zed displays.
    ///
    /// Without `contexts` this is gpui's `Keymap::bindings_for_action`: a binding is left out
    /// when a `null` or matching `zed::Unbind` loaded after it has the same keystrokes and a
    /// context that covers its own ([`ContextPredicate::is_superset`]). With `contexts`,
    /// typing the binding's keystrokes in that stack must also run this action before any
    /// other, as in gpui's `DispatchTree::bindings_for_action`: a binding shadowed by another
    /// action, or whose keys do nothing there, is left out. The test is on what the keys do,
    /// so a binding from a section that is not active is still listed when another section
    /// gives its keys the same action, and the same keystrokes can appear more than once.
    /// Bindings no key press can type (`fn-f`) are always left out.
    ///
    /// Each binding of the action costs one resolution of its keys against the whole keymap
    /// when `contexts` is given, so keep the answers when filling a long list.
    pub fn bindings_for_action(
        &self,
        name: &str,
        args: Option<&Value>,
        contexts: Option<&[KeyContext]>,
    ) -> Vec<&Binding> {
        let Some(wanted) = lookup_target(name, args) else {
            return Vec::new();
        };
        let mut found: Vec<&Binding> = self
            .indexed()
            .filter(|(_, binding)| binding.target == wanted && binding.is_typable())
            .filter(|(index, binding)| !self.disabled_later(*index, binding))
            .filter(|(_, binding)| {
                contexts.is_none_or(|contexts| self.runs_first(binding, contexts))
            })
            .map(|(_, binding)| binding)
            .collect();
        found.reverse();
        found
    }

    /// Whether a `null` or `zed::Unbind` loaded after `binding` switches it off wherever it
    /// applies.
    fn disabled_later(&self, index: usize, binding: &Binding) -> bool {
        self.bindings[index + 1..].iter().any(|later| {
            (later.is_null() || later.unbinds_binding(binding))
                && later.has_keys_of(binding)
                && later.context_covers(binding)
        })
    }

    /// Whether typing `binding` in `contexts` runs its action before any other.
    fn runs_first(&self, binding: &Binding, contexts: &[KeyContext]) -> bool {
        let (found, _) = resolve(&binding.events(), contexts, self.indexed());
        found
            .first()
            .is_some_and(|(_, first)| first.target == binding.target)
    }

    /// What can follow the keys typed so far, for a which-key popup: the rest of each longer
    /// binding in keymap spelling (`cmd-s`, `g g`) and the action it runs (a sequence comes
    /// as one `action::Sequence` action, see [`Binding::action`]).
    ///
    /// A port of gpui's `possible_next_bindings_for_input`. A binding is listed when typing
    /// its remaining keystrokes after `pending` would dispatch it, so entries disabled by
    /// `null` or `zed::Unbind` and entries out of context are left out. The order is the
    /// dispatch order: deepest context first, then last loaded first. Two bindings with the
    /// same remaining keys are both listed when both would be tried; the first one is tried
    /// first. With no pending keys there is nothing to complete and the result is empty.
    pub fn pending_completions(
        &self,
        pending: &[KeyEvent],
        contexts: &[KeyContext],
    ) -> Vec<(String, Action)> {
        if pending.is_empty() {
            return Vec::new();
        }
        // The `null` and `zed::Unbind` entries stay in: they can disable candidates.
        let extending: Vec<Indexed<'_>> = self
            .indexed()
            .filter(|(_, binding)| binding.match_keystrokes(pending) == Some(true))
            .collect();
        let mut completions: Vec<(usize, usize, String, Action)> = Vec::new();
        for &(index, binding) in &extending {
            let (Some(action), Some(depth)) = (binding.action(), binding.depth(contexts)) else {
                continue;
            };
            // What was really typed, then the rest of the binding as it would be typed.
            let remaining = &binding.keystrokes[pending.len()..];
            let mut input = pending.to_vec();
            input.extend(remaining.iter().map(Keystroke::to_event));
            let (dispatched, _) = resolve(&input, contexts, extending.iter().copied());
            if dispatched
                .iter()
                .any(|(dispatched, _)| *dispatched == index)
            {
                completions.push((depth, index, sequence_text(remaining), action));
            }
        }
        completions.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        completions
            .into_iter()
            .map(|(_, _, keys, action)| (keys, action))
            .collect()
    }
}

/// The target a binding must have to count as a binding of the action `name` with `args`.
fn lookup_target(name: &str, args: Option<&Value>) -> Option<Target> {
    let written = match args {
        Some(args) => Value::Array(vec![Value::String(name.to_string()), args.clone()]),
        None => Value::String(name.to_string()),
    };
    match crate::load::parse_target(&written) {
        Ok(target @ (Target::Action(_) | Target::Sequence(_))) => Some(target),
        _ => None,
    }
}

/// gpui's `resolve_bindings_for_input` over `bindings`, which must include every binding
/// that could match `input` (leaving out a `null` would let what it disables through).
fn resolve<'a>(
    input: &[KeyEvent],
    contexts: &[KeyContext],
    bindings: impl Iterator<Item = Indexed<'a>>,
) -> (Vec<Indexed<'a>>, bool) {
    let mut complete: Vec<(usize, usize, &Binding)> = Vec::new();
    let mut longer: Vec<Indexed<'a>> = Vec::new();
    for (index, binding) in bindings {
        // The keystroke test is the cheap one and rules out nearly every binding.
        let Some(more_keys_needed) = binding.match_keystrokes(input) else {
            continue;
        };
        let Some(depth) = binding.depth(contexts) else {
            continue;
        };
        if more_keys_needed {
            longer.push((index, binding));
        } else {
            complete.push((depth, index, binding));
        }
    }
    complete.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));

    let mut found: Vec<Indexed<'a>> = Vec::new();
    let mut unbinds: Vec<&Binding> = Vec::new();
    // The strongest source that has a `null` so far. It hides what follows from that source
    // and from weaker ones; a binding from a stronger source still applies.
    let mut null_rank: Option<u8> = None;
    for (_, index, binding) in complete {
        let rank = binding.source.rank();
        if binding.is_null() {
            null_rank = Some(null_rank.map_or(rank, |strongest| strongest.min(rank)));
            continue;
        }
        if null_rank.is_some_and(|strongest| rank >= strongest) {
            continue;
        }
        if binding.unbinds().is_some() {
            unbinds.push(binding);
            continue;
        }
        if unbinds.iter().any(|unbind| unbind.unbinds_binding(binding)) {
            continue;
        }
        found.push((index, binding));
    }

    // A longer binding keeps the keys pending only if it does not rank below the binding
    // that would run now, and only if no later `null` or `zed::Unbind` for the same
    // keystrokes cancels it.
    let first_found = found.first().map(|(index, _)| *index);
    longer.sort_by_key(|(index, _)| *index);
    let mut pending: HashSet<&[Keystroke]> = HashSet::new();
    for (index, binding) in longer {
        if first_found.is_some_and(|first| first > index) {
            continue;
        }
        if binding.is_null() || binding.unbinds().is_some() {
            pending.remove(binding.keystrokes.as_slice());
        } else {
            pending.insert(binding.keystrokes.as_slice());
        }
    }
    (found, !pending.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A keymap from `(source, json)` layers, which must load without warnings.
    fn layers(layers: &[(KeymapSource, &str)]) -> Keymap {
        let mut keymap = Keymap::new();
        for (source, text) in layers {
            let warnings = keymap.load(text, *source);
            assert!(warnings.is_empty(), "{warnings:?}");
        }
        keymap
    }

    fn user(text: &str) -> Keymap {
        layers(&[(KeymapSource::User, text)])
    }

    /// A context stack from gpui's compact form, one context per `/` separated part.
    fn stack(text: &str) -> Vec<KeyContext> {
        if text.is_empty() {
            return Vec::new();
        }
        text.split('/')
            .map(|part| KeyContext::parse(part).unwrap())
            .collect()
    }

    fn keys(text: &str) -> Vec<KeyEvent> {
        Keystroke::parse_sequence(text)
            .unwrap()
            .iter()
            .map(Keystroke::to_event)
            .collect()
    }

    /// The names of the actions `input` resolves to in `contexts`, and the pending flag.
    fn resolved(keymap: &Keymap, input: &str, contexts: &str) -> (Vec<String>, bool) {
        let (bindings, pending) = keymap.bindings_for_input(&keys(input), &stack(contexts));
        (bindings.iter().map(|b| name(b)).collect(), pending)
    }

    fn name(binding: &Binding) -> String {
        binding
            .actions()
            .iter()
            .map(|action| action.name.clone())
            .collect::<Vec<String>>()
            .join("+")
    }

    fn names(expected: &[&str]) -> Vec<String> {
        expected.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn enabled_depth() {
        let keymap = user(
            r#"[
                {"bindings": {"ctrl-a": "Alpha"}},
                {"context": "pane", "bindings": {"ctrl-a": "Beta"}},
                {"context": "editor && mode==full", "bindings": {"ctrl-a": "Gamma"}}
            ]"#,
        );
        let [global, pane, editor] = keymap.bindings() else {
            panic!("three bindings expected");
        };
        // Global bindings are enabled everywhere, at the deepest level.
        assert_eq!(global.depth(&[]), Some(0));
        assert_eq!(global.depth(&stack("terminal")), Some(1));
        // Contextual bindings are enabled where their predicate holds.
        assert_eq!(pane.depth(&stack("barf x=y")), None);
        assert_eq!(pane.depth(&stack("pane x=y")), Some(1));
        assert_eq!(pane.depth(&[]), None);
        assert_eq!(editor.depth(&stack("editor")), None);
        assert_eq!(editor.depth(&stack("editor mode=full")), Some(1));
        assert_eq!(editor.depth(&stack("pane/editor mode=full/menu")), Some(2));
    }

    #[test]
    fn deeper_context_wins() {
        let keymap = user(
            r#"[
                {"context": "pane", "bindings": {"ctrl-a": "Beta"}},
                {"context": "editor", "bindings": {"ctrl-a": "Gamma"}}
            ]"#,
        );
        assert_eq!(
            resolved(&keymap, "ctrl-a", "pane/editor"),
            (names(&["Gamma", "Beta"]), false)
        );
        // Depth beats load order.
        let keymap = user(
            r#"[
                {"context": "Editor", "bindings": {"cmd-r": "Beta"}},
                {"context": "Workspace", "bindings": {"cmd-r": "Alpha"}}
            ]"#,
        );
        assert_eq!(
            resolved(&keymap, "cmd-r", "Workspace/Editor"),
            (names(&["Beta", "Alpha"]), false)
        );
        assert_eq!(
            resolved(&keymap, "cmd-r", "Workspace/Terminal"),
            (names(&["Alpha"]), false)
        );
        assert_eq!(resolved(&keymap, "cmd-r", ""), (names(&[]), false));
    }

    #[test]
    fn later_binding_wins_at_equal_depth() {
        let built_in = r#"[{"context": "Editor", "bindings": {"cmd-r": "Alpha"}}]"#;
        let users = r#"[{"context": "Editor", "bindings": {"cmd-r": "Beta"}}]"#;
        let keymap = layers(&[
            (KeymapSource::Default, built_in),
            (KeymapSource::User, users),
        ]);
        assert_eq!(
            resolved(&keymap, "cmd-r", "Editor"),
            (names(&["Beta", "Alpha"]), false)
        );
        // Load order decides, not the source: the same two layers the other way round.
        let keymap = layers(&[
            (KeymapSource::User, users),
            (KeymapSource::Default, built_in),
        ]);
        assert_eq!(
            resolved(&keymap, "cmd-r", "Editor"),
            (names(&["Alpha", "Beta"]), false)
        );
        // `Picker > Editor` and `Editor` end at the same depth, so the later one wins.
        let keymap = user(
            r#"[
                {"context": "Picker > Editor", "bindings": {"ctrl-alt-down": "Picker"}},
                {"context": "Editor", "bindings": {"ctrl-alt-down": "Base"}}
            ]"#,
        );
        assert_eq!(
            resolved(&keymap, "ctrl-alt-down", "Picker/Editor").0,
            names(&["Base", "Picker"])
        );
    }

    #[test]
    fn no_context_counts_as_the_deepest_level() {
        let keymap = user(
            r#"[
                {"bindings": {"enter": "menu::Confirm"}},
                {"context": "Editor", "bindings": {"enter": "editor::Newline"}},
                {"context": "Pane", "bindings": {"enter": "pane::Enter"}}
            ]"#,
        );
        // The global binding ties with the Editor one on depth and loses on load order; both
        // come before the shallower Pane binding.
        assert_eq!(
            resolved(&keymap, "enter", "Workspace/Pane/Editor").0,
            names(&["editor::Newline", "menu::Confirm", "pane::Enter"])
        );
        assert_eq!(
            resolved(&keymap, "enter", "Workspace/Pane").0,
            names(&["pane::Enter", "menu::Confirm"])
        );
        assert_eq!(resolved(&keymap, "enter", "").0, names(&["menu::Confirm"]));
        let keymap = user(
            r#"[
                {"context": "Editor", "bindings": {"enter": "editor::Newline"}},
                {"bindings": {"enter": "menu::Confirm"}}
            ]"#,
        );
        assert_eq!(
            resolved(&keymap, "enter", "Workspace/Editor").0,
            names(&["menu::Confirm", "editor::Newline"])
        );
    }

    #[test]
    fn bindings_keep_their_order_in_the_file() {
        // A JSON object has no order of its own once parsed, and `serde_json` sorts the keys
        // unless a feature says otherwise. Here the order in the file decides twice: which of
        // two spellings of one key wins, and whether a longer binding makes the keys wait.
        let keymap = user(
            r#"[{"bindings": {
                "cmd-shift-[": "Later",
                "cmd-{": "Latest",
                "z b": "Longer",
                "z": "Short",
                "a": "ShortFirst",
                "a b": "LongerAfter"
            }}]"#,
        );
        let written: Vec<String> = keymap
            .bindings()
            .iter()
            .map(Binding::keystroke_text)
            .collect();
        assert_eq!(written, ["shift-cmd-[", "cmd-{", "z b", "z", "a", "a b"]);
        assert_eq!(
            resolved(&keymap, "cmd-{", ""),
            (names(&["Latest", "Later"]), false)
        );
        // `z b` was written before `z`, so `z` runs at once.
        assert_eq!(resolved(&keymap, "z", ""), (names(&["Short"]), false));
        // `a b` was written after `a`, so `a` waits for a possible `b`.
        assert_eq!(resolved(&keymap, "a", ""), (names(&["ShortFirst"]), true));
    }

    #[test]
    fn null_disables_in_its_context() {
        let keymap = user(
            r#"[
                {"context": "editor", "bindings": {"ctrl-a": "Alpha", "ctrl-b": "Alpha"}},
                {"context": "editor && mode==full", "bindings": {"ctrl-a": null}},
                {"bindings": {"ctrl-b": null}}
            ]"#,
        );
        assert_eq!(resolved(&keymap, "ctrl-a", "barf").0, names(&[]));
        assert_eq!(resolved(&keymap, "ctrl-a", "editor").0, names(&["Alpha"]));
        // Disabled in the more specific context.
        assert_eq!(
            resolved(&keymap, "ctrl-a", "editor mode=full").0,
            names(&[])
        );
        // Disabled everywhere.
        assert_eq!(resolved(&keymap, "ctrl-b", "barf").0, names(&[]));
        assert_eq!(resolved(&keymap, "ctrl-b", "editor").0, names(&[]));
    }

    #[test]
    fn null_only_disables_what_it_outranks() {
        // The simple case: same context, loaded later.
        let keymap = user(
            r#"[
                {"context": "editor", "bindings": {"ctrl-x": "Alpha"}},
                {"context": "editor", "bindings": {"ctrl-x": null}}
            ]"#,
        );
        assert_eq!(resolved(&keymap, "ctrl-x", "editor"), (names(&[]), false));
        // Loaded earlier at the same depth: the later binding outranks the null.
        let keymap = user(
            r#"[
                {"context": "editor", "bindings": {"ctrl-x": null}},
                {"context": "editor", "bindings": {"ctrl-x": "Alpha"}}
            ]"#,
        );
        assert_eq!(resolved(&keymap, "ctrl-x", "editor").0, names(&["Alpha"]));
        // Disabled at the wrong level: the editor binding is deeper than the null.
        let keymap = user(
            r#"[
                {"context": "editor", "bindings": {"ctrl-x": "Alpha"}},
                {"context": "workspace", "bindings": {"ctrl-x": null}}
            ]"#,
        );
        assert_eq!(
            resolved(&keymap, "ctrl-x", "workspace/editor"),
            (names(&["Alpha"]), false)
        );
        // Disabled from a deeper level.
        let keymap = user(
            r#"[
                {"context": "workspace", "bindings": {"ctrl-x": "Alpha"}},
                {"context": "editor", "bindings": {"ctrl-x": null}}
            ]"#,
        );
        assert_eq!(
            resolved(&keymap, "ctrl-x", "workspace/editor"),
            (names(&[]), false)
        );
        assert_eq!(
            resolved(&keymap, "ctrl-x", "workspace").0,
            names(&["Alpha"])
        );
    }

    #[test]
    fn null_disables_its_own_source_and_weaker_ones_only() {
        let default_binding = r#"[{"context": "editor", "bindings": {"ctrl-x": "Alpha"}}]"#;
        let null = r#"[{"context": "editor", "bindings": {"ctrl-x": null}}]"#;
        let global = r#"[{"bindings": {"ctrl-x": "Beta"}}]"#;
        let workspace = r#"[{"context": "workspace", "bindings": {"ctrl-x": "Beta"}}]"#;

        use KeymapSource::{Default, User};

        // A null in the built-in keymap disables a built-in binding.
        let keymap = layers(&[(Default, default_binding), (Default, null)]);
        assert_eq!(resolved(&keymap, "ctrl-x", "editor").0, names(&[]));

        // A user binding is not affected by a built-in null that outranks it.
        let keymap = layers(&[(Default, global), (Default, null)]);
        assert_eq!(resolved(&keymap, "ctrl-x", "editor").0, names(&[]));
        let keymap = layers(&[(User, global), (Default, null)]);
        assert_eq!(resolved(&keymap, "ctrl-x", "editor").0, names(&["Beta"]));

        // Nor is a user binding at a shallower context.
        let keymap = layers(&[(Default, null), (User, workspace)]);
        assert_eq!(
            resolved(&keymap, "ctrl-x", "workspace/editor").0,
            names(&["Beta"])
        );
        let keymap = layers(&[(Default, null), (Default, workspace)]);
        assert_eq!(
            resolved(&keymap, "ctrl-x", "workspace/editor").0,
            names(&[])
        );

        // A user null disables everything it outranks.
        let keymap = layers(&[
            (Default, default_binding),
            (Default, workspace),
            (User, null),
        ]);
        assert_eq!(
            resolved(&keymap, "ctrl-x", "workspace/editor").0,
            names(&[])
        );

        // With nulls from both sources the stronger one decides.
        let keymap = layers(&[(User, workspace), (Default, null), (User, null)]);
        assert_eq!(
            resolved(&keymap, "ctrl-x", "workspace/editor").0,
            names(&[])
        );
    }

    #[test]
    fn zed_no_action_is_null() {
        let keymap = user(
            r#"[
                {"context": "editor", "bindings": {"ctrl-x": "Alpha", "ctrl-y": "Alpha"}},
                {"context": "editor", "bindings": {
                    "ctrl-x": "zed::NoAction",
                    "ctrl-y": ["zed::NoAction", {"ignored": true}]
                }}
            ]"#,
        );
        assert!(keymap.bindings()[2].is_null());
        assert!(keymap.bindings()[3].is_null());
        assert_eq!(resolved(&keymap, "ctrl-x", "editor").0, names(&[]));
        assert_eq!(resolved(&keymap, "ctrl-y", "editor").0, names(&[]));
    }

    #[test]
    fn longer_binding_makes_the_keys_pending() {
        let keymap =
            user(r#"[{"context": "Editor", "bindings": {"cmd-k cmd-s": "Save", "g g": "Top"}}]"#);
        assert_eq!(resolved(&keymap, "cmd-k", "Editor"), (names(&[]), true));
        assert_eq!(
            resolved(&keymap, "cmd-k cmd-s", "Editor"),
            (names(&["Save"]), false)
        );
        assert_eq!(
            resolved(&keymap, "cmd-k cmd-x", "Editor"),
            (names(&[]), false)
        );
        assert_eq!(
            resolved(&keymap, "cmd-k cmd-s cmd-s", "Editor"),
            (names(&[]), false)
        );
        assert_eq!(resolved(&keymap, "g", "Editor"), (names(&[]), true));
        // Out of context there is nothing to wait for.
        assert_eq!(resolved(&keymap, "cmd-k", "Terminal"), (names(&[]), false));
        // No keys typed yet: every enabled binding is still to come.
        let (bindings, pending) = keymap.bindings_for_input(&[], &stack("Editor"));
        assert!(bindings.is_empty());
        assert!(pending);
    }

    #[test]
    fn pending_needs_a_binding_that_does_not_rank_below_the_match() {
        let contexts = "Workspace/Pane/Editor vim_mode=normal";
        // The longer binding was loaded after the short one: both the match and pending.
        let keymap = user(
            r#"[
                {"context": "vim_mode == normal", "bindings": {"ctrl-x": "Beta"}},
                {"context": "Workspace", "bindings": {"ctrl-x 0": "Alpha"}}
            ]"#,
        );
        assert_eq!(
            resolved(&keymap, "ctrl-x", contexts),
            (names(&["Beta"]), true)
        );
        // The longer binding was loaded before the short one: the short one overrides it.
        let keymap = user(
            r#"[
                {"context": "Workspace", "bindings": {"ctrl-x 0": "Alpha"}},
                {"context": "vim_mode == normal", "bindings": {"ctrl-x": "Beta"}}
            ]"#,
        );
        assert_eq!(
            resolved(&keymap, "ctrl-x", contexts),
            (names(&["Beta"]), false)
        );
        // A null longer binding is nothing to wait for, wherever it sits.
        for sections in [
            r#"[
                {"context": "vim_mode == normal", "bindings": {"ctrl-x": "Beta"}},
                {"context": "Workspace", "bindings": {"ctrl-x 0": null}}
            ]"#,
            r#"[
                {"context": "Workspace", "bindings": {"ctrl-x": "Beta"}},
                {"context": "vim_mode == normal", "bindings": {"ctrl-x 0": null}}
            ]"#,
        ] {
            let keymap = user(sections);
            assert_eq!(
                resolved(&keymap, "ctrl-x", contexts),
                (names(&["Beta"]), false)
            );
        }
    }

    #[test]
    fn null_on_the_prefix_does_not_cancel_the_longer_binding() {
        let keymap = user(
            r#"[{"context": "editor", "bindings": {"ctrl-w left": "Alpha", "ctrl-w": null}}]"#,
        );
        assert_eq!(resolved(&keymap, "ctrl-w", "editor"), (names(&[]), true));
        let keymap = user(
            r#"[{"context": "editor", "bindings": {"ctrl-w left": "Alpha", "ctrl-w": "Beta"}}]"#,
        );
        assert_eq!(
            resolved(&keymap, "ctrl-w", "editor"),
            (names(&["Beta"]), false)
        );
    }

    #[test]
    fn null_on_a_longer_binding_cancels_the_wait() {
        // https://github.com/zed-industries/zed/issues/30259
        let keymap = user(
            r#"[
                {"context": "workspace", "bindings": {"space w w": "Alpha"}},
                {"context": "editor", "bindings": {"space w w": null}}
            ]"#,
        );
        assert_eq!(resolved(&keymap, "space", "workspace"), (names(&[]), true));
        assert_eq!(
            resolved(&keymap, "space", "workspace/editor"),
            (names(&[]), false)
        );
        assert_eq!(
            resolved(&keymap, "space w", "workspace"),
            (names(&[]), true)
        );
        assert_eq!(
            resolved(&keymap, "space w", "workspace/editor"),
            (names(&[]), false)
        );
        assert_eq!(
            resolved(&keymap, "space w w", "workspace"),
            (names(&["Alpha"]), false)
        );
        assert_eq!(
            resolved(&keymap, "space w w", "workspace/editor"),
            (names(&[]), false)
        );

        // Another longer binding keeps the keys pending, whether it comes after the null,
        // before it, or from a shallower context.
        for sections in [
            r#"[
                {"context": "workspace", "bindings": {"space w w": "Alpha"}},
                {"context": "editor", "bindings": {"space w w": null}},
                {"context": "editor", "bindings": {"space w x": "Alpha"}}
            ]"#,
            r#"[
                {"context": "workspace", "bindings": {"space w w": "Alpha"}},
                {"context": "editor", "bindings": {"space w x": "Alpha"}},
                {"context": "editor", "bindings": {"space w w": null}}
            ]"#,
            r#"[
                {"context": "workspace", "bindings": {"space w w": "Alpha"}},
                {"context": "workspace", "bindings": {"space w x": "Alpha"}},
                {"context": "editor", "bindings": {"space w w": null}}
            ]"#,
        ] {
            let keymap = user(sections);
            assert_eq!(
                resolved(&keymap, "space", "workspace/editor"),
                (names(&[]), true)
            );
        }
    }

    #[test]
    fn unbind_removes_the_named_action_only() {
        // The unbind's own context does not have to cover the target's.
        let keymap = user(
            r#"[
                {"context": "Editor", "bindings": {"tab": "Alpha"}},
                {"context": "Editor && showing_completions", "bindings": {"tab": "Beta"}},
                {"context": "Editor && edit_prediction", "bindings": {"tab": ["zed::Unbind", "Alpha"]}}
            ]"#,
        );
        assert_eq!(keymap.bindings()[2].unbinds(), Some("Alpha"));
        assert_eq!(
            resolved(&keymap, "tab", "Editor showing_completions edit_prediction"),
            (names(&["Beta"]), false)
        );
        // Where the unbind is not enabled the binding is intact.
        assert_eq!(
            resolved(&keymap, "tab", "Editor showing_completions").0,
            names(&["Beta", "Alpha"])
        );
        // An unbind only reaches the bindings it outranks.
        let keymap = user(
            r#"[
                {"context": "Editor", "bindings": {"tab": ["zed::Unbind", "Alpha"]}},
                {"context": "Editor", "bindings": {"tab": "Alpha"}}
            ]"#,
        );
        assert_eq!(resolved(&keymap, "tab", "Editor").0, names(&["Alpha"]));
    }

    #[test]
    fn unbind_section_is_loaded_before_the_bindings() {
        let keymap = user(
            r#"[
                {"context": "Editor", "bindings": {"tab": "Alpha", "enter": ["Gamma", {"n": 1}]}},
                {
                    "context": "Editor",
                    "bindings": {"tab": "Beta"},
                    "unbind": {"tab": "Alpha", "enter": ["Gamma", {"n": 2}]}
                }
            ]"#,
        );
        let written: Vec<(String, Option<&str>)> = keymap
            .bindings()
            .iter()
            .map(|binding| (binding.keystroke_text(), binding.unbinds()))
            .collect();
        assert_eq!(
            written,
            [
                ("tab".to_string(), None),
                ("enter".to_string(), None),
                ("tab".to_string(), Some("Alpha")),
                ("enter".to_string(), Some("Gamma")),
                ("tab".to_string(), None),
            ]
        );
        assert_eq!(resolved(&keymap, "tab", "Editor").0, names(&["Beta"]));
        // The argument of the unbound action is not compared, only its name.
        assert_eq!(resolved(&keymap, "enter", "Editor").0, names(&[]));
    }

    #[test]
    fn unbind_reaches_another_spelling_of_the_same_keys() {
        let keymap = user(
            r#"[
                {"context": "Pane", "bindings": {"cmd-{": "pane::Previous", "alt-s": "Alpha"}},
                {"context": "Pane", "bindings": {
                    "cmd-shift-[": ["zed::Unbind", "pane::Previous"],
                    "ß": ["zed::Unbind", "Alpha"]
                }}
            ]"#,
        );
        assert_eq!(resolved(&keymap, "cmd-{", "Pane").0, names(&[]));
        // A character an alt chord happens to type is not the same keys.
        let typed =
            [KeyEvent::new(bakedit_core::Key::Char('s'), bakedit_core::Mods::ALT).with_text("ß")];
        let (bindings, _) = keymap.bindings_for_input(&typed, &stack("Pane"));
        assert_eq!(bindings.len(), 1);
        assert_eq!(name(bindings[0]), "Alpha");
    }

    #[test]
    fn sequences_and_arguments_are_carried_through() {
        let keymap = user(
            r#"[{"context": "Workspace", "bindings": {
                "cmd-alt-g": ["action::Sequence", ["git::Diff", "workspace::ToggleZoom"]],
                "cmd-up": ["editor::MoveUpByLines", {"lines": 5}],
                "cmd-alt-left": ["terminal::SendText", "\u0002h"],
                "cmd-1": ["pane::ActivateItem", 0],
                "cmd-2": ["editor::Copy"],
                "cmd-3": ["editor::Paste", null],
                "cmd-4": ["action::Sequence", [
                    "a",
                    null,
                    ["action::Sequence", [["b", {"x": 1}], "zed::NoAction", "c"]],
                    ["zed::Unbind", "d"],
                    "e"
                ]],
                "cmd-5": ["action::Sequence", []]
            }}]"#,
        );
        let action = |keys: &str| -> Vec<Action> {
            let (bindings, _) = keymap.bindings_for_input(&self::keys(keys), &stack("Workspace"));
            assert_eq!(bindings.len(), 1, "{keys}");
            bindings[0].actions().to_vec()
        };
        assert_eq!(
            action("cmd-alt-g"),
            [
                Action::new("git::Diff"),
                Action::new("workspace::ToggleZoom")
            ]
        );
        assert_eq!(
            action("cmd-up"),
            [Action::with_args(
                "editor::MoveUpByLines",
                json!({"lines": 5})
            )]
        );
        assert_eq!(action("cmd-alt-left")[0].arg_str("text"), Some("\u{2}h"));
        assert_eq!(action("cmd-1")[0].arg_i64("index"), Some(0));
        assert_eq!(action("cmd-2"), [Action::new("editor::Copy")]);
        assert_eq!(action("cmd-3"), [Action::new("editor::Paste")]);
        // Nested sequences are flattened in the order written; the entries that do nothing
        // are dropped.
        assert_eq!(
            action("cmd-4"),
            [
                Action::new("a"),
                Action::with_args("b", json!({"x": 1})),
                Action::new("c"),
                Action::new("e")
            ]
        );
        assert_eq!(action("cmd-5"), []);

        let binding = |keys: &str| -> &Binding {
            keymap
                .bindings()
                .iter()
                .find(|binding| binding.keystroke_text() == keys)
                .unwrap_or_else(|| panic!("no binding {keys}"))
        };
        assert_eq!(
            binding("alt-cmd-g").action(),
            Some(Action::with_args(
                SEQUENCE,
                json!(["git::Diff", "workspace::ToggleZoom"])
            ))
        );
        assert_eq!(
            binding("cmd-4").action(),
            Some(Action::with_args(
                SEQUENCE,
                json!(["a", ["b", {"x": 1}], "c", "e"])
            ))
        );
        assert_eq!(
            binding("cmd-5").action(),
            Some(Action::with_args(SEQUENCE, json!([])))
        );
        assert_eq!(
            binding("cmd-up").action(),
            Some(Action::with_args(
                "editor::MoveUpByLines",
                json!({"lines": 5})
            ))
        );
        assert_eq!(binding("cmd-up").display_mac(), "⌘↑");
        assert_eq!(binding("cmd-up").source(), KeymapSource::User);
        assert_eq!(
            binding("cmd-up").context().map(ToString::to_string),
            Some("Workspace".to_string())
        );
    }

    #[test]
    fn binding_accessors_for_disabling_entries() {
        let keymap =
            user(r#"[{"bindings": {"cmd-k cmd-s": null, "tab": ["zed::Unbind", "Alpha"]}}]"#);
        let [null, unbind] = keymap.bindings() else {
            panic!("two bindings expected");
        };
        assert!(null.is_null());
        assert_eq!(null.unbinds(), None);
        assert_eq!(null.actions(), []);
        assert_eq!(null.action(), None);
        assert_eq!(null.context(), None);
        assert_eq!(null.keystroke_text(), "cmd-k cmd-s");
        assert_eq!(null.display_mac(), "⌘K ⌘S");
        assert_eq!(null.keystrokes().len(), 2);
        assert!(!unbind.is_null());
        assert_eq!(unbind.unbinds(), Some("Alpha"));
        assert_eq!(unbind.actions(), []);
        assert_eq!(unbind.action(), None);
        assert_eq!(keymap.len(), 2);
        assert!(!keymap.is_empty());
        let mut keymap = keymap;
        keymap.clear();
        assert!(keymap.is_empty());
        assert_eq!(resolved(&keymap, "tab", "Editor"), (names(&[]), false));
    }

    #[test]
    fn events_match_through_their_shifted_form() {
        let keymap = user(
            r#"[{"context": "Editor", "bindings": {
                "cmd-k {": "Brace",
                "cmd-k \"": "Quote",
                "shift-g": "Bottom"
            }}]"#,
        );
        use bakedit_core::{Key, Mods};
        let cmd_k = KeyEvent::new(Key::Char('k'), Mods::SUPER);
        let contexts = stack("Editor");
        // US: shift+[ is a brace. JIS: shift+2 is a double quote.
        let brace = KeyEvent::typed('{');
        let (bindings, _) = keymap.bindings_for_input(&[cmd_k.clone(), brace], &contexts);
        assert_eq!(name(bindings[0]), "Brace");
        let jis_quote = KeyEvent::new(Key::Char('2'), Mods::SHIFT).with_shifted('"');
        let (bindings, _) = keymap.bindings_for_input(&[cmd_k.clone(), jis_quote], &contexts);
        assert_eq!(name(bindings[0]), "Quote");
        let us_at = KeyEvent::typed('@');
        let (bindings, pending) = keymap.bindings_for_input(&[cmd_k, us_at], &contexts);
        assert!(bindings.is_empty() && !pending);
        let (bindings, _) = keymap.bindings_for_input(&[KeyEvent::typed('G')], &contexts);
        assert_eq!(name(bindings[0]), "Bottom");
    }

    fn shortcuts(keymap: &Keymap, action: &str, contexts: Option<&str>) -> Vec<String> {
        let contexts = contexts.map(stack);
        keymap
            .bindings_for_action(action, None, contexts.as_deref())
            .iter()
            .map(|binding| binding.keystroke_text())
            .collect()
    }

    #[test]
    fn bindings_for_action_without_a_context_stack() {
        let keymap = user(
            r#"[
                {"context": "pane", "bindings": {"ctrl-a": "Alpha"}},
                {"context": "editor && mode == full", "bindings": {"ctrl-b": "Beta"}},
                {"context": "workspace", "bindings": {"ctrl-c": "Gamma"}},
                {"context": "pane && active", "bindings": {"ctrl-a": null}},
                {"context": "editor", "bindings": {"ctrl-b": null}}
            ]"#,
        );
        // The null for ctrl-a is narrower than the binding, the one for ctrl-b covers it.
        assert_eq!(shortcuts(&keymap, "Alpha", None), ["ctrl-a"]);
        assert_eq!(shortcuts(&keymap, "Beta", None), [] as [&str; 0]);
        assert_eq!(shortcuts(&keymap, "Gamma", None), ["ctrl-c"]);
        assert_eq!(shortcuts(&keymap, "Missing", None), [] as [&str; 0]);
        // A null with no context disables every binding of the keys, a null with a context
        // never disables a binding without one, and only later nulls count.
        let keymap = user(
            r#"[
                {"context": "pane", "bindings": {"ctrl-a": "Alpha", "ctrl-d": null}},
                {"bindings": {"ctrl-b": "Beta", "ctrl-d": "Delta"}},
                {"bindings": {"ctrl-a": null}},
                {"context": "pane", "bindings": {"ctrl-b": null}}
            ]"#,
        );
        assert_eq!(shortcuts(&keymap, "Alpha", None), [] as [&str; 0]);
        assert_eq!(shortcuts(&keymap, "Beta", None), ["ctrl-b"]);
        assert_eq!(shortcuts(&keymap, "Delta", None), ["ctrl-d"]);
    }

    #[test]
    fn bindings_for_action_lists_the_last_loaded_first() {
        let keymap = layers(&[
            (
                KeymapSource::Default,
                r#"[{"context": "Workspace", "bindings": {"cmd-s": "Save", "cmd-k s": "Save", "fn-s": "Save"}}]"#,
            ),
            (
                KeymapSource::User,
                r#"[{"context": "Editor", "bindings": {"ctrl-s": "Save"}}]"#,
            ),
        ]);
        assert_eq!(
            shortcuts(&keymap, "Save", None),
            ["ctrl-s", "cmd-k s", "cmd-s"]
        );
        assert_eq!(
            keymap.bindings_for_action("Save", None, None)[0].display_mac(),
            "⌃S"
        );
        // In a terminal the Editor binding is out of context.
        assert_eq!(
            shortcuts(&keymap, "Save", Some("Workspace/Terminal")),
            ["cmd-k s", "cmd-s"]
        );
        assert_eq!(
            shortcuts(&keymap, "Save", Some("Workspace/Pane/Editor")),
            ["ctrl-s", "cmd-k s", "cmd-s"]
        );
        assert_eq!(shortcuts(&keymap, "Save", Some("")), [] as [&str; 0]);
    }

    #[test]
    fn bindings_for_action_compares_arguments() {
        let keymap = user(
            r#"[{"bindings": {
                "cmd-up": ["editor::MoveUpByLines", {"lines": 5}],
                "alt-up": ["editor::MoveUpByLines", {"lines": 1}],
                "up": "editor::MoveUp",
                "cmd-alt-g": ["action::Sequence", ["git::Diff", ["workspace::ToggleZoom", {"x": 1}]]]
            }}]"#,
        );
        let find = |name: &str, args: Option<Value>| -> Vec<String> {
            keymap
                .bindings_for_action(name, args.as_ref(), None)
                .iter()
                .map(|binding| binding.keystroke_text())
                .collect()
        };
        assert_eq!(
            find("editor::MoveUpByLines", Some(json!({"lines": 5}))),
            ["cmd-up"]
        );
        assert_eq!(
            find("editor::MoveUpByLines", Some(json!({"lines": 1}))),
            ["alt-up"]
        );
        assert_eq!(
            find("editor::MoveUpByLines", Some(json!({"lines": 2}))),
            [] as [&str; 0]
        );
        assert_eq!(find("editor::MoveUpByLines", None), [] as [&str; 0]);
        assert_eq!(find("editor::MoveUp", None), ["up"]);
        assert_eq!(find("editor::MoveUp", Some(Value::Null)), ["up"]);
        assert_eq!(find("editor::MoveUp", Some(json!({}))), [] as [&str; 0]);
        assert_eq!(
            find(
                SEQUENCE,
                Some(json!(["git::Diff", ["workspace::ToggleZoom", {"x": 1}]]))
            ),
            ["alt-cmd-g"]
        );
        assert_eq!(find(SEQUENCE, Some(json!(["git::Diff"]))), [] as [&str; 0]);
        assert_eq!(find(SEQUENCE, None), [] as [&str; 0]);
        assert_eq!(find("git::Diff", None), [] as [&str; 0]);
        assert_eq!(find(NO_ACTION, None), [] as [&str; 0]);
        assert_eq!(find("", None), [] as [&str; 0]);
    }

    #[test]
    fn bindings_for_action_in_context_hides_shadowed_bindings() {
        // The binding is found in its own context.
        let keymap = user(r#"[{"context": "ProjectPanel", "bindings": {"cmd-n": "New"}}]"#);
        assert_eq!(
            shortcuts(&keymap, "New", Some("Workspace/ProjectPanel")),
            ["cmd-n"]
        );
        assert_eq!(
            shortcuts(&keymap, "New", Some("Workspace/Editor")),
            [] as [&str; 0]
        );

        // `Picker > Editor` and `Editor` resolve at the same depth, so a base binding loaded
        // later shadows the picker binding until an overlay loaded after it binds it again.
        let shadowed = user(
            r#"[
                {"context": "Picker > Editor", "bindings": {"ctrl-alt-down": "Picker"}},
                {"context": "Editor", "bindings": {"ctrl-alt-down": "Base"}}
            ]"#,
        );
        assert_eq!(
            shortcuts(&shadowed, "Picker", Some("Picker/Editor")),
            [] as [&str; 0]
        );
        assert_eq!(
            shortcuts(&shadowed, "Base", Some("Picker/Editor")),
            ["ctrl-alt-down"]
        );
        assert_eq!(shortcuts(&shadowed, "Picker", None), ["ctrl-alt-down"]);
        let fixed = user(
            r#"[
                {"context": "Picker > Editor", "bindings": {"ctrl-alt-down": "Picker"}},
                {"context": "Editor", "bindings": {"ctrl-alt-down": "Base"}},
                {"context": "Picker > Editor", "bindings": {"ctrl-alt-down": "Picker"}}
            ]"#,
        );
        // Both picker bindings count: the test is that their keys run the action first.
        assert_eq!(
            shortcuts(&fixed, "Picker", Some("Picker/Editor")),
            ["ctrl-alt-down", "ctrl-alt-down"]
        );
        assert_eq!(
            shortcuts(&fixed, "Base", Some("Picker/Editor")),
            [] as [&str; 0]
        );
        // Binding it twice before the base binding does not help: the later one still wins.
        let too_early = user(
            r#"[
                {"context": "Picker > Editor", "bindings": {"ctrl-alt-down": "Picker"}},
                {"context": "Picker > Editor", "bindings": {"ctrl-alt-down": "Picker"}},
                {"context": "Editor", "bindings": {"ctrl-alt-down": "Base"}}
            ]"#,
        );
        assert_eq!(
            shortcuts(&too_early, "Picker", Some("Picker/Editor")),
            [] as [&str; 0]
        );
    }

    #[test]
    fn bindings_for_action_respects_unbind() {
        let sections = r#"[
            {"context": "Editor", "bindings": {"tab": "Alpha"}},
            {"context": "Editor && edit_prediction", "bindings": {"tab": ["zed::Unbind", "Alpha"]}},
            {"context": "Editor && showing_completions", "bindings": {"tab": "Beta"}}
        ]"#;
        let keymap = user(sections);
        // Where the unbind is active the binding is hidden, the fallback is not.
        let active = Some("Workspace/Editor showing_completions edit_prediction");
        assert_eq!(shortcuts(&keymap, "Alpha", active), [] as [&str; 0]);
        assert_eq!(shortcuts(&keymap, "Beta", active), ["tab"]);
        // Outside the unbind's context the binding is kept.
        assert_eq!(
            shortcuts(&keymap, "Alpha", Some("Workspace/Editor")),
            ["tab"]
        );
        // Without a stack a narrower unbind does not hide the binding, a broader one does.
        assert_eq!(shortcuts(&keymap, "Alpha", None), ["tab"]);
        assert_eq!(shortcuts(&keymap, "Beta", None), ["tab"]);
        let keymap = user(
            r#"[
                {"context": "Editor && edit_prediction", "bindings": {"tab": "Alpha", "enter": "Beta"}},
                {"context": "Editor", "bindings": {"tab": ["zed::Unbind", "Alpha"], "enter": ["zed::Unbind", "Alpha"]}}
            ]"#,
        );
        assert_eq!(shortcuts(&keymap, "Alpha", None), [] as [&str; 0]);
        assert_eq!(shortcuts(&keymap, "Beta", None), ["enter"]);
    }

    #[test]
    fn bindings_for_action_sees_through_spellings() {
        let keymap = user(
            r#"[
                {"context": "Pane", "bindings": {"cmd-{": "pane::Previous"}},
                {"context": "Pane", "bindings": {"cmd-shift-[": null}}
            ]"#,
        );
        assert_eq!(shortcuts(&keymap, "pane::Previous", None), [] as [&str; 0]);
        assert_eq!(
            shortcuts(&keymap, "pane::Previous", Some("Pane")),
            [] as [&str; 0]
        );
    }

    fn completions(keymap: &Keymap, pending: &str, contexts: &str) -> Vec<(String, String)> {
        keymap
            .pending_completions(&keys(pending), &stack(contexts))
            .into_iter()
            .map(|(keys, action)| (keys, action.name))
            .collect()
    }

    fn rows(expected: &[(&str, &str)]) -> Vec<(String, String)> {
        expected
            .iter()
            .map(|(keys, action)| (keys.to_string(), action.to_string()))
            .collect()
    }

    #[test]
    fn completions_leave_out_disabled_bindings() {
        let keymap = user(
            r#"[
                {"context": "Editor", "bindings": {"ctrl-k a": "Alpha", "ctrl-k b": "Beta"}},
                {"context": "Editor", "bindings": {"ctrl-k a": null}}
            ]"#,
        );
        assert_eq!(
            completions(&keymap, "ctrl-k", "Editor"),
            rows(&[("b", "Beta")])
        );
        let keymap = user(
            r#"[
                {"context": "Editor", "bindings": {"ctrl-k a": "Alpha", "ctrl-k b": "Beta"}},
                {"context": "Editor", "bindings": {"ctrl-k a": ["zed::Unbind", "Alpha"]}}
            ]"#,
        );
        assert_eq!(
            completions(&keymap, "ctrl-k", "Editor"),
            rows(&[("b", "Beta")])
        );
        // A user binding survives a null from the built-in keymap.
        let keymap = layers(&[
            (
                KeymapSource::Default,
                r#"[
                    {"context": "Editor", "bindings": {"ctrl-k a": "Alpha"}},
                    {"context": "Editor", "bindings": {"ctrl-k a": null}}
                ]"#,
            ),
            (
                KeymapSource::User,
                r#"[{"bindings": {"ctrl-k a": "Beta"}}]"#,
            ),
        ]);
        assert_eq!(
            completions(&keymap, "ctrl-k", "Editor"),
            rows(&[("a", "Beta")])
        );
    }

    #[test]
    fn completions_follow_the_context_stack() {
        let keymap = user(
            r#"[
                {"context": "Workspace", "bindings": {"ctrl-k a": "Alpha"}},
                {"context": "Editor", "bindings": {"ctrl-k a": "Beta", "ctrl-k b": "Gamma"}},
                {"context": "Editor", "bindings": {"ctrl-k a": ["zed::Unbind", "Beta"]}},
                {"context": "Terminal", "bindings": {"ctrl-k b": null}}
            ]"#,
        );
        // The unbind only removes the editor binding, and the Terminal null is out of context.
        assert_eq!(
            completions(&keymap, "ctrl-k", "Workspace/Editor"),
            rows(&[("b", "Gamma"), ("a", "Alpha")])
        );
        assert_eq!(
            completions(&keymap, "ctrl-k", "Workspace"),
            rows(&[("a", "Alpha")])
        );
        assert_eq!(completions(&keymap, "ctrl-k", "Terminal"), rows(&[]));
        assert_eq!(
            completions(&keymap, "ctrl-x", "Workspace/Editor"),
            rows(&[])
        );

        // A longer chord disabled in a deeper context is gone at every step.
        let keymap = user(
            r#"[
                {"context": "Workspace", "bindings": {"space w w": "Alpha", "space w x": "Beta"}},
                {"context": "Panel", "bindings": {"space w w": null}}
            ]"#,
        );
        assert_eq!(
            completions(&keymap, "space", "Workspace/Panel"),
            rows(&[("w x", "Beta")])
        );
        assert_eq!(
            completions(&keymap, "space w", "Workspace/Panel"),
            rows(&[("x", "Beta")])
        );
        assert_eq!(
            completions(&keymap, "space", "Workspace"),
            rows(&[("w x", "Beta"), ("w w", "Alpha")])
        );
    }

    #[test]
    fn completions_list_every_binding_that_would_be_tried() {
        let keymap = user(
            r#"[
                {"context": "Workspace", "bindings": {"cmd-k cmd-s": "Keymap", "cmd-k t": "Theme"}},
                {"context": "Editor", "bindings": {
                    "cmd-k cmd-s": ["action::Sequence", ["Save", "Format"]],
                    "cmd-k {": "Brace"
                }}
            ]"#,
        );
        let found = keymap.pending_completions(&keys("cmd-k"), &stack("Workspace/Editor"));
        assert_eq!(
            found,
            [
                ("{".to_string(), Action::new("Brace")),
                (
                    "cmd-s".to_string(),
                    Action::with_args(SEQUENCE, json!(["Save", "Format"]))
                ),
                ("t".to_string(), Action::new("Theme")),
                ("cmd-s".to_string(), Action::new("Keymap")),
            ]
        );
        // Nothing pending, nothing to complete.
        assert_eq!(
            keymap.pending_completions(&[], &stack("Workspace/Editor")),
            []
        );
    }

    #[test]
    fn completions_use_what_was_typed() {
        // Option+s typed a sharp s: the chord can continue through either spelling, and a
        // null written with one spelling disables the binding written with the other.
        use bakedit_core::{Key, Mods};
        let typed = [KeyEvent::new(Key::Char('s'), Mods::ALT).with_text("ß")];
        let keymap = user(r#"[{"bindings": {"ß a": "Alpha", "alt-s b": "Beta"}}]"#);
        let found: Vec<String> = keymap
            .pending_completions(&typed, &[])
            .into_iter()
            .map(|(keys, _)| keys)
            .collect();
        assert_eq!(found, ["b", "a"]);
        let keymap = user(r#"[{"bindings": {"ß a": "Alpha", "alt-s a": null}}]"#);
        assert_eq!(keymap.pending_completions(&typed, &[]), []);
        let mut then_a = typed.to_vec();
        then_a.push(KeyEvent::typed('a'));
        assert!(keymap.bindings_for_input(&then_a, &[]).0.is_empty());
    }
}
