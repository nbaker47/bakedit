//! Themes, read from Zed's theme JSON so an existing Zed theme can be dropped in unchanged.
//!
//! A Zed theme is a flat map of dotted keys to `#rrggbb[aa]` strings plus a `syntax` map.
//! [`Theme`] resolves that once into plain fields: every colour the UI needs has a value
//! (missing keys fall back along a sensible chain) and translucent colours are already
//! blended over the surface they sit on, because a terminal cell cannot be translucent.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::color::Rgb;
use crate::jsonc;

/// The theme bakedit ships with: the user's Zed "Ghostty" theme (SilkCircuit Neon).
const BUILTIN_THEME_JSON: &str = include_str!("../../../assets/themes/ghostty.json");

/// The `theme_overrides` from the user's Zed settings, so the built-in theme matches what
/// their Zed actually shows.
const BUILTIN_OVERRIDES: &[(&str, &str)] = &[
    ("panel.focused_border", "#82aaff"),
    ("element.active", "#82aaff55"),
    ("element.selected", "#82aaff45"),
    ("ghost_element.active", "#82aaff55"),
    ("ghost_element.selected", "#82aaff45"),
];

#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("theme file is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("theme file has no themes")]
    Empty,
    #[error("no theme named {0:?} in the theme file")]
    NotFound(String),
}

/// How one syntax capture is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyntaxStyle {
    pub color: Option<Rgb>,
    pub italic: bool,
    pub bold: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub name: String,
    pub dark: bool,

    // Surfaces
    pub background: Rgb,
    pub surface: Rgb,
    pub elevated_surface: Rgb,
    pub editor_bg: Rgb,
    pub gutter_bg: Rgb,
    pub active_line_bg: Rgb,
    pub panel_bg: Rgb,
    pub title_bar_bg: Rgb,
    pub status_bar_bg: Rgb,
    pub toolbar_bg: Rgb,
    pub tab_bar_bg: Rgb,
    pub tab_active_bg: Rgb,
    pub tab_inactive_bg: Rgb,
    pub scrollbar_track: Rgb,
    pub scrollbar_thumb: Rgb,

    // Borders
    pub border: Rgb,
    pub border_variant: Rgb,
    pub border_focused: Rgb,
    pub border_selected: Rgb,
    pub panel_focused_border: Rgb,

    // Text
    pub text: Rgb,
    pub text_muted: Rgb,
    pub text_placeholder: Rgb,
    pub text_accent: Rgb,
    pub editor_fg: Rgb,
    pub line_number: Rgb,
    pub active_line_number: Rgb,
    pub icon: Rgb,
    pub icon_muted: Rgb,

    // Interactive element backgrounds (blended over `background`)
    pub element_hover: Rgb,
    pub element_active: Rgb,
    pub element_selected: Rgb,
    pub ghost_element_hover: Rgb,
    pub ghost_element_active: Rgb,
    pub ghost_element_selected: Rgb,

    // Editor decorations
    pub cursor: Rgb,
    pub selection_bg: Rgb,
    pub search_match_bg: Rgb,
    pub indent_guide: Rgb,
    pub indent_guide_active: Rgb,
    pub wrap_guide: Rgb,
    pub matching_bracket_bg: Rgb,

    // Status
    pub error: Rgb,
    pub warning: Rgb,
    pub info: Rgb,
    pub success: Rgb,
    pub hint: Rgb,
    pub created: Rgb,
    pub modified: Rgb,
    pub deleted: Rgb,
    pub conflict: Rgb,
    pub renamed: Rgb,
    pub ignored: Rgb,
    pub hidden: Rgb,

    // Terminal
    pub terminal_bg: Rgb,
    pub terminal_fg: Rgb,
    pub terminal_bright_fg: Rgb,
    /// The 16 ANSI colours: 0-7 normal, 8-15 bright.
    pub ansi: [Rgb; 16],

    /// Colours for rainbow brackets, outermost first.
    pub accents: Vec<Rgb>,

    syntax: HashMap<String, SyntaxStyle>,
}

impl Theme {
    /// The built-in theme. Never fails: the JSON is compiled in and covered by a test.
    pub fn builtin() -> Theme {
        Theme::builtin_with(&Map::new())
    }

