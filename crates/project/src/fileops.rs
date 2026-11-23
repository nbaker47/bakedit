//! File operations: create, rename, move, copy, duplicate, trash and delete.
//!
//! These are the operations the project panel offers. They work on plain paths and know
//! nothing about the tree, so they can be used on their own; [`crate::FileTree`] wraps each
//! of them and keeps its rows in step.
//!
//! # No data loss
//!
//! Nothing here overwrites an existing file or directory. A destination name is claimed
//! first (an empty placeholder is created with "fail if it exists" semantics) and the real
//! content is then renamed over that placeholder, so there is no window in which another
//! program's file could be replaced. Copies are assembled under a temporary name next to
//! the destination and only appear under their final name once they are complete; a copy
//! or a cross-device move that fails half-way removes its partial result and leaves the
//! source untouched.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::ignore_rules::TEMP_PREFIX;

/// Why a file operation did not happen.
#[derive(Debug, thiserror::Error)]
pub enum FileOpError {
    /// The name is empty, absolute, or has a `.` or `..` component.
    #[error("{0:?} is not a valid name")]
    InvalidName(String),
    /// Something already has the destination name. Nothing was overwritten.
    #[error("{} already exists", .0.display())]
    AlreadyExists(PathBuf),
    /// The path to operate on is not there (any more).
    #[error("{} does not exist", .0.display())]
    NotFound(PathBuf),
    /// A directory was needed and the path is something else.
    #[error("{} is not a directory", .0.display())]
    NotADirectory(PathBuf),
    /// The path is not inside the project. Only [`crate::FileTree`] reports this.
    #[error("{} is outside the project", .0.display())]
    OutsideRoot(PathBuf),
    /// The project root itself cannot be renamed, moved, duplicated or deleted from the
    /// tree. Only [`crate::FileTree`] reports this.
    #[error("the project root cannot be renamed, moved, duplicated or deleted")]
    IsRoot,
    /// A directory cannot be moved or copied into itself or one of its own descendants.
    #[error("{} cannot be moved or copied into itself", .0.display())]
    IntoItself(PathBuf),
    /// The path is a socket, a pipe or a device, which cannot be copied.
    #[error("{} is not a regular file, directory or link and cannot be copied", .0.display())]
    Unsupported(PathBuf),
    /// The system trash refused the path.
    #[error("could not move {} to the trash: {message}", .path.display())]
    Trash {
        /// The entry that was to be trashed.
        path: PathBuf,
        /// What the system said.
        message: String,
    },
    /// Any other file-system error, with the path it happened on.
    #[error("{}: {source}", .path.display())]
    Io {
        /// The path the operation failed on.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
}

fn io_error(path: &Path, source: io::Error) -> FileOpError {
    match source.kind() {
        io::ErrorKind::NotFound => FileOpError::NotFound(path.to_path_buf()),
        io::ErrorKind::AlreadyExists => FileOpError::AlreadyExists(path.to_path_buf()),
        io::ErrorKind::NotADirectory => FileOpError::NotADirectory(path.to_path_buf()),
        _ => FileOpError::Io {
            path: path.to_path_buf(),
            source,
        },
    }
}

/// Split a name the user typed into path components.
///
/// The name is trimmed, may contain `/` to mean nested directories, and may end in `/`
/// (reported as the second value). Empty names, absolute names and names with a `.` or
/// `..` component are rejected, so the result can never point outside its parent.
fn split_name(name: &str) -> Result<(Vec<&str>, bool), FileOpError> {
    let invalid = || FileOpError::InvalidName(name.to_string());
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.starts_with('/') || trimmed.contains('\0') {
        return Err(invalid());
    }
    let ends_with_slash = trimmed.ends_with('/');
    let mut components = Vec::new();
    for part in trimmed.split('/') {
        match part {
            "" => continue,
            "." | ".." => return Err(invalid()),
            part => {
                // A component that the platform would read as something other than one
                // plain name (a Windows drive prefix, for instance) is refused too.
                let mut parsed = Path::new(part).components();
                match (parsed.next(), parsed.next()) {
                    (Some(Component::Normal(_)), None) => components.push(part),
                    _ => return Err(invalid()),
                }
            }
        }
    }
    if components.is_empty() {
        return Err(invalid());
    }
    Ok((components, ends_with_slash))
}

fn join_components(dir: &Path, components: &[&str]) -> PathBuf {
    let mut path = dir.to_path_buf();
    for component in components {
        path.push(component);
    }
    path
}

fn require_dir(dir: &Path) -> Result<(), FileOpError> {
    match fs::metadata(dir) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(FileOpError::NotADirectory(dir.to_path_buf())),
        Err(source) => Err(io_error(dir, source)),
    }
}

