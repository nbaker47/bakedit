//! The built-in keymap resolved by the real engine against the shell: the user's key
//! presses, not just action names.

use std::path::Path;

use bakedit_core::{Key, KeyEvent, Mods};
use bakedit_keymap::{Keymap, KeymapSource};

use crate::keys::{KeyRouter, DEFAULT_KEYMAP};
use crate::shell::{Modal, Shell};
use crate::shell_tests::{fake, fake_mut, shell};
use crate::workspace::{DockPosition, Focus, PanelId, Zoomed};

const CMD: Mods = Mods::SUPER;

fn press(router: &mut KeyRouter, shell: &mut Shell, key: Key, mods: Mods) {
    router.key(shell, &KeyEvent::new(key, mods));
}

fn ch(router: &mut KeyRouter, shell: &mut Shell, c: char, mods: Mods) {
    press(router, shell, Key::Char(c), mods);
}

fn setup() -> (KeyRouter, Shell) {
    let (router, warnings) = KeyRouter::new(None);
    assert!(warnings.is_empty());
    (router, shell().0)
}

#[test]
fn the_built_in_keymap_loads_without_warnings() {
    let mut keymap = Keymap::new();
    let warnings = keymap.load(DEFAULT_KEYMAP, KeymapSource::Default);
    assert!(warnings.is_empty(), "{}", warnings.iter().map(|w| w.to_string()).collect::<Vec<_>>().join("\n"));
    assert!(keymap.len() > 500);
}

#[test]
fn panel_keys_from_the_editor() {
    let (mut router, mut shell) = setup();
    shell.open_file(Path::new("/project/a.rs"), None, None, false, true);
    ch(&mut router, &mut shell, 't', CMD);
    assert_eq!(shell.workspace.focus(), Focus::Panel(PanelId::Terminal));
    ch(&mut router, &mut shell, 'e', CMD);
    assert_eq!(shell.workspace.focus(), Focus::Center);
    ch(&mut router, &mut shell, 'g', CMD);
    assert_eq!(shell.workspace.focus(), Focus::Panel(PanelId::Git));
    assert_eq!(shell.workspace.visible_panel(DockPosition::Left), Some(PanelId::Git));
    ch(&mut router, &mut shell, 'g', CMD);
    assert_eq!(shell.workspace.visible_panel(DockPosition::Left), None, "cmd-g again closes it");
    // cmd-alt-t: the terminal, fullscreen, from anywhere.
    ch(&mut router, &mut shell, 't', CMD | Mods::ALT);
    assert_eq!(shell.workspace.zoomed(), Some(Zoomed::Panel(PanelId::Terminal)));
    // In the terminal cmd-alt-f un-zooms it; cmd-alt-e goes to the editor with docks hidden.
    ch(&mut router, &mut shell, 'f', CMD | Mods::ALT);
    assert_eq!(shell.workspace.zoomed(), None);
    ch(&mut router, &mut shell, 'e', CMD | Mods::ALT);
    assert_eq!(shell.workspace.focus(), Focus::Center);
    assert_eq!(shell.workspace.visible_panel(DockPosition::Left), None);
}

#[test]
fn cmd_shift_e_reveals_then_closes() {
    let (mut router, mut shell) = setup();
    shell.open_file(Path::new("/project/a.rs"), None, None, false, true);
    // From the editor: pane::RevealInProjectPanel.
    ch(&mut router, &mut shell, 'e', CMD | Mods::SHIFT);
    assert_eq!(shell.workspace.focus(), Focus::Panel(PanelId::Project));
    // In the tree: workspace::CloseActiveDock.
    ch(&mut router, &mut shell, 'e', CMD | Mods::SHIFT);
    assert_eq!(shell.workspace.focus(), Focus::Center);
    assert_eq!(shell.workspace.visible_panel(DockPosition::Right), None);
}

#[test]
fn cmd_alt_g_opens_the_diff_fullscreen() {
    let (mut router, mut shell) = setup();
    ch(&mut router, &mut shell, 't', CMD);
    ch(&mut router, &mut shell, 'g', CMD | Mods::ALT);
    assert_eq!(shell.workspace.focus(), Focus::Center);
    assert_eq!(shell.workspace.zoomed(), Some(Zoomed::Center));
    assert_eq!(fake(&shell, 0).kind, "project_diff");
    ch(&mut router, &mut shell, 'g', CMD | Mods::ALT);
    assert_eq!(shell.workspace.zoomed(), None, "again un-zooms it");
}

