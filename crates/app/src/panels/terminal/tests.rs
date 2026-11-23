use std::time::{Duration, Instant};

use bakedit_core::{Action, Key, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind, Theme};

use super::*;
use crate::settings::TerminalCommand;
use crate::view::test_support::Harness;
use bakedit_term::TermColors;

const WAIT: Duration = Duration::from_secs(15);

/// A panel running `script` with `sh -c` in `dir`. Terminals added to it run plain `sh`:
/// a test never starts the user's login shell, which would read their dotfiles.
fn panel(dir: &Path, script: &str) -> TerminalPanel {
    TerminalPanel::new(launch(dir, script))
}

fn launch(dir: &Path, script: &str) -> TerminalLaunch {
    let sh = TermConfig { program: Some("/bin/sh".into()), cwd: Some(dir.to_path_buf()), ..TermConfig::shell() };
    let config = TermConfig { args: vec!["-c".into(), script.into()], ..sh.clone() };
    TerminalLaunch { config, fallback: None, herdr: None, notice: None, program: "sh".into(), shell: sh, shell_name: "sh".into() }
}

/// Tick until `done` says so.
fn until(panel: &mut TerminalPanel, harness: &mut Harness, mut done: impl FnMut(&TerminalPanel) -> bool) {
    let started = Instant::now();
    while !done(panel) {
        panel.tick(&mut harness.cx());
        assert!(started.elapsed() < WAIT, "timed out; the screen was:\n{}", panel.screen_text());
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_program_starts_at_the_panel_size_and_its_output_is_drawn() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = Harness::new(dir.path());
    let mut panel = panel(dir.path(), "stty size; printf 'hello from sh'; sleep 5");
    assert!(!panel.is_running(), "nothing runs before the first draw");
    harness.draw(&mut panel, 50, 9);
    until(&mut panel, &mut harness, |p| p.screen_text().contains("hello from sh"));
    assert!(panel.screen_text().starts_with("8 50"), "{}", panel.screen_text());
    assert!(panel.is_running());
    panel.tick(&mut harness.cx());
    let (text, cursor, buf) = harness.draw(&mut panel, 50, 9);
    assert!(text.contains("hello from sh"), "{text}");
    assert_eq!(cursor.map(|c| (c.col, c.row)), Some((13, 2)), "the cursor follows the output below the header");
    assert_eq!(buf[(0, 8)].bg, ui::color(harness.theme.terminal_bg), "the theme's terminal background");
    assert!(!panel.tick(&mut harness.cx()), "nothing new since the draw");
}

#[test]
fn keys_and_pastes_reach_the_program() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = Harness::new(dir.path());
    // `cat` with the terminal's own echo: typed text comes back on the screen.
    let mut panel = panel(dir.path(), "cat");
    harness.draw(&mut panel, 40, 6);
    for c in "hi there".chars() {
        assert!(panel.handle_key(&KeyEvent::typed(c), &mut harness.cx()));
    }
    panel.handle_key(&KeyEvent::new(Key::Enter, Mods::empty()), &mut harness.cx());
    // The terminal echoes the line, then cat writes it back.
    until(&mut panel, &mut harness, |p| p.screen_text().matches("hi there").count() == 2);
    assert!(panel.handle_paste("pasted", &mut harness.cx()));
    until(&mut panel, &mut harness, |p| p.screen_text().contains("pasted"));
    // Text that belongs to no key (an input method) is typed as it is.
    let composed = KeyEvent::new(Key::Other(0), Mods::empty()).with_text("é");
    assert!(panel.handle_key(&composed, &mut harness.cx()));
    until(&mut panel, &mut harness, |p| p.screen_text().contains("pastedé"));
}

