//! Nothing in the crate may panic on bad input, and the matcher must agree with gpui's own
//! (recursive) formulation of key dispatch. Both are checked here on generated input. The
//! generator is seeded, so a failure reproduces.

use bakedit_core::{Action, Key, KeyEvent, KeyKind, ModKey, Mods};
use bakedit_keymap::{
    Binding, ContextPredicate, KeyContext, Keymap, KeymapSource, Keystroke, Matcher, PendingTimer,
    Step,
};

/// xorshift64*: small, seeded and good enough to shuffle test input.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a, T: ?Sized>(&mut self, items: &[&'a T]) -> &'a T {
        items[self.below(items.len())]
    }

    fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }

    /// Up to `max` pieces glued together with one of the separators.
    fn glue(&mut self, pieces: &[&str], separators: &[&str], max: usize) -> String {
        let mut out = String::new();
        for index in 0..self.below(max + 1) {
            if index > 0 {
                out.push_str(self.pick(separators));
            }
            out.push_str(self.pick(pieces));
        }
        out
    }
}

const KEY_PIECES: &[&str] = &[
    "cmd",
    "ctrl",
    "alt",
    "shift",
    "fn",
    "secondary",
    "hyper",
    "meta",
    "option",
    "win",
    "CMD",
    "",
    "-",
    "--",
    "a",
    "k",
    "G",
    "Z",
    "0",
    "{",
    "}",
    "\"",
    "'",
    "`",
    "?",
    "<",
    ">",
    "_",
    "=",
    "+",
    "enter",
    "Enter",
    "space",
    "tab",
    "escape",
    "up",
    "f5",
    "f35",
    "f99",
    "é",
    "Ä",
    "¥",
    "ß",
    "с",
    " ",
    "\t",
    "\u{7}",
    "nope",
    "😀",
    "->",
    ">x",
];

#[test]
fn keystroke_parsing_never_panics_and_round_trips() {
    let mut rng = Rng(0x5EED_0001);
    let mut parsed = 0;
    for _ in 0..40_000 {
        let text = rng.glue(KEY_PIECES, &["-", "-", "-", "", " "], 5);
        let Ok(keystroke) = Keystroke::parse(&text) else {
            continue;
        };
        parsed += 1;
        // The spelling parses back to the same keystroke and is stable.
        let spelled = keystroke.to_string();
        assert_eq!(
            Keystroke::parse(&spelled).as_ref(),
            Ok(&keystroke),
            "{text:?}"
        );
        assert!(!keystroke.display_mac().is_empty(), "{text:?}");
        // Its own key press matches it, unless nothing can type it: `fn`, or shift on top of a
        // character that already needs shift.
        let event = keystroke.to_event();
        let needs_shift = key_char(&keystroke).is_some_and(|c| event.shifted == Some(c));
        let untypable = keystroke.function || (keystroke.mods.contains(Mods::SHIFT) && needs_shift);
        assert_eq!(keystroke.matches(&event), !untypable, "{text:?}");
        // The name given to a key press matches that press.
        assert!(Keystroke::from_event(&event).matches(&event), "{text:?}");
        // A release of the same key never matches.
        assert!(
            !keystroke.matches(&event.with_kind(KeyKind::Release)),
            "{text:?}"
        );
    }
    assert!(parsed > 2_000, "only {parsed} keystrokes parsed");

    for _ in 0..10_000 {
        let text = rng.glue(KEY_PIECES, &["-", " ", "  ", ""], 8);
        if let Ok(keystrokes) = Keystroke::parse_sequence(&text) {
            assert!(!keystrokes.is_empty(), "{text:?}");
        }
    }
}

fn key_char(keystroke: &Keystroke) -> Option<char> {
    match keystroke.key {
        Key::Char(c) => Some(c),
        _ => None,
    }
}

const PREDICATE_PIECES: &[&str] = &[
    "Editor",
    "Pane",
    "Workspace",
    "mode",
    "full",
    "a",
    "b",
    "os",
    "macos",
    "not_editing",
    "é",
    "!",
    "!",
    "(",
    ")",
    "&&",
    "&&",
    "||",
    "==",
    "!=",
    ">",
    "<",
    "~",
    "\"",
    "?",
    "-",
    "_",
    "&",
    "|",
    "=",
    "$",
    "g~",
    "",
];

const CONTEXT_PIECES: &[&str] = &[
    "Editor",
    "Pane",
    "Workspace",
    "mode=full",
    "mode = auto",
    "a",
    "b",
    "a=b",
    "os=plan9",
    "not_editing",
    "é",
    "=",
    "x=",
    "!",
    "(",
    "g~",
];

