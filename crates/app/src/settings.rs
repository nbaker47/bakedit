//! Settings.
//!
//! The file format is Zed's `settings.json` (JSON with comments) and the keys bakedit
//! understands keep Zed's names and meanings, so an existing Zed settings file can be read
//! as it is. Keys bakedit does not know are ignored. A handful of keys are bakedit's own
//! (`terminal.command`, `nav_layer`, `icons`, `link_click_modifier`, `formatters`,
//! `window`).
//!
//! `"window": true | false` says where a plain `bakedit` runs: `true` (the default on
//! macOS) opens a dedicated Ghostty window and returns at once (not over SSH), `false` (the
//! default elsewhere) runs in the terminal it was started from. `--here` and `--window`
//! override it for one run.
//!
//! Defaults are Zed's defaults (v1.29) wherever the setting exists there.
//!
//! Load order: built-in defaults, then `~/.config/bakedit/settings.json`. When that file
//! does not exist, `~/.config/zed/settings.json` is read instead, so a fresh install looks
//! and behaves like the user's Zed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bakedit_core::jsonc;
use serde_json::{Map, Value};

use crate::workspace::DockPosition;

/// Approximate width of one terminal cell in the pixels Zed's `default_width` settings use
/// (a 12 to 13 px monospace font), for converting those settings to columns.
const PIXELS_PER_CELL: f32 = 7.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoftWrap {
    None,
    EditorWidth,
    PreferredLineLength,
}

