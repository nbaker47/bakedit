//! File and folder icons as the views draw them: a Nerd Font glyph in its colour, or a plain
//! character every font has, depending on the `icons` setting.

use bakedit_core::Rgb;

use crate::settings::{Icons, Settings};

/// The glyph and colour for a file or folder called `name`. The glyph is always followed by
/// one space when drawn (the Nerd Font glyphs are drawn wider than one cell).
pub fn file_icon(settings: &Settings, name: &str, is_dir: bool, expanded: bool) -> (String, Rgb) {
    let icon = match settings.icons {
        Icons::Nerd => bakedit_project::icon_for(name, is_dir, expanded),
        Icons::Ascii => bakedit_project::ascii_icon_for(name, is_dir, expanded),
    };
    (icon.glyph.to_string(), icon.color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_the_icons_setting() {
        let mut settings = Settings::default();
        let (nerd, _) = file_icon(&settings, "main.rs", false, false);
        settings.icons = Icons::Ascii;
        let (ascii, _) = file_icon(&settings, "main.rs", false, false);
        assert_ne!(nerd, ascii);
        assert!(ascii.chars().all(|c| !('\u{e000}'..='\u{f8ff}').contains(&c)), "no private-use glyphs: {ascii:?}");
        let (open, _) = file_icon(&settings, "src", true, true);
        let (closed, _) = file_icon(&settings, "src", true, false);
        assert_ne!(open, closed, "an open folder looks open");
    }
}