#[test]
fn cmd_click_on_a_printed_path_opens_it_at_the_line() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    let mut harness = Harness::new(dir.path());
    let mut panel = panel(dir.path(), "printf 'error at src/main.rs:12:5 here'; sleep 5");
    harness.draw(&mut panel, 60, 7);
    until(&mut panel, &mut harness, |p| p.screen_text().contains("here"));
    let area = Rect::new(0, 0, 60, 7);
    // A plain click selects; it opens nothing.
    let plain = MouseEvent::new(MouseKind::Press(MouseButton::Left), 12, 1);
    panel.handle_mouse(&plain, area, &mut harness.cx());
    assert!(harness.take_effects().is_empty());
    // With cmd held (the app adds it to the event from the keys it saw) it opens the file.
    let mut click = MouseEvent::new(MouseKind::Press(MouseButton::Left), 12, 1);
    click.mods = Mods::SUPER;
    assert!(panel.handle_mouse(&click, area, &mut harness.cx()));
    let expected = Effect::OpenFile { path: dir.path().join("src/main.rs"), line: Some(12), column: Some(5), preview: false, focus: true };
    assert_eq!(harness.take_effects(), vec![expected]);
    // Its release is swallowed with it.
    let release = MouseEvent::new(MouseKind::Release(MouseButton::Left), 12, 1);
    assert!(panel.handle_mouse(&release, area, &mut harness.cx()));
    // The link picker offers the same path, newest first, with where it resolved.
    let rows = panel.picker_rows(PickerKind::Links, &mut harness.cx()).unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].label, "src/main.rs:12:5");
    assert_eq!(rows[0].detail.as_deref(), Some("src/main.rs"));
    assert!(rows[0].icon.is_some(), "a file row has its icon");
    panel.picker_confirm(PickerKind::Links, 0, false, &mut harness.cx());
    assert!(matches!(&harness.take_effects()[..], [Effect::OpenFile { line: Some(12), .. }]));
    assert!(panel.picker_rows(PickerKind::Outline, &mut harness.cx()).is_none());
}

#[test]
fn a_path_that_does_not_exist_is_not_offered() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = Harness::new(dir.path());
    let mut panel = panel(dir.path(), "printf 'see nowhere/missing.rs:3 and https://example.com/x'; sleep 5");
    harness.draw(&mut panel, 80, 4);
    until(&mut panel, &mut harness, |p| p.screen_text().contains("example.com"));
    let rows = panel.picker_rows(PickerKind::Links, &mut harness.cx()).unwrap();
    let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(labels, ["https://example.com/x"]);
    panel.picker_confirm(PickerKind::Links, 0, false, &mut harness.cx());
    assert_eq!(harness.take_effects(), vec![Effect::OpenExternally("https://example.com/x".into())]);
}

#[test]
fn an_exited_program_says_so_and_enter_starts_it_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = Harness::new(dir.path());
    let mut panel = panel(dir.path(), "printf 'bye'; exit 3");
    harness.draw(&mut panel, 50, 6);
    until(&mut panel, &mut harness, |p| !p.is_running());
    let (text, cursor, _) = harness.draw(&mut panel, 50, 6);
    assert!(text.lines().nth(5).unwrap().contains("sh exited with code 3  ·  enter starts it again"), "{text}");
    assert_eq!(cursor, None);
    // Enter arrives as menu::Confirm first; the panel takes it to restart.
    assert!(panel.handle_action(&Action::new("menu::Confirm"), &mut harness.cx()));
    harness.draw(&mut panel, 50, 6);
    until(&mut panel, &mut harness, |p| !p.is_running() && p.screen_text().contains("bye"));
}

#[test]
fn menu_keys_and_unknown_actions_fall_through_to_the_program() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = Harness::new(dir.path());
    let mut panel = panel(dir.path(), "sleep 5");
    harness.draw(&mut panel, 30, 4);
    for name in ["menu::Confirm", "menu::Cancel", "menu::SelectNext", "editor::MoveUp", "herdr::Bogus"] {
        assert!(!panel.handle_action(&Action::new(name), &mut harness.cx()), "{name} is not the terminal's while sh runs");
    }
    assert!(panel.handle_action(&Action::new("terminal::Copy"), &mut harness.cx()), "nothing selected: cmd-c is still the terminal's");
    assert!(harness.clipboard.get().is_none(), "and copies nothing");
    let mut contexts = Vec::new();
    panel.contexts(&mut contexts);
    assert_eq!(contexts.iter().map(|c| c.name).collect::<Vec<_>>(), ["Pane", "Terminal"]);
}