    /// The built-in theme with `overrides` (the user's `theme_overrides`, in Zed's key
    /// format) laid over it after the built-in overrides, so a user's override wins and the
    /// rest of the built-in ones stay. Never fails; an override that is not a colour is
    /// skipped like any unreadable key.
    pub fn builtin_with(overrides: &Map<String, Value>) -> Theme {
        let mut merged: Map<String, Value> = BUILTIN_OVERRIDES
            .iter()
            .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
            .collect();
        for (key, value) in overrides {
            merged.insert(key.clone(), value.clone());
        }
        Theme::from_zed_json(BUILTIN_THEME_JSON, None, Some(&merged))
            .expect("the built-in theme is valid")
    }

    /// Load a theme from a Zed theme family file (JSON with comments allowed).
    ///
    /// `name` picks one theme out of the family; `None` takes the first. `overrides` is laid
    /// over the theme's style map first, in the same key format (Zed's `theme_overrides`).
    pub fn from_zed_json(
        text: &str,
        name: Option<&str>,
        overrides: Option<&Map<String, Value>>,
    ) -> Result<Theme, ThemeError> {
        let family: Value = jsonc::from_str(text)?;
        let themes = family.get("themes").and_then(Value::as_array).ok_or(ThemeError::Empty)?;
        let entry = match name {
            Some(wanted) => themes
                .iter()
                .find(|t| t.get("name").and_then(Value::as_str) == Some(wanted))
                .ok_or_else(|| ThemeError::NotFound(wanted.to_string()))?,
            None => themes.first().ok_or(ThemeError::Empty)?,
        };
        let theme_name = entry.get("name").and_then(Value::as_str).unwrap_or("Untitled").to_string();
        let dark = entry.get("appearance").and_then(Value::as_str) != Some("light");
        let mut style = entry.get("style").and_then(Value::as_object).cloned().unwrap_or_default();
        if let Some(overrides) = overrides {
            for (key, value) in overrides {
                style.insert(key.clone(), value.clone());
            }
        }
        Ok(Theme::from_style(theme_name, dark, &style))
    }

