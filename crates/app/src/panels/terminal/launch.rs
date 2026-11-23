//! What the terminal panel runs: the `terminal.command` setting (or `--terminal`) turned
//! into a program, a fallback and the herdr session its actions drive.

use std::path::{Path, PathBuf};

use bakedit_core::Theme;
use bakedit_herdr::{launch, Client};
use bakedit_term::{TermColors, TermConfig};

use crate::settings::{Settings, TerminalCommand};

/// What the terminal panel runs.
#[derive(Debug, Clone)]
pub struct TerminalLaunch {
    /// What to start, without its size (that is the panel's).
    pub config: TermConfig,
    /// What to start instead when `config` cannot be started (the program is not
    /// installed): the login shell. `None` when `config` is the shell already.
    pub fallback: Option<TermConfig>,
    /// The herdr session the panel shows, for the `herdr::*` actions. `None` when the panel
    /// does not run herdr's interface.
    pub herdr: Option<Client>,
    /// Why the panel does not run what the settings ask for, shown once when it starts.
    pub notice: Option<String>,
    /// A short name for what runs: "herdr", "zsh", "tmux".
    pub program: String,
    /// What a terminal added later (`workspace::NewTerminal`, the header's `+`) runs: the
    /// login shell, with the panel's environment and colours.
    pub shell: TermConfig,
    /// The name of [`TerminalLaunch::shell`] for its tab ("zsh").
    pub shell_name: String,
}

/// The parts of the environment that decide what the panel runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchEnv {
    pub path: Option<std::ffi::OsString>,
    pub home: Option<PathBuf>,
    /// `HERDR_ENV`: set to 1 inside a herdr pane.
    pub herdr_env: Option<std::ffi::OsString>,
    pub xdg_config_home: Option<std::ffi::OsString>,
    /// `SHELL`, for naming the shell in messages.
    pub shell: Option<String>,
    /// Where herdr is installed, if it is (`launch::find_binary` over `PATH` and the usual
    /// places).
    pub herdr_binary: Option<PathBuf>,
}

impl LaunchEnv {
    /// This process's environment.
    pub fn current() -> LaunchEnv {
        let path = std::env::var_os("PATH");
        let home = dirs::home_dir();
        let herdr_binary = launch::find_binary(path.as_deref(), home.as_deref());
        LaunchEnv {
            path,
            home,
            herdr_env: std::env::var_os(launch::INSIDE_ENV_VAR),
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            shell: std::env::var("SHELL").ok(),
            herdr_binary,
        }
    }
}

impl TerminalLaunch {
    /// Work out what to run for `command` in this process's environment. Starts nothing.
    pub fn resolve(command: &TerminalCommand, settings: &Settings, theme: &Theme, root: &Path) -> TerminalLaunch {
        TerminalLaunch::resolve_in(command, settings, theme, root, &LaunchEnv::current())
    }

    /// [`TerminalLaunch::resolve`] in a given environment.
    pub fn resolve_in(command: &TerminalCommand, settings: &Settings, theme: &Theme, root: &Path, env: &LaunchEnv) -> TerminalLaunch {
        let base = TermConfig {
            cwd: Some(root.to_path_buf()),
            env: settings.terminal.env.clone(),
            // What runs in the panel is not the bakedit a Ghostty window runs: a `bakedit`
            // typed there (or in a herdr server started there) opens a window as anywhere.
            env_remove: crate::window::HOST_VARIABLES.iter().map(|name| name.to_string()).collect(),
            scrollback: settings.terminal.scrollback,
            colors: TermColors::from_theme(theme),
            ..TermConfig::shell()
        };
        let home = env.home.clone();
        let shell_name = || {
            env.shell
                .as_deref()
                .and_then(|shell| Path::new(shell).file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "shell".to_string())
        };
        let shell = |notice: Option<String>| TerminalLaunch {
            config: base.clone(),
            fallback: None,
            herdr: None,
            notice,
            program: shell_name(),
            shell: base.clone(),
            shell_name: shell_name(),
        };
        let herdr_client = |session: launch::Session| -> Option<Client> {
            let config_dir = launch::config_dir(env.xdg_config_home.as_deref(), home.as_deref())?;
            Some(Client::from_env_or_default().with_socket_path(session.socket_path(&config_dir)).allow_live_mutations(true))
        };
        match command {
            TerminalCommand::Shell => shell(None),
            TerminalCommand::Herdr => {
                if launch::running_inside_herdr(env.herdr_env.as_deref()) {
                    return shell(Some("Already inside herdr: the terminal runs your shell (herdr does not nest)".to_string()));
                }
                match env.herdr_binary.clone() {
                    None => shell(Some("herdr is not installed: the terminal runs your shell".to_string())),
                    Some(binary) => TerminalLaunch {
                        config: TermConfig { program: Some(binary.to_string_lossy().into_owned()), ..base.clone() },
                        fallback: Some(base.clone()),
                        herdr: herdr_client(launch::Session::Default),
                        notice: None,
                        program: "herdr".to_string(),
                        shell: base.clone(),
                        shell_name: shell_name(),
                    },
                }
            }
            TerminalCommand::Custom(words) => {
                let Some((program, args)) = words.split_first() else { return shell(None) };
                let name = Path::new(program).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| program.clone());
                TerminalLaunch {
                    config: TermConfig { program: Some(program.clone()), args: args.to_vec(), ..base.clone() },
                    fallback: Some(base.clone()),
                    herdr: launch::session_of_command(words).and_then(herdr_client),
                    notice: None,
                    program: name,
                    shell: base.clone(),
                    shell_name: shell_name(),
                }
            }
        }
    }
}