fn create_parents(path: &Path) -> Result<(), FileOpError> {
    match path.parent() {
        Some(parent) => fs::create_dir_all(parent).map_err(|source| io_error(parent, source)),
        None => Ok(()),
    }
}

/// Create an empty file called `name` inside `dir` and return its path.
///
/// `name` may contain `/` to create the directories in between, as in Zed (`a/b/c.rs`),
/// and a name ending in `/` creates a directory instead of a file. Fails with
/// [`FileOpError::AlreadyExists`] rather than touching an existing entry.
pub fn create_file(dir: &Path, name: &str) -> Result<PathBuf, FileOpError> {
    let (components, wants_dir) = split_name(name)?;
    if wants_dir {
        return create_dir(dir, name);
    }
    require_dir(dir)?;
    let path = join_components(dir, &components);
    create_parents(&path)?;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|source| io_error(&path, source))?;
    Ok(path)
}

/// Create a directory called `name` inside `dir` and return its path.
///
/// `name` may contain `/` to create nested directories. Fails with
/// [`FileOpError::AlreadyExists`] if the final directory is already there.
pub fn create_dir(dir: &Path, name: &str) -> Result<PathBuf, FileOpError> {
    let (components, _) = split_name(name)?;
    require_dir(dir)?;
    let path = join_components(dir, &components);
    create_parents(&path)?;
    fs::create_dir(&path).map_err(|source| io_error(&path, source))?;
    Ok(path)
}

/// Rename `path` to `new_name` and return the new path.
///
/// `new_name` is relative to the directory `path` is in; it may contain `/` to move the
/// entry into a subdirectory, which is created if needed. Renaming to the current name
/// does nothing. Refuses to overwrite: if anything else already has the new name the
/// result is [`FileOpError::AlreadyExists`]. A rename that only changes the case of the
/// name works on case-insensitive file systems.
pub fn rename(path: &Path, new_name: &str) -> Result<PathBuf, FileOpError> {
    let (components, _) = split_name(new_name)?;
    let meta = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    let parent = path
        .parent()
        .ok_or_else(|| FileOpError::InvalidName(new_name.to_string()))?;
    let target = join_components(parent, &components);
    if target == path {
        return Ok(target);
    }
    if meta.is_dir() && target.starts_with(path) {
        return Err(FileOpError::IntoItself(path.to_path_buf()));
    }
    create_parents(&target)?;
    rename_no_replace(path, &target)?;
    Ok(target)
}

/// Move `path` into the directory `dir`, keeping its name, and return the new path.
///
/// Moving an entry into the directory it is already in does nothing. Refuses to overwrite
/// ([`FileOpError::AlreadyExists`]) and refuses to move a directory into itself
/// ([`FileOpError::IntoItself`]). A move to another file system copies first and removes
/// the source only once the copy is complete.
pub fn move_into(path: &Path, dir: &Path) -> Result<PathBuf, FileOpError> {
    let meta = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    require_dir(dir)?;
    let name = path
        .file_name()
        .ok_or_else(|| FileOpError::NotFound(path.to_path_buf()))?;
    let target = dir.join(name);
    if target == path {
        return Ok(target);
    }
    if meta.is_dir() && is_inside(dir, path) {
        return Err(FileOpError::IntoItself(path.to_path_buf()));
    }
    rename_no_replace(path, &target)?;
    Ok(target)
}