fn random_stack(rng: &mut Rng) -> Vec<KeyContext> {
    (0..rng.below(5))
        .map(|_| {
            let text = rng.glue(&CONTEXT_PIECES[..11], &[" "], 3);
            KeyContext::parse(&text).unwrap_or_else(|e| panic!("{text:?}: {e}"))
        })
        .collect()
}

#[test]
fn context_parsing_and_evaluation_never_panic() {
    let mut rng = Rng(0x5EED_0002);
    let mut parsed = 0;
    for _ in 0..40_000 {
        let text = rng.glue(PREDICATE_PIECES, &[" ", " ", ""], 9);
        let predicate = match ContextPredicate::parse(&text) {
            Ok(predicate) => predicate,
            Err(error) => {
                // The column points into the text, or just past its end.
                assert!(error.column >= 1 && error.column <= text.chars().count() + 1);
                assert!(!error.to_string().is_empty());
                continue;
            }
        };
        parsed += 1;
        // What it prints as is a predicate again, and prints the same.
        let printed = predicate.to_string();
        let reparsed = ContextPredicate::parse(&printed)
            .unwrap_or_else(|e| panic!("{text:?} printed as {printed:?}: {e}"));
        assert_eq!(reparsed.to_string(), printed, "{text:?}");
        assert!(predicate.is_superset(&predicate));
        for _ in 0..4 {
            let stack = random_stack(&mut rng);
            let depth = predicate.depth_of(&stack);
            assert!(depth.is_none_or(|d| d >= 1 && d <= stack.len()), "{text:?}");
            // It holds for the focused element exactly when its depth is the whole stack.
            let holds = !stack.is_empty() && depth == Some(stack.len());
            assert_eq!(predicate.eval(&stack), holds, "{text:?}");
        }
    }
    assert!(parsed > 2_000, "only {parsed} predicates parsed");

    for _ in 0..20_000 {
        let text = rng.glue(CONTEXT_PIECES, &[" ", " ", "", " = "], 6);
        if let Ok(context) = KeyContext::parse(&text) {
            let printed = context.to_string();
            assert_eq!(
                KeyContext::parse(&printed).as_ref(),
                Ok(&context),
                "{text:?}"
            );
        }
    }
}

/// A random JSON value that is sometimes a sensible part of a keymap and sometimes not.
fn random_action(rng: &mut Rng) -> String {
    let simple = [
        "\"editor::Copy\"",
        "\"workspace::Save\"",
        "null",
        "\"zed::NoAction\"",
        "[\"pane::ActivateItem\", 3]",
        "[\"task::Spawn\", {\"task_name\": \"x\"}]",
        "[\"zed::Unbind\", \"editor::Copy\"]",
        "[\"editor::Copy\"]",
        "[\"editor::Copy\", null]",
        "\"\"",
        "\"zed::Unbind\"",
        "\"action::Sequence\"",
        "[]",
        "[1, 2]",
        "[\"a\", 1, 2]",
        "7",
        "true",
        "{\"name\": \"x\"}",
    ];
    if rng.one_in(4) {
        let items: Vec<String> = (0..rng.below(4)).map(|_| random_action(rng)).collect();
        format!("[\"action::Sequence\", [{}]]", items.join(", "))
    } else {
        rng.pick(&simple).to_string()
    }
}

fn json_string(text: &str) -> String {
    serde_json::Value::String(text.to_string()).to_string()
}

/// Keystrokes for a binding: usually sensible, sometimes anything at all.
fn random_keys(rng: &mut Rng) -> String {
    if rng.one_in(3) {
        rng.glue(KEY_PIECES, &["-", "-", " ", ""], 4)
    } else {
        let sensible = [
            "cmd-k",
            "a",
            "enter",
            "G",
            "{",
            "space",
            "ctrl-alt--",
            "shift-tab",
        ];
        rng.glue(&sensible, &[" "], 3)
    }
}

fn random_bindings(rng: &mut Rng) -> String {
    let entries: Vec<String> = (0..rng.below(6))
        .map(|_| format!("{}: {}", json_string(&random_keys(rng)), random_action(rng)))
        .collect();
    format!("{{{}}}", entries.join(", "))
}