#[test]
fn a_program_that_cannot_start_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = Harness::new(dir.path());
    let config = TermConfig { program: Some("/nonexistent/bakedit-test-program".into()), ..TermConfig::shell() };
    let mut panel = TerminalPanel::new(TerminalLaunch { config, program: "x".into(), ..launch(dir.path(), "true") });
    let (text, _, _) = harness.draw(&mut panel, 70, 6);
    assert!(text.contains("The terminal could not start"), "{text}");
    panel.tick(&mut harness.cx());
    assert!(matches!(&harness.take_effects()[..], [Effect::Status { error: true, .. }]));
}

fn env(path: Option<&Path>, inside_herdr: bool, home: &Path) -> LaunchEnv {
    LaunchEnv {
        path: path.map(|p| p.as_os_str().to_owned()),
        home: Some(home.to_path_buf()),
        herdr_env: inside_herdr.then(|| "1".into()),
        xdg_config_home: None,
        shell: Some("/bin/zsh".into()),
        // Only the PATH given here: never the machine's own herdr.
        herdr_binary: path.and_then(|dir| Some(dir.join("herdr")).filter(|p| p.exists())),
    }
}

#[test]
fn herdr_by_default_with_the_shell_when_it_cannot_run() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let fake = bin.join("herdr");
    std::fs::write(&fake, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let settings = Settings::default();
    let theme = Theme::builtin();
    let home = dir.path().join("home");
    let resolve = |command: &TerminalCommand, env: &LaunchEnv| TerminalLaunch::resolve_in(command, &settings, &theme, dir.path(), env);

    let launch = resolve(&TerminalCommand::Herdr, &env(Some(&bin), false, &home));
    assert_eq!(launch.program, "herdr");
    assert_eq!(launch.config.program.as_deref(), Some(fake.to_str().unwrap()));
    assert_eq!(launch.config.cwd.as_deref(), Some(dir.path()));
    assert!(launch.fallback.is_some(), "the shell if herdr will not start");
    let client = launch.herdr.expect("herdr actions drive the default session");
    assert_eq!(client.socket_path(), home.join(".config/herdr/herdr.sock"));
    assert!(launch.config.env_remove.is_empty(), "herdr's variables are never hidden from herdr");

    let inside = resolve(&TerminalCommand::Herdr, &env(Some(&bin), true, &home));
    assert_eq!((inside.config.program.as_deref(), inside.herdr.is_none(), inside.program.as_str()), (None, true, "zsh"));
    assert!(inside.notice.unwrap().contains("Already inside herdr"));

    let missing = resolve(&TerminalCommand::Herdr, &env(None, false, &home));
    assert_eq!(missing.config.program, None);
    assert!(missing.notice.unwrap().contains("herdr is not installed"));

    let shell = resolve(&TerminalCommand::Shell, &env(Some(&bin), false, &home));
    assert_eq!((shell.config.program, shell.fallback.is_none(), shell.herdr.is_none(), shell.notice), (None, true, true, None));

    let tmux = resolve(&TerminalCommand::Custom(vec!["tmux".into(), "new".into(), "-A".into()]), &env(Some(&bin), false, &home));
    assert_eq!((tmux.config.program.as_deref(), tmux.config.args.clone()), (Some("tmux"), vec!["new".to_string(), "-A".to_string()]));
    assert!(tmux.herdr.is_none() && tmux.fallback.is_some());
    assert_eq!(tmux.program, "tmux");

    let work = resolve(&TerminalCommand::Custom(vec!["herdr".into(), "--session".into(), "work".into()]), &env(Some(&bin), false, &home));
    assert_eq!(work.herdr.unwrap().socket_path(), home.join(".config/herdr/sessions/work/herdr.sock"));
}

