//! The engine against real keymap files: the user's own Zed keymap, which is checked in as a
//! fixture, and Zed's default macOS and vim keymaps, which are not (licence) and are read
//! from the reference directory when it is there.
//!
//! The reference directory is `.reference/zed` at the root of the workspace, or wherever
//! `BAKEDIT_REFERENCE_DIR` points. Tests that need it pass without checking anything when it
//! is absent.

use std::path::PathBuf;

use bakedit_core::{Action, Key, KeyEvent, KeyKind, Mods};
use bakedit_keymap::{
    Binding, KeyContext, Keymap, KeymapSource, Keystroke, Matcher, PendingTimer, Step,
};
use serde_json::json;

const USER_KEYMAP: &str = include_str!("fixtures/user-keymap.json");

/// A file from the reference directory, or `None` when it is not on this machine.
fn reference(name: &str) -> Option<String> {
    let dir = match std::env::var_os("BAKEDIT_REFERENCE_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.reference/zed"),
    };
    std::fs::read_to_string(dir.join(name)).ok()
}

fn user_keymap() -> Keymap {
    let mut keymap = Keymap::new();
    let warnings = keymap.load(USER_KEYMAP, KeymapSource::User);
    assert!(warnings.is_empty(), "{warnings:#?}");
    keymap
}

/// Zed's default keymap with the user's loaded after it, the way the app loads them.
fn full_keymap() -> Option<Keymap> {
    let mut keymap = Keymap::new();
    let warnings = keymap.load(&reference("default-macos.json")?, KeymapSource::Default);
    assert!(warnings.is_empty(), "{warnings:#?}");
    let warnings = keymap.load(USER_KEYMAP, KeymapSource::User);
    assert!(warnings.is_empty(), "{warnings:#?}");
    Some(keymap)
}

fn workspace() -> KeyContext {
    KeyContext::new("Workspace")
}

fn editor_stack() -> Vec<KeyContext> {
    vec![
        workspace(),
        KeyContext::new("Pane"),
        KeyContext::new("Editor").set("mode", "full"),
    ]
}

fn terminal_stack() -> Vec<KeyContext> {
    vec![workspace(), KeyContext::new("Terminal")]
}

fn project_panel_stack() -> Vec<KeyContext> {
    vec![
        workspace(),
        KeyContext::new("Dock"),
        KeyContext::new("ProjectPanel").add("not_editing"),
    ]
}

fn key(text: &str) -> KeyEvent {
    Keystroke::parse(text).unwrap().to_event()
}

fn keys(text: &str) -> Vec<KeyEvent> {
    Keystroke::parse_sequence(text)
        .unwrap()
        .iter()
        .map(Keystroke::to_event)
        .collect()
}

/// What `input` resolves to: each candidate's action names joined with `+`, best first.
fn candidates(keymap: &Keymap, input: &str, contexts: &[KeyContext]) -> Vec<String> {
    let (bindings, _) = keymap.bindings_for_input(&keys(input), contexts);
    bindings.iter().map(|binding| names(binding)).collect()
}

fn names(binding: &Binding) -> String {
    let names: Vec<&str> = binding
        .actions()
        .iter()
        .map(|action| action.name.as_str())
        .collect();
    names.join("+")
}

fn first_binding<'a>(keymap: &'a Keymap, input: &str, contexts: &[KeyContext]) -> &'a Binding {
    let (bindings, _) = keymap.bindings_for_input(&keys(input), contexts);
    bindings
        .first()
        .copied()
        .unwrap_or_else(|| panic!("nothing is bound to {input}"))
}

/// The position of `name` among the candidates.
fn position(candidates: &[String], name: &str) -> usize {
    candidates
        .iter()
        .position(|candidate| candidate == name)
        .unwrap_or_else(|| panic!("{name} is not among {candidates:?}"))
}