fn random_section(rng: &mut Rng) -> String {
    if rng.one_in(12) {
        return rng
            .pick(&["null", "3", "\"section\"", "[]", "{}"])
            .to_string();
    }
    let mut fields = Vec::new();
    if !rng.one_in(4) {
        let context = match rng.below(10) {
            0 => "7".to_string(),
            1..=4 => json_string(&rng.glue(PREDICATE_PIECES, &[" "], 5)),
            _ => json_string(rng.pick(MODEL_CONTEXTS)),
        };
        fields.push(format!("\"context\": {context}"));
    }
    if rng.one_in(3) {
        fields.push("\"use_key_equivalents\": true".to_string());
    }
    if rng.one_in(4) {
        fields.push(format!("\"unbind\": {}", random_bindings(rng)));
    }
    if rng.one_in(8) {
        fields.push("\"bindings\": [\"not\", \"an object\"]".to_string());
    } else {
        fields.push(format!("\"bindings\": {}", random_bindings(rng)));
    }
    if rng.one_in(6) {
        fields.push("\"extra\": {\"x\": [1, 2.5, null]}".to_string());
    }
    // Trailing commas and comments are part of the format.
    format!("{{ {}, // section\n }}", fields.join(", /* field */ "))
}

#[test]
fn loading_never_panics_and_only_keeps_usable_bindings() {
    let mut rng = Rng(0x5EED_0003);
    let mut loaded = 0;
    for _ in 0..3_000 {
        let sections: Vec<String> = (0..rng.below(5))
            .map(|_| random_section(&mut rng))
            .collect();
        let mut text = format!("// keymap\n[{}]", sections.join(",\n"));
        if rng.one_in(10) {
            // Cut it off somewhere, on a character boundary.
            let cut = rng.below(text.chars().count() + 1);
            text = text.chars().take(cut).collect();
        }
        let mut keymap = Keymap::new();
        let warnings = keymap.load(&text, KeymapSource::User);
        for warning in &warnings {
            assert!(!warning.to_string().is_empty());
            assert!(warning
                .section
                .is_none_or(|section| section < sections.len()));
        }
        for binding in keymap.bindings() {
            assert!(!binding.keystrokes().is_empty());
            let spelled = binding.keystroke_text();
            assert_eq!(
                Keystroke::parse_sequence(&spelled).as_deref(),
                Ok(binding.keystrokes()),
                "{spelled:?}"
            );
            assert!(binding
                .actions()
                .iter()
                .all(|action| !action.name.is_empty()));
            assert_eq!(
                binding.action().is_none(),
                binding.is_null() || binding.unbinds().is_some()
            );
        }
        loaded += keymap.len();
        // Whatever loaded can be asked about without trouble.
        let stack = random_stack(&mut rng);
        for binding in keymap.bindings().iter().take(8) {
            let typed: Vec<KeyEvent> = binding
                .keystrokes()
                .iter()
                .map(Keystroke::to_event)
                .collect();
            let _ = keymap.bindings_for_input(&typed, &stack);
            let _ = keymap.pending_completions(&typed[..1], &stack);
            if let Some(action) = binding.action() {
                let _ =
                    keymap.bindings_for_action(&action.name, action.args.as_ref(), Some(&stack));
                let _ = keymap.bindings_for_action(&action.name, action.args.as_ref(), None);
            }
        }
    }
    assert!(loaded > 1_000, "only {loaded} bindings loaded");
}

/// The keys the matcher test types. Few enough that sequences often meet a binding.
const MODEL_KEYS: &[&str] = &["a", "b", "c", "cmd-k", "enter"];

const MODEL_CONTEXTS: &[&str] = &["", "", "A", "B", "A && x", "A || B", "!B", "A > B"];

/// A keymap of short overlapping sequences over [`MODEL_KEYS`].
fn random_model_keymap(rng: &mut Rng) -> Keymap {
    let mut keymap = Keymap::new();
    for (layer, source) in [KeymapSource::Default, KeymapSource::User]
        .into_iter()
        .enumerate()
    {
        let sections: Vec<String> = (0..1 + rng.below(4))
            .map(|section| {
                let bindings: Vec<String> = (0..1 + rng.below(5))
                    .map(|binding| {
                        let keys: Vec<&str> = (0..1 + rng.below(3))
                            .map(|_| rng.pick(MODEL_KEYS))
                            .collect();
                        let action = if rng.one_in(6) {
                            "null".to_string()
                        } else if rng.one_in(8) {
                            "[\"action::Sequence\", [\"S1\", \"S2\"]]".to_string()
                        } else {
                            format!("\"A{layer}_{section}_{binding}\"")
                        };
                        format!("{}: {action}", json_string(&keys.join(" ")))
                    })
                    .collect();
                format!(
                    "{{\"context\": {}, \"bindings\": {{{}}}}}",
                    json_string(rng.pick(MODEL_CONTEXTS)),
                    bindings.join(", ")
                )
            })
            .collect();
        let warnings = keymap.load(&format!("[{}]", sections.join(", ")), source);
        assert!(warnings.is_empty(), "{warnings:?}");
    }
    keymap
}

