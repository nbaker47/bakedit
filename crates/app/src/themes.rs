//! Choosing the theme the settings name.
//!
//! `"theme"` names a built-in theme (`"Ghostty"`, `"Terminal"`, `"Catppuccin Mocha"`,
//! `"Light"`) or a Zed theme in `~/.config/bakedit/themes/` and then
//! `~/.config/zed/themes/`, so a theme installed for Zed works here as it is.
//! `theme_overrides` is laid over whichever theme it is.

use std::path::{Path, PathBuf};

use bakedit_core::{TerminalPalette, Theme};

use crate::settings::Settings;

/// The name of the default built-in theme.
pub const BUILTIN: &str = "Ghostty";

/// The built-in theme that follows the host terminal's live palette.
pub const TERMINAL: &str = "Terminal";

/// The four built-in themes, in the order shown in onboarding and the theme picker.
pub const BUILTIN_CHOICES: &[(&str, &str)] = &[
    ("Ghostty", "Default · SilkCircuit Neon dark"),
    ("Terminal", "Follow host terminal palette (OSC 10/11/4)"),
    ("Catppuccin Mocha", "Warm pastel dark"),
    ("Light", "Catppuccin Latte light mode"),
];

/// Where theme files are looked for, in order.
pub fn theme_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![crate::settings::config_dir().join("themes")];
    if let Some(zed) = crate::settings::config_dir().parent().map(|config| config.join("zed").join("themes")) {
        dirs.push(zed);
    }
    dirs
}

/// Whether `settings` currently asks to follow the host terminal's palette.
pub fn follows_terminal(settings: &Settings) -> bool {
    matches!(settings.theme.as_deref(), Some("Terminal" | "Follow Terminal"))
}

/// The theme the settings ask for, and a line for the status bar when it could not be had.
pub fn load(settings: &Settings) -> (Theme, Option<String>) {
    load_from_with_palette(settings, &theme_dirs(), None)
}

/// [`load`] with the host terminal's reported palette (for `"Terminal"`).
pub fn load_with_palette(settings: &Settings, palette: &TerminalPalette) -> (Theme, Option<String>) {
    load_from_with_palette(settings, &theme_dirs(), Some(palette))
}

/// [`load`] looking in `dirs` (for tests).
pub fn load_from(settings: &Settings, dirs: &[PathBuf]) -> (Theme, Option<String>) {
    load_from_with_palette(settings, dirs, None)
}

/// [`load_from`] with an optional host terminal palette.
pub fn load_from_with_palette(
    settings: &Settings,
    dirs: &[PathBuf],
    palette: Option<&TerminalPalette>,
) -> (Theme, Option<String>) {
    let builtin = || Theme::builtin_with(&settings.theme_overrides);
    let name = match settings.theme.as_deref() {
        None => return (builtin(), None),
        Some(name) => name,
    };
    if let Some(theme) = Theme::builtin_named(name, palette, &settings.theme_overrides) {
        return (theme, None);
    }
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
    use bakedit_core::{ColorSlot, Rgb};
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
    fn builtin_choices_load_without_disk_files() {
        let mut pal = TerminalPalette::default();
        pal.set(ColorSlot::Background, Rgb::new(0x0a, 0x0b, 0x0c));
        for &(name, _) in BUILTIN_CHOICES {
            let settings = Settings { theme: Some(name.into()), ..Settings::default() };
            let (theme, warning) = load_from_with_palette(&settings, &[], Some(&pal));
            assert_eq!(warning, None, "built-in theme {name} should not warn");
            assert_eq!(theme.name, name);
        }
        let settings = Settings { theme: Some("Terminal".into()), ..Settings::default() };
        assert!(follows_terminal(&settings));
        let (theme, _) = load_from_with_palette(&settings, &[], Some(&pal));
        assert_eq!(theme.background.to_hex(), "#0a0b0c");
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
