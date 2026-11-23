//! The action catalogue: what the command palette lists, and how an action's name reads.

use bakedit_core::Action;

/// `editor::MoveToBeginningOfLine` -> `editor: move to beginning of line`, the way Zed's
/// command palette words it.
pub fn title(name: &str) -> String {
    let (namespace, action) = name.split_once("::").unwrap_or(("", name));
    let mut words = String::new();
    let chars: Vec<char> = action.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let starts_word = c.is_uppercase()
            && i > 0
            && (chars[i - 1].is_lowercase()
                || chars[i - 1].is_ascii_digit()
                || chars.get(i + 1).is_some_and(|next| next.is_lowercase()));
        if starts_word || (c.is_ascii_digit() && i > 0 && chars[i - 1].is_alphabetic()) {
            words.push(' ');
        }
        if c == '_' {
            words.push(' ');
        } else {
            words.extend(c.to_lowercase());
        }
    }
    if namespace.is_empty() {
        words
    } else {
        format!("{}: {}", namespace.replace('_', " "), words)
    }
}

/// One command palette entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub title: String,
    pub action: Action,
}

/// (action name, JSON arguments, title override). Actions that need arguments chosen at run
/// time (a line number, a path) are not listed: they have their own prompts.
const PALETTE: &[(&str, Option<&str>, Option<&str>)] = &[
    // Files
    ("workspace::Save", None, None),
    ("workspace::SaveAs", None, None),
    ("workspace::SaveAll", None, None),
    ("workspace::SaveWithoutFormat", None, None),
    ("workspace::NewFile", None, None),
    ("file_finder::Toggle", None, Some("file finder: open a file")),
    ("pane::DeploySearch", None, Some("project search: find in files")),
    ("pane::DeploySearch", Some(r#"{"replace_enabled": true}"#), Some("project search: replace in files")),
    ("tab_switcher::Toggle", None, Some("tab switcher: switch tab")),
    ("pane::CloseActiveItem", None, Some("pane: close tab")),
    ("pane::CloseOtherItems", None, Some("pane: close other tabs")),
    ("pane::CloseCleanItems", None, Some("pane: close saved tabs")),
    ("pane::CloseAllItems", None, Some("pane: close all tabs")),
    ("pane::ReopenClosedItem", None, Some("pane: reopen closed tab")),
    ("pane::ActivateNextItem", None, Some("pane: next tab")),
    ("pane::ActivatePreviousItem", None, Some("pane: previous tab")),
    ("pane::GoBack", None, None),
    ("pane::GoForward", None, None),
    ("pane::RevealInProjectPanel", None, None),
    ("editor::RevealInFileManager", None, Some("editor: reveal in Finder")),
    ("editor::CopyPath", None, None),
    ("workspace::CopyRelativePath", None, None),
    ("editor::CopyPermalinkToLine", None, None),
    // Panels and layout
    ("terminal_panel::Toggle", None, None),
    ("terminal_panel::ToggleZoom", None, Some("terminal panel: toggle fullscreen")),
    ("workspace::NewTerminal", None, None),
    ("git_panel::Toggle", None, None),
    ("git_panel::ToggleFocus", None, None),
    ("project_panel::Toggle", None, None),
    ("project_panel::ToggleFocus", None, None),
    ("workspace::FocusCenterPane", None, Some("workspace: focus the editor")),
    ("workspace::ToggleZoom", None, Some("workspace: toggle fullscreen for the focused pane")),
    ("workspace::ToggleAllDocks", None, None),
    ("workspace::ToggleLeftDock", None, None),
    ("workspace::ToggleRightDock", None, None),
    ("workspace::CloseActiveDock", None, None),
    ("workspace::IncreaseActiveDockSize", None, None),
    ("workspace::DecreaseActiveDockSize", None, None),
    ("workspace::ResetActiveDockSize", None, None),
    // Git
    ("git::Diff", None, Some("git: project diff")),
    ("git::Commit", None, None),
    ("git::Amend", None, None),
    ("git::StageAll", None, None),
    ("git::UnstageAll", None, None),
    ("git::Fetch", None, None),
    ("git::Pull", None, None),
    ("git::PullRebase", None, None),
    ("git::Push", None, None),
    ("git::ForcePush", None, Some("git: force push (with lease)")),
    ("git::RestoreTrackedFiles", None, Some("git: discard all tracked changes")),
    ("git::TrashUntrackedFiles", None, None),
    ("branches::OpenRecent", None, Some("git: switch branch")),
    // Editing
    ("editor::Undo", None, None),
    ("editor::Redo", None, None),
    ("editor::Cut", None, None),
    ("editor::Copy", None, None),
    ("editor::Paste", None, None),
    ("editor::SelectAll", None, None),
    ("editor::SelectLine", None, None),
    ("editor::SelectNext", Some(r#"{"replace_newest": false}"#), Some("editor: select next occurrence")),
    ("editor::SelectAllMatches", None, Some("editor: select all occurrences")),
    ("editor::AddSelectionAbove", None, Some("editor: add cursor above")),
    ("editor::AddSelectionBelow", None, Some("editor: add cursor below")),
    ("editor::SelectLargerSyntaxNode", None, Some("editor: expand selection")),
    ("editor::SelectSmallerSyntaxNode", None, Some("editor: shrink selection")),
    ("editor::ToggleComments", None, None),
    ("editor::ToggleBlockComments", None, None),
    ("editor::Indent", None, None),
    ("editor::Outdent", None, None),
    ("editor::MoveLineUp", None, None),
    ("editor::MoveLineDown", None, None),
    ("editor::DuplicateLineUp", None, None),
    ("editor::DuplicateLineDown", None, None),
    ("editor::DeleteLine", None, None),
    ("editor::JoinLines", None, None),
    ("editor::Transpose", None, None),
    ("editor::Format", None, Some("editor: format document")),
    ("editor::MoveToEnclosingBracket", None, Some("editor: go to matching bracket")),
    ("editor::MoveToBeginning", None, Some("editor: go to start of file")),
    ("editor::MoveToEnd", None, Some("editor: go to end of file")),
    ("go_to_line::Toggle", None, Some("go to line")),
    ("outline::Toggle", None, Some("outline: go to symbol")),
    ("buffer_search::Deploy", None, Some("buffer search: find in file")),
    ("search::ToggleReplace", None, None),
    ("search::ToggleCaseSensitive", None, None),
    ("search::ToggleWholeWord", None, None),
    ("search::ToggleRegex", None, None),
    // View
    ("editor::ToggleSoftWrap", None, None),
    ("editor::ToggleLineNumbers", None, None),
    ("editor::ToggleMinimap", None, None),
    ("editor::ToggleRelativeLineNumbers", None, None),
    ("editor::ScrollCursorCenter", None, None),
    // Terminal and herdr
    ("terminal::Clear", None, None),
    ("terminal::Copy", None, None),
    ("terminal::Paste", None, None),
    ("terminal::OpenLinks", None, Some("terminal: open a path or link shown in the terminal")),
    ("herdr::ToggleZoom", None, Some("herdr: zoom pane")),
    ("herdr::ToggleCollapse", None, Some("herdr: collapse pane")),
    ("herdr::ToggleExpand", None, Some("herdr: expand pane")),
    ("herdr::NextWorkspace", None, None),
    ("herdr::PreviousWorkspace", None, None),
    // bakedit
    ("bakedit::ToggleNavLayer", None, Some("bakedit: toggle the nav layer (hold ; for arrows)")),
    ("bakedit::ToggleEditMode", None, Some("bakedit: toggle nav and edit modes on or off")),
    ("bakedit::ShowKeybindings", None, Some("bakedit: show key bindings")),
    ("bakedit::ReloadConfig", None, Some("bakedit: reload settings and keymap")),
    ("zed::OpenSettingsFile", None, Some("bakedit: open settings")),
    ("zed::OpenKeymap", None, Some("bakedit: open keymap")),
    ("zed::OpenDefaultKeymap", None, Some("bakedit: open the built-in keymap")),
    ("zed::Quit", None, Some("bakedit: quit")),
];

/// Everything the command palette offers, in catalogue order.
pub fn palette() -> Vec<Command> {
    PALETTE
        .iter()
        .map(|&(name, args, title_override)| Command {
            title: title_override.map(str::to_string).unwrap_or_else(|| title(name)),
            action: Action {
                name: name.to_string(),
                args: args.map(|json| serde_json::from_str(json).expect("catalogue arguments are valid JSON")),
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn titles_read_like_zeds() {
        assert_eq!(title("editor::MoveToBeginningOfLine"), "editor: move to beginning of line");
        assert_eq!(title("terminal_panel::Toggle"), "terminal panel: toggle");
        assert_eq!(title("workspace::SaveAs"), "workspace: save as");
        assert_eq!(title("git::Diff"), "git: diff");
        assert_eq!(title("editor::FoldAtLevel_2"), "editor: fold at level 2");
        assert_eq!(title("zed::OpenSettingsFile"), "zed: open settings file");
        assert_eq!(title("editor::ToggleLSPStuff"), "editor: toggle lsp stuff");
        assert_eq!(title("Standalone"), "standalone");
    }

    #[test]
    fn the_palette_is_well_formed() {
        let commands = palette();
        assert!(commands.len() > 80);
        let mut titles = HashSet::new();
        for command in &commands {
            assert!(command.action.name.contains("::"), "{:?}", command.action.name);
            assert!(!command.title.is_empty());
            assert_eq!(command.title, command.title.trim());
            assert!(titles.insert(command.title.clone()), "duplicate title {:?}", command.title);
        }
    }
}
