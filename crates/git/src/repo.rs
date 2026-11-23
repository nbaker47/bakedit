//! A repository: where it lives and how its git commands are run.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{GitError, Result};
use crate::exec::Git;
use crate::paths::{normalize_relative, path_from_bytes};

/// How long `fetch`, `pull` and `push` may run before they are stopped, unless
/// [`Repo::set_network_timeout`] says otherwise. Generous on purpose: a push may run a
/// test suite in its pre-push hook.
pub const DEFAULT_NETWORK_TIMEOUT: Duration = Duration::from_secs(300);

/// A git work tree.
///
/// A `Repo` is only a handful of paths: it is cheap to clone and can be sent to and
/// shared between threads. Every operation starts a `git` process and blocks until it has
/// finished, so call them from worker threads.
///
/// Path arguments may be absolute (anywhere inside the work tree) or relative to
/// [`Repo::root`], which is also the form in which paths come back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    root: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
    network_timeout: Option<Duration>,
    /// Git composes decomposed file names in this repository (`core.precomposeUnicode`),
    /// so paths given to it have to be composed too.
    #[cfg(target_os = "macos")]
    composes_names: bool,
}

impl Repo {
    /// Finds the work tree that contains `path`, which may be a directory, a file, or a
    /// file that does not exist yet (its nearest existing directory is used).
    ///
    /// Works from inside linked work trees and submodules, where the result is that work
    /// tree or submodule and not the repository it belongs to. Returns `None` when `path`
    /// is not inside a work tree (which includes bare repositories and the inside of a
    /// `.git` directory), and when git cannot be run.
    pub fn discover(path: impl AsRef<Path>) -> Option<Repo> {
        let start = nearest_directory(path.as_ref())?;
        let root = rev_parse_path(&start, "--show-toplevel")?;
        let git_dir = rev_parse_path(&start, "--absolute-git-dir")?;
        let common_dir = rev_parse_path(&start, "--git-common-dir")?;
        let common_dir = start.join(common_dir);
        #[cfg(target_os = "macos")]
        let composes_names = Git::new(&root)
            .args(["config", "--type=bool", "--get", "core.precomposeUnicode"])
            .probe()
            .is_ok_and(|output| output.line() == b"true");
        Some(Repo {
            root,
            git_dir,
            common_dir: common_dir.canonicalize().unwrap_or(common_dir),
            network_timeout: Some(DEFAULT_NETWORK_TIMEOUT),
            #[cfg(target_os = "macos")]
            composes_names,
        })
    }

    /// Finds git work trees nested inside `root` up to `max_depth` subdirectories deep
    /// (for example, `~/Code/starbase` or `~/Code/worktrees/starbase/feat`).
    ///
    /// Stops descending into any directory that is itself a repository, skips hidden
    /// directories and common build/dependency caches, and ignores unreadable folders.
    /// Returns repositories deduplicated by root and sorted shallowest-first, then by path.
    pub fn discover_nested(root: impl AsRef<Path>, max_depth: usize) -> Vec<Repo> {
        let root = root.as_ref();
        if !root.is_dir() || max_depth == 0 {
            return Vec::new();
        }
        let mut candidates = Vec::new();
        collect_nested_git_dirs(root, 1, max_depth, &mut candidates);
        candidates.sort_by(|a, b| {
            let da = a.components().count();
            let db = b.components().count();
            da.cmp(&db).then_with(|| a.cmp(b))
        });
        let mut repos: Vec<Repo> = Vec::new();
        for dir in candidates {
            if let Some(repo) = Repo::discover(&dir) {
                if !repos.iter().any(|existing| existing.root() == repo.root()) {
                    repos.push(repo);
                }
            }
        }
        repos
    }

    /// The top directory of the work tree, as git reports it: absolute, with symbolic
    /// links resolved.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// This work tree's git directory. For an ordinary clone that is `<root>/.git`; for a
    /// linked work tree it is `<main>/.git/worktrees/<name>`, and for a submodule
    /// `<superproject>/.git/modules/<name>`. `HEAD`, `index` and the files that mark a
    /// merge or rebase in progress live here.
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// The git directory all work trees of the repository share, where branches, remotes
    /// and objects are stored. The same as [`Repo::git_dir`] unless this is a linked work
    /// tree.
    pub fn common_dir(&self) -> &Path {
        &self.common_dir
    }

    /// The time limit for `fetch`, `pull` and `push`; `None` means they may run forever.
    pub fn network_timeout(&self) -> Option<Duration> {
        self.network_timeout
    }

    /// Changes the time limit for `fetch`, `pull` and `push`. `None` removes it.
    pub fn set_network_timeout(&mut self, limit: Option<Duration>) {
        self.network_timeout = limit;
    }

