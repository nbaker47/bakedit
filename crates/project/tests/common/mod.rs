//! Helpers shared by the integration tests: a throwaway project directory.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A temporary project directory that is removed when dropped.
pub struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    pub fn new() -> Project {
        Project {
            dir: tempfile::tempdir().expect("create a temp directory"),
        }
    }

    /// A project with the given files, written with their path as contents.
    pub fn with_files(files: &[&str]) -> Project {
        let project = Project::new();
        for file in files {
            project.write(file, file);
        }
        project
    }

    /// The project root, as the temp directory reports it (not canonicalised).
    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.root().join(relative)
    }

    /// Write a file, creating the directories above it.
    pub fn write(&self, relative: &str, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.path(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directories");
        }
        fs::write(&path, contents).expect("write file");
        path
    }

    pub fn mkdir(&self, relative: &str) -> PathBuf {
        let path = self.path(relative);
        fs::create_dir_all(&path).expect("create directory");
        path
    }

    pub fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.path(relative)).expect("read file")
    }

    pub fn read_bytes(&self, relative: &str) -> Vec<u8> {
        fs::read(self.path(relative)).expect("read file")
    }

    /// Whether something (even a dangling link) is at the path.
    pub fn exists(&self, relative: &str) -> bool {
        fs::symlink_metadata(self.path(relative)).is_ok()
    }

    pub fn remove(&self, relative: &str) {
        let path = self.path(relative);
        let meta = fs::symlink_metadata(&path).expect("path exists");
        if meta.is_dir() {
            fs::remove_dir_all(&path).expect("remove directory");
        } else {
            fs::remove_file(&path).expect("remove file");
        }
    }

    /// Create a symbolic link at `link` pointing at `target` (taken as written).
    #[cfg(unix)]
    pub fn symlink(&self, target: impl AsRef<Path>, link: &str) -> PathBuf {
        let path = self.path(link);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directories");
        }
        std::os::unix::fs::symlink(target, &path).expect("create symlink");
        path
    }

    /// Every file and directory below the root, relative, sorted, for exact comparisons.
    pub fn listing(&self) -> Vec<String> {
        let mut found = Vec::new();
        collect(self.root(), self.root(), &mut found);
        found.sort();
        found
    }
}

fn collect(root: &Path, dir: &Path, found: &mut Vec<String>) {
    for entry in fs::read_dir(dir).expect("read directory").flatten() {
        let path = entry.path();
        let relative = path.strip_prefix(root).expect("inside the root");
        let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
        found.push(format!(
            "{}{}",
            relative.display(),
            if is_dir { "/" } else { "" }
        ));
        if is_dir {
            collect(root, &path, found);
        }
    }
}

/// Poll `condition` until it holds or `timeout` passes. Returns whether it held.
pub fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