    /// Resolve a Zed style map into a theme.
    pub fn from_style(name: String, dark: bool, style: &Map<String, Value>) -> Theme {
        let raw = |key: &str| -> Option<(Rgb, u8)> { style.get(key)?.as_str().and_then(Rgb::parse_hex) };
        // First key that is present, blended over `under` if translucent.
        let pick = |keys: &[&str], under: Rgb, fallback: Rgb| -> Rgb {
            keys.iter()
                .find_map(|key| raw(key))
                .map(|(rgb, alpha)| rgb.over(under, alpha))
                .unwrap_or(fallback)
        };

        let default_bg = if dark { Rgb::new(0x1e, 0x1e, 0x2e) } else { Rgb::new(0xfa, 0xfa, 0xfa) };
        let default_fg = if dark { Rgb::new(0xe0, 0xe0, 0xe0) } else { Rgb::new(0x24, 0x24, 0x24) };

        let background = pick(&["background"], default_bg, default_bg);
        let text = pick(&["text"], background, default_fg);
        let text_muted = pick(&["text.muted"], background, text.over(background, 150));

        let surface = pick(&["surface.background"], background, background);
        let elevated_surface = pick(&["elevated_surface.background"], background, text.over(background, 24));
        let editor_bg = pick(&["editor.background"], background, background);
        let gutter_bg = pick(&["editor.gutter.background"], editor_bg, editor_bg);
        let active_line_bg = pick(&["editor.active_line.background"], editor_bg, text.over(editor_bg, 12));
        let panel_bg = pick(&["panel.background"], background, surface);
        let title_bar_bg = pick(&["title_bar.background"], background, background);
        let status_bar_bg = pick(&["status_bar.background"], background, background);
        let toolbar_bg = pick(&["toolbar.background"], background, editor_bg);
        let tab_bar_bg = pick(&["tab_bar.background"], background, background);
        let tab_active_bg = pick(&["tab.active_background"], background, editor_bg);
        let tab_inactive_bg = pick(&["tab.inactive_background"], background, tab_bar_bg);
        let scrollbar_track = pick(&["scrollbar.track.background"], editor_bg, editor_bg);
        let scrollbar_thumb = pick(&["scrollbar.thumb.background"], editor_bg, text.over(editor_bg, 40));

        let border = pick(&["border"], background, text.over(background, 40));
        let border_variant = pick(&["border.variant"], background, text.over(background, 20));
        let border_focused = pick(&["border.focused"], background, text);
        let border_selected = pick(&["border.selected"], background, border_focused);
        let panel_focused_border = pick(&["panel.focused_border"], background, border_focused);

        let editor_fg = pick(&["editor.foreground"], editor_bg, text);
        let line_number = pick(&["editor.line_number"], gutter_bg, text_muted);
        let active_line_number = pick(&["editor.active_line_number"], gutter_bg, text);
        let icon = pick(&["icon"], background, text);
        let icon_muted = pick(&["icon.muted"], background, text_muted);

        let element_hover = pick(&["element.hover"], background, text.over(background, 16));
        let element_active = pick(&["element.active"], background, text.over(background, 40));
        let element_selected = pick(&["element.selected"], background, text.over(background, 32));
        let ghost_element_hover = pick(&["ghost_element.hover"], background, element_hover);
        let ghost_element_active = pick(&["ghost_element.active"], background, element_active);
        let ghost_element_selected = pick(&["ghost_element.selected"], background, element_selected);

        // The local player supplies the cursor and selection colours.
        let player = style.get("players").and_then(Value::as_array).and_then(|p| p.first());
        let player_color = |key: &str| -> Option<(Rgb, u8)> {
            player?.get(key)?.as_str().and_then(Rgb::parse_hex)
        };
        let cursor = player_color("cursor").map(|(rgb, a)| rgb.over(editor_bg, a)).unwrap_or(border_focused);
        let selection_bg = player_color("selection")
            .map(|(rgb, a)| rgb.over(editor_bg, a))
            .unwrap_or_else(|| cursor.over(editor_bg, 64));

        let terminal_bg = pick(&["terminal.background"], background, background);
        let terminal_fg = pick(&["terminal.foreground"], terminal_bg, text);
        let terminal_bright_fg = pick(&["terminal.bright_foreground"], terminal_bg, terminal_fg);

        const ANSI_NAMES: [&str; 8] = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];
        const ANSI_DARK: [[u8; 3]; 16] = [
            [0x1e, 0x1e, 0x2e], [0xf3, 0x8b, 0xa8], [0xa6, 0xe3, 0xa1], [0xf9, 0xe2, 0xaf],
            [0x89, 0xb4, 0xfa], [0xf5, 0xc2, 0xe7], [0x94, 0xe2, 0xd5], [0xba, 0xc2, 0xde],
            [0x58, 0x5b, 0x70], [0xf3, 0x8b, 0xa8], [0xa6, 0xe3, 0xa1], [0xf9, 0xe2, 0xaf],
            [0x89, 0xb4, 0xfa], [0xf5, 0xc2, 0xe7], [0x94, 0xe2, 0xd5], [0xff, 0xff, 0xff],
        ];
        let mut ansi = [Rgb::default(); 16];
        for (i, slot) in ansi.iter_mut().enumerate() {
            let key = if i < 8 {
                format!("terminal.ansi.{}", ANSI_NAMES[i])
            } else {
                format!("terminal.ansi.bright_{}", ANSI_NAMES[i - 8])
            };
            let [r, g, b] = ANSI_DARK[i];
            *slot = pick(&[key.as_str()], terminal_bg, Rgb::new(r, g, b));
        }
        let (red, green, yellow, blue, magenta, cyan) = (ansi[1], ansi[2], ansi[3], ansi[4], ansi[5], ansi[6]);

        let text_placeholder = pick(&["text.placeholder"], background, text_muted);
        let text_accent = pick(&["text.accent"], background, border_focused);

