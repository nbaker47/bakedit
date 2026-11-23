//! Throwaway repositories for the integration tests.
//!
//! Every repository lives in its own temp directory, and every git process (the ones the
//! tests start and the ones the crate starts) is cut off from the user's global and
//! system configuration.

#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Once;

use bakedit_git::Repo;
use tempfile::TempDir;

static ISOLATE: Once = Once::new();

/// Makes sure no git process started from this test binary reads or writes the user's
/// configuration. Called before the first git process of every test.
pub fn isolate_git() {
    ISOLATE.call_once(|| {
        std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        // The default global ignore and attributes files live under this directory.
        std::env::set_var("XDG_CONFIG_HOME", "/dev/null");
        // Looking for a repository never climbs out of the temp directory, whatever the
        // machine keeps above it.
        let temp = std::env::temp_dir();
        let mut ceilings = temp.clone().into_os_string();
        if let Ok(real) = temp.canonicalize() {
            ceilings.push(":");
            ceilings.push(real);
        }
        std::env::set_var("GIT_CEILING_DIRECTORIES", ceilings);
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_TEMPLATE_DIR",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_PARAMETERS",
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_AUTHOR_DATE",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
            "GIT_COMMITTER_DATE",
        ] {
            std::env::remove_var(name);
        }
    });
}