#[test]
fn the_users_keymap_loads_without_warnings() {
    let keymap = user_keymap();
    assert_eq!(keymap.len(), 72);
    assert!(keymap
        .bindings()
        .iter()
        .all(|binding| binding.source() == KeymapSource::User && !binding.is_null()));
    // Every binding can be typed and its spelling parses back to the same keystrokes.
    for binding in keymap.bindings() {
        let text = binding.keystroke_text();
        assert_eq!(
            Keystroke::parse_sequence(&text).unwrap(),
            binding.keystrokes(),
            "{text}"
        );
        let typed: Vec<KeyEvent> = binding
            .keystrokes()
            .iter()
            .map(Keystroke::to_event)
            .collect();
        for (keystroke, event) in binding.keystrokes().iter().zip(&typed) {
            assert!(keystroke.matches(event), "{text}");
        }
    }
}

#[test]
fn zeds_default_keymap_loads_without_warnings() {
    let Some(text) = reference("default-macos.json") else {
        return;
    };
    let mut keymap = Keymap::new();
    let warnings = keymap.load(&text, KeymapSource::Default);
    assert!(warnings.is_empty(), "{warnings:#?}");
    assert!(keymap.len() > 900, "{} bindings", keymap.len());
    for binding in keymap.bindings() {
        let text = binding.keystroke_text();
        assert_eq!(
            Keystroke::parse_sequence(&text).unwrap(),
            binding.keystrokes(),
            "{text}"
        );
        // What a context prints as parses again.
        if let Some(context) = binding.context() {
            let printed = context.to_string();
            assert!(
                bakedit_keymap::ContextPredicate::parse(&printed).is_ok(),
                "{printed}"
            );
        }
    }
    // `fn-f` is in there. It loads, and no key press can type it.
    let globe: Vec<&Binding> = keymap
        .bindings()
        .iter()
        .filter(|binding| {
            binding
                .keystrokes()
                .iter()
                .any(|keystroke| keystroke.function)
        })
        .collect();
    assert!(!globe.is_empty());
    let stack = [workspace()];
    let (found, pending) = keymap.bindings_for_input(&[KeyEvent::typed('f')], &stack);
    assert!(found.is_empty() && !pending, "typing f must stay typing");
}

#[test]
fn zeds_vim_keymap_parses() {
    let Some(text) = reference("vim.json") else {
        return;
    };
    let mut keymap = Keymap::new();
    let warnings = keymap.load(&text, KeymapSource::Default);
    assert!(warnings.is_empty(), "{warnings:#?}");
    assert!(keymap.len() > 800, "{} bindings", keymap.len());
    // A few of its awkward entries resolve as written.
    let normal = [
        workspace(),
        KeyContext::new("Editor")
            .set("mode", "full")
            .add("VimControl")
            .set("vim_mode", "normal"),
    ];
    assert_eq!(
        candidates(&keymap, "g g", &normal)[0],
        "vim::StartOfDocument"
    );
    assert_eq!(candidates(&keymap, "G", &normal)[0], "vim::EndOfDocument");
    assert_eq!(
        candidates(&keymap, "shift-g", &normal)[0],
        "vim::EndOfDocument"
    );
    let (found, pending) = keymap.bindings_for_input(&keys("g"), &normal);
    assert!(found.is_empty() && pending);
    let operator = [
        workspace(),
        KeyContext::new("Editor")
            .set("vim_mode", "operator")
            .set("vim_operator", ">"),
    ];
    assert_eq!(candidates(&keymap, ">", &operator)[0], "vim::CurrentLine");
}

#[test]
fn cmd_g_in_an_editor_is_the_users_git_panel_toggle() {
    // The user's keymap alone: the `Editor || Pane` section is deeper than `Workspace`.
    let keymap = user_keymap();
    assert_eq!(
        candidates(&keymap, "cmd-g", &editor_stack()),
        ["git_panel::Toggle", "git_panel::Toggle"]
    );
    let Some(keymap) = full_keymap() else {
        return;
    };
    let found = candidates(&keymap, "cmd-g", &editor_stack());
    assert_eq!(found[0], "git_panel::Toggle");
    let first = first_binding(&keymap, "cmd-g", &editor_stack());
    assert_eq!(first.source(), KeymapSource::User);
    assert_eq!(
        first.context().map(ToString::to_string).as_deref(),
        Some("Editor || Pane")
    );
    // Zed's own binding for the key is still there to fall back on.
    assert!(position(&found, "search::SelectNextMatch") > 0);
}

