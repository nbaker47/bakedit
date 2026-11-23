use std::time::{Duration, Instant};

use bakedit_core::{Action, Key, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind, Theme};

use super::*;
use crate::view::test_support::Harness;

const WAIT: Duration = Duration::from_secs(15);

/// A panel running `script` with `sh -c` in `dir`.
fn panel(dir: &Path, script: &str) -> TerminalPanel {
    let config = TermConfig {
        program: Some("/bin/sh".into()),
        args: vec!["-c".into(), script.into()],
        cwd: Some(dir.to_path_buf()),
        ..TermConfig::shell()
    };
    TerminalPanel::new(TerminalLaunch { config, fallback: None, herdr: None, notice: None, program: "sh".into() })
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
    harness.draw(&mut panel, 50, 8);
    until(&mut panel, &mut harness, |p| p.screen_text().contains("hello from sh"));
    assert!(panel.screen_text().starts_with("8 50"), "{}", panel.screen_text());
    assert!(panel.is_running());
    panel.tick(&mut harness.cx());
    let (text, cursor, buf) = harness.draw(&mut panel, 50, 8);
    assert!(text.contains("hello from sh"), "{text}");
    assert_eq!(cursor.map(|c| (c.col, c.row)), Some((13, 1)), "the cursor follows the output");
    assert_eq!(buf[(0, 7)].bg, ui::color(harness.theme.terminal_bg), "the theme's terminal background");
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
    harness.draw(&mut panel, 60, 6);
    until(&mut panel, &mut harness, |p| p.screen_text().contains("here"));
    let area = Rect::new(0, 0, 60, 6);
    // A plain click selects; it opens nothing.
    let plain = MouseEvent::new(MouseKind::Press(MouseButton::Left), 12, 0);
    panel.handle_mouse(&plain, area, &mut harness.cx());
    assert!(harness.take_effects().is_empty());
    // With cmd held (the app adds it to the event from the keys it saw) it opens the file.
    let mut click = MouseEvent::new(MouseKind::Press(MouseButton::Left), 12, 0);
    click.mods = Mods::SUPER;
    assert!(panel.handle_mouse(&click, area, &mut harness.cx()));
    let expected = Effect::OpenFile { path: dir.path().join("src/main.rs"), line: Some(12), column: Some(5), preview: false, focus: true };
    assert_eq!(harness.take_effects(), vec![expected]);
    // Its release is swallowed with it.
    let release = MouseEvent::new(MouseKind::Release(MouseButton::Left), 12, 0);
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
    harness.draw(&mut panel, 50, 5);
    until(&mut panel, &mut harness, |p| !p.is_running());
    let (text, cursor, _) = harness.draw(&mut panel, 50, 5);
    assert!(text.lines().nth(4).unwrap().contains("sh exited with code 3  ·  enter starts it again"), "{text}");
    assert_eq!(cursor, None);
    // Enter arrives as menu::Confirm first; the panel takes it to restart.
    assert!(panel.handle_action(&Action::new("menu::Confirm"), &mut harness.cx()));
    harness.draw(&mut panel, 50, 5);
    until(&mut panel, &mut harness, |p| !p.is_running() && p.screen_text().contains("bye"));
}

#[test]
fn menu_keys_and_unknown_actions_fall_through_to_the_program() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = Harness::new(dir.path());
    let mut panel = panel(dir.path(), "sleep 5");
    harness.draw(&mut panel, 30, 4);
    for name in ["menu::Confirm", "menu::Cancel", "menu::SelectNext", "editor::MoveUp", "herdr::ToggleZoom"] {
        assert!(!panel.handle_action(&Action::new(name), &mut harness.cx()), "{name} is not the terminal's while sh runs");
    }
    assert!(!panel.handle_action(&Action::new("terminal::Copy"), &mut harness.cx()), "nothing selected: cmd-c goes to the program");
    let mut contexts = Vec::new();
    panel.contexts(&mut contexts);
    assert_eq!(contexts.iter().map(|c| c.name).collect::<Vec<_>>(), ["Pane", "Terminal"]);
}

#[test]
fn a_program_that_cannot_start_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = Harness::new(dir.path());
    let config = TermConfig { program: Some("/nonexistent/bakedit-test-program".into()), ..TermConfig::shell() };
    let mut panel = TerminalPanel::new(TerminalLaunch { config, fallback: None, herdr: None, notice: None, program: "x".into() });
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