#[test]
fn chords_wait_then_complete_or_time_out() {
    let (mut router, mut shell) = setup();
    shell.open_file(Path::new("/project/a.rs"), None, None, false, true);
    fake_mut(&mut shell, 0).handles = vec!["editor::SelectBetween"];
    fake_mut(&mut shell, 0).context = "Editor";
    ch(&mut router, &mut shell, 'k', CMD);
    assert_eq!(shell.pending_keys.as_deref(), Some("cmd-k"));
    router.show_hints(&mut shell);
    assert!(shell.which_key.iter().any(|(keys, title)| keys == "cmd-s" && title == "open keymap"), "{:?}", shell.which_key);
    // cmd-k ( is the user's "select inside parentheses".
    router.key(&mut shell, &KeyEvent::typed('('));
    assert_eq!(shell.pending_keys, None);
    assert!(shell.which_key.is_empty());
    assert!(fake(&shell, 0).seen.contains(&"editor::SelectBetween".to_string()));
    // A chord that is abandoned gives up quietly.
    ch(&mut router, &mut shell, 'k', CMD);
    router.chord_timeout(&mut shell);
    assert_eq!(shell.pending_keys, None);
}

#[test]
fn typing_reaches_the_view_and_menu_keys_fall_through() {
    let (mut router, mut shell) = setup();
    shell.open_file(Path::new("/project/a.rs"), None, None, false, true);
    router.key(&mut shell, &KeyEvent::typed('x'));
    assert!(fake(&shell, 0).seen.contains(&"typed x".to_string()));
    // enter is menu::Confirm first; with no menu open the editor's own binding is offered.
    press(&mut router, &mut shell, Key::Enter, Mods::empty());
    let seen = &fake(&shell, 0).seen;
    assert!(seen.contains(&"menu::Confirm".to_string()));
    // Releases and bare modifiers are not keys to act on.
    let release = KeyEvent::new(Key::Char('t'), CMD).with_kind(bakedit_core::KeyKind::Release);
    assert!(!router.key(&mut shell, &release));
    assert_eq!(shell.workspace.focus(), Focus::Center);
}

#[test]
fn pickers_take_menu_keys_and_text() {
    let (mut router, mut shell) = setup();
    ch(&mut router, &mut shell, 'p', CMD);
    assert!(matches!(shell.modal, Some(Modal::Picker { .. })));
    for c in "lib".chars() {
        router.key(&mut shell, &KeyEvent::typed(c));
    }
    press(&mut router, &mut shell, Key::Down, Mods::empty());
    press(&mut router, &mut shell, Key::Up, Mods::empty());
    press(&mut router, &mut shell, Key::Enter, Mods::empty());
    assert!(shell.modal.is_none());
    assert_eq!(fake(&shell, 0).name, "lib.rs");
    // cmd-shift-p opens the palette, escape closes it.
    ch(&mut router, &mut shell, 'p', CMD | Mods::SHIFT);
    assert!(matches!(shell.modal, Some(Modal::Picker { .. })));
    press(&mut router, &mut shell, Key::Escape, Mods::empty());
    assert!(shell.modal.is_none());
}

#[test]
fn the_fallback_prefix_works_without_cmd() {
    let (mut router, mut shell) = setup();
    ch(&mut router, &mut shell, ']', Mods::CTRL);
    assert_eq!(shell.pending_keys.as_deref(), Some("ctrl-]"));
    router.key(&mut shell, &KeyEvent::typed('t'));
    assert_eq!(shell.workspace.focus(), Focus::Panel(PanelId::Terminal));
    // From inside the terminal too.
    ch(&mut router, &mut shell, ']', Mods::CTRL);
    router.key(&mut shell, &KeyEvent::typed('c'));
    assert_eq!(shell.workspace.focus(), Focus::Center);
}

#[test]
fn command_hints_show_the_bound_keys() {
    let (router, shell) = setup();
    let hint = router.hint_for(&bakedit_core::Action::new("file_finder::Toggle"), &shell.contexts());
    assert_eq!(hint.as_deref(), Some("⌘P"));
    assert_eq!(router.hint_for(&bakedit_core::Action::new("no::SuchAction"), &shell.contexts()), None);
}

#[test]
fn the_key_bindings_page_lists_every_binding_by_context() {
    let (router, _) = KeyRouter::new(Some(r#"[{ "context": "Workspace", "bindings": { "cmd-y": "command_palette::Toggle", "cmd-p": null } }]"#));
    let page = router.listing();
    assert!(page.starts_with("Key bindings: the built-in keymap, then ~/.config/bakedit/keymap.json."));
    assert!(page.contains("\nWorkspace\n"), "a heading per context");
    assert!(page.contains("\nEverywhere\n"), "bindings without a context");
    assert!(page.lines().any(|l| l.trim_start().starts_with("cmd-p") && l.ends_with("file_finder::Toggle")), "built-in bindings");
    assert!(page.lines().any(|l| l.contains("cmd-y") && l.contains("command_palette::Toggle") && l.ends_with("· keymap.json")), "the user's are marked");
    assert!(page.lines().any(|l| l.contains("cmd-p") && l.contains("(switched off)")), "a null binding says so");
    assert!(page.contains(r#"editor::SelectBetween {"kind":"paren"}"#), "arguments are shown");
}