#[test]
fn the_terminal_settings_reach_the_program() {
    let mut settings = Settings::default();
    settings.terminal.env = vec![("BAKEDIT_TEST_VAR".into(), "set by settings".into())];
    settings.terminal.scrollback = 1234;
    let launch = TerminalLaunch::resolve_in(&TerminalCommand::Shell, &settings, &Theme::builtin(), Path::new("/p"), &LaunchEnv::default());
    assert_eq!(launch.config.env, settings.terminal.env);
    assert_eq!(launch.config.scrollback, 1234);
    assert_eq!(launch.config.colors, TermColors::from_theme(&Theme::builtin()));
    assert_eq!(launch.program, "shell", "no SHELL: a generic name");
}

fn click(kind: MouseKind, col: u16, row: u16, mods: Mods) -> MouseEvent {
    let mut event = MouseEvent::new(kind, col, row);
    event.mods = mods;
    event
}

/// A panel that runs `script` and drives herdr through `client`.
fn herdr_panel(dir: &Path, script: &str, client: bakedit_herdr::Client) -> TerminalPanel {
    let mut panel = panel(dir, script);
    panel.launch.herdr = Some(client);
    panel.tabs[0].herdr = true;
    panel
}

#[test]
fn new_terminals_are_tabs_in_the_header_and_close_one_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = Harness::new(dir.path());
    let mut panel = panel(dir.path(), "printf first; sleep 30");
    harness.draw(&mut panel, 60, 6);
    until(&mut panel, &mut harness, |p| p.screen_text().contains("first"));
    let (text, _, _) = harness.draw(&mut panel, 60, 6);
    let header = text.lines().next().unwrap();
    assert!(header.starts_with(" 1 sh "), "{header:?}");
    assert!(header.contains(" +"), "a button for a new terminal: {header:?}");
    assert!(!header.contains("fullscreen"));

    assert!(panel.handle_action(&Action::new("workspace::NewTerminal"), &mut harness.cx()));
    assert_eq!((panel.tab_count(), panel.active_tab()), (2, 1));
    let (text, _, _) = harness.draw(&mut panel, 60, 6);
    assert!(text.lines().next().unwrap().starts_with(" 1 sh  2 "), "{text}");
    until(&mut panel, &mut harness, |p| p.is_running());
    assert!(!panel.screen_text().contains("first"), "a new terminal is a new program");

    // Switch back: the first program's screen is there, untouched.
    assert!(panel.handle_action(&Action::with_args("pane::ActivateItem", serde_json::json!(0)), &mut harness.cx()));
    assert_eq!(panel.active_tab(), 0);
    assert!(panel.screen_text().contains("first"));
    assert!(panel.handle_action(&Action::new("pane::ActivateNextItem"), &mut harness.cx()));
    assert_eq!(panel.active_tab(), 1);
    assert!(panel.handle_action(&Action::new("pane::ActivateNextItem"), &mut harness.cx()));
    assert_eq!(panel.active_tab(), 0, "next wraps around");
    assert!(panel.handle_action(&Action::new("pane::ActivateLastItem"), &mut harness.cx()));
    assert_eq!(panel.active_tab(), 1);

    // Closing the one in front leaves the other.
    assert!(panel.handle_action(&Action::new("pane::CloseActiveItem"), &mut harness.cx()));
    assert_eq!((panel.tab_count(), panel.active_tab()), (1, 0));
    assert!(panel.screen_text().contains("first"));
    assert!(harness.take_effects().is_empty());

    // The last one closes the panel instead, and the next opening starts afresh.
    assert!(panel.handle_action(&Action::new("pane::CloseActiveItem"), &mut harness.cx()));
    assert_eq!(harness.take_effects(), vec![Effect::Dispatch(Action::new("terminal_panel::Toggle"))]);
    assert_eq!(panel.tab_count(), 1);
    assert!(!panel.is_running());
}