    /// `path` relative to [`Repo::root`], which is how git names it.
    ///
    /// A relative path is taken as relative to the root already and is only tidied (`.`
    /// and `..` components resolved). An absolute path has the root removed; if it reaches
    /// the work tree through a symbolic link it is resolved first, and that also works
    /// for files that no longer exist. The root itself gives the empty path.
    ///
    /// On macOS the result is also spelled the way git spells it: a file name the file
    /// system holds in decomposed form ("u" plus a combining diaeresis) comes back
    /// composed ("ü") when the repository has `core.precomposeUnicode` on, which is what
    /// [`Repo::status`] and every other call report for that file. Every method that
    /// takes a path does this itself; use it directly to match your own paths (from a
    /// directory listing, say) against paths in results.
    ///
    /// Fails with [`GitError::PathOutsideRepo`] when the path is not inside the work
    /// tree.
    pub fn relative_path(&self, path: impl AsRef<Path>) -> Result<PathBuf> {
        let path = path.as_ref();
        let outside = || GitError::PathOutsideRepo(path.to_path_buf());
        let relative = if path.is_relative() {
            normalize_relative(path)
        } else if let Ok(rest) = path.strip_prefix(&self.root) {
            normalize_relative(rest)
        } else {
            let resolved = resolve_existing_part(path);
            let rest = resolved.strip_prefix(&self.root).map_err(|_| outside())?;
            normalize_relative(rest)
        };
        Ok(self.spelled_like_git(relative.ok_or_else(outside)?))
    }

    /// The full hash of the commit `HEAD` points at, or `None` before the first commit.
    pub fn head_sha(&self) -> Result<Option<String>> {
        let output = self
            .git()
            .args(["rev-parse", "--quiet", "--verify", "HEAD^{commit}"])
            .probe()?;
        Ok((output.code == Some(0)).then(|| output.line_text()))
    }

    /// The name of the branch that is checked out (also before its first commit), or
    /// `None` when `HEAD` is detached.
    pub fn current_branch(&self) -> Result<Option<String>> {
        let output = self
            .git()
            .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
            .probe()?;
        Ok((output.code == Some(0)).then(|| output.line_text()))
    }

    /// Best-guess default base ref for branch diffs (`origin/HEAD` target such as
    /// `origin/main`, else `origin/main`, `origin/master`, `main`, or `master`).
    pub fn default_base_ref(&self) -> Option<String> {
        if let Ok(out) = self
            .git()
            .args(["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"])
            .probe()
        {
            if out.code == Some(0) {
                let name = out.line_text();
                if !name.is_empty() {
                    return Some(name);
                }
            }
        }
        for candidate in ["origin/main", "origin/master", "main", "master"] {
            let rev = format!("{candidate}^{{commit}}");
            if self
                .git()
                .args(["rev-parse", "--quiet", "--verify", &rev])
                .probe()
                .is_ok_and(|out| out.code == Some(0))
            {
                return Some(candidate.to_string());
            }
        }
        None
    }

    /// A git command that runs at the top of this work tree.
    pub(crate) fn git(&self) -> Git<'_> {
        Git::new(&self.root)
    }

    /// `relative` in the form git uses for file names in this repository.
    fn spelled_like_git(&self, relative: PathBuf) -> PathBuf {
        #[cfg(target_os = "macos")]
        if self.composes_names {
            return crate::precompose::precompose_path(relative);
        }
        relative
    }

    /// The value of the configuration variable `key`, or `None` when it is not set.
    pub(crate) fn config(&self, key: &str) -> Result<Option<String>> {
        let output = self.git().args(["config", "--get", key]).probe()?;
        Ok((output.code == Some(0)).then(|| output.line_text()))
    }

    /// Every path of `paths` relative to the root. See [`Repo::relative_path`].
    pub(crate) fn relative_paths<P: AsRef<Path>>(&self, paths: &[P]) -> Result<Vec<PathBuf>> {
        paths.iter().map(|path| self.relative_path(path)).collect()
    }
}

/// Directory names skipped while looking for nested repositories.
const SKIP_DIRS: &[&str] = &["node_modules", "target", "dist", "build", "venv", "__pycache__", "vendor"];

fn collect_nested_git_dirs(dir: &Path, depth: usize, max_depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else { continue };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
            continue;
        }
        let path = entry.path();
        if path.join(".git").exists() {
            out.push(path);
            continue;
        }
        if depth < max_depth {
            collect_nested_git_dirs(&path, depth + 1, max_depth, out);
        }
    }
}