/// Like [`move_into`], but a name clash picks a free "copy" name instead of failing. This
/// is what pasting a cut entry does.
pub(crate) fn move_into_unique(path: &Path, dir: &Path) -> Result<PathBuf, FileOpError> {
    let meta = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    require_dir(dir)?;
    let name = path
        .file_name()
        .ok_or_else(|| FileOpError::NotFound(path.to_path_buf()))?;
    if dir.join(name) == path {
        return Ok(path.to_path_buf());
    }
    if meta.is_dir() && is_inside(dir, path) {
        return Err(FileOpError::IntoItself(path.to_path_buf()));
    }
    let name = name.to_string_lossy().into_owned();
    for attempt in 0.. {
        let target = dir.join(copy_name(&name, meta.is_dir(), attempt));
        match rename_no_replace(path, &target) {
            Err(FileOpError::AlreadyExists(_)) => continue,
            Err(error) => return Err(error),
            Ok(()) => return Ok(target),
        }
    }
    unreachable!("the attempt counter does not end")
}

/// Copy `path` into the directory `dir` and return the path of the copy.
///
/// Directories are copied with everything in them; symbolic links are copied as links.
/// If the name is taken in `dir`, the copy is called `name copy`, then `name copy 2`, and
/// so on (the extension of a file stays at the end: `notes copy.txt`). Nothing is ever
/// overwritten. Refuses to copy a directory into itself ([`FileOpError::IntoItself`]).
pub fn copy_into(path: &Path, dir: &Path) -> Result<PathBuf, FileOpError> {
    let meta = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    require_dir(dir)?;
    if meta.is_dir() && is_inside(dir, path) {
        return Err(FileOpError::IntoItself(path.to_path_buf()));
    }
    let name = path
        .file_name()
        .ok_or_else(|| FileOpError::NotFound(path.to_path_buf()))?
        .to_string_lossy()
        .into_owned();

    let temp = temp_path(dir);
    if let Err(error) = copy_tree(path, &temp) {
        remove_any(&temp);
        return Err(error);
    }
    for attempt in 0.. {
        let target = dir.join(copy_name(&name, meta.is_dir(), attempt));
        match rename_no_replace(&temp, &target) {
            Err(FileOpError::AlreadyExists(_)) => continue,
            Err(error) => {
                remove_any(&temp);
                return Err(error);
            }
            Ok(()) => return Ok(target),
        }
    }
    unreachable!("the attempt counter does not end")
}

/// Copy `path` next to itself and return the path of the copy, named as in
/// [`copy_into`]: `name copy`, `name copy 2`, ...
pub fn duplicate(path: &Path) -> Result<PathBuf, FileOpError> {
    let parent = path
        .parent()
        .ok_or_else(|| FileOpError::NotFound(path.to_path_buf()))?;
    copy_into(path, parent)
}

/// The name of the `attempt`-th candidate for a copy of `name`: the name itself, then
/// `name copy`, `name copy 2`, `name copy 3`, ...
///
/// For a file the suffix goes before the last extension (`a copy.txt`,
/// `archive.tar copy.gz`); dotfiles and directories get it at the end (`.env copy`,
/// `my.dir copy`).
pub fn copy_name(name: &str, is_dir: bool, attempt: usize) -> String {
    let suffix = match attempt {
        0 => return name.to_string(),
        1 => " copy".to_string(),
        n => format!(" copy {n}"),
    };
    let extension_dot = if is_dir {
        None
    } else {
        name.rfind('.').filter(|dot| *dot > 0)
    };
    match extension_dot {
        Some(dot) => format!("{}{}{}", &name[..dot], suffix, &name[dot..]),
        None => format!("{name}{suffix}"),
    }
}

