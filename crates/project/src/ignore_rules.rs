//! One place that decides which paths the project hides and which it treats as ignored.
//!
//! The tree, the index, project search and the watcher all ask the same questions here, so
//! a path cannot be visible in one and missing from another by accident.

use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use ignore::{IncrementalIgnore, WalkBuilder};

/// Names that are never shown, indexed, searched or reported, at any depth.
///
/// This is Zed's default `file_scan_exclusions`.
pub const ALWAYS_HIDDEN: &[&str] = &[
    ".git",
    ".svn",
    ".hg",
    ".jj",
    ".sl",
    ".repo",
    "CVS",
    ".DS_Store",
    "Thumbs.db",
    ".classpath",
    ".settings",
];

/// The prefix of the temporary names this crate uses while it writes or copies a file.
pub(crate) const TEMP_PREFIX: &str = ".bakedit-tmp-";

/// Whether a file or directory with this name is in [`ALWAYS_HIDDEN`].
///
/// The comparison is exact and case-sensitive, like Zed's globs.
pub fn is_always_hidden(name: &OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| ALWAYS_HIDDEN.contains(&name))
}

/// Whether any component of a root-relative path is always hidden.
pub(crate) fn has_hidden_component(relative: &Path) -> bool {
    relative.components().any(|component| match component {
        Component::Normal(name) => is_always_hidden(name),
        _ => false,
    })
}

/// Whether the last component is one of this crate's own temporary files.
pub(crate) fn is_own_temp_file(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.starts_with(TEMP_PREFIX))
}

/// Whether a change to this path changes which paths are ignored: a `.gitignore`, a
/// `.ignore`, or a repository's `info/exclude`.
pub(crate) fn is_ignore_file(path: &Path) -> bool {
    match path.file_name().and_then(OsStr::to_str) {
        Some(".gitignore") | Some(".ignore") => true,
        Some("exclude") => path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|parent| parent == "info"),
        _ => false,
    }
}

/// Whether `root` is inside a git (or jj) repository: it or a directory above it has a
/// `.git` or `.jj` entry.
pub(crate) fn inside_repository(root: &Path) -> bool {
    root.ancestors().any(|dir| {
        fs::symlink_metadata(dir.join(".git")).is_ok()
            || fs::symlink_metadata(dir.join(".jj")).is_ok()
    })
}

/// A predicate on a root-relative path and whether it is a directory. Returning `false`
/// prunes the entry (and everything below it) from a walk.
pub(crate) type EntryFilter = Arc<dyn Fn(&Path, bool) -> bool + Send + Sync>;

/// The directory walker every part of the crate uses. It walks `start`, which is the
/// project `root` or a directory inside it, with the rules of the project as a whole.
///
/// Dotfiles are visited, [`ALWAYS_HIDDEN`] names are not, symbolic links are reported but
/// not followed. Unless `include_ignored` is set, `.gitignore`, `.ignore`,
/// `.git/info/exclude` and the global gitignore are honoured, including the ignore files
/// in directories above the root.
///
/// Inside a repository the rules are git's own, so a `.gitignore` above the repository
/// root does not apply. Outside of one, every `.gitignore` is honoured anyway (as Zed
/// does), so a project that is not under version control yet still hides its build output.
pub(crate) fn walk_builder(
    root: &Path,
    start: &Path,
    include_ignored: bool,
    filter: Option<EntryFilter>,
) -> WalkBuilder {
    let mut builder = WalkBuilder::new(start);
    builder
        .hidden(false)
        .parents(!include_ignored)
        .ignore(!include_ignored)
        .git_ignore(!include_ignored)
        .git_global(!include_ignored)
        .git_exclude(!include_ignored)
        .require_git(inside_repository(root))
        .follow_links(false)
        // Global gitignore patterns are anchored at the project, not at wherever the
        // process happened to be started.
        .current_dir(root);
    let root = root.to_path_buf();
    builder.filter_entry(move |entry| {
        if entry.depth() == 0 {
            return true;
        }
        if is_always_hidden(entry.file_name()) {
            return false;
        }
        match &filter {
            Some(filter) => {
                let relative = entry.path().strip_prefix(&root).unwrap_or(entry.path());
                let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
                filter(relative, is_dir)
            }
            None => true,
        }
    });
    builder
}

/// Answers "is this path ignored?" for single paths, without walking anything.
///
/// The ignore files of a directory are read the first time a path inside it is asked
/// about and are then cached, so call [`IgnoreRules::reload`] after an ignore file changes.
pub(crate) struct IgnoreRules {
    root: PathBuf,
    matcher: Option<IncrementalIgnore>,
}

impl IgnoreRules {
    /// Rules for the project at `root`. Nothing is read from disk until the first query.
    pub(crate) fn new(root: &Path) -> IgnoreRules {
        let mut rules = IgnoreRules {
            root: root.to_path_buf(),
            matcher: None,
        };
        rules.reload();
        rules
    }

    /// Forget every cached ignore file. The next query reads them again.
    pub(crate) fn reload(&mut self) {
        self.matcher = walk_builder(&self.root, &self.root, false, None)
            .build_matchers()
            .pop();
    }

    /// Whether the root-relative path is ignored, either itself or because a directory
    /// above it is. The empty path (the root) is never ignored.
    pub(crate) fn is_ignored(&mut self, relative: &Path, is_dir: bool) -> bool {
        if relative.as_os_str().is_empty() || !is_plain_relative(relative) {
            return false;
        }
        match self.matcher.as_mut() {
            Some(matcher) => matcher.matched(relative, is_dir).is_ignore(),
            None => false,
        }
    }