/// The directory to start looking from: `path` itself or its closest ancestor that exists
/// and is a directory.
fn nearest_directory(path: &Path) -> Option<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    absolute
        .ancestors()
        .find(|ancestor| ancestor.is_dir())
        .map(Path::to_path_buf)
}

/// One path answer from `git rev-parse`, or `None` when git says no.
///
/// Each question gets a run of its own: `rev-parse` separates answers with line feeds,
/// which a path may contain, so only a single answer can be read back exactly.
fn rev_parse_path(dir: &Path, question: &str) -> Option<PathBuf> {
    match Git::new(dir).args(["rev-parse", question]).run() {
        Ok(output) if !output.line().is_empty() => Some(path_from_bytes(output.line())),
        Ok(_) => None,
        // The everyday answer: this directory is not in a repository or a work tree.
        Err(GitError::Failed {
            code: Some(128),
            stderr,
            ..
        }) if stderr.contains("not a git repository") || stderr.contains("work tree") => {
            log::debug!("no work tree at {}", dir.display());
            None
        }
        // Anything else (git missing, a repository git refuses to trust) is worth a
        // line in the log, because the git features silently stay off.
        Err(error) => {
            log::warn!("cannot use git in {}: {error}", dir.display());
            None
        }
    }
}

/// `path` with symbolic links resolved for as much of it as exists on disk. The rest (a
/// file that was deleted, say) is appended unchanged.
fn resolve_existing_part(path: &Path) -> PathBuf {
    for ancestor in path.ancestors() {
        if let Ok(real) = ancestor.canonicalize() {
            return match path.strip_prefix(ancestor) {
                Ok(rest) => real.join(rest),
                Err(_) => real,
            };
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_at(root: &str) -> Repo {
        Repo {
            root: PathBuf::from(root),
            git_dir: PathBuf::from(root).join(".git"),
            common_dir: PathBuf::from(root).join(".git"),
            network_timeout: Some(DEFAULT_NETWORK_TIMEOUT),
            #[cfg(target_os = "macos")]
            composes_names: false,
        }
    }

    #[test]
    fn relative_paths_are_taken_from_the_root() {
        let repo = repo_at("/work/project");
        let rel = |path: &str| repo.relative_path(path).ok();
        assert_eq!(rel("src/main.rs"), Some(PathBuf::from("src/main.rs")));
        assert_eq!(rel("./src/../README.md"), Some(PathBuf::from("README.md")));
        assert_eq!(rel(""), Some(PathBuf::new()));
        assert_eq!(rel("../elsewhere"), None);
    }

    #[test]
    fn absolute_paths_lose_the_root() {
        let repo = repo_at("/work/project");
        let rel = |path: &str| repo.relative_path(path).ok();
        assert_eq!(
            rel("/work/project/src/main.rs"),
            Some(PathBuf::from("src/main.rs"))
        );
        assert_eq!(rel("/work/project"), Some(PathBuf::new()));
        assert_eq!(
            rel("/work/project/src/../a.txt"),
            Some(PathBuf::from("a.txt"))
        );
        assert_eq!(rel("/work/project/../other/a.txt"), None);
        assert_eq!(rel("/work/projector/a.txt"), None);
    }

    #[test]
    fn paths_outside_report_the_path() {
        let repo = repo_at("/work/project");
        match repo.relative_path("/elsewhere/file") {
            Err(GitError::PathOutsideRepo(path)) => assert_eq!(path, Path::new("/elsewhere/file")),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn decomposed_names_are_composed_when_git_composes_them() {
        let mut repo = repo_at("/work/project");
        let decomposed = "/work/project/dir/u\u{308}.txt";
        assert_eq!(
            repo.relative_path(decomposed).unwrap(),
            Path::new("dir/u\u{308}.txt")
        );
        repo.composes_names = true;
        assert_eq!(
            repo.relative_path(decomposed).unwrap(),
            Path::new("dir/\u{fc}.txt")
        );
        assert_eq!(
            repo.relative_path("dir/u\u{308}.txt").unwrap(),
            Path::new("dir/\u{fc}.txt")
        );
        assert_eq!(
            repo.relative_path("plain/ascii.txt").unwrap(),
            Path::new("plain/ascii.txt")
        );
    }

    #[test]
    fn network_timeout_can_be_changed_and_removed() {
        let mut repo = repo_at("/work/project");
        assert_eq!(repo.network_timeout(), Some(DEFAULT_NETWORK_TIMEOUT));
        repo.set_network_timeout(Some(Duration::from_secs(5)));
        assert_eq!(repo.network_timeout(), Some(Duration::from_secs(5)));
        repo.set_network_timeout(None);
        assert_eq!(repo.network_timeout(), None);
    }

    #[test]
    fn repositories_cross_threads() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<Repo>();
    }
}