/// Move `path` to the system trash.
///
/// A symbolic link is trashed itself; its target is left alone. On macOS this goes through
/// `NSFileManager`, which needs no extra permission and makes no sound (Finder's "Put
/// Back" may be unavailable for items trashed this way).
pub fn trash(path: &Path) -> Result<(), FileOpError> {
    fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    system_trash(path).map_err(|error| FileOpError::Trash {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

#[cfg(target_os = "macos")]
fn system_trash(path: &Path) -> Result<(), ::trash::Error> {
    use ::trash::macos::{DeleteMethod, TrashContextExtMacos};
    let mut context = ::trash::TrashContext::default();
    context.set_delete_method(DeleteMethod::NsFileManager);
    context.delete(path)
}

#[cfg(not(target_os = "macos"))]
fn system_trash(path: &Path) -> Result<(), ::trash::Error> {
    ::trash::delete(path)
}

/// Delete `path` for good, bypassing the trash.
///
/// A directory is removed with everything in it. A symbolic link is removed itself, never
/// what it points at.
pub fn delete(path: &Path) -> Result<(), FileOpError> {
    let meta = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    let result = if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    result.map_err(|source| io_error(path, source))
}

/// Replace the contents of the file at `path` in one step.
///
/// The bytes are written to a temporary file in the same directory, flushed to disk, given
/// the permissions of the file they replace, and renamed over it, so a reader sees either
/// the old contents or the new ones and never a partial write. A symbolic link is written
/// through: the file it points at is replaced and the link stays. The file is created if
/// it does not exist. Hard links to the old file keep the old contents.
pub fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    write_atomic_checked(path, contents, |_| true).map(|_| ())
}

/// [`write_atomic`] with a last look at the destination: `still_current` is called with
/// the path that is about to be replaced right before the rename, and returning `false`
/// abandons the write. The result says whether the file was replaced.
pub(crate) fn write_atomic_checked(
    path: &Path,
    contents: &[u8],
    still_current: impl FnOnce(&Path) -> bool,
) -> io::Result<bool> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let temp = temp_path(dir);
    let written = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(contents)?;
        if let Ok(existing) = fs::metadata(&target) {
            file.set_permissions(existing.permissions())?;
        }
        file.sync_all()
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    if !still_current(&target) {
        let _ = fs::remove_file(&temp);
        return Ok(false);
    }
    match fs::rename(&temp, &target) {
        Ok(()) => Ok(true),
        Err(error) => {
            let _ = fs::remove_file(&temp);
            Err(error)
        }
    }
}

/// Whether `path` is `ancestor` or lies below it, compared both as written and with
/// symbolic links resolved.
fn is_inside(path: &Path, ancestor: &Path) -> bool {
    if path.starts_with(ancestor) {
        return true;
    }
    match (fs::canonicalize(path), fs::canonicalize(ancestor)) {
        (Ok(path), Ok(ancestor)) => path.starts_with(ancestor),
        _ => false,
    }
}

/// A name for a temporary entry in `dir` that nothing else uses.
fn temp_path(dir: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    loop {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.subsec_nanos())
            .unwrap_or(0);
        let name = format!(
            "{TEMP_PREFIX}{}-{}-{}",
            process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
            nanos
        );
        let path = dir.join(name);
        if fs::symlink_metadata(&path).is_err() {
            return path;
        }
    }
}

fn remove_any(path: &Path) {
    let removed = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(_) => return,
    };
    if let Err(error) = removed {
        log::warn!("could not clean up {}: {error}", path.display());
    }
}

#[cfg(unix)]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_file(_: &fs::Metadata, _: &fs::Metadata) -> bool {
    false
}

/// Whether two paths in the same directory have names that differ only in case.
fn differs_only_in_case(a: &Path, b: &Path) -> bool {
    if a.parent() != b.parent() {
        return false;
    }
    match (a.file_name(), b.file_name()) {
        (Some(a), Some(b)) => {
            a != b && a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
        }
        _ => false,
    }
}