fn random_model_stack(rng: &mut Rng) -> Vec<KeyContext> {
    let all = [
        KeyContext::new("A"),
        KeyContext::new("A").add("x"),
        KeyContext::new("B"),
        KeyContext::new("C"),
    ];
    (0..rng.below(4))
        .map(|_| all[rng.below(all.len())].clone())
        .collect()
}

fn dispatch_step(bindings: &[&Binding], event: KeyEvent) -> Step {
    let candidates: Vec<Vec<Action>> = bindings
        .iter()
        .map(|binding| binding.actions().to_vec())
        .collect();
    Step::Dispatch { candidates, event }
}

/// gpui's `DispatchTree::replay_prefix`, written the way gpui writes it.
fn model_replay_prefix(
    keymap: &Keymap,
    mut input: Vec<KeyEvent>,
    contexts: &[KeyContext],
) -> (Vec<KeyEvent>, Vec<Step>) {
    let mut to_replay = Vec::new();
    for last in (0..input.len()).rev() {
        let (bindings, _) = keymap.bindings_for_input(&input[..=last], contexts);
        if !bindings.is_empty() {
            let event = input.drain(..=last).next_back().unwrap();
            to_replay.push(dispatch_step(&bindings, event));
            break;
        }
    }
    if to_replay.is_empty() {
        to_replay.push(Step::Input(input.remove(0)));
    }
    (input, to_replay)
}

/// What gpui's `DispatchTree::dispatch_key` and the window around it come to: the steps, the
/// keys left pending, and whether those keys are also a complete binding.
fn model_dispatch_key(
    keymap: &Keymap,
    mut input: Vec<KeyEvent>,
    event: &KeyEvent,
    contexts: &[KeyContext],
) -> (Vec<Step>, Vec<KeyEvent>, bool) {
    input.push(event.clone());
    let (bindings, pending) = keymap.bindings_for_input(&input, contexts);
    if pending {
        return (Vec::new(), input, !bindings.is_empty());
    } else if !bindings.is_empty() {
        return (
            vec![dispatch_step(&bindings, event.clone())],
            Vec::new(),
            false,
        );
    } else if input.len() == 1 {
        return (vec![Step::Input(event.clone())], Vec::new(), false);
    }
    input.pop();
    let (suffix, mut to_replay) = model_replay_prefix(keymap, input, contexts);
    let (steps, pending, has_binding) = model_dispatch_key(keymap, suffix, event, contexts);
    to_replay.extend(steps);
    (to_replay, pending, has_binding)
}

/// gpui's `DispatchTree::flush_dispatch`.
fn model_flush(keymap: &Keymap, input: Vec<KeyEvent>, contexts: &[KeyContext]) -> Vec<Step> {
    let (suffix, mut to_replay) = model_replay_prefix(keymap, input, contexts);
    if !suffix.is_empty() {
        to_replay.extend(model_flush(keymap, suffix, contexts));
    }
    to_replay
}