#[test]
fn a_new_terminal_before_the_first_one_started_is_that_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = Harness::new(dir.path());
    let mut panel = panel(dir.path(), "printf first; sleep 30");
    // ctrl-~ from the editor before the panel was ever shown: its own program is the new one.
    assert!(panel.handle_action(&Action::new("workspace::NewTerminal"), &mut harness.cx()));
    assert_eq!(panel.tab_count(), 1);
    harness.draw(&mut panel, 60, 6);
    until(&mut panel, &mut harness, |p| p.screen_text().contains("first"));
    // Once it runs, another one is added beside it, running the panel's shell.
    assert!(panel.handle_action(&Action::new("workspace::NewTerminal"), &mut harness.cx()));
    assert_eq!((panel.tab_count(), panel.active_tab()), (2, 1));
    assert_eq!(panel.program(), "sh", "new terminals run the launch's shell, never the user's login shell in a test");
}

#[test]
fn clicking_the_header_switches_adds_and_closes_tabs() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = Harness::new(dir.path());
    let mut panel = panel(dir.path(), "sleep 30");
    let area = Rect::new(0, 0, 60, 6);
    harness.draw(&mut panel, 60, 6);
    let (text, _, _) = harness.draw(&mut panel, 60, 6);
    let plus = text.lines().next().unwrap().find('+').unwrap() as u16;
    let press = |col| click(MouseKind::Press(MouseButton::Left), col, 0, Mods::empty());
    assert!(panel.handle_mouse(&press(plus), area, &mut harness.cx()));
    assert_eq!(panel.tab_count(), 2);
    assert!(panel.handle_mouse(&click(MouseKind::Release(MouseButton::Left), plus, 0, Mods::empty()), area, &mut harness.cx()));
    harness.draw(&mut panel, 60, 6);
    assert!(panel.handle_mouse(&press(2), area, &mut harness.cx()), "the first tab");
    assert_eq!(panel.active_tab(), 0);
    panel.handle_mouse(&click(MouseKind::Release(MouseButton::Left), 2, 0, Mods::empty()), area, &mut harness.cx());
    // Middle click closes the tab under it.
    let middle = click(MouseKind::Press(MouseButton::Middle), 2, 0, Mods::empty());
    assert!(panel.handle_mouse(&middle, area, &mut harness.cx()));
    assert_eq!(panel.tab_count(), 1);
}

#[test]
fn the_header_says_when_the_panel_is_fullscreen_and_keeps_the_active_tab_in_view() {
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path());
    let mut panel = panel(dir.path(), "sleep 30");
    let mut render_cx = harness.render_cx(true);
    render_cx.zoomed = true;
    let area = Rect::new(0, 0, 50, 4);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    panel.render(&mut Canvas::new(&mut buf, area), area, &render_cx);
    let header = crate::dump::to_text(&buf).lines().next().unwrap().to_string();
    assert!(header.ends_with(" fullscreen"), "{header:?}");
    assert!(header.starts_with(" 1 sh "), "{header:?}");

    // Many terminals in a narrow panel: the tab in front is always drawn, whole.
    let mut harness = Harness::new(dir.path());
    for _ in 0..7 {
        panel.new_tab(&harness.cx());
    }
    let area = Rect::new(0, 0, 30, 4);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    panel.render(&mut Canvas::new(&mut buf, area), area, &harness.render_cx(true));
    let header = crate::dump::to_text(&buf).lines().next().unwrap().to_string();
    assert!(header.contains(" 8 "), "{header:?}");
    assert!(header.chars().count() <= 30);
}