/// Rename `from` to `to` without ever replacing something that is already at `to`.
///
/// `to` is claimed with an empty placeholder (creation fails if the name is taken), and
/// `from` is then renamed over that placeholder, which the file system does atomically.
fn rename_no_replace(from: &Path, to: &Path) -> Result<(), FileOpError> {
    let from_meta = fs::symlink_metadata(from).map_err(|source| io_error(from, source))?;
    match fs::symlink_metadata(to) {
        Ok(to_meta) => {
            // On a case-insensitive file system `Notes.txt` and `notes.txt` are the same
            // entry; renaming one to the other replaces nothing.
            if same_file(&from_meta, &to_meta) && differs_only_in_case(from, to) {
                return fs::rename(from, to).map_err(|source| io_error(from, source));
            }
            return Err(FileOpError::AlreadyExists(to.to_path_buf()));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(io_error(to, source)),
    }

    let is_dir = from_meta.is_dir();
    let claimed = if is_dir {
        fs::create_dir(to)
    } else {
        File::create_new(to).map(|_| ())
    };
    claimed.map_err(|source| io_error(to, source))?;

    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            move_by_copy(from, to, is_dir)
        }
        Err(source) => {
            remove_placeholder(to, is_dir);
            Err(io_error(from, source))
        }
    }
}

fn remove_placeholder(path: &Path, is_dir: bool) {
    let _ = if is_dir {
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    };
}

/// Move across file systems: copy `from` over the placeholder at `to`, and remove `from`
/// only after the copy is complete. If the copy fails, the partial copy is removed and
/// `from` is left as it was.
fn move_by_copy(from: &Path, to: &Path, is_dir: bool) -> Result<(), FileOpError> {
    let copied = match fs::symlink_metadata(from) {
        Err(source) => Err(io_error(from, source)),
        // The placeholder directory is filled in place, so the name stays claimed.
        Ok(meta) if meta.is_dir() => copy_dir_contents(from, to, &meta),
        // A link cannot be written over a placeholder; creating it fails rather than
        // replaces if something else takes the name in between.
        Ok(meta) if meta.file_type().is_symlink() => {
            remove_placeholder(to, is_dir);
            copy_symlink(from, to)
        }
        // Copying a file over the empty placeholder replaces nothing but that.
        Ok(meta) if meta.is_file() => fs::copy(from, to)
            .map(|_| ())
            .map_err(|source| io_error(from, source)),
        Ok(_) => Err(FileOpError::Unsupported(from.to_path_buf())),
    };
    if let Err(error) = copied {
        remove_any(to);
        return Err(error);
    }
    let removed = if is_dir {
        fs::remove_dir_all(from)
    } else {
        fs::remove_file(from)
    };
    removed.map_err(|source| io_error(from, source))
}

/// Copy a file, a symbolic link or a whole directory from `from` to the unused path `to`.
fn copy_tree(from: &Path, to: &Path) -> Result<(), FileOpError> {
    let meta = fs::symlink_metadata(from).map_err(|source| io_error(from, source))?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        copy_symlink(from, to)
    } else if kind.is_dir() {
        fs::create_dir(to).map_err(|source| io_error(to, source))?;
        copy_dir_contents(from, to, &meta)
    } else if kind.is_file() {
        fs::copy(from, to)
            .map(|_| ())
            .map_err(|source| io_error(from, source))
    } else {
        Err(FileOpError::Unsupported(from.to_path_buf()))
    }
}

/// Copy everything inside the directory `from` into the existing directory `to`, then
/// give `to` the permissions of `from`.
fn copy_dir_contents(from: &Path, to: &Path, meta: &fs::Metadata) -> Result<(), FileOpError> {
    let entries = fs::read_dir(from).map_err(|source| io_error(from, source))?;
    for entry in entries {
        let entry = entry.map_err(|source| io_error(from, source))?;
        match copy_tree(&entry.path(), &to.join(entry.file_name())) {
            // Sockets and pipes inside a directory are left out rather than failing the
            // whole copy; they hold no data.
            Err(FileOpError::Unsupported(skipped)) => {
                log::warn!("not copying special file {}", skipped.display());
            }
            other => other?,
        }
    }
    // Last, so a read-only directory can still be filled.
    fs::set_permissions(to, meta.permissions()).map_err(|source| io_error(to, source))
}