#[test]
fn cmd_alt_t_in_an_editor_prefers_the_deeper_default_binding() {
    let Some(keymap) = full_keymap() else {
        return;
    };
    let found = candidates(&keymap, "cmd-alt-t", &editor_stack());
    // Zed binds it in `Pane`, the user in `Workspace`: depth beats load order.
    assert_eq!(found[0], "pane::CloseOtherItems");
    assert!(position(&found, "pane::CloseOtherItems") < position(&found, "workspace::ToggleZoom"));
    let first = first_binding(&keymap, "cmd-alt-t", &editor_stack());
    assert_eq!(first.source(), KeymapSource::Default);
    assert_eq!(first.actions()[0].arg_bool("close_pinned"), Some(false));
}

#[test]
fn cmd_alt_left_in_a_terminal_sends_the_herdr_prefix() {
    let expected = [Action::with_args("terminal::SendText", json!("\u{2}h"))];
    let keymap = user_keymap();
    assert_eq!(
        first_binding(&keymap, "cmd-alt-left", &terminal_stack()).actions(),
        expected
    );
    let Some(keymap) = full_keymap() else {
        return;
    };
    let first = first_binding(&keymap, "cmd-alt-left", &terminal_stack());
    assert_eq!(first.actions(), expected);
    assert_eq!(first.actions()[0].arg_str("text"), Some("\u{2}h"));
}

#[test]
fn cmd_shift_e_in_the_project_panel_closes_the_dock() {
    let keymap = user_keymap();
    assert_eq!(
        candidates(&keymap, "cmd-shift-e", &project_panel_stack())[0],
        "workspace::CloseActiveDock"
    );
    let Some(keymap) = full_keymap() else {
        return;
    };
    let found = candidates(&keymap, "cmd-shift-e", &project_panel_stack());
    assert_eq!(found[0], "workspace::CloseActiveDock");
    // Elsewhere the same key reveals or focuses the panel.
    assert_eq!(
        candidates(&keymap, "cmd-shift-e", &editor_stack())[0],
        "pane::RevealInProjectPanel"
    );
    assert_eq!(
        candidates(&keymap, "cmd-shift-e", &terminal_stack())[0],
        "project_panel::ToggleFocus+project_panel::ScrollCursorCenter"
    );
}

#[test]
fn enter_in_an_editor_tries_newline_before_menu_confirm() {
    let Some(keymap) = full_keymap() else {
        return;
    };
    let found = candidates(&keymap, "enter", &editor_stack());
    // `menu::Confirm` has no context, so it sits at the deepest level together with the
    // `Editor && mode == full` binding. At equal depth the binding loaded later wins, and
    // that is the editor's.
    assert_eq!(found, ["editor::Newline", "menu::Confirm"]);
    // In the project panel the user's binding is the latest at the deepest level.
    let found = candidates(&keymap, "enter", &project_panel_stack());
    assert_eq!(found[0], "project_panel::OpenPermanent");
    assert!(position(&found, "menu::Confirm") > 0);
}

#[test]
fn the_users_terminal_bindings_carry_their_bytes() {
    let keymap = user_keymap();
    for (keys, text) in [
        ("cmd-alt-left", "\u{2}h"),
        ("cmd-alt-right", "\u{2}l"),
        ("cmd-alt-up", "\u{2}k"),
        ("cmd-alt-down", "\u{2}j"),
        ("cmd-alt-shift-left", "\u{2}\u{8}"),
        ("cmd-alt-shift-down", "\u{2}\n"),
        ("ctrl-alt-cmd-up", "\u{2}u"),
        ("cmd-alt-1", "\u{2}!"),
        ("cmd-alt-2", "\u{2}@"),
        ("cmd-alt-9", "\u{2}("),
        ("cmd-alt-z", "\u{2}z"),
        ("shift-up", "\u{2}\u{15}"),
        ("cmd-shift-down", "\u{2}\u{5}"),
    ] {
        let binding = first_binding(&keymap, keys, &terminal_stack());
        assert_eq!(names(binding), "terminal::SendText", "{keys}");
        assert_eq!(binding.actions()[0].arg_str("text"), Some(text), "{keys}");
    }
    // Outside the terminal those keys are not bound by the user.
    assert!(candidates(&keymap, "cmd-alt-left", &editor_stack()).is_empty());
    assert!(candidates(&keymap, "shift-up", &editor_stack()).is_empty());
    // The sequences keep their order.
    assert_eq!(
        candidates(&keymap, "cmd-alt-g", &terminal_stack())[0],
        "git::Diff+workspace::ToggleZoom"
    );
    let git_changes = [
        workspace(),
        KeyContext::new("Dock"),
        KeyContext::new("GitPanel").add("ChangesList"),
    ];
    assert_eq!(
        candidates(&keymap, "down", &git_changes),
        ["menu::SelectNext+menu::Confirm+pane::RevealInProjectPanel+git_panel::FocusChanges"]
    );
}