#[test]
fn holding_the_link_modifier_underlines_the_link_under_the_pointer() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();
    let mut harness = Harness::new(dir.path());
    let mut panel = panel(dir.path(), "printf 'see a.rs:2 and nowhere.rs:9'; sleep 30");
    harness.draw(&mut panel, 60, 5);
    until(&mut panel, &mut harness, |p| p.screen_text().contains("nowhere"));
    let area = Rect::new(0, 0, 60, 5);
    let underlined = |buf: &ratatui::buffer::Buffer, col: u16| buf[(col, 1)].modifier.contains(Modifier::UNDERLINED);

    // Pointer over the existing file: nothing without the modifier ...
    let moved = click(MouseKind::Move, 6, 1, Mods::empty());
    assert!(!panel.handle_mouse(&moved, area, &mut harness.cx()), "no redraw for a plain move");
    let (_, _, buf) = harness.draw(&mut panel, 60, 5);
    assert!(!underlined(&buf, 6));
    // ... with it, the whole link and only the link.
    harness.held_mods = Mods::SUPER;
    let (_, _, buf) = harness.draw(&mut panel, 60, 5);
    assert!((4..=9).all(|col| underlined(&buf, col)), "a.rs:2 is underlined");
    assert!(!underlined(&buf, 3) && !underlined(&buf, 10), "its neighbours are not");
    // A path that does not exist opens nothing, so it is not offered as a link.
    let away = click(MouseKind::Move, 24, 1, Mods::SUPER);
    assert!(panel.handle_mouse(&away, area, &mut harness.cx()));
    let (_, _, buf) = harness.draw(&mut panel, 60, 5);
    assert!((0..60).all(|col| !underlined(&buf, col)));
}

#[test]
fn a_link_click_never_reaches_a_program_that_wants_the_mouse() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "").unwrap();
    let mut harness = Harness::new(dir.path());
    // Mouse reporting on (SGR), and cat shows what arrives.
    let mut panel = panel(dir.path(), "printf '\\033[?1000h\\033[?1006h a.rs:2\\n'; cat -v");
    harness.draw(&mut panel, 60, 5);
    until(&mut panel, &mut harness, |p| p.screen_text().contains("a.rs:2"));
    let area = Rect::new(0, 0, 60, 5);
    let on_link = |kind, mods| click(kind, 3, 1, mods);
    assert!(panel.handle_mouse(&on_link(MouseKind::Press(MouseButton::Left), Mods::SUPER), area, &mut harness.cx()));
    assert!(panel.handle_mouse(&on_link(MouseKind::Release(MouseButton::Left), Mods::SUPER), area, &mut harness.cx()));
    assert!(matches!(&harness.take_effects()[..], [Effect::OpenFile { line: Some(2), .. }]));
    // The program heard nothing of it; a plain click is reported to it.
    panel.handle_mouse(&on_link(MouseKind::Press(MouseButton::Left), Mods::empty()), area, &mut harness.cx());
    until(&mut panel, &mut harness, |p| p.screen_text().contains("[<0;4;1M"));
    assert_eq!(panel.screen_text().matches("[<0;").count(), 1, "{}", panel.screen_text());
}

