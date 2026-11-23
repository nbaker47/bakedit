//! Choosing the theme the settings name.
//!
//! `"theme"` names a Zed theme. "Ghostty" (and no setting at all) is the built-in theme, the
//! user's Zed theme compiled in. Any other name is looked up in the theme files of
//! `~/.config/bakedit/themes/` and then `~/.config/zed/themes/`, so a theme installed for Zed
//! works here as it is. `theme_overrides` is laid over whichever theme it is.

use std::path::{Path, PathBuf};

use bakedit_core::Theme;

use crate::settings::Settings;

/// The name of the built-in theme.
pub const BUILTIN: &str = "Ghostty";

/// Where theme files are looked for, in order.
pub fn theme_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![crate::settings::config_dir().join("themes")];
    if let Some(zed) = crate::settings::config_dir().parent().map(|config| config.join("zed").join("themes")) {
        dirs.push(zed);
    }
    dirs
}

/// The theme the settings ask for, and a line for the status bar when it could not be had.
pub fn load(settings: &Settings) -> (Theme, Option<String>) {
    load_from(settings, &theme_dirs())
}

/// [`load`] looking in `dirs` (for tests).
pub fn load_from(settings: &Settings, dirs: &[PathBuf]) -> (Theme, Option<String>) {
    let builtin = || Theme::builtin_with(&settings.theme_overrides);
    let name = match settings.theme.as_deref() {
        None => return (builtin(), None),
        Some(name) if name == BUILTIN => return (builtin(), None),
        Some(name) => name,
    };
    for dir in dirs {
        if let Some(theme) = find_in(dir, name, settings) {
            return (theme, None);
        }
    }
    let looked: Vec<String> = dirs.iter().map(|dir| tilde(dir)).collect();
    let warning = format!("Theme \"{name}\" is not in {}: using {BUILTIN}", looked.join(" or "));
    (builtin(), Some(warning))
}

/// The theme called `name` in a `.json` file of `dir`, if one has it.
fn find_in(dir: &Path, name: &str, settings: &Settings) -> Option<Theme> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    files.into_iter().find_map(|file| {
        let text = std::fs::read_to_string(&file).ok()?;
        match Theme::from_zed_json(&text, Some(name), Some(&settings.theme_overrides)) {
            Ok(theme) => Some(theme),
            Err(bakedit_core::ThemeError::NotFound(_)) => None,
            Err(error) => {
                log::warn!("skipping theme file {}: {error}", file.display());
                None
            }
        }
    })
}

fn tilde(path: &Path) -> String {
    match dirs::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const OTHER: &str = r##"{
      "name": "Other family",
      "themes": [
        { "name": "Midnight", "appearance": "dark", "style": { "background": "#010203", "text": "#ffffff" } },
        { "name": "Paper", "appearance": "light", "style": { "background": "#fafafa" } }
      ]
    }"##;

    #[test]
    fn the_builtin_theme_with_overrides() {
        let mut settings = Settings::default();
        let (theme, warning) = load_from(&settings, &[]);
        assert_eq!((theme.name.as_str(), warning), (BUILTIN, None));
        settings.theme = Some("Ghostty".into());
        settings.theme_overrides.insert("editor.background".into(), Value::String("#000000".into()));
        let (theme, _) = load_from(&settings, &[]);
        assert_eq!(theme.editor_bg.to_hex(), "#000000");
    }

    #[test]
    fn a_named_theme_is_found_in_the_theme_folders() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        std::fs::write(first.path().join("broken.json"), "{ not json").unwrap();
        std::fs::write(second.path().join("other.json"), OTHER).unwrap();
        let mut settings = Settings {
            theme: Some("Paper".into()),
            theme_overrides: json!({ "text": "#123456" }).as_object().unwrap().clone(),
            ..Settings::default()
        };
        let dirs = vec![first.path().to_path_buf(), second.path().to_path_buf()];
        let (theme, warning) = load_from(&settings, &dirs);
        assert_eq!(warning, None);
        assert_eq!(theme.name, "Paper");
        assert!(!theme.dark);
        assert_eq!(theme.text.to_hex(), "#123456", "overrides apply to a theme from a file too");
        settings.theme = Some("Nope".into());
        let (theme, warning) = load_from(&settings, &dirs);
        assert_eq!(theme.name, BUILTIN);
        assert!(warning.unwrap().starts_with("Theme \"Nope\" is not in "));
    }
}