#[test]
fn the_users_delimiter_chords_work_on_us_and_jis_keyboards() {
    let keymap = user_keymap();
    let contexts = editor_stack();
    let cmd_k = KeyEvent::new(Key::Char('k'), Mods::CMD);
    let task = |second: KeyEvent| -> String {
        let (bindings, pending) = keymap.bindings_for_input(&[cmd_k.clone(), second], &contexts);
        assert!(!pending);
        assert_eq!(bindings.len(), 1);
        assert_eq!(names(bindings[0]), "task::Spawn");
        bindings[0].actions()[0]
            .arg_str("task_name")
            .unwrap()
            .to_string()
    };
    let shifted = |key: char, produces: char| {
        KeyEvent::new(Key::Char(key), Mods::SHIFT)
            .with_shifted(produces)
            .with_text(produces.to_string())
    };
    // A US layout.
    for (typed, name) in [
        ('(', "select-between-paren"),
        (')', "select-between-paren"),
        ('[', "select-between-bracket"),
        (']', "select-between-bracket"),
        ('{', "select-between-brace"),
        ('}', "select-between-brace"),
        ('"', "select-between-dquote"),
        ('\'', "select-between-squote"),
        ('`', "select-between-backtick"),
        ('<', "select-between-angle"),
        ('>', "select-between-angle"),
    ] {
        assert_eq!(task(KeyEvent::typed(typed)), name, "{typed}");
    }
    // A JIS layout, where the terminal reports what shift really produces.
    for (key, produces, name) in [
        ('8', '(', "select-between-paren"),
        ('9', ')', "select-between-paren"),
        ('[', '{', "select-between-brace"),
        (']', '}', "select-between-brace"),
        ('2', '"', "select-between-dquote"),
        ('7', '\'', "select-between-squote"),
        ('@', '`', "select-between-backtick"),
        (',', '<', "select-between-angle"),
        ('.', '>', "select-between-angle"),
    ] {
        assert_eq!(task(shifted(key, produces)), name, "shift+{key}");
    }
    assert_eq!(task(KeyEvent::typed('[')), "select-between-bracket");
    // JIS shift+8 is a parenthesis and not the asterisk a US layout has there.
    let (bindings, _) =
        keymap.bindings_for_input(&[cmd_k.clone(), KeyEvent::typed('*')], &contexts);
    assert!(bindings.is_empty());
}

#[test]
fn a_chord_from_the_users_keymap_through_the_matcher() {
    let keymap = user_keymap();
    let contexts = editor_stack();
    let mut matcher = Matcher::new();

    let result = matcher.push(&keymap, &key("cmd-k"), &contexts, true);
    assert!(result.steps.is_empty());
    assert!(result.pending);
    // Nothing is bound to cmd-k alone and it types nothing, so it waits without a timer.
    assert_eq!(result.timer, PendingTimer::Stop);
    assert_eq!(matcher.pending_display(), "cmd-k");

    // Shift going down and coming up between the two keys changes nothing.
    let shift = KeyEvent::new(Key::Modifier(bakedit_core::ModKey::LeftShift), Mods::SHIFT);
    for event in [shift.clone(), shift.with_kind(KeyKind::Release)] {
        let result = matcher.push(&keymap, &event, &contexts, true);
        assert!(result.steps.is_empty() && result.pending);
        assert_eq!(result.timer, PendingTimer::Keep);
    }

    let brace = KeyEvent::typed('{');
    let result = matcher.push(&keymap, &brace, &contexts, true);
    assert!(!result.pending);
    assert_eq!(
        result.steps,
        [Step::Dispatch {
            candidates: vec![vec![Action::with_args(
                "task::Spawn",
                json!({"task_name": "select-between-brace"})
            )]],
            event: brace,
        }]
    );

    // cmd-k then a key the chord does not know: both come back as input.
    matcher.push(&keymap, &key("cmd-k"), &contexts, true);
    let result = matcher.push(&keymap, &key("x"), &contexts, true);
    assert_eq!(
        result.steps,
        [Step::Input(key("cmd-k")), Step::Input(key("x"))]
    );

    // In the project panel `cmd-k 0` and `cmd-k cmd-0` collapse the tree.
    for second in ["0", "cmd-0"] {
        let contexts = project_panel_stack();
        assert!(
            matcher
                .push(&keymap, &key("cmd-k"), &contexts, false)
                .pending
        );
        let result = matcher.push(&keymap, &key(second), &contexts, false);
        let [Step::Dispatch { candidates, .. }] = result.steps.as_slice() else {
            panic!("a dispatch expected for cmd-k {second}");
        };
        assert_eq!(
            candidates[0],
            [Action::new("project_panel::CollapseAllEntries")]
        );
    }
}

