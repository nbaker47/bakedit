//! What a terminal panel needs to know to start herdr.
//!
//! The IDE's terminal runs herdr unless the user configured something else. Whether and how
//! is the application's decision; this module only answers the questions about herdr itself:
//! where its executable is, whether the IDE is already running inside a herdr pane, and which
//! environment variables belong to that outer pane.

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The name of herdr's executable.
pub const BINARY_NAME: &str = "herdr";

/// The variable herdr sets to `1` in every pane it runs.
pub const INSIDE_ENV_VAR: &str = "HERDR_ENV";

/// The variables herdr sets in every pane: they describe the pane a process was started in.
///
/// A program that starts herdr again from inside a pane hands these on unless it removes
/// them, and herdr then refuses to start (nesting is off unless the user turned on
/// `experimental.allow_nested`). They also make the user's `herdr-*` scripts, and
/// [`crate::Client::from_env_or_default`], talk to the outer session.
pub const PANE_ENV_VARS: [&str; 7] = [
    "HERDR_ENV",
    "HERDR_SOCKET_PATH",
    "HERDR_SESSION",
    "HERDR_PANE_ID",
    "HERDR_TAB_ID",
    "HERDR_WORKSPACE_ID",
    "HERDR_BIN_PATH",
];

/// Directories searched after `PATH`, as the user's `herdr-*` scripts do; the last one is
/// relative to the home directory.
const EXTRA_DIRS: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];
const HOME_BIN_DIR: &str = ".local/bin";

/// Where the herdr executable is, or `None` when it is not installed.
///
/// `path_var` is the value of `PATH` and `home` the user's home directory; the caller passes
/// them in so that this function reads nothing global. The directories of `path_var` are
/// searched in order, then `/opt/homebrew/bin`, `/usr/local/bin` and `<home>/.local/bin`,
/// which is where the user's scripts look when herdr starts them with a bare `PATH`. Empty
/// `PATH` entries are skipped rather than taken to mean the current directory.
pub fn find_binary(path_var: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    let from_path = path_var.into_iter().flat_map(std::env::split_paths);
    let extra = EXTRA_DIRS.iter().map(PathBuf::from);
    let home_bin = home.map(|home| home.join(HOME_BIN_DIR));
    from_path
        .chain(extra)
        .chain(home_bin)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(BINARY_NAME))
        .find(|candidate| is_executable_file(candidate))
}

/// Whether a process whose `HERDR_ENV` variable has the value `herdr_env` is running inside a
/// herdr pane. Pass `std::env::var_os(INSIDE_ENV_VAR).as_deref()`.
///
/// An IDE that finds itself inside herdr should not start herdr in its own terminal by
/// default: herdr would refuse, and with the variables removed it would attach to the very
/// session the IDE is being shown in.
pub fn running_inside_herdr(herdr_env: Option<&OsStr>) -> bool {
    herdr_env == Some(OsStr::new("1"))
}

fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::fs;

    fn make_file(path: &Path, mode: u32) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn path_var(dirs: &[&Path]) -> OsString {
        std::env::join_paths(dirs).unwrap()
    }

    #[test]
    fn finds_herdr_in_the_first_path_directory_that_has_it() {
        let dir = tempfile::tempdir().unwrap();
        let (first, second, third) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("c"),
        );
        fs::create_dir_all(&first).unwrap();
        make_file(&second.join("herdr"), 0o755);
        make_file(&third.join("herdr"), 0o755);
        let path = path_var(&[&first, &second, &third]);
        assert_eq!(find_binary(Some(&path), None), Some(second.join("herdr")));
    }

    #[test]
    fn skips_files_that_cannot_be_run_and_directories() {
        let dir = tempfile::tempdir().unwrap();
        let (plain, folder, real) = (
            dir.path().join("plain"),
            dir.path().join("folder"),
            dir.path().join("real"),
        );
        make_file(&plain.join("herdr"), 0o644);
        fs::create_dir_all(folder.join("herdr")).unwrap();
        make_file(&real.join("herdr"), 0o700);
        let path = path_var(&[&plain, &folder, &real]);
        assert_eq!(find_binary(Some(&path), None), Some(real.join("herdr")));
    }

    #[test]
    fn falls_back_to_the_home_bin_directory() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        make_file(&home.join(".local/bin/herdr"), 0o755);
        let path = path_var(&[&elsewhere]);
        // The system-wide fallbacks may hold a herdr of their own on some machine; anything
        // found has to be one of the places searched, and with none there it is the home one.
        let found = find_binary(Some(&path), Some(&home)).unwrap();
        let system_wide = EXTRA_DIRS
            .iter()
            .map(|dir| Path::new(dir).join("herdr"))
            .any(|path| path == found);
        assert!(
            found == home.join(".local/bin/herdr") || system_wide,
            "{}",
            found.display()
        );
        if !EXTRA_DIRS
            .iter()
            .any(|dir| is_executable_file(&Path::new(dir).join("herdr")))
        {
            assert_eq!(found, home.join(".local/bin/herdr"));
            assert_eq!(
                find_binary(None, Some(&home)),
                Some(home.join(".local/bin/herdr"))
            );
        }
    }

    #[test]
    fn path_wins_over_the_fallback_directories() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let on_path = dir.path().join("bin");
        make_file(&home.join(".local/bin/herdr"), 0o755);
        make_file(&on_path.join("herdr"), 0o755);
        let path = path_var(&[&on_path]);
        assert_eq!(
            find_binary(Some(&path), Some(&home)),
            Some(on_path.join("herdr"))
        );
    }

    #[test]
    fn an_empty_path_entry_is_not_the_current_directory() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        make_file(&real.join("herdr"), 0o755);
        let mut path = OsString::from(":");
        path.push(real.as_os_str());
        path.push("::");
        assert_eq!(find_binary(Some(&path), None), Some(real.join("herdr")));
    }

    #[test]
    fn nothing_installed_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        let path = path_var(&[&empty]);
        if !EXTRA_DIRS
            .iter()
            .any(|dir| is_executable_file(&Path::new(dir).join("herdr")))
        {
            assert_eq!(find_binary(Some(&path), Some(&empty)), None);
            assert_eq!(find_binary(None, None), None);
        }
    }

    #[test]
    fn only_the_value_one_means_inside_herdr() {
        assert!(running_inside_herdr(Some(OsStr::new("1"))));
        assert!(!running_inside_herdr(Some(OsStr::new("0"))));
        assert!(!running_inside_herdr(Some(OsStr::new(""))));
        assert!(!running_inside_herdr(Some(OsStr::new("true"))));
        assert!(!running_inside_herdr(None));
    }

    #[test]
    fn the_pane_variables_include_the_ones_the_client_and_the_guard_read() {
        assert!(PANE_ENV_VARS.contains(&INSIDE_ENV_VAR));
        assert!(PANE_ENV_VARS.contains(&crate::client::SOCKET_ENV_VAR));
        assert!(PANE_ENV_VARS.iter().all(|name| name.starts_with("HERDR_")));
    }
}