impl SoftWrap {
    fn parse(value: &str) -> Option<SoftWrap> {
        match value {
            "none" | "prefer_line" => Some(SoftWrap::None),
            "editor_width" => Some(SoftWrap::EditorWidth),
            "preferred_line_length" | "bounded" => Some(SoftWrap::PreferredLineLength),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Autosave {
    Off,
    /// Save when the editor loses the focus to another tab or panel.
    OnFocusChange,
    /// Save when the terminal window loses the focus.
    OnWindowChange,
    AfterDelay { milliseconds: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MinimapShow {
    Always,
    /// Shown when the editor is wide enough to spare the columns.
    Auto,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icons {
    /// Nerd Font glyphs.
    Nerd,
    /// Plain characters every font has.
    Ascii,
}

/// Which modifier turns a click on a path in the terminal into "open it in the editor".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkModifier {
    Cmd,
    Alt,
    Ctrl,
    /// A plain click opens links.
    None,
}

/// What the terminal panel runs when it opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalCommand {
    /// herdr, attaching to the user's session (falls back to the shell when herdr is not
    /// installed or bakedit is itself running inside herdr).
    Herdr,
    /// The user's login shell.
    Shell,
    /// Any other program and its arguments.
    Custom(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerminalSettings {
    pub dock: DockPosition,
    pub command: TerminalCommand,
    pub default_width: u16,
    pub scrollback: usize,
    pub copy_on_select: bool,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PanelSettings {
    pub dock: DockPosition,
    pub default_width: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectPanelSettings {
    pub dock: DockPosition,
    pub default_width: u16,
    pub auto_reveal_entries: bool,
    pub hide_gitignore: bool,
    pub git_status: bool,
    pub git_diff_on_select: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct LanguageSettings {
    pub tab_size: Option<u8>,
    pub hard_tabs: Option<bool>,
    pub soft_wrap: Option<SoftWrap>,
    pub preferred_line_length: Option<u16>,
    pub format_on_save: Option<bool>,
    pub remove_trailing_whitespace_on_save: Option<bool>,
    pub ensure_final_newline_on_save: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    // Appearance
    pub theme: Option<String>,
    pub theme_overrides: Map<String, Value>,
    pub icons: Icons,
    pub minimap: MinimapShow,
    pub minimap_max_columns: u16,
    pub line_numbers: bool,
    pub relative_line_numbers: bool,
    pub current_line_highlight: bool,
    pub indent_guides: bool,
    pub colorize_brackets: bool,
    pub cursor_blink: bool,
    pub breadcrumbs: bool,
    pub inline_blame: bool,

    // Editing
    pub vim_mode: bool,
    pub tab_size: u8,
    pub hard_tabs: bool,
    pub soft_wrap: SoftWrap,
    pub preferred_line_length: u16,
    pub use_autoclose: bool,
    pub use_auto_surround: bool,
    pub auto_indent: bool,
    pub auto_indent_on_paste: bool,
    pub vertical_scroll_margin: u16,
    pub horizontal_scroll_margin: u16,

    // Saving
    pub autosave: Autosave,
    pub format_on_save: bool,
    pub ensure_final_newline_on_save: bool,
    pub remove_trailing_whitespace_on_save: bool,

    // Search
    pub search_whole_word: bool,
    pub search_case_sensitive: bool,
    pub search_regex: bool,
    pub search_wrap: bool,
    pub seed_search_query_from_cursor: bool,

    // Workspace
    pub restore_on_startup: bool,
    pub preview_tabs: bool,
    pub close_panel_on_toggle: bool,
    pub terminal: TerminalSettings,
    pub git_panel: PanelSettings,
    pub project_panel: ProjectPanelSettings,

    // bakedit's own
    pub nav_layer: bool,
    /// `"window"`: a plain `bakedit` opens a dedicated Ghostty window (`true`) or runs in
    /// the terminal it was started from (`false`). On by default on macOS, the only place
    /// the window exists.
    pub window: bool,
    /// Show the usage strip (AI subscription quotas) on the bottom row.
    pub usage_meter: bool,
    /// Show Agent Dots (herdr AI agent monitor) in the status bar and as a floating overlay
    /// when leaving the bakedit window.
    pub agent_dots: bool,
    pub link_click_modifier: LinkModifier,
    /// Language name -> the command that formats that language (reads stdin, writes stdout).
    pub formatters: HashMap<String, Vec<String>>,

    /// Per-language overrides, keyed by the language's display name as in Zed ("Markdown").
    pub languages: HashMap<String, LanguageSettings>,
}

impl Default for Settings {
    fn default() -> Settings {
        let mut languages = HashMap::new();
        let wrap = |soft_wrap| LanguageSettings { soft_wrap: Some(soft_wrap), ..Default::default() };
        languages.insert(
            "Markdown".to_string(),
            LanguageSettings {
                remove_trailing_whitespace_on_save: Some(false),
                ..wrap(SoftWrap::EditorWidth)
            },
        );
        languages.insert("Plain Text".to_string(), wrap(SoftWrap::EditorWidth));
        languages.insert(
            "Git Commit".to_string(),
            LanguageSettings { preferred_line_length: Some(72), ..wrap(SoftWrap::EditorWidth) },
        );
        languages.insert("Go".to_string(), LanguageSettings { hard_tabs: Some(true), ..Default::default() });
        languages.insert(
            "Diff".to_string(),
            LanguageSettings {
                remove_trailing_whitespace_on_save: Some(false),
                ensure_final_newline_on_save: Some(false),
                ..Default::default()
            },
        );
        languages.insert("Dart".to_string(), LanguageSettings { tab_size: Some(2), ..Default::default() });

        Settings {
            theme: None,
            theme_overrides: Map::new(),
            icons: Icons::Nerd,
            minimap: MinimapShow::Never,
            minimap_max_columns: 80,
            line_numbers: true,
            relative_line_numbers: false,
            current_line_highlight: true,
            indent_guides: true,
            colorize_brackets: false,
            cursor_blink: true,
            breadcrumbs: true,
            inline_blame: true,

            vim_mode: true,
            tab_size: 4,
            hard_tabs: false,
            soft_wrap: SoftWrap::None,
            preferred_line_length: 80,
            use_autoclose: true,
            use_auto_surround: true,
            auto_indent: true,
            auto_indent_on_paste: true,
            vertical_scroll_margin: 3,
            horizontal_scroll_margin: 5,

            autosave: Autosave::Off,
            format_on_save: false,
            ensure_final_newline_on_save: true,
            remove_trailing_whitespace_on_save: true,

            search_whole_word: false,
            search_case_sensitive: false,
            search_regex: false,
            search_wrap: true,
            seed_search_query_from_cursor: true,

            restore_on_startup: true,
            preview_tabs: true,
            close_panel_on_toggle: false,
            terminal: TerminalSettings {
                dock: DockPosition::Left,
                command: TerminalCommand::Herdr,
                default_width: px_to_cells(640.0),
                scrollback: 10_000,
                copy_on_select: false,
                env: Vec::new(),
            },
            git_panel: PanelSettings { dock: DockPosition::Left, default_width: px_to_cells(360.0) },
            project_panel: ProjectPanelSettings {
                dock: DockPosition::Right,
                default_width: px_to_cells(240.0),
                auto_reveal_entries: true,
                hide_gitignore: false,
                git_status: true,
                git_diff_on_select: true,
            },

            nav_layer: true,
            window: cfg!(target_os = "macos"),
            usage_meter: true,
            agent_dots: false,
            link_click_modifier: LinkModifier::Cmd,
            formatters: HashMap::new(),

            languages,
        }
    }
}

fn px_to_cells(px: f32) -> u16 {
    (px / PIXELS_PER_CELL).round().clamp(10.0, 500.0) as u16
}

/// Where a settings layer came from, for messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsSource {
    Defaults,
    Bakedit(PathBuf),
    Zed(PathBuf),
}

/// The result of loading settings: the values, where they came from, and anything in the
/// file that could not be understood (shown to the user once, never fatal).
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedSettings {
    pub settings: Settings,
    pub source: SettingsSource,
    pub warnings: Vec<String>,
}

/// bakedit's configuration directory: `$XDG_CONFIG_HOME/bakedit`, else `~/.config/bakedit`.
/// (`~/.config` on every platform, next to the user's zed, herdr and ghostty folders.)
pub fn config_dir() -> PathBuf {
    config_home().join("bakedit")
}

fn config_home() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".config"),
    }
}

pub fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

pub fn keymap_path() -> PathBuf {
    config_dir().join("keymap.json")
}

fn zed_settings_path() -> PathBuf {
    config_home().join("zed").join("settings.json")
}

/// The text of a configuration file that does not exist yet, written when the user first
/// opens it (`zed::OpenSettingsFile`, `zed::OpenKeymap`).
///
/// For `settings.json` it is a copy of Zed's settings when `zed_settings` can be read:
/// bakedit has been reading that file until now and reads only its own once it exists, so
/// starting from a copy changes nothing. Otherwise, and for `keymap.json` (which is loaded
/// after the built-in keymap and so may start empty), a short note on the format around an
/// empty object or list.
pub fn first_version(name: &str, zed_settings: Option<&Path>) -> String {
    if name == "keymap.json" {
        return "// bakedit's own key bindings, in Zed's keymap format: https://zed.dev/docs/key-bindings\n\
                // They are read after the built-in keymap (zed: open default keymap), so they win.\n\
                // Set a key to null to switch a built-in binding off.\n[\n]\n"
            .to_string();
    }
    match zed_settings.and_then(|path| std::fs::read_to_string(path).ok()) {
        Some(zed) => format!(
            "// bakedit's settings, copied from Zed's settings.json when this file was first opened.\n\
             // From now on bakedit reads this file instead of Zed's.\n{zed}"
        ),
        None => "// bakedit's settings, in Zed's settings format: https://zed.dev/docs/configuring-zed\n{\n}\n".to_string(),
    }
}

/// Load the user's settings following the order in the module docs.
pub fn load() -> LoadedSettings {
    load_from(&settings_path(), &zed_settings_path())
}

/// [`load`] with explicit paths (for tests).
pub fn load_from(bakedit_file: &Path, zed_file: &Path) -> LoadedSettings {
    let candidates = [
        (bakedit_file, SettingsSource::Bakedit(bakedit_file.to_path_buf())),
        (zed_file, SettingsSource::Zed(zed_file.to_path_buf())),
    ];
    for (path, source) in candidates {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let mut warnings = Vec::new();
                let settings = Settings::parse(&text, &mut warnings);
                let warnings = warnings.into_iter().map(|w| format!("{}: {w}", path.display())).collect();
                return LoadedSettings { settings, source, warnings };
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                return LoadedSettings {
                    settings: Settings::default(),
                    source: SettingsSource::Defaults,
                    warnings: vec![format!("{}: {err}", path.display())],
                };
            }
        }
    }
    LoadedSettings { settings: Settings::default(), source: SettingsSource::Defaults, warnings: Vec::new() }
}

/// Update or insert top-level keys in `path` (creating the file and parent folder when
/// needed), keeping every other key intact.
pub fn save_values(path: &Path, pairs: &[(&str, Value)]) -> Result<(), String> {
    let mut map = match std::fs::read_to_string(path) {
        Ok(text) => match jsonc::from_str::<Value>(&text) {
            Ok(Value::Object(obj)) => obj,
            Ok(_) => return Err(format!("{} is not a JSON object", path.display())),
            Err(err) => return Err(format!("cannot parse {}: {err}", path.display())),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Map::new(),
        Err(err) => return Err(format!("cannot read {}: {err}", path.display())),
    };
    for (k, v) in pairs {
        map.insert((*k).to_string(), v.clone());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(&Value::Object(map))
        .map_err(|e| format!("cannot format settings JSON: {e}"))?;
    std::fs::write(path, format!("{text}\n"))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Update or insert a nested key (`section.key`) in `path` (creating the file, parent
/// folder, and section object when needed), keeping every other key intact.
pub fn save_nested_value(path: &Path, section: &str, key: &str, value: Value) -> Result<(), String> {
    let mut map = match std::fs::read_to_string(path) {
        Ok(text) => match jsonc::from_str::<Value>(&text) {
            Ok(Value::Object(obj)) => obj,
            Ok(_) => return Err(format!("{} is not a JSON object", path.display())),
            Err(err) => return Err(format!("cannot parse {}: {err}", path.display())),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Map::new(),
        Err(err) => return Err(format!("cannot read {}: {err}", path.display())),
    };
    let entry = map.entry(section.to_string()).or_insert_with(|| Value::Object(Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(Map::new());
    }
    if let Value::Object(obj) = entry {
        obj.insert(key.to_string(), value);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(&Value::Object(map))
        .map_err(|e| format!("cannot format settings JSON: {e}"))?;
    std::fs::write(path, format!("{text}\n"))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Reads typed values out of a JSON object, recording a warning for every value of the
/// wrong type so one bad line never takes the rest of the file down.
struct Reader<'a> {
    map: &'a Map<String, Value>,
    prefix: String,
    warnings: &'a mut Vec<String>,
}

impl<'a> Reader<'a> {
    fn warn(&mut self, key: &str, expected: &str) {
        self.warnings.push(format!("\"{}{key}\" should be {expected}", self.prefix));
    }

    fn bool(&mut self, key: &str, target: &mut bool) {
        match self.map.get(key) {
            None | Some(Value::Null) => {}
            Some(Value::Bool(b)) => *target = *b,
            Some(_) => self.warn(key, "true or false"),
        }
    }

    fn number<T: TryFrom<u64>>(&mut self, key: &str, min: u64, max: u64) -> Option<T> {
        match self.map.get(key) {
            None | Some(Value::Null) => None,
            Some(value) => {
                let number = value.as_u64().or_else(|| value.as_f64().filter(|f| *f >= 0.0).map(|f| f.round() as u64));
                match number.filter(|n| (min..=max).contains(n)).and_then(|n| T::try_from(n).ok()) {
                    Some(n) => Some(n),
                    None => {
                        self.warn(key, &format!("a number from {min} to {max}"));
                        None
                    }
                }
            }
        }
    }

    fn string(&mut self, key: &str) -> Option<&'a str> {
        match self.map.get(key) {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.as_str()),
            Some(_) => {
                self.warn(key, "a string");
                None
            }
        }
    }

    /// A string that must be one of a known set; `parse` returns `None` for unknown values.
    fn choice<T>(&mut self, key: &str, expected: &str, parse: impl Fn(&str) -> Option<T>) -> Option<T> {
        let value = self.string(key)?;
        let parsed = parse(value);
        if parsed.is_none() {
            self.warn(key, expected);
        }
        parsed
    }

    fn object(&mut self, key: &str) -> Option<&'a Map<String, Value>> {
        match self.map.get(key) {
            None | Some(Value::Null) => None,
            Some(Value::Object(map)) => Some(map),
            Some(_) => {
                self.warn(key, "an object");
                None
            }
        }
    }

    fn child(&mut self, key: &str) -> Option<Reader<'_>> {
        let map = self.object(key)?;
        Some(Reader { map, prefix: format!("{}{key}.", self.prefix), warnings: self.warnings })
    }
}

impl Settings {
    /// Parse a settings file over the defaults. Problems are appended to `warnings`.
    pub fn parse(text: &str, warnings: &mut Vec<String>) -> Settings {
        let mut settings = Settings::default();
        let value: Value = match jsonc::from_str(text) {
            Ok(value) => value,
            Err(err) => {
                warnings.push(format!("not valid JSON ({err}); using defaults"));
                return settings;
            }
        };
        let Some(map) = value.as_object() else {
            warnings.push("the top level should be an object; using defaults".to_string());
            return settings;
        };
        settings.apply(map, warnings);
        settings
    }

    fn apply(&mut self, map: &Map<String, Value>, warnings: &mut Vec<String>) {
        let mut r = Reader { map, prefix: String::new(), warnings };

        // "theme" is a name, or {"mode", "light", "dark"}. bakedit draws inside a terminal
        // with one fixed background, so of the pair it takes the dark theme.
        match map.get("theme") {
            Some(Value::String(name)) => self.theme = Some(name.clone()),
            Some(Value::Object(theme)) => {
                if let Some(name) = theme.get("dark").or_else(|| theme.get("light")).and_then(Value::as_str) {
                    self.theme = Some(name.to_string());
                }
            }
            None | Some(Value::Null) => {}
            Some(_) => r.warn("theme", "a theme name or an object"),
        }
        // Zed has had two spellings: a flat map, and a map keyed by theme name.
        if let Some(overrides) = r.object("experimental.theme_overrides") {
            self.theme_overrides.extend(overrides.clone());
        }
        if let Some(by_theme) = r.object("theme_overrides") {
            let named = self.theme.as_deref().and_then(|name| by_theme.get(name)).and_then(Value::as_object);
            match named {
                Some(overrides) => self.theme_overrides.extend(overrides.clone()),
                None if by_theme.values().all(|v| !v.is_object()) => self.theme_overrides.extend(by_theme.clone()),
                None => {}
            }
        }

        if let Some(icons) = r.choice("icons", "\"nerd\" or \"ascii\"", |v| match v {
            "nerd" => Some(Icons::Nerd),
            "ascii" => Some(Icons::Ascii),
            _ => None,
        }) {
            self.icons = icons;
        }

        if let Some(mut minimap) = r.child("minimap") {
            if let Some(show) = minimap.choice("show", "\"always\", \"auto\" or \"never\"", |v| match v {
                "always" => Some(MinimapShow::Always),
                "auto" => Some(MinimapShow::Auto),
                "never" => Some(MinimapShow::Never),
                _ => None,
            }) {
                self.minimap = show;
            }
            if let Some(columns) = minimap.number("max_width_columns", 10, 400) {
                self.minimap_max_columns = columns;
            }
        }

        if let Some(mut gutter) = r.child("gutter") {
            gutter.bool("line_numbers", &mut self.line_numbers);
        }
        match map.get("relative_line_numbers") {
            Some(Value::Bool(b)) => self.relative_line_numbers = *b,
            Some(Value::String(s)) => self.relative_line_numbers = s != "disabled",
            None | Some(Value::Null) => {}
            Some(_) => r.warn("relative_line_numbers", "true, false, \"enabled\" or \"disabled\""),
        }
        if let Some(highlight) = r.string("current_line_highlight") {
            self.current_line_highlight = highlight != "none";
        }
        if let Some(mut guides) = r.child("indent_guides") {
            guides.bool("enabled", &mut self.indent_guides);
        }
        r.bool("colorize_brackets", &mut self.colorize_brackets);
        r.bool("cursor_blink", &mut self.cursor_blink);
        if let Some(mut toolbar) = r.child("toolbar") {
            toolbar.bool("breadcrumbs", &mut self.breadcrumbs);
        }
        if let Some(mut git) = r.child("git") {
            if let Some(mut blame) = git.child("inline_blame") {
                blame.bool("enabled", &mut self.inline_blame);
            }
        }

        r.bool("vim_mode", &mut self.vim_mode);
        if let Some(n) = r.number("tab_size", 1, 16) {
            self.tab_size = n;
        }
        r.bool("hard_tabs", &mut self.hard_tabs);
        if let Some(wrap) = r.choice("soft_wrap", "\"none\", \"editor_width\" or \"preferred_line_length\"", SoftWrap::parse) {
            self.soft_wrap = wrap;
        }
        if let Some(n) = r.number("preferred_line_length", 20, 1000) {
            self.preferred_line_length = n;
        }
        r.bool("use_autoclose", &mut self.use_autoclose);
        r.bool("use_auto_surround", &mut self.use_auto_surround);
        match map.get("auto_indent") {
            Some(Value::Bool(b)) => self.auto_indent = *b,
            Some(Value::String(s)) => self.auto_indent = s != "none",
            None | Some(Value::Null) => {}
            Some(_) => r.warn("auto_indent", "true, false or a mode name"),
        }
        r.bool("auto_indent_on_paste", &mut self.auto_indent_on_paste);
        if let Some(n) = r.number("vertical_scroll_margin", 0, 50) {
            self.vertical_scroll_margin = n;
        }
        if let Some(n) = r.number("horizontal_scroll_margin", 0, 50) {
            self.horizontal_scroll_margin = n;
        }

        match map.get("autosave") {
            Some(Value::String(mode)) => match mode.as_str() {
                "off" => self.autosave = Autosave::Off,
                "on_focus_change" => self.autosave = Autosave::OnFocusChange,
                "on_window_change" => self.autosave = Autosave::OnWindowChange,
                _ => r.warn("autosave", "\"off\", \"on_focus_change\", \"on_window_change\" or {\"after_delay\": {\"milliseconds\": N}}"),
            },
            Some(Value::Object(mode)) => {
                let delay = mode.get("after_delay").and_then(|d| d.get("milliseconds")).and_then(Value::as_u64);
                match delay {
                    Some(milliseconds) => self.autosave = Autosave::AfterDelay { milliseconds },
                    None => r.warn("autosave", "{\"after_delay\": {\"milliseconds\": N}}"),
                }
            }
            None | Some(Value::Null) => {}
            Some(_) => r.warn("autosave", "a string or an object"),
        }
        self.format_on_save = parse_format_on_save(map.get("format_on_save")).unwrap_or(self.format_on_save);
        r.bool("ensure_final_newline_on_save", &mut self.ensure_final_newline_on_save);
        r.bool("remove_trailing_whitespace_on_save", &mut self.remove_trailing_whitespace_on_save);

        if let Some(mut search) = r.child("search") {
            search.bool("whole_word", &mut self.search_whole_word);
            search.bool("case_sensitive", &mut self.search_case_sensitive);
            search.bool("regex", &mut self.search_regex);
        }
        r.bool("search_wrap", &mut self.search_wrap);
        match map.get("seed_search_query_from_cursor") {
            Some(Value::String(mode)) => self.seed_search_query_from_cursor = mode != "never",
            Some(Value::Bool(b)) => self.seed_search_query_from_cursor = *b,
            _ => {}
        }

        if let Some(mode) = r.string("restore_on_startup") {
            self.restore_on_startup = mode != "none" && mode != "launchpad";
        }
        if let Some(mut tabs) = r.child("preview_tabs") {
            let mut enabled = true;
            let mut from_panel = true;
            tabs.bool("enabled", &mut enabled);
            tabs.bool("enable_preview_from_project_panel", &mut from_panel);
            self.preview_tabs = enabled && from_panel;
        }
        r.bool("close_panel_on_toggle", &mut self.close_panel_on_toggle);

        let dock = |v: &str| DockPosition::from_setting(v);
        let dock_hint = "\"left\", \"right\" or \"bottom\"";
        if let Some(mut terminal) = r.child("terminal") {
            if let Some(position) = terminal.choice("dock", dock_hint, dock) {
                self.terminal.dock = position;
            }
            if let Some(px) = terminal.number::<u32>("default_width", 40, 20_000) {
                self.terminal.default_width = px_to_cells(px as f32);
            }
            if let Some(lines) = terminal.number::<u32>("max_scroll_history_lines", 0, 1_000_000) {
                self.terminal.scrollback = lines as usize;
            }
            terminal.bool("copy_on_select", &mut self.terminal.copy_on_select);
            if let Some(env) = terminal.object("env") {
                self.terminal.env = env
                    .iter()
                    .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                    .collect();
            }
            match terminal.map.get("command") {
                None => {}
                Some(Value::Null) => self.terminal.command = TerminalCommand::Shell,
                Some(Value::String(line)) => self.terminal.command = parse_command_line(line),
                Some(Value::Array(parts)) => {
                    let words: Vec<String> = parts.iter().filter_map(|p| p.as_str().map(str::to_string)).collect();
                    if words.len() == parts.len() {
                        self.terminal.command = command_from_words(words);
                    } else {
                        terminal.warn("command", "a string, an array of strings, or null");
                    }
                }
                Some(_) => terminal.warn("command", "a string, an array of strings, or null"),
            }
        }
        if let Some(mut panel) = r.child("git_panel") {
            if let Some(position) = panel.choice("dock", dock_hint, dock) {
                self.git_panel.dock = position;
            }
            if let Some(px) = panel.number::<u32>("default_width", 40, 20_000) {
                self.git_panel.default_width = px_to_cells(px as f32);
            }
        }
        if let Some(mut panel) = r.child("project_panel") {
            if let Some(position) = panel.choice("dock", dock_hint, dock) {
                self.project_panel.dock = position;
            }
            if let Some(px) = panel.number::<u32>("default_width", 40, 20_000) {
                self.project_panel.default_width = px_to_cells(px as f32);
            }
            panel.bool("auto_reveal_entries", &mut self.project_panel.auto_reveal_entries);
            panel.bool("hide_gitignore", &mut self.project_panel.hide_gitignore);
            panel.bool("git_status", &mut self.project_panel.git_status);
            panel.bool("git_diff_on_select", &mut self.project_panel.git_diff_on_select);
        }

        match map.get("nav_layer") {
            Some(Value::Bool(b)) => self.nav_layer = *b,
            Some(Value::Object(layer)) => {
                if let Some(enabled) = layer.get("enabled").and_then(Value::as_bool) {
                    self.nav_layer = enabled;
                }
            }
            None | Some(Value::Null) => {}
            Some(_) => r.warn("nav_layer", "true, false or {\"enabled\": bool}"),
        }
        r.bool("window", &mut self.window);
        match map.get("agent_dots") {
            Some(Value::Bool(b)) => self.agent_dots = *b,
            Some(Value::Object(dots)) => {
                if let Some(enabled) = dots.get("enabled").and_then(Value::as_bool) {
                    self.agent_dots = enabled;
                }
            }
            None | Some(Value::Null) => {}
            Some(_) => r.warn("agent_dots", "true, false or {\"enabled\": bool}"),
        }
        match map.get("usage_meter") {
            Some(Value::Bool(b)) => self.usage_meter = *b,
            Some(Value::Object(meter)) => {
                if let Some(show) = meter.get("show").and_then(Value::as_bool) {
                    self.usage_meter = show;
                }
            }
            None | Some(Value::Null) => {}
            Some(_) => r.warn("usage_meter", "true, false or {\"show\": bool}"),
        }
        if let Some(modifier) = r.choice("link_click_modifier", "\"cmd\", \"alt\", \"ctrl\" or \"none\"", |v| match v {
            "cmd" => Some(LinkModifier::Cmd),
            "alt" => Some(LinkModifier::Alt),
            "ctrl" => Some(LinkModifier::Ctrl),
            "none" => Some(LinkModifier::None),
            _ => None,
        }) {
            self.link_click_modifier = modifier;
        }
        if let Some(formatters) = r.object("formatters") {
            for (language, command) in formatters {
                let words: Option<Vec<String>> = match command {
                    Value::String(line) => Some(split_words(line)),
                    Value::Array(parts) => parts.iter().map(|p| p.as_str().map(str::to_string)).collect(),
                    _ => None,
                };
                match words {
                    Some(words) if !words.is_empty() => {
                        self.formatters.insert(language.clone(), words);
                    }
                    _ => r.warn(&format!("formatters.{language}"), "a command line or an array of strings"),
                }
            }
        }

        if let Some(languages) = r.object("languages") {
            for (name, value) in languages {
                let Some(language) = value.as_object() else { continue };
                let entry = self.languages.entry(name.clone()).or_default();
                let mut lr = Reader { map: language, prefix: format!("languages.{name}."), warnings: r.warnings };
                if let Some(n) = lr.number("tab_size", 1, 16) {
                    entry.tab_size = Some(n);
                }
                if let Some(Value::Bool(b)) = language.get("hard_tabs") {
                    entry.hard_tabs = Some(*b);
                }
                if let Some(wrap) = lr.choice("soft_wrap", "\"none\" or \"editor_width\"", SoftWrap::parse) {
                    entry.soft_wrap = Some(wrap);
                }
                if let Some(n) = lr.number("preferred_line_length", 20, 1000) {
                    entry.preferred_line_length = Some(n);
                }
                if let Some(on) = parse_format_on_save(language.get("format_on_save")) {
                    entry.format_on_save = Some(on);
                }
                if let Some(Value::Bool(b)) = language.get("remove_trailing_whitespace_on_save") {
                    entry.remove_trailing_whitespace_on_save = Some(*b);
                }
                if let Some(Value::Bool(b)) = language.get("ensure_final_newline_on_save") {
                    entry.ensure_final_newline_on_save = Some(*b);
                }
            }
        }
    }

    /// The overrides for a language, by its display name.
    pub fn language(&self, name: &str) -> Option<&LanguageSettings> {
        self.languages.get(name)
    }
}

fn parse_format_on_save(value: Option<&Value>) -> Option<bool> {
    match value? {
        Value::Bool(b) => Some(*b),
        Value::String(mode) => Some(mode == "on"),
        _ => None,
    }
}

/// Split a command line on whitespace, honouring single and double quotes and backslash
/// escapes outside single quotes. Enough for a configured command; not a shell.
fn split_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('\''), c) => current.push(c),
            (_, '\\') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                    in_word = true;
                }
            }
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            (None, c) => {
                current.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(current);
    }
    words
}

fn parse_command_line(line: &str) -> TerminalCommand {
    command_from_words(split_words(line))
}

impl TerminalCommand {
    /// A command line as `--terminal` takes it: `herdr`, `shell` (or nothing) for the login
    /// shell, anything else run as it is written (quotes group words; no shell involved).
    pub fn parse(line: &str) -> TerminalCommand {
        parse_command_line(line)
    }
}

fn command_from_words(words: Vec<String>) -> TerminalCommand {
    match words.as_slice() {
        [] => TerminalCommand::Shell,
        [only] if only == "shell" => TerminalCommand::Shell,
        [only] if only == "herdr" => TerminalCommand::Herdr,
        _ => TerminalCommand::Custom(words),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The user's real Zed settings file, as of 2026-11-22.
    const ZED_SETTINGS: &str = r##"
    // Zed settings
    {
      "fullscreen_mode": "simple",
      "cli_default_open_behavior": "existing_window",
      "icon_theme": "Catppuccin Frappé",
      "agent": { "button": false, "favorite_models": [], "model_parameters": [] },
      "terminal": {
        "dock": "left",
        "max_scroll_history_lines": 10000,
        "path_hyperlink_timeout_ms": 25,
        "font_family": "MesloLGSDZ Nerd Font Mono",
        "font_size": 12
      },
      "git_panel": { "dock": "left", "default_width": 627 },
      "project_panel": { "dock": "right", "default_width": 249, "auto_fold_dirs": false },
      "minimap": { "show": "always" },
      "autosave": "on_focus_change",
      "colorize_brackets": true,
      "auto_indent_on_paste": false,
      "use_on_type_format": false,
      "ensure_final_newline_on_save": true,
      "format_on_save": "on",
      "session": { "trust_all_worktrees": true },
      "base_keymap": "VSCode",
      "vim_mode": true,
      "restore_on_startup": "last_session",
      "ui_font_size": 13,
      "buffer_font_size": 11,
      "theme": { "mode": "system", "light": "Ayu Light", "dark": "Ghostty", },
      "experimental.theme_overrides": {
        "panel.focused_border": "#82aaff",
        "element.active": "#82aaff55"
      },
      "theme_overrides": {
        "Ghostty": { "panel.focused_border": "#82aaff", "element.selected": "#82aaff45" }
      },
    }"##;

    fn parse(text: &str) -> (Settings, Vec<String>) {
        let mut warnings = Vec::new();
        let settings = Settings::parse(text, &mut warnings);
        (settings, warnings)
    }

    #[test]
    fn reads_the_users_zed_settings_without_complaint() {
        let (s, warnings) = parse(ZED_SETTINGS);
        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(s.theme.as_deref(), Some("Ghostty"));
        assert_eq!(s.theme_overrides["panel.focused_border"], "#82aaff");
        assert_eq!(s.theme_overrides["element.active"], "#82aaff55");
        assert_eq!(s.theme_overrides["element.selected"], "#82aaff45");
        assert!(s.vim_mode);
        assert_eq!(s.minimap, MinimapShow::Always);
        assert_eq!(s.autosave, Autosave::OnFocusChange);
        assert!(s.colorize_brackets);
        assert!(!s.auto_indent_on_paste);
        assert!(s.format_on_save);
        assert!(s.ensure_final_newline_on_save);
        assert!(s.restore_on_startup);
        assert_eq!(s.terminal.dock, DockPosition::Left);
        assert_eq!(s.terminal.scrollback, 10_000);
        assert_eq!(s.git_panel.dock, DockPosition::Left);
        assert_eq!(s.git_panel.default_width, 84);
        assert_eq!(s.project_panel.dock, DockPosition::Right);
        assert_eq!(s.project_panel.default_width, 33);
        // Untouched keys keep Zed's defaults.
        assert_eq!(s.tab_size, 4);
        assert!(s.remove_trailing_whitespace_on_save);
        assert_eq!(s.terminal.command, TerminalCommand::Herdr);
        assert!(s.nav_layer);
    }

    #[test]
    fn defaults_mirror_zed() {
        let s = Settings::default();
        assert!(s.vim_mode);
        assert_eq!(s.autosave, Autosave::Off);
        assert_eq!(s.minimap, MinimapShow::Never);
        assert_eq!(s.soft_wrap, SoftWrap::None);
        assert_eq!(s.language("Markdown").unwrap().soft_wrap, Some(SoftWrap::EditorWidth));
        assert_eq!(s.language("Markdown").unwrap().remove_trailing_whitespace_on_save, Some(false));
        assert_eq!(s.language("Go").unwrap().hard_tabs, Some(true));
        assert_eq!(s.language("Git Commit").unwrap().preferred_line_length, Some(72));
        assert_eq!(s.terminal.default_width, 85);
    }

    #[test]
    fn terminal_command_forms() {
        let command = |json: &str| parse(&format!("{{\"terminal\": {{\"command\": {json}}}}}")).0.terminal.command;
        assert_eq!(command("\"herdr\""), TerminalCommand::Herdr);
        assert_eq!(command("\"\""), TerminalCommand::Shell);
        assert_eq!(command("\"shell\""), TerminalCommand::Shell);
        assert_eq!(command("null"), TerminalCommand::Shell);
        assert_eq!(
            command("\"herdr --session work\""),
            TerminalCommand::Custom(vec!["herdr".into(), "--session".into(), "work".into()])
        );
        assert_eq!(
            command("[\"tmux\", \"new\", \"-A\", \"-s\", \"my session\"]"),
            TerminalCommand::Custom(vec!["tmux".into(), "new".into(), "-A".into(), "-s".into(), "my session".into()])
        );
        assert_eq!(
            command("\"sh -c 'echo \\\"a b\\\"'\""),
            TerminalCommand::Custom(vec!["sh".into(), "-c".into(), "echo \"a b\"".into()])
        );
    }

    #[test]
    fn bakedit_keys() {
        let (s, warnings) = parse(
            r#"{
              "nav_layer": false,
              "icons": "ascii",
              "link_click_modifier": "alt",
              "formatters": { "Rust": ["rustfmt", "--edition", "2021"], "Python": "ruff format -" },
              "languages": { "Markdown": { "soft_wrap": "none", "tab_size": 2 }, "Rust": { "format_on_save": "off" } },
              "autosave": { "after_delay": { "milliseconds": 750 } }
            }"#,
        );
        assert_eq!(warnings, Vec::<String>::new());
        assert!(!s.nav_layer);
        assert_eq!(s.icons, Icons::Ascii);
        assert_eq!(s.link_click_modifier, LinkModifier::Alt);
        assert_eq!(s.formatters["Rust"], vec!["rustfmt", "--edition", "2021"]);
        assert_eq!(s.formatters["Python"], vec!["ruff", "format", "-"]);
        assert_eq!(s.language("Markdown").unwrap().soft_wrap, Some(SoftWrap::None));
        assert_eq!(s.language("Markdown").unwrap().tab_size, Some(2));
        // The built-in Markdown override that was not mentioned survives.
        assert_eq!(s.language("Markdown").unwrap().remove_trailing_whitespace_on_save, Some(false));
        assert_eq!(s.language("Rust").unwrap().format_on_save, Some(false));
        assert_eq!(s.autosave, Autosave::AfterDelay { milliseconds: 750 });
        let (s, _) = parse(r#"{"nav_layer": {"enabled": false}}"#);
        assert!(!s.nav_layer);
        assert!(s.usage_meter, "on unless switched off");
        assert!(!parse(r#"{"usage_meter": false}"#).0.usage_meter);
        assert!(!parse(r#"{"usage_meter": {"show": false}}"#).0.usage_meter);
    }

    #[test]
    fn the_window_setting() {
        assert_eq!(Settings::default().window, cfg!(target_os = "macos"), "on where Ghostty.app windows exist");
        assert_eq!(parse(ZED_SETTINGS).0.window, Settings::default().window, "Zed has no such key");
        assert!(!parse(r#"{"window": false}"#).0.window);
        assert!(parse(r#"{"window": true}"#).0.window);
        let (s, warnings) = parse(r#"{"window": "here"}"#);
        assert_eq!(s.window, Settings::default().window, "a bad value keeps the default");
        assert_eq!(warnings, ["\"window\" should be true or false"]);
    }

    #[test]
    fn bad_values_warn_and_keep_the_default() {
        let (s, warnings) = parse(
            r#"{
              "tab_size": "four",
              "vim_mode": "yes",
              "minimap": { "show": "sometimes" },
              "terminal": { "dock": "top", "command": 7 },
              "autosave": "whenever",
              "colorize_brackets": true
            }"#,
        );
        assert_eq!(s.tab_size, 4);
        assert!(s.vim_mode);
        assert_eq!(s.minimap, MinimapShow::Never);
        assert_eq!(s.terminal.dock, DockPosition::Left);
        assert_eq!(s.terminal.command, TerminalCommand::Herdr);
        assert_eq!(s.autosave, Autosave::Off);
        assert!(s.colorize_brackets, "the good line still applies");
        assert_eq!(warnings.len(), 6, "{warnings:#?}");
        assert!(warnings.iter().any(|w| w.contains("\"terminal.dock\"")));
        assert!(warnings.iter().any(|w| w.contains("\"minimap.show\"")));
    }

    #[test]
    fn broken_json_falls_back_to_defaults() {
        let (s, warnings) = parse("{ \"tab_size\": ");
        assert_eq!(s, Settings::default());
        assert_eq!(warnings.len(), 1);
        let (s, warnings) = parse("[1, 2]");
        assert_eq!(s, Settings::default());
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn load_prefers_bakedit_then_zed_then_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let bakedit = dir.path().join("bakedit.json");
        let zed = dir.path().join("zed.json");

        let loaded = load_from(&bakedit, &zed);
        assert_eq!(loaded.source, SettingsSource::Defaults);
        assert_eq!(loaded.settings, Settings::default());

        std::fs::write(&zed, r#"{"tab_size": 2}"#).unwrap();
        let loaded = load_from(&bakedit, &zed);
        assert_eq!(loaded.source, SettingsSource::Zed(zed.clone()));
        assert_eq!(loaded.settings.tab_size, 2);

        std::fs::write(&bakedit, r#"{"tab_size": 8, "vim_mode": 3}"#).unwrap();
        let loaded = load_from(&bakedit, &zed);
        assert_eq!(loaded.source, SettingsSource::Bakedit(bakedit.clone()));
        assert_eq!(loaded.settings.tab_size, 8);
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("bakedit.json"));
    }

    #[test]
    fn splits_command_lines() {
        assert_eq!(split_words("  a  b\tc "), vec!["a", "b", "c"]);
        assert_eq!(split_words("a 'b c' \"d e\""), vec!["a", "b c", "d e"]);
        assert_eq!(split_words("a\\ b ''"), vec!["a b", ""]);
        assert_eq!(split_words(""), Vec::<String>::new());
    }

    #[test]
    fn save_nested_value_preserves_siblings_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"tab_size": 2, "project_panel": {"dock": "left"}}"#).unwrap();

        save_nested_value(&path, "project_panel", "git_diff_on_select", Value::Bool(false)).unwrap();
        let loaded = load_from(&path, &dir.path().join("missing.json"));
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert_eq!(loaded.settings.tab_size, 2);
        assert_eq!(loaded.settings.project_panel.dock, DockPosition::Left);
        assert!(!loaded.settings.project_panel.git_diff_on_select);
    }
}