/// Runs git in `dir` and returns what it printed, or its standard error when it failed.
pub fn try_git_in(dir: &Path, args: &[&str]) -> Result<String, String> {
    isolate_git();
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("LC_ALL", "C")
        // A fixture command must never wait for someone to type something.
        .env("GIT_EDITOR", "true")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .expect("git can be started");
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

/// Runs git in `dir`, panicking with git's message when it fails.
pub fn git_in(dir: &Path, args: &[&str]) -> String {
    match try_git_in(dir, args) {
        Ok(stdout) => stdout,
        Err(stderr) => panic!("git {args:?} failed in {}: {stderr}", dir.display()),
    }
}

/// Creates an empty repository on branch `main` at `path`, with a local identity and
/// commit signing switched off.
pub fn init_repository(path: &Path) {
    fs::create_dir_all(path).expect("create the repository directory");
    git_in(path, &["-c", "init.defaultBranch=main", "init", "--quiet"]);
    git_in(path, &["config", "user.name", "Test Author"]);
    git_in(path, &["config", "user.email", "author@example.com"]);
    git_in(path, &["config", "commit.gpgsign", "false"]);
}

/// A repository in a temp directory that disappears with this value.
pub struct TestRepo {
    /// Holds the work tree at `work/` and leaves room for remotes and clones next to it.
    sandbox: TempDir,
    root: PathBuf,
    pub repo: Repo,
}

impl TestRepo {
    /// A repository with no commits yet.
    pub fn new() -> TestRepo {
        isolate_git();
        let sandbox = tempfile::Builder::new()
            .prefix("bakedit-git-test-")
            .tempdir()
            .expect("create a temp directory");
        let work = sandbox.path().join("work");
        init_repository(&work);
        let root = work.canonicalize().expect("the work tree exists");
        let repo = Repo::discover(&root).expect("the new repository is found");
        TestRepo {
            sandbox,
            root,
            repo,
        }
    }

    /// A repository whose first commit holds `README.md`.
    pub fn with_commit() -> TestRepo {
        let test = TestRepo::new();
        test.write("README.md", "# test\n");
        test.commit_all("initial commit");
        test
    }

    /// The top of the work tree, with symbolic links resolved.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory that holds the work tree; siblings (remotes, clones) go here.
    pub fn sandbox(&self) -> &Path {
        self.sandbox.path()
    }

    /// The absolute path of a file in the work tree.
    pub fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// Writes a file in the work tree, creating its directories.
    pub fn write(&self, relative: &str, content: impl AsRef<[u8]>) {
        let path = self.path(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directories");
        }
        fs::write(&path, content).expect("write the file");
    }

    /// Reads a file from the work tree as text.
    pub fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.path(relative)).expect("read the file")
    }

    /// Deletes a file from the work tree.
    pub fn remove(&self, relative: &str) {
        fs::remove_file(self.path(relative)).expect("remove the file");
    }

    /// Whether a file exists in the work tree.
    pub fn exists(&self, relative: &str) -> bool {
        self.path(relative).exists()
    }

    /// Marks a file in the work tree as executable.
    pub fn make_executable(&self, relative: &str) {
        let path = self.path(relative);
        let mut permissions = fs::metadata(&path).expect("stat the file").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("chmod the file");
    }

    /// Runs git in the work tree, panicking when it fails.
    pub fn git(&self, args: &[&str]) -> String {
        git_in(&self.root, args)
    }

    /// Runs git in the work tree and hands back its standard error when it fails.
    pub fn try_git(&self, args: &[&str]) -> Result<String, String> {
        try_git_in(&self.root, args)
    }

    /// Stages everything and commits it. Returns the new commit's full hash.
    pub fn commit_all(&self, message: &str) -> String {
        self.git(&["add", "--all"]);
        self.git(&["commit", "--quiet", "--message", message]);
        self.head()
    }

    /// The full hash of `HEAD`.
    pub fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"]).trim().to_string()
    }

    /// Installs an executable hook script.
    pub fn install_hook(&self, name: &str, script: &str) {
        let hooks = self.repo.common_dir().join("hooks");
        fs::create_dir_all(&hooks).expect("create the hooks directory");
        let path = hooks.join(name);
        fs::write(&path, script).expect("write the hook");
        let mut permissions = fs::metadata(&path).expect("stat the hook").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("chmod the hook");
    }

    /// Makes the repository sign its commits with a stand-in for gpg, which "signs"
    /// anything and, asked to check a signature, prints a line that is easy to spot.
    pub fn sign_commits_with_a_stand_in(&self) {
        let script = "#!/bin/sh\n\
            cat > /dev/null\n\
            case \"$*\" in *--verify*)\n\
              echo 'gpg: Signature made by the stand-in' >&2\n\
              echo '[GNUPG:] GOODSIG 0123456789ABCDEF Test Author'\n\
              exit 0;;\n\
            esac\n\
            echo '[GNUPG:] SIG_CREATED D 1 8 00 1700000000 0123456789ABCDEF' >&2\n\
            echo '-----BEGIN PGP SIGNATURE-----'\n\
            echo 'signed by the stand-in'\n\
            echo '-----END PGP SIGNATURE-----'\n";
        let gpg = self.sandbox().join("stand-in-gpg");
        fs::write(&gpg, script).expect("write the stand-in");
        fs::set_permissions(&gpg, fs::Permissions::from_mode(0o755)).expect("chmod the stand-in");
        self.git(&[
            "config",
            "gpg.program",
            gpg.to_str().expect("temp paths are UTF-8"),
        ]);
        self.git(&["config", "commit.gpgsign", "true"]);
        self.git(&["config", "user.signingkey", "0123456789ABCDEF"]);
    }

    /// Creates a bare repository next to the work tree, adds it as remote `origin` and
    /// pushes `main` to it with upstream tracking. Returns the remote's path.
    pub fn add_origin(&self) -> PathBuf {
        let remote = self.sandbox().join("origin.git");
        fs::create_dir_all(&remote).expect("create the remote directory");
        git_in(
            &remote,
            &["-c", "init.defaultBranch=main", "init", "--quiet", "--bare"],
        );
        let url = remote.to_str().expect("temp paths are UTF-8");
        self.git(&["remote", "add", "origin", url]);
        self.git(&["push", "--quiet", "--set-upstream", "origin", "main"]);
        remote
    }

    /// Clones `remote` next to the work tree as `name`, with its own identity.
    pub fn clone_of(&self, remote: &Path, name: &str) -> PathBuf {
        let target = self.sandbox().join(name);
        git_in(
            self.sandbox(),
            &[
                "clone",
                "--quiet",
                remote.to_str().expect("temp paths are UTF-8"),
                target.to_str().expect("temp paths are UTF-8"),
            ],
        );
        git_in(&target, &["config", "user.name", "Other Author"]);
        git_in(&target, &["config", "user.email", "other@example.com"]);
        git_in(&target, &["config", "commit.gpgsign", "false"]);
        target
    }
}