        let search_match_bg = pick(&["search.match_background"], editor_bg, yellow.over(editor_bg, 70));
        let indent_guide = pick(&["editor.indent_guide"], editor_bg, text.over(editor_bg, 38));
        let indent_guide_active = pick(&["editor.indent_guide_active"], editor_bg, text.over(editor_bg, 96));
        let wrap_guide = pick(&["editor.wrap_guide"], editor_bg, indent_guide);
        let matching_bracket_bg = pick(
            &["editor.document_highlight.bracket_background", "editor.document_highlight.read_background"],
            editor_bg,
            cursor.over(editor_bg, 70),
        );

        let error = pick(&["error"], background, red);
        let warning = pick(&["warning"], background, yellow);
        let info = pick(&["info"], background, blue);
        let success = pick(&["success"], background, green);
        let hint = pick(&["hint"], background, text_muted);
        let created = pick(&["version_control.added", "created"], background, green);
        let modified = pick(&["version_control.modified", "modified"], background, yellow);
        let deleted = pick(&["version_control.deleted", "deleted"], background, red);
        let conflict = pick(&["version_control.conflict", "conflict"], background, magenta);
        let renamed = pick(&["version_control.renamed", "renamed"], background, blue);
        let ignored = pick(&["version_control.ignored", "ignored"], background, text_muted.over(background, 170));
        let hidden = pick(&["hidden"], background, text_muted);

        let accents: Vec<Rgb> = match style.get("accents").and_then(Value::as_array) {
            Some(list) if !list.is_empty() => list
                .iter()
                .filter_map(|v| v.as_str().and_then(Rgb::parse_hex))
                .map(|(rgb, a)| rgb.over(editor_bg, a))
                .collect(),
            _ => vec![yellow, magenta, blue, green, cyan, red],
        };

        let mut syntax = HashMap::new();
        if let Some(map) = style.get("syntax").and_then(Value::as_object) {
            for (capture, value) in map {
                let color = value
                    .get("color")
                    .and_then(Value::as_str)
                    .and_then(Rgb::parse_hex)
                    .map(|(rgb, a)| rgb.over(editor_bg, a));
                let italic = value.get("font_style").and_then(Value::as_str) == Some("italic");
                let bold = value.get("font_weight").and_then(Value::as_u64).is_some_and(|w| w >= 600);
                syntax.insert(capture.clone(), SyntaxStyle { color, italic, bold });
            }
        }