#[cfg(unix)]
fn copy_symlink(from: &Path, to: &Path) -> Result<(), FileOpError> {
    let target = fs::read_link(from).map_err(|source| io_error(from, source))?;
    std::os::unix::fs::symlink(target, to).map_err(|source| io_error(to, source))
}

#[cfg(not(unix))]
fn copy_symlink(from: &Path, to: &Path) -> Result<(), FileOpError> {
    // Without a portable way to recreate the link, copy what it points at.
    let meta = fs::metadata(from).map_err(|source| io_error(from, source))?;
    if meta.is_dir() {
        return Err(FileOpError::Unsupported(from.to_path_buf()));
    }
    fs::copy(from, to)
        .map(|_| ())
        .map_err(|source| io_error(from, source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_split_and_validated() {
        assert_eq!(split_name("a.txt").unwrap(), (vec!["a.txt"], false));
        assert_eq!(
            split_name("  a/b/c.rs ").unwrap(),
            (vec!["a", "b", "c.rs"], false)
        );
        assert_eq!(split_name("dir/").unwrap(), (vec!["dir"], true));
        assert_eq!(split_name("a//b").unwrap(), (vec!["a", "b"], false));
        for bad in [
            "",
            "   ",
            "/",
            "/abs",
            "..",
            "a/../b",
            "./a",
            "a/.",
            "nul\0byte",
        ] {
            assert!(
                matches!(split_name(bad), Err(FileOpError::InvalidName(_))),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn copy_names_count_up_like_finder() {
        assert_eq!(copy_name("notes.txt", false, 0), "notes.txt");
        assert_eq!(copy_name("notes.txt", false, 1), "notes copy.txt");
        assert_eq!(copy_name("notes.txt", false, 2), "notes copy 2.txt");
        assert_eq!(copy_name("notes.txt", false, 11), "notes copy 11.txt");
        assert_eq!(copy_name("archive.tar.gz", false, 1), "archive.tar copy.gz");
        assert_eq!(copy_name("Makefile", false, 1), "Makefile copy");
        assert_eq!(copy_name(".env", false, 1), ".env copy");
        assert_eq!(copy_name("my.dir", true, 1), "my.dir copy");
        assert_eq!(copy_name("src", true, 3), "src copy 3");
    }

    #[test]
    fn case_only_renames_are_detected() {
        assert!(differs_only_in_case(
            Path::new("/a/Notes.txt"),
            Path::new("/a/notes.txt")
        ));
        assert!(!differs_only_in_case(
            Path::new("/a/notes.txt"),
            Path::new("/a/notes.txt")
        ));
        assert!(!differs_only_in_case(
            Path::new("/a/notes.txt"),
            Path::new("/b/NOTES.txt")
        ));
        assert!(!differs_only_in_case(
            Path::new("/a/one.txt"),
            Path::new("/a/two.txt")
        ));
    }

    #[test]
    fn temp_paths_are_unique_and_recognisable() {
        let dir = tempfile::tempdir().unwrap();
        let first = temp_path(dir.path());
        let second = temp_path(dir.path());
        assert_ne!(first, second);
        assert!(crate::ignore_rules::is_own_temp_file(&first));
        assert_eq!(first.parent(), Some(dir.path()));
    }

    #[test]
    fn a_cross_device_move_copies_then_removes_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        fs::create_dir_all(from.join("nested")).unwrap();
        fs::write(from.join("nested/file.txt"), "contents").unwrap();
        let to = dir.path().join("to");
        fs::create_dir(&to).unwrap(); // the placeholder rename_no_replace would have made

        move_by_copy(&from, &to, true).unwrap();
        assert!(!from.exists());
        assert_eq!(
            fs::read_to_string(to.join("nested/file.txt")).unwrap(),
            "contents"
        );
    }

    #[test]
    fn a_failed_cross_device_move_keeps_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("missing-source");
        let to = dir.path().join("to");
        File::create_new(&to).unwrap();
        assert!(move_by_copy(&from, &to, false).is_err());
        assert!(!to.exists(), "the placeholder is cleaned up");
    }
}
