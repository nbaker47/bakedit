//! The `bakedit` command: the editor, and the small tools around it (`--help` lists them).

use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _};
use bakedit::app::{self, App, StartOptions};
use bakedit::cli::{self, Mode};
use bakedit::host::{self, Host, HostEvent};
use bakedit::settings::TerminalCommand;
use bakedit_core::{ColorSlot, InputEvent, Key, KeyEvent, Mods, TerminalReply};
use bakedit_input::{describe, Parser};

/// How long `--probe` waits for the terminal's answers.
const PROBE_WAIT: Duration = Duration::from_millis(1500);
/// Two ctrl-c within this time end `--dump-input`.
const DOUBLE_CTRL_C: Duration = Duration::from_millis(1500);

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            eprintln!("bakedit: no working directory: {error}");
            return ExitCode::FAILURE;
        }
    };
    let args = match cli::parse(raw.clone(), &cwd) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("bakedit: {message}");
            return ExitCode::from(2);
        }
    };
    if let Err(error) = bakedit::logging::init(args.log_file.as_deref()) {
        eprintln!("bakedit: cannot write the log: {error}");
        return ExitCode::FAILURE;
    }
    let result = match &args.mode {
        Mode::Help => {
            print!("{}", cli::USAGE);
            Ok(())
        }
        Mode::Version => {
            println!("bakedit {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Mode::PrintKeymap => {
            print!("{}", bakedit::keys::DEFAULT_KEYMAP);
            Ok(())
        }
        Mode::PrintGhosttyConfig => {
            print!("{}", bakedit::window::describe(&user_keymap()));
            Ok(())
        }
        Mode::Action { name, args } => app::send_action(name, args.clone()).map_err(anyhow::Error::msg),
        Mode::DumpInput => dump_input(),
        Mode::Probe => probe(),
        Mode::Edit => edit(args, &raw, &cwd),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bakedit: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// The keymap as the editor would load it: the built-in one, then the user's.
fn user_keymap() -> bakedit_keymap::Keymap {
    let text = std::fs::read_to_string(bakedit::settings::keymap_path()).ok();
    bakedit::keys::KeyRouter::new(text.as_deref()).0.keymap
}

fn edit(args: cli::Args, raw: &[String], cwd: &std::path::Path) -> anyhow::Result<()> {
    if args.window {
        return bakedit::window::open_window(&user_keymap(), raw).map_err(anyhow::Error::msg);
    }
    // Files while a window is up go to that window, as `zed file` does.
    if !args.new_instance && !args.paths.is_empty() && args.paths.iter().all(|spec| !spec.path.is_dir()) {
        match app::send_files(&args.paths) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => log::warn!("starting a new window: {error}"),
        }
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("bakedit needs a terminal to run in (for a running window use --action, or pass files without --new)");
    }
    let root = cli::project_root(&args.paths, cwd);
    let files = args.paths.iter().filter(|spec| !spec.path.is_dir()).cloned().collect();
    let terminal_command = args.terminal_command.as_deref().map(TerminalCommand::parse);
    App::run(StartOptions { root, files, terminal_command, restore: !args.no_restore })
}

fn is_ctrl_c(event: &InputEvent) -> bool {
    matches!(event, InputEvent::Key(KeyEvent { key: Key::Char('c'), mods, kind, .. }) if *mods == Mods::CTRL && *kind != bakedit_core::KeyKind::Release)
}

/// `--dump-input`: every event as bakedit decodes it, one line each, until ctrl-c twice.
fn dump_input() -> anyhow::Result<()> {
    if !io::stdin().is_terminal() {
        bail!("--dump-input needs a terminal");
    }
    host::install_panic_hook();
    let host = Host::enter(host::inline_options()).context("could not put the terminal in raw mode")?;
    let (sender, events) = crossbeam_channel::unbounded();
    host::spawn_reader(move |event| {
        let _ = sender.send(event);
    })?;
    let mut out = io::stdout();
    write!(out, "bakedit --dump-input: press keys, click, paste. ctrl-c twice quits.\r\n")?;
    out.flush()?;
    let mut parser = Parser::new();
    let mut last_ctrl_c: Option<Instant> = None;
    'reading: loop {
        let decoded = if parser.has_pending() {
            match events.recv_timeout(Duration::from_millis(25)) {
                Ok(HostEvent::Bytes(bytes)) => parser.feed(&bytes),
                Ok(_) => break,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => parser.flush(),
                Err(_) => break,
            }
        } else {
            match events.recv() {
                Ok(HostEvent::Bytes(bytes)) => parser.feed(&bytes),
                _ => break,
            }
        };
        for event in decoded {
            if let InputEvent::Reply(TerminalReply::KittyKeyboardFlags(flags)) = &event {
                parser.set_kitty_flags(*flags);
            }
            write!(out, "{}\r\n", describe(&event))?;
            if is_ctrl_c(&event) {
                if last_ctrl_c.is_some_and(|at| at.elapsed() < DOUBLE_CTRL_C) {
                    break 'reading;
                }
                last_ctrl_c = Some(Instant::now());
            }
        }
        out.flush()?;
    }
    drop(host);
    Ok(())
}