        Theme {
            name,
            dark,
            background,
            surface,
            elevated_surface,
            editor_bg,
            gutter_bg,
            active_line_bg,
            panel_bg,
            title_bar_bg,
            status_bar_bg,
            toolbar_bg,
            tab_bar_bg,
            tab_active_bg,
            tab_inactive_bg,
            scrollbar_track,
            scrollbar_thumb,
            border,
            border_variant,
            border_focused,
            border_selected,
            panel_focused_border,
            text,
            text_muted,
            text_placeholder,
            text_accent,
            editor_fg,
            line_number,
            active_line_number,
            icon,
            icon_muted,
            element_hover,
            element_active,
            element_selected,
            ghost_element_hover,
            ghost_element_active,
            ghost_element_selected,
            cursor,
            selection_bg,
            search_match_bg,
            indent_guide,
            indent_guide_active,
            wrap_guide,
            matching_bracket_bg,
            error,
            warning,
            info,
            success,
            hint,
            created,
            modified,
            deleted,
            conflict,
            renamed,
            ignored,
            hidden,
            terminal_bg,
            terminal_fg,
            terminal_bright_fg,
            ansi,
            accents,
            syntax,
        }
    }

    /// Style for a syntax capture such as `function.method`. Falls back along the dotted
    /// name (`function.method` then `function`), so themes only need the broad categories.
    pub fn syntax(&self, capture: &str) -> Option<SyntaxStyle> {
        let mut name = capture;
        loop {
            if let Some(style) = self.syntax.get(name) {
                return Some(*style);
            }
            match name.rfind('.') {
                Some(dot) => name = &name[..dot],
                None => return None,
            }
        }
    }

    /// Every syntax capture this theme styles.
    pub fn syntax_captures(&self) -> impl Iterator<Item = &str> {
        self.syntax.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_theme_matches_the_zed_ghostty_theme() {
        let theme = Theme::builtin();
        assert_eq!(theme.name, "Ghostty");
        assert!(theme.dark);
        assert_eq!(theme.background.to_hex(), "#12101a");
        assert_eq!(theme.editor_fg.to_hex(), "#f8f8f2");
        assert_eq!(theme.line_number.to_hex(), "#768d8d");
        assert_eq!(theme.border_focused.to_hex(), "#82aaff");
        assert_eq!(theme.panel_focused_border.to_hex(), "#82aaff");
        assert_eq!(theme.cursor.to_hex(), "#82aaff");
        assert_eq!(theme.ansi[1].to_hex(), "#ff6363");
        assert_eq!(theme.ansi[15].to_hex(), "#ffffff");
    }

    #[test]
    fn builtin_overrides_are_applied_and_blended() {
        let theme = Theme::builtin();
        // element.selected is #82aaff45 from the settings override, not #82aaff38 from the file.
        let expected = Rgb::new(0x82, 0xaa, 0xff).over(theme.background, 0x45);
        assert_eq!(theme.element_selected, expected);
        assert_ne!(theme.element_selected, theme.background);
        // The selection is translucent in the file and must come out opaque but tinted.
        let expected = Rgb::new(0x82, 0xaa, 0xff).over(theme.editor_bg, 0x40);
        assert_eq!(theme.selection_bg, expected);
    }

    #[test]
    fn user_overrides_go_over_the_builtin_ones() {
        let mut overrides = Map::new();
        overrides.insert("editor.background".into(), Value::String("#000000".into()));
        overrides.insert("panel.focused_border".into(), Value::String("#ff0000".into()));
        let theme = Theme::builtin_with(&overrides);
        assert_eq!(theme.editor_bg.to_hex(), "#000000");
        assert_eq!(theme.panel_focused_border.to_hex(), "#ff0000", "the user's wins over the built-in override");
        // The built-in overrides the user did not mention stay.
        let expected = Rgb::new(0x82, 0xaa, 0xff).over(theme.background, 0x45);
        assert_eq!(theme.element_selected, expected);
        assert_eq!(Theme::builtin_with(&Map::new()), Theme::builtin());
    }

    #[test]
    fn syntax_lookup_falls_back_along_dots() {
        let theme = Theme::builtin();
        let keyword = theme.syntax("keyword").unwrap();
        assert_eq!(keyword.color.unwrap().to_hex(), "#ff00ff");
        assert_eq!(theme.syntax("keyword.control.flow"), Some(keyword));
        let comment = theme.syntax("comment.doc").unwrap();
        assert!(comment.italic);
        assert!(theme.syntax("title").unwrap().bold);
        assert_eq!(theme.syntax("string.escape").unwrap().color.unwrap().to_hex(), "#80ffea");
        assert_eq!(theme.syntax("no_such_capture"), None);
    }

    #[test]
    fn missing_keys_fall_back() {
        let json = r##"{"themes": [{"name": "Tiny", "appearance": "light", "style": {"background": "#ffffff", "text": "#000000"}}]}"##;
        let theme = Theme::from_zed_json(json, None, None).unwrap();
        assert!(!theme.dark);
        assert_eq!(theme.editor_bg, theme.background);
        assert_eq!(theme.editor_fg, theme.text);
        assert_ne!(theme.text_muted, theme.text);
        assert_eq!(theme.accents.len(), 6);
    }

    #[test]
    fn picks_a_theme_by_name() {
        let json = r##"{"themes": [
            {"name": "One", "appearance": "dark", "style": {"background": "#000000"}},
            {"name": "Two", "appearance": "dark", "style": {"background": "#111111"}}
        ]}"##;
        assert_eq!(Theme::from_zed_json(json, Some("Two"), None).unwrap().background.to_hex(), "#111111");
        assert_eq!(Theme::from_zed_json(json, None, None).unwrap().name, "One");
        assert!(matches!(Theme::from_zed_json(json, Some("Three"), None), Err(ThemeError::NotFound(_))));
        assert!(matches!(Theme::from_zed_json("{}", None, None), Err(ThemeError::Empty)));
    }
}
