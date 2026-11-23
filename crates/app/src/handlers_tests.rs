//! Every action the built-in keymap binds, and every command the palette lists, is acted on
//! by something in the running app: a view, the pane, the workspace, a picker or a prompt.
//!
//! The test builds the app as it runs (the real shell, editor, panels, pickers and item
//! factory over a throwaway git project) in each place the keymap's contexts describe, and
//! dispatches every action there until one place takes it. An action nothing takes in any
//! place is a key that silently does nothing, and fails the test, unless it is listed in
//! [`DELIBERATE_NO_OPS`] with the reason.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bakedit_core::{Action, Theme};
use bakedit_keymap::{Keymap, KeymapSource};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{json, Value};

use crate::background::Waker;
use crate::clipboard::{Clipboard, MemoryBackend};
use crate::factory::{AppFactory, AppSource};
use crate::panels::git::GitPanel;
use crate::panels::project::ProjectPanel;
use crate::panels::terminal::{LaunchEnv, TerminalLaunch, TerminalPanel};
use crate::project::test_support::git;
use crate::project::Project;
use crate::settings::{Settings, TerminalCommand};
use crate::shell::{Modal, Shell};
use crate::workspace::{Focus, PanelId};

/// Actions that are bound on purpose and acted on by nothing, with the reason.
const DELIBERATE_NO_OPS: &[(&str, &str)] = &[];

/// The places the keymap's contexts describe, in the order they are tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// A Rust file in the centre, nav mode.
    Editor,
    /// The same in edit mode.
    EditMode,
    FindBar,
    /// Project search with the keyboard in the query.
    Search,
    /// Project search with the keyboard in the results.
    SearchResults,
    ProjectDiff,
    Terminal,
    Tree,
    /// The tree with a new file's name being typed.
    TreeEditing,
    GitList,
    CommitEditor,
    FileFinder,
    CommandPalette,
    TabSwitcher,
    /// A yes/no question (closing an unsaved file).
    Prompt,
    GoToLine,
}

const PLACES: [Place; 16] = [
    Place::Editor,
    Place::EditMode,
    Place::FindBar,
    Place::Search,
    Place::SearchResults,
    Place::ProjectDiff,
    Place::Terminal,
    Place::Tree,
    Place::TreeEditing,
    Place::GitList,
    Place::CommitEditor,
    Place::FileFinder,
    Place::CommandPalette,
    Place::TabSwitcher,
    Place::Prompt,
    Place::GoToLine,
];

/// The app on a throwaway project, with the keyboard in one place.
struct World {
    shell: Shell,
    _project: tempfile::TempDir,
    _elsewhere: tempfile::TempDir,
}