#[test]
fn both_keymaps_through_the_matcher() {
    let Some(keymap) = full_keymap() else {
        return;
    };
    let contexts = editor_stack();
    let mut matcher = Matcher::new();
    let actions = |result: &bakedit_keymap::PushResult| -> Vec<String> {
        let [Step::Dispatch { candidates, .. }] = result.steps.as_slice() else {
            panic!("one dispatch expected, got {:?}", result.steps);
        };
        candidates
            .iter()
            .map(|actions| {
                let names: Vec<&str> = actions.iter().map(|a| a.name.as_str()).collect();
                names.join("+")
            })
            .collect()
    };

    // Zed's `cmd-k cmd-s` opens the keymap.
    assert!(
        matcher
            .push(&keymap, &key("cmd-k"), &contexts, true)
            .pending
    );
    let result = matcher.push(&keymap, &key("cmd-s"), &contexts, true);
    assert_eq!(actions(&result)[0], "zed::OpenKeymap");

    // A plain letter is input, and nothing waits on it.
    let result = matcher.push(&keymap, &KeyEvent::typed('a'), &contexts, true);
    assert_eq!(result.steps, [Step::Input(KeyEvent::typed('a'))]);

    // Keys Zed binds in an editor and the user leaves alone.
    let result = matcher.push(&keymap, &key("cmd-s"), &contexts, true);
    assert_eq!(actions(&result)[0], "workspace::Save");
    let result = matcher.push(&keymap, &key("cmd-{"), &contexts, true);
    assert_eq!(actions(&result)[0], "pane::ActivatePreviousItem");
    // The user's override of cmd-up, with Zed's binding behind it.
    let result = matcher.push(&keymap, &key("cmd-up"), &contexts, true);
    let found = actions(&result);
    assert_eq!(found[0], "editor::MoveUpByLines");
    assert!(position(&found, "editor::MoveToBeginning") > 0);

    // In the terminal Zed binds cmd-k to clear, so the chord never starts there.
    let result = matcher.push(&keymap, &key("cmd-k"), &terminal_stack(), true);
    assert!(!result.pending);
    assert_eq!(actions(&result)[0], "terminal::Clear");
    // `ctrl-_` is unbound there so the shell gets it: no candidates at all.
    let result = matcher.push(&keymap, &key("ctrl-_"), &terminal_stack(), true);
    assert_eq!(result.steps, [Step::Input(key("ctrl-_"))]);
    let result = matcher.push(&keymap, &key("ctrl-_"), &editor_stack(), true);
    assert_eq!(actions(&result)[0], "pane::GoForward");
}