    /// Whether a directory above the root-relative path is ignored. The path itself may
    /// be ignored or not; only its ancestors are looked at.
    pub(crate) fn inside_ignored_dir(&mut self, relative: &Path) -> bool {
        match relative.parent() {
            Some(parent) => self.is_ignored(parent, true),
            None => false,
        }
    }
}

/// Whether a path is made of normal components only (no root, no `..`), which is what the
/// incremental matcher requires.
fn is_plain_relative(path: &Path) -> bool {
    path.components()
        .all(|component| matches!(component, Component::Normal(_)))
}

/// `path` made absolute against the current directory and with `.` and `..` resolved
/// textually. Symbolic links are left alone.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// The absolute form of a path the caller handed in: relative paths are taken to be
/// relative to `root`, and `.`/`..` are resolved textually.
pub(crate) fn resolve(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize(path)
    } else {
        normalize(&root.join(path))
    }
}

/// A root-relative path as the text a picker shows: `/` separators on every platform.
/// Names that are not valid UTF-8 are converted lossily.
pub(crate) fn display_relative(relative: &Path) -> String {
    let text = relative.to_string_lossy();
    if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_names_are_exact() {
        assert!(is_always_hidden(OsStr::new(".git")));
        assert!(is_always_hidden(OsStr::new(".DS_Store")));
        assert!(is_always_hidden(OsStr::new("CVS")));
        assert!(!is_always_hidden(OsStr::new(".gitignore")));
        assert!(!is_always_hidden(OsStr::new(".github")));
        assert!(!is_always_hidden(OsStr::new("cvs")));
    }

    #[test]
    fn hidden_components_are_found_at_any_depth() {
        assert!(has_hidden_component(Path::new(".git/HEAD")));
        assert!(has_hidden_component(Path::new("vendor/lib/.git/config")));
        assert!(has_hidden_component(Path::new("docs/.DS_Store")));
        assert!(!has_hidden_component(Path::new("src/.gitignore")));
        assert!(!has_hidden_component(Path::new("")));
    }

    #[test]
    fn ignore_files_are_recognised() {
        assert!(is_ignore_file(Path::new("/p/.gitignore")));
        assert!(is_ignore_file(Path::new("/p/sub/.ignore")));
        assert!(is_ignore_file(Path::new("/p/.git/info/exclude")));
        assert!(!is_ignore_file(Path::new("/p/exclude")));
        assert!(!is_ignore_file(Path::new("/p/.gitattributes")));
    }

    #[test]
    fn own_temp_files_are_recognised() {
        assert!(is_own_temp_file(Path::new("/p/src/.bakedit-tmp-12-3")));
        assert!(!is_own_temp_file(Path::new("/p/src/main.rs")));
    }

    #[test]
    fn normalize_resolves_dots_without_touching_links() {
        assert_eq!(
            normalize(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
        assert_eq!(normalize(Path::new("/a/../..")), PathBuf::from("/"));
        assert_eq!(
            resolve(Path::new("/root"), Path::new("src/../lib.rs")),
            PathBuf::from("/root/lib.rs")
        );
        assert_eq!(
            resolve(Path::new("/root"), Path::new("/else/x")),
            PathBuf::from("/else/x")
        );
    }

    #[test]
    fn rules_match_files_and_everything_under_ignored_directories() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join(".gitignore"), "build-out/\n*.tmpdata\n").unwrap();
        fs::create_dir_all(root.join("build-out/deep")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();

        let mut rules = IgnoreRules::new(root);
        assert!(rules.is_ignored(Path::new("build-out"), true));
        assert!(rules.is_ignored(Path::new("build-out/deep/file.rs"), false));
        assert!(rules.is_ignored(Path::new("src/cache.tmpdata"), false));
        assert!(!rules.is_ignored(Path::new("src/main.rs"), false));
        assert!(!rules.is_ignored(Path::new(""), true));

        assert!(rules.inside_ignored_dir(Path::new("build-out/deep/file.rs")));
        assert!(!rules.inside_ignored_dir(Path::new("build-out")));
        assert!(!rules.inside_ignored_dir(Path::new("src/cache.tmpdata")));
    }

    #[test]
    fn rules_are_a_snapshot_until_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join(".gitignore"), "").unwrap();
        let mut rules = IgnoreRules::new(root);
        assert!(!rules.is_ignored(Path::new("notes.scratch"), false));

        fs::write(root.join(".gitignore"), "*.scratch\n").unwrap();
        rules.reload();
        assert!(rules.is_ignored(Path::new("notes.scratch"), false));
    }

    #[test]
    fn a_repository_does_not_inherit_ignore_files_from_above_it() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path();
        fs::write(outer.join(".gitignore"), "*.outerdata\n").unwrap();
        let repo = outer.join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join(".gitignore"), "*.innerdata\n").unwrap();

        assert!(inside_repository(&repo));
        let mut rules = IgnoreRules::new(&repo);
        assert!(rules.is_ignored(Path::new("a.innerdata"), false));
        assert!(!rules.is_ignored(Path::new("a.outerdata"), false));
    }

    #[test]
    fn outside_a_repository_every_gitignore_counts() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path();
        // The temp directory itself must not sit inside a repository for this to mean
        // anything; skip quietly if some ancestor happens to be one.
        if inside_repository(outer) {
            return;
        }
        fs::write(outer.join(".gitignore"), "*.outerdata\n").unwrap();
        let project = outer.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join(".gitignore"), "*.innerdata\n").unwrap();

        let mut rules = IgnoreRules::new(&project);
        assert!(rules.is_ignored(Path::new("a.innerdata"), false));
        assert!(rules.is_ignored(Path::new("a.outerdata"), false));
    }
}