/// A repository with one commit, then one changed file and one new one.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "--quiet"]);
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {\n    let words = vec![\"one\", \"two\"];\n    println!(\"{}\", words.len());\n}\n").unwrap();
    std::fs::write(root.join("README.md"), "# Demo\n\nwords and more words\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "--quiet", "-m", "first"]);
    std::fs::write(root.join("src/main.rs"), "fn main() {\n    let words = vec![\"one\", \"two\", \"three\"];\n    println!(\"{}\", words.len());\n}\n").unwrap();
    std::fs::write(root.join("notes.txt"), "notes\n").unwrap();
    dir
}

/// Poll the project and tick the views until the frame shows `text` (or, with `None`, until
/// the git status is in), for at most five seconds.
fn settle(shell: &mut Shell, text: Option<&str>) {
    let started = Instant::now();
    loop {
        shell.poll_project();
        shell.tick();
        let done = match text {
            Some(text) => draw(shell).contains(text),
            None => shell.project.status().is_some(),
        };
        if done || started.elapsed() > Duration::from_secs(5) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Draw the whole window and return it as text.
fn draw(shell: &mut Shell) -> String {
    let mut buf = Buffer::empty(Rect::new(0, 0, 200, 50));
    shell.render(&mut buf);
    crate::dump::to_text(&buf)
}

fn act(shell: &mut Shell, name: &str) {
    shell.dispatch(&Action::new(name));
}

fn world(place: Place) -> World {
    let dir = project();
    let elsewhere = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let mut settings = Settings { vim_mode: true, usage_meter: false, ..Settings::default() };
    settings.autosave = crate::settings::Autosave::Off;
    let theme = Theme::builtin();
    let mut launch = TerminalLaunch::resolve_in(&TerminalCommand::Custom(vec!["/bin/sh".into()]), &settings, &theme, &root, &LaunchEnv::default());
    // Terminals added later run sh too, never the user's login shell and its dotfiles.
    launch.shell = launch.config.clone();
    launch.fallback = None;
    let mut project = Project::open(&root, Waker::detached());
    project.settle(Duration::from_secs(10));
    let mut shell = Shell::new(
        settings,
        theme,
        project,
        Clipboard::with_backend(Box::new(MemoryBackend::default())),
        Box::new(AppFactory),
        Box::new(AppSource::new(elsewhere.path().join("state"))),
    );
    shell.config_dir = elsewhere.path().join("config");
    shell.set_panel(PanelId::Terminal, Box::new(TerminalPanel::new(launch)));
    shell.build_panel(PanelId::Git, |cx| Box::new(GitPanel::new(cx)));
    shell.build_panel(PanelId::Project, |cx| Box::new(ProjectPanel::new(cx)));
    shell.workspace.open_panel(PanelId::Terminal);
    shell.workspace.open_panel(PanelId::Project);
    settle(&mut shell, None);
    let main = root.join("src/main.rs");
    match place {
        Place::Editor | Place::EditMode | Place::FindBar | Place::Prompt | Place::GoToLine => {
            shell.open_file(&main, Some(2), Some(5), false, true);
            match place {
                Place::EditMode => act(&mut shell, "vim::InsertBefore"),
                Place::FindBar => act(&mut shell, "buffer_search::Deploy"),
                Place::Prompt => {
                    act(&mut shell, "vim::InsertBefore");
                    shell.handle_paste("x");
                    act(&mut shell, "pane::CloseActiveItem");
                    assert!(matches!(shell.modal, Some(Modal::Prompt { .. })), "closing an unsaved file asks");
                }
                Place::GoToLine => act(&mut shell, "go_to_line::Toggle"),
                _ => {}
            }
        }
        Place::Search | Place::SearchResults => {
            act(&mut shell, "pane::DeploySearch");
            if place == Place::SearchResults {
                shell.dispatch(&Action::with_args("search::FocusSearch", json!({ "query": "words" })));
                settle(&mut shell, Some("results in 2 files"));
                act(&mut shell, "menu::Confirm");
            }
        }
        Place::ProjectDiff => {
            act(&mut shell, "git::Diff");
            settle(&mut shell, Some("three"));
        }
        Place::Terminal => {
            shell.set_focus(Focus::Panel(PanelId::Terminal));
            draw(&mut shell);
        }
        Place::Tree | Place::TreeEditing => {
            shell.set_focus(Focus::Panel(PanelId::Project));
            let readme = root.join("README.md").to_string_lossy().into_owned();
            shell.dispatch(&Action::with_args("project_panel::RevealPath", json!({ "path": readme })));
            if place == Place::TreeEditing {
                act(&mut shell, "project_panel::NewFile");
            }
        }
        Place::GitList | Place::CommitEditor => {
            act(&mut shell, "git_panel::ToggleFocus");
            settle(&mut shell, Some("main.rs"));
            if place == Place::CommitEditor {
                act(&mut shell, "git_panel::FocusEditor");
            }
        }
        Place::FileFinder => act(&mut shell, "file_finder::Toggle"),
        Place::CommandPalette => act(&mut shell, "command_palette::Toggle"),
        Place::TabSwitcher => {
            shell.open_file(&main, None, None, false, true);
            shell.open_file(&root.join("README.md"), None, None, false, true);
            act(&mut shell, "tab_switcher::Toggle");
            assert!(matches!(shell.modal, Some(Modal::Picker { .. })), "two tabs: the switcher opens");
        }
    }
    draw(&mut shell);
    World { shell, _project: dir, _elsewhere: elsewhere }
}

/// The action as it is tried: a binding that skips a question asks it instead, so that
/// nothing is thrown away while the test runs.
fn harmless(action: &Action) -> Action {
    let mut action = action.clone();
    if let Some(Value::Object(args)) = &mut action.args {
        if args.get("skip_prompt") == Some(&Value::Bool(true)) {
            args.insert("skip_prompt".into(), Value::Bool(false));
        }
    }
    action
}

/// Every action bound in the built-in keymap (sequences taken apart) and listed in the
/// command palette, once per name, with the arguments of the first binding of that name.
fn every_action() -> BTreeMap<String, Action> {
    let mut keymap = Keymap::new();
    assert!(keymap.load(crate::keys::DEFAULT_KEYMAP, KeymapSource::Default).is_empty());
    let mut actions = BTreeMap::new();
    for binding in keymap.bindings() {
        for action in binding.actions() {
            actions.entry(action.name.clone()).or_insert_with(|| action.clone());
        }
    }
    for command in crate::actions::palette() {
        actions.entry(command.action.name.clone()).or_insert(command.action);
    }
    actions
}

/// What says the keyboard is still where a place put it: the context stack, the modal, the
/// tabs and what is on screen around them.
fn fingerprint(shell: &Shell) -> String {
    format!(
        "{:?} {} {:?} {:?} {:?} {}",
        shell.contexts(),
        shell.pane.len(),
        shell.workspace.zoomed(),
        crate::workspace::DockPosition::ALL.map(|dock| shell.workspace.visible_panel(dock)),
        shell.workspace.focus(),
        shell.quit,
    )
}

/// The places to try an action in, the likeliest first.
fn places_for(name: &str) -> Vec<Place> {
    let first: &[Place] = match name.split("::").next().unwrap_or("") {
        "terminal" | "herdr" => &[Place::Terminal],
        "project_panel" => &[Place::Tree, Place::TreeEditing],
        "git" | "git_panel" => &[Place::GitList, Place::CommitEditor, Place::ProjectDiff],
        "project_diff" => &[Place::ProjectDiff],
        "project_search" => &[Place::SearchResults, Place::Search],
        "search" | "buffer_search" => &[Place::FindBar, Place::Search, Place::SearchResults],
        "picker" | "file_finder" => &[Place::FileFinder],
        "tab_switcher" => &[Place::TabSwitcher],
        "menu" => &[Place::FileFinder, Place::Prompt, Place::Tree, Place::GitList],
        _ => &[Place::Editor],
    };
    let mut places = first.to_vec();
    places.extend(PLACES.iter().copied().filter(|place| !first.contains(place)));
    // A question takes every key while it is up, so that nothing happens behind it: only
    // its own menu actions count as acting on something there.
    if !name.starts_with("menu::") {
        places.retain(|place| *place != Place::Prompt);
    }
    places
}

#[test]
fn every_bound_or_listed_action_is_acted_on_somewhere() {
    // One world per place, used until an action moves the keyboard out of it. An action a
    // used world turns down is tried again in a fresh one: what ran before may have taken
    // away what it acts on (a find query cleared, a tab closed).
    let mut worlds: Vec<(Place, World, String)> = Vec::new();
    let mut nobody: Vec<String> = Vec::new();
    for (name, action) in every_action() {
        if DELIBERATE_NO_OPS.iter().any(|(no_op, _)| *no_op == name) {
            continue;
        }
        let action = harmless(&action);
        let mut taken = false;
        'places: for place in places_for(&name) {
            loop {
                let (index, fresh) = match worlds.iter().position(|(p, _, _)| *p == place) {
                    Some(index) => (index, false),
                    None => {
                        let world = world(place);
                        let print = fingerprint(&world.shell);
                        worlds.push((place, world, print));
                        (worlds.len() - 1, true)
                    }
                };
                let (_, world, print) = &mut worlds[index];
                taken = world.shell.dispatch(&action);
                world.shell.tick();
                let moved = fingerprint(&world.shell) != *print;
                if moved || !(taken || fresh) {
                    worlds.remove(index);
                }
                if taken {
                    break 'places;
                }
                if fresh {
                    break;
                }
            }
        }
        if !taken {
            nobody.push(name);
        }
    }
    assert!(nobody.is_empty(), "no place acts on these actions: {nobody:#?}");
}

#[test]
fn the_deliberate_no_ops_are_still_bound() {
    let actions = every_action();
    for (name, why) in DELIBERATE_NO_OPS {
        assert!(actions.contains_key(*name), "{name} is listed as a deliberate no-op ({why}) but nothing binds it any more");
    }
}

#[test]
fn the_places_are_what_their_names_say() {
    let contexts = |place: Place| -> Vec<String> {
        let world = world(place);
        world.shell.contexts().iter().map(|c| c.name.to_string()).collect()
    };
    assert_eq!(contexts(Place::Editor).last().map(String::as_str), Some("Editor"));
    assert!(contexts(Place::FindBar).contains(&"BufferSearchBar".to_string()));
    assert!(contexts(Place::Search).contains(&"ProjectSearchView".to_string()));
    assert!(contexts(Place::ProjectDiff).contains(&"ProjectDiff".to_string()));
    assert_eq!(contexts(Place::Terminal).last().map(String::as_str), Some("Terminal"));
    assert!(contexts(Place::Tree).contains(&"ProjectPanel".to_string()));
    assert!(contexts(Place::GitList).contains(&"GitPanel".to_string()));
    assert!(contexts(Place::CommitEditor).contains(&"CommitEditor".to_string()));
    assert!(contexts(Place::FileFinder).contains(&"FileFinder".to_string()));
    assert!(contexts(Place::TabSwitcher).contains(&"TabSwitcher".to_string()));
    assert_eq!(contexts(Place::Prompt).last().map(String::as_str), Some("Prompt"));
    assert!(contexts(Place::GoToLine).contains(&"GoToLine".to_string()));
}