#[test]
fn shortcuts_for_the_command_palette() {
    let keymap = user_keymap();
    let shortcut = |keymap: &Keymap, action: &str, contexts: &[KeyContext]| -> Vec<String> {
        keymap
            .bindings_for_action(action, None, Some(contexts))
            .iter()
            .map(|binding| binding.display_mac())
            .collect()
    };
    // The user binds it in three sections. All three are listed wherever cmd-g runs the
    // action, also the ones whose own context is not active there: the question gpui asks
    // is what the binding's keys do, not which section they came from.
    assert_eq!(
        shortcut(&keymap, "git_panel::Toggle", &editor_stack()),
        ["⌘G", "⌘G", "⌘G"]
    );
    assert_eq!(
        shortcut(&keymap, "git_panel::Toggle", &terminal_stack()),
        ["⌘G", "⌘G", "⌘G"]
    );
    assert_eq!(shortcut(&keymap, "git_panel::Toggle", &[]), [] as [&str; 0]);
    assert_eq!(
        shortcut(
            &keymap,
            "workspace::CloseActiveDock",
            &project_panel_stack()
        ),
        ["⇧⌘E"]
    );
    assert_eq!(
        shortcut(&keymap, "workspace::CloseActiveDock", &editor_stack()),
        [] as [&str; 0]
    );
    assert_eq!(
        shortcut(
            &keymap,
            "project_panel::CollapseAllEntries",
            &project_panel_stack()
        ),
        ["⌘K ⌘0", "⌘K 0"]
    );
    let with_args = keymap.bindings_for_action(
        "editor::MoveUpByLines",
        Some(&json!({"lines": 5})),
        Some(&editor_stack()),
    );
    assert_eq!(with_args.len(), 1);
    assert_eq!(with_args[0].display_mac(), "⌘↑");

    let Some(keymap) = full_keymap() else {
        return;
    };
    // In an editor the user's cmd-alt-t is shadowed by Zed's deeper `Pane` binding, so it is
    // not offered as the shortcut for the zoom there; in the terminal it is.
    let in_editor = shortcut(&keymap, "workspace::ToggleZoom", &editor_stack());
    assert!(!in_editor.contains(&"⌥⌘T".to_string()), "{in_editor:?}");
    assert!(in_editor.contains(&"⇧⎋".to_string()), "{in_editor:?}");
    let in_terminal = shortcut(&keymap, "workspace::ToggleZoom", &terminal_stack());
    assert_eq!(in_terminal[..2], ["⌥⌘F", "⌥⌘T"]);
    // Zed's fullscreen toggle has `fn-f` and `ctrl-cmd-f`; only the second can be typed here.
    assert_eq!(
        shortcut(&keymap, "zed::ToggleFullScreen", &editor_stack()),
        ["⌃⌘F"]
    );
    // The user rebinds cmd-g, so Zed's "find next" loses that shortcut in an editor.
    let find_next = shortcut(&keymap, "search::SelectNextMatch", &editor_stack());
    assert!(!find_next.contains(&"⌘G".to_string()), "{find_next:?}");
}

#[test]
fn which_key_for_a_pending_cmd_k() {
    let keymap = user_keymap();
    let rows = |keymap: &Keymap, contexts: &[KeyContext]| -> Vec<(String, String)> {
        keymap
            .pending_completions(&[key("cmd-k")], contexts)
            .into_iter()
            .map(|(keys, action)| (keys, action.name))
            .collect()
    };
    let in_editor = rows(&keymap, &editor_stack());
    let remaining: Vec<&str> = in_editor.iter().map(|(keys, _)| keys.as_str()).collect();
    // Last loaded first, which is the reverse of the order in the file.
    assert_eq!(
        remaining,
        [">", "<", "`", "'", "\"", "}", "{", "]", "[", ")", "("]
    );
    assert!(in_editor.iter().all(|(_, action)| action == "task::Spawn"));
    assert_eq!(
        rows(&keymap, &project_panel_stack()),
        [
            (
                "cmd-0".to_string(),
                "project_panel::CollapseAllEntries".to_string()
            ),
            (
                "0".to_string(),
                "project_panel::CollapseAllEntries".to_string()
            ),
        ]
    );
    assert!(rows(&keymap, &terminal_stack()).is_empty());

    let Some(keymap) = full_keymap() else {
        return;
    };
    let in_editor = rows(&keymap, &editor_stack());
    // The user's chords are the deepest and latest, so they lead; Zed's follow.
    assert_eq!(in_editor[0], (">".to_string(), "task::Spawn".to_string()));
    assert!(in_editor.len() > 11, "{} rows", in_editor.len());
    assert!(in_editor.contains(&("cmd-s".to_string(), "zed::OpenKeymap".to_string())));
    assert!(in_editor.contains(&("z".to_string(), "editor::ToggleSoftWrap".to_string())));
}