#[test]
fn herdr_actions_use_the_socket_and_fall_back_to_the_prefix_chord() {
    use bakedit_herdr::mock::MockServer;
    let dir = tempfile::tempdir().unwrap();
    let mut harness = Harness::new(dir.path());

    // The socket answers: herdr is driven natively and the program is not typed at.
    let server = MockServer::start();
    server.ok("pane.zoom", serde_json::json!({"type": "pane_zoom", "zoom": {"changed": true, "zoomed": true, "pane_id": "w1:p1"}}));
    let mut panel = herdr_panel(dir.path(), RAW_CAT, server.client());
    harness.draw(&mut panel, 50, 6);
    assert!(panel.handle_action(&Action::new("herdr::ToggleZoom"), &mut harness.cx()));
    let started = Instant::now();
    while server.methods().is_empty() {
        assert!(started.elapsed() < WAIT, "the worker never called herdr");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(server.methods(), ["pane.zoom"]);
    panel.tick(&mut harness.cx());
    assert!(!panel.screen_text().contains("^B"), "nothing typed: {}", panel.screen_text());
    assert!(harness.take_effects().is_empty());
    // A bad argument is not an action of ours.
    assert!(!panel.handle_action(&Action::with_args("herdr::Navigate", serde_json::json!("sideways")), &mut harness.cx()));

    // No socket: the same keys type the chords the user's herdr config binds.
    let gone = bakedit_herdr::Client::new(dir.path().join("nothing.sock"));
    let mut panel = herdr_panel(dir.path(), RAW_CAT, gone);
    harness.draw(&mut panel, 50, 6);
    assert!(panel.handle_action(&Action::with_args("herdr::Navigate", serde_json::json!("left")), &mut harness.cx()));
    until(&mut panel, &mut harness, |p| p.screen_text().contains("^Bh"));

    // No session of ours at all (a plain shell): the chord goes straight to the terminal.
    let mut panel = panel_with_cat(dir.path());
    harness.draw(&mut panel, 50, 6);
    let chords = [
        (Action::with_args("herdr::Navigate", serde_json::json!("right")), "^Bl"),
        (Action::with_args("herdr::Navigate", serde_json::json!("up")), "^Bk"),
        (Action::with_args("herdr::MovePane", serde_json::json!("left")), "^B^H"),
        (Action::new("herdr::PreviousWorkspace"), "^Bu"),
        (Action::with_args("herdr::FocusWorkspace", serde_json::json!(3)), "^B#"),
        (Action::new("herdr::ToggleCollapse"), "^Bm"),
        (Action::new("herdr::ToggleExpand"), "^Be"),
        (Action::with_args("herdr::Scroll", serde_json::json!({"direction": "up", "lines": 9})), "^B^Y"),
    ];
    for (action, shown) in chords {
        assert!(panel.handle_action(&action, &mut harness.cx()), "{}", action.name);
        until(&mut panel, &mut harness, |p| p.screen_text().contains(shown));
    }
}

fn panel_with_cat(dir: &Path) -> TerminalPanel {
    panel(dir, RAW_CAT)
}

/// `cat` that shows control characters and gets them without the terminal's own echo.
const RAW_CAT: &str = "stty raw -echo; cat -v";

#[test]
fn a_path_printed_in_a_herdr_pane_resolves_against_that_pane() {
    use bakedit_herdr::mock::{MockServer, Reply};
    use serde_json::json;
    let project = tempfile::tempdir().unwrap();
    let pane_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(pane_dir.path().join("sub")).unwrap();
    std::fs::write(pane_dir.path().join("sub/lib.rs"), "").unwrap();
    let cwd = pane_dir.path().display().to_string();
    // One pane covering a 60x6 terminal (no sidebar in this stand-in), in `cwd`.
    let server = MockServer::with_handler(move |request| match request.method.as_str() {
        "pane.layout" => Some(Reply::Result(json!({"type": "pane_layout", "layout": {
            "workspace_id": "w1", "tab_id": "w1:t1", "zoomed": false,
            "area": {"x": 0, "y": 0, "width": 60, "height": 6}, "focused_pane_id": "w1:p1",
            "panes": [{"pane_id": "w1:p1", "focused": true, "rect": {"x": 0, "y": 0, "width": 60, "height": 6}}],
            "splits": [],
        }}))),
        "pane.get" | "pane.info" => Some(Reply::Result(json!({"type": "pane_info", "pane": {
            "pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1", "cwd": cwd, "foreground_cwd": cwd,
        }}))),
        _ => None,
    });
    let mut harness = Harness::new(project.path());
    let mut panel = herdr_panel(project.path(), "printf 'see sub/lib.rs:3'; sleep 30", server.client());
    harness.draw(&mut panel, 60, 7);
    until(&mut panel, &mut harness, |p| p.screen_text().contains("lib.rs:3"));
    let area = Rect::new(0, 0, 60, 7);
    let press = click(MouseKind::Press(MouseButton::Left), 8, 1, Mods::SUPER);
    assert!(panel.handle_mouse(&press, area, &mut harness.cx()));
    let expected = Effect::OpenFile { path: pane_dir.path().canonicalize().unwrap().join("sub/lib.rs"), line: Some(3), column: None, preview: false, focus: true };
    let effects = harness.take_effects();
    match &effects[..] {
        [Effect::OpenFile { path, line, .. }] => {
            assert_eq!((path.canonicalize().unwrap(), *line), (pane_dir.path().canonicalize().unwrap().join("sub/lib.rs"), Some(3)), "{expected:?}")
        }
        other => panic!("{other:?}; herdr was asked {:?}", server.methods()),
    }
    // Asking again within a moment does not ask herdr again.
    let asked = server.methods().len();
    panel.handle_mouse(&click(MouseKind::Release(MouseButton::Left), 8, 1, Mods::SUPER), area, &mut harness.cx());
    panel.handle_mouse(&press, area, &mut harness.cx());
    assert_eq!(server.methods().len(), asked, "{:?}", server.methods());
    // The picker lists it too, with where it resolved.
    let rows = panel.picker_rows(PickerKind::Links, &mut harness.cx()).unwrap();
    assert_eq!(rows.len(), 1);
}

/// Just enough of an item factory to open the files the terminal's links name.
struct OpensFiles;

impl crate::view::ItemFactory for OpensFiles {
    fn open_path(&mut self, path: &Path, _: &mut Cx<'_>) -> Result<Box<dyn crate::view::Item>, String> {
        Ok(crate::pane::test_support::FakeItem::file(&path.to_string_lossy()))
    }
    fn untitled(&mut self, _: &mut Cx<'_>) -> Box<dyn crate::view::Item> {
        crate::pane::test_support::FakeItem::untitled("untitled")
    }
    fn project_diff(&mut self, _: &mut Cx<'_>) -> Box<dyn crate::view::Item> {
        crate::pane::test_support::FakeItem::untitled("diff")
    }
    fn project_search(&mut self, _: bool, _: &mut Cx<'_>) -> Box<dyn crate::view::Item> {
        crate::pane::test_support::FakeItem::untitled("search")
    }
    fn read_only(&mut self, title: &str, _: &str, _: Option<&str>, _: &mut Cx<'_>) -> Box<dyn crate::view::Item> {
        crate::pane::test_support::FakeItem::untitled(title)
    }
}

struct NoSource;

impl crate::view::PickerSource for NoSource {
    fn rows(&mut self, _: PickerKind, _: &str, _: &mut Cx<'_>) -> Vec<PickerItem> {
        Vec::new()
    }
    fn confirm(&mut self, _: PickerKind, _: Option<usize>, _: &str, _: bool, _: &mut Cx<'_>) {}
}

#[test]
fn a_link_clicked_in_the_fullscreen_terminal_shows_the_file_in_the_editor() {
    use crate::workspace::{Focus, PanelId, Zoomed};
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();
    let mut shell = crate::shell::Shell::new(
        Settings::default(),
        Theme::builtin(),
        crate::project::Project::detached(dir.path()),
        crate::clipboard::Clipboard::with_backend(Box::new(crate::clipboard::MemoryBackend::default())),
        Box::new(OpensFiles),
        Box::new(NoSource),
    );
    shell.set_panel(PanelId::Terminal, Box::new(panel(dir.path(), "printf 'at a.rs:1 now'; sleep 30")));
    shell.dispatch(&Action::new("terminal_panel::ToggleZoom"));
    assert_eq!(shell.workspace.zoomed(), Some(Zoomed::Panel(PanelId::Terminal)));
    let draw = |shell: &mut crate::shell::Shell| {
        let mut buf = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 100, 20));
        shell.render(&mut buf);
        crate::dump::to_text(&buf)
    };
    let started = Instant::now();
    let text = loop {
        shell.tick();
        let text = draw(&mut shell);
        if text.contains("at a.rs:1 now") {
            break text;
        }
        assert!(started.elapsed() < WAIT, "{text}");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(text.lines().next().unwrap().ends_with(" fullscreen"), "{text}");
    // Cmd held: the terminal is the whole screen, the link is on its first program row.
    shell.held_mods = Mods::SUPER;
    let mut press = MouseEvent::new(MouseKind::Press(MouseButton::Left), 5, 1);
    press.mods = Mods::SUPER;
    assert!(shell.handle_mouse(&press));
    assert_eq!(shell.workspace.focus(), Focus::Center, "the editor has the focus");
    assert_eq!(shell.workspace.zoomed(), None, "the terminal is no longer in the way");
    assert_eq!(shell.pane.len(), 1);
    assert_eq!(shell.pane.active().unwrap().path(), Some(dir.path().join("a.rs").as_path()));
    assert!(draw(&mut shell).contains("a.rs"));
}