/// `--probe`: ask the terminal what it supports and print the answers.
fn probe() -> anyhow::Result<()> {
    if !io::stdin().is_terminal() {
        bail!("--probe needs a terminal");
    }
    host::install_panic_hook();
    let host = Host::enter(host::inline_options()).context("could not put the terminal in raw mode")?;
    let (sender, events) = crossbeam_channel::unbounded();
    host::spawn_reader(move |event| {
        let _ = sender.send(event);
    })?;
    let mut parser = Parser::new();
    let mut replies = Vec::new();
    let deadline = Instant::now() + PROBE_WAIT;
    'waiting: while let Ok(HostEvent::Bytes(bytes)) = events.recv_deadline(deadline) {
        for event in parser.feed(&bytes) {
            if let InputEvent::Reply(reply) = event {
                let last = matches!(reply, TerminalReply::DeviceAttributes(_));
                replies.push(reply);
                if last {
                    break 'waiting;
                }
            }
        }
    }
    drop(host);
    host::restore();
    print!("{}", probe_report(&replies));
    Ok(())
}

fn probe_report(replies: &[TerminalReply]) -> String {
    let mut out = String::new();
    let flags = replies.iter().find_map(|reply| match reply {
        TerminalReply::KittyKeyboardFlags(flags) => Some(*flags),
        _ => None,
    });
    let version = replies.iter().find_map(|reply| match reply {
        TerminalReply::Version(version) => Some(version.clone()),
        _ => None,
    });
    let answered = replies.iter().any(|reply| matches!(reply, TerminalReply::DeviceAttributes(_)));
    out.push_str(&format!("terminal          {}\n", version.unwrap_or_else(|| "did not say (no XTVERSION)".into())));
    match flags {
        Some(flags) => {
            let names: Vec<&str> = [(1, "disambiguate"), (2, "event types"), (4, "alternates"), (8, "all keys"), (16, "text")]
                .iter()
                .filter(|(bit, _)| flags & bit != 0)
                .map(|(_, name)| *name)
                .collect();
            out.push_str(&format!("kitty keyboard    flags {flags} ({})\n", names.join(", ")));
        }
        None => out.push_str("kitty keyboard    not supported: cmd keys do not arrive, use the ctrl-] prefix\n"),
    }
    let layer = if flags.is_some_and(bakedit_input::NavLayer::works_with) { "works (key releases are reported)" } else { "cannot work here (no key releases)" };
    out.push_str(&format!("; nav layer       {layer}\n"));
    for reply in replies {
        match reply {
            TerminalReply::Color { slot: ColorSlot::Foreground, rgb } => out.push_str(&format!("foreground        {}\n", rgb.to_hex())),
            TerminalReply::Color { slot: ColorSlot::Background, rgb } => out.push_str(&format!("background        {}\n", rgb.to_hex())),
            TerminalReply::DarkMode(dark) => out.push_str(&format!("appearance        {}\n", if *dark { "dark" } else { "light" })),
            _ => {}
        }
    }
    if !answered {
        out.push_str("note              the terminal did not answer the device attributes query in time\n");
    }
    let env = |name: &str| std::env::var(name).unwrap_or_else(|_| "unset".into());
    out.push_str(&format!("TERM              {}\n", env("TERM")));
    out.push_str(&format!("TERM_PROGRAM      {}\n", env("TERM_PROGRAM")));
    out.push_str(&format!("BAKEDIT_HOST      {}\n", env("BAKEDIT_HOST")));
    out.push_str(&format!("inside herdr      {}\n", if bakedit_term::inside_herdr() { "yes: the terminal panel runs your shell" } else { "no" }));
    out
}