#[test]
fn the_matcher_agrees_with_gpuis_recursive_dispatch() {
    let mut rng = Rng(0x5EED_0004);
    let (mut dispatches, mut inputs, mut waits, mut timeouts) = (0, 0, 0, 0);
    for _ in 0..1_500 {
        let keymap = random_model_keymap(&mut rng);
        let mut contexts = random_model_stack(&mut rng);
        let mut matcher = Matcher::new();
        // The model's state: the keys held and whether they wait on a timer.
        let mut held: Vec<KeyEvent> = Vec::new();
        let mut timer = false;
        for _ in 0..40 {
            if rng.one_in(12) {
                contexts = random_model_stack(&mut rng);
            }
            if rng.one_in(10) {
                // The timeout fires, or the app flushes on its own account.
                let result = matcher.timeout(&keymap, &contexts);
                let expected = if held.is_empty() {
                    Vec::new()
                } else {
                    model_flush(&keymap, std::mem::take(&mut held), &contexts)
                };
                timer = false;
                assert_eq!(result.steps, expected);
                assert!(!result.pending && !matcher.has_pending());
                assert_eq!(result.timer, PendingTimer::Stop);
                timeouts += 1;
                continue;
            }
            if rng.one_in(10) {
                // Events the matcher does not take leave everything as it was.
                let event = if rng.one_in(2) {
                    KeyEvent::typed('a').with_kind(KeyKind::Release)
                } else {
                    KeyEvent::new(Key::Modifier(ModKey::LeftSuper), Mods::SUPER)
                };
                let result = matcher.push(&keymap, &event, &contexts, true);
                assert!(result.steps.is_empty());
                assert_eq!(result.pending, !held.is_empty());
                assert_eq!(result.timer, PendingTimer::Keep);
                assert_eq!(matcher.pending(), held);
                continue;
            }
            let event = Keystroke::parse(rng.pick(MODEL_KEYS)).unwrap().to_event();
            let accepts_text = !rng.one_in(3);
            let result = matcher.push(&keymap, &event, &contexts, accepts_text);

            let previous = std::mem::take(&mut held);
            let (steps, pending, has_binding) =
                model_dispatch_key(&keymap, previous, &event, &contexts);
            held = pending;
            let types_text = event.text.is_some() || event.key == Key::Enter;
            timer = !held.is_empty() && (timer || has_binding || (accepts_text && types_text));
            let expected_timer = if timer {
                PendingTimer::Start
            } else {
                PendingTimer::Stop
            };

            assert_eq!(result.steps, steps);
            assert_eq!(result.pending, !held.is_empty());
            assert_eq!(matcher.pending(), held);
            assert_eq!(matcher.has_pending(), !held.is_empty());
            assert_eq!(result.timer, expected_timer);
            assert_eq!(matcher.pending_display().is_empty(), held.is_empty());
            for step in &result.steps {
                match step {
                    Step::Dispatch { candidates, .. } => {
                        assert!(!candidates.is_empty());
                        dispatches += 1;
                    }
                    Step::Input(_) => inputs += 1,
                }
            }
            waits += usize::from(result.pending);
        }
        // Whatever is still held comes out, and then nothing is.
        assert!(!matcher.timeout(&keymap, &contexts).pending);
        assert!(!matcher.has_pending());
        assert!(matcher.timeout(&keymap, &contexts).steps.is_empty());
    }
    // The generated keymaps exercised every path, not just one of them.
    assert!(dispatches > 2_000, "{dispatches} dispatches");
    assert!(inputs > 2_000, "{inputs} inputs");
    assert!(waits > 2_000, "{waits} waits");
    assert!(timeouts > 2_000, "{timeouts} timeouts");
}

#[test]
fn every_key_pushed_comes_out_once_or_is_used_by_a_binding() {
    // Each key press gets its own text, so it can be followed through the matcher: it must
    // come back as input or as the key that completed a binding, in the order typed, or be
    // one of the earlier keys of a binding that ran.
    let mut rng = Rng(0x5EED_0005);
    for _ in 0..500 {
        let keymap = random_model_keymap(&mut rng);
        let contexts = random_model_stack(&mut rng);
        let mut matcher = Matcher::new();
        let mut pushed: Vec<String> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        let record = |steps: &[Step], seen: &mut Vec<String>| {
            for step in steps {
                let (Step::Dispatch { event, .. } | Step::Input(event)) = step;
                seen.push(event.text.clone().unwrap_or_default());
            }
        };
        for serial in 0..60 {
            if rng.one_in(9) {
                record(&matcher.timeout(&keymap, &contexts).steps, &mut seen);
                continue;
            }
            let tag = format!("#{serial}");
            let event = Keystroke::parse(rng.pick(MODEL_KEYS))
                .unwrap()
                .to_event()
                .with_text(tag.clone());
            pushed.push(tag);
            record(
                &matcher.push(&keymap, &event, &contexts, true).steps,
                &mut seen,
            );
        }
        record(&matcher.timeout(&keymap, &contexts).steps, &mut seen);
        // `seen` is `pushed` with some keys left out, never reordered or repeated.
        let mut rest = pushed.iter();
        for tag in &seen {
            assert!(
                rest.any(|typed| typed == tag),
                "{tag} out of order in {seen:?}"
            );
        }
    }
}

#[test]
fn the_types_can_cross_threads() {
    fn send_and_sync<T: Send + Sync>() {}
    send_and_sync::<Keymap>();
    send_and_sync::<Binding>();
    send_and_sync::<Matcher>();
    send_and_sync::<KeyContext>();
    send_and_sync::<ContextPredicate>();
    send_and_sync::<Keystroke>();
    send_and_sync::<Step>();
    // A keymap behind a shared reference can be read from another thread.
    let mut keymap = Keymap::new();
    assert!(keymap
        .load(
            r#"[{"bindings": {"cmd-s": "workspace::Save"}}]"#,
            KeymapSource::User
        )
        .is_empty());
    let keymap = std::sync::Arc::new(keymap);
    let shared = keymap.clone();
    let found = std::thread::spawn(move || {
        let event = KeyEvent::new(Key::Char('s'), Mods::CMD);
        shared.bindings_for_input(&[event], &[]).0.len()
    })
    .join();
    assert_eq!(found.ok(), Some(1));
}
