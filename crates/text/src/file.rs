//! Reading and writing buffer files: decoding, line endings, atomic saves.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use thiserror::Error;

/// How lines end in the file on disk. In memory a buffer always uses `\n`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LineEnding {
    /// `\n`, the Unix convention.
    #[default]
    Lf,
    /// `\r\n`, the Windows convention.
    CrLf,
}

impl LineEnding {
    /// The characters written at the end of each line.
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
        }
    }
}

/// How the file on disk compares with what the buffer last read or wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiskState {
    /// The file is as the buffer left it (or the buffer has no file to compare with).
    Unchanged,
    /// The file's modification time or size changed since the last load or save.
    Modified,
    /// The file the buffer was loaded from or saved to no longer exists.
    Deleted,
}

/// Options for [`crate::Buffer::save`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SaveOptions {
    /// Make the text end with exactly one line break before writing, as Zed's
    /// `ensure_final_newline_on_save` does: trailing spaces, tabs and blank lines at the end
    /// of the file are replaced by a single `\n`. An empty buffer stays empty.
    pub ensure_final_newline: bool,
}

/// Why a buffer could not be loaded, saved or reloaded.
#[derive(Debug, Error)]
pub enum BufferError {
    /// The operating system refused: the file is missing, unreadable, not writable, ...
    #[error("{}: {source}", .path.display())]
    Io {
        /// The file the operation was about.
        path: PathBuf,
        /// What went wrong.
        #[source]
        source: io::Error,
    },
    /// The file has a NUL byte in its first 8 KiB, so it is not text.
    #[error("{} is a binary file", .path.display())]
    Binary {
        /// The file that was refused.
        path: PathBuf,
    },
    /// The buffer has no path, so there is nothing to save to or reload from.
    #[error("the buffer has no file path")]
    NoPath,
}

/// What a buffer remembers about its file to notice outside changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DiskStamp {
    pub modified: Option<SystemTime>,
    pub len: u64,
}

/// The stamp of the file at `path`, following symlinks. `Ok(None)` when it does not exist.
pub(crate) fn stamp(path: &Path) -> io::Result<Option<DiskStamp>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(DiskStamp {
            modified: metadata.modified().ok(),
            len: metadata.len(),
        })),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// A file's contents ready for a buffer.
#[derive(Debug)]
pub(crate) struct Decoded {
    /// The text with every line ending turned into `\n`.
    pub text: String,
    pub line_ending: LineEnding,
    /// The file started with a UTF-8 byte order mark (not part of `text`).
    pub has_bom: bool,
    /// The file was not valid UTF-8; the bad bytes became U+FFFD.
    pub lossy: bool,
    pub stamp: Option<DiskStamp>,
}

/// How many leading bytes are checked for NUL to decide a file is binary.
const BINARY_SNIFF_LEN: usize = 8 * 1024;

const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// Read and decode the file at `path`.
pub(crate) fn read(path: &Path) -> Result<Decoded, BufferError> {
    let io_error = |source| BufferError::Io {
        path: path.to_path_buf(),
        source,
    };
    // Stamp first: if the file changes while it is being read, the stale stamp makes the
    // buffer report it as modified later instead of hiding the change.
    let stamp = stamp(path).map_err(io_error)?;
    let bytes = fs::read(path).map_err(io_error)?;
    let sniff = &bytes[..bytes.len().min(BINARY_SNIFF_LEN)];
    if memchr::memchr(0, sniff).is_some() {
        return Err(BufferError::Binary {
            path: path.to_path_buf(),
        });
    }
    let mut decoded = decode(bytes);
    decoded.stamp = stamp;
    Ok(decoded)
}

/// Decode file bytes: strip a UTF-8 byte order mark, replace invalid UTF-8, and normalise
/// line endings to `\n`, remembering which ending the file used.
pub(crate) fn decode(mut bytes: Vec<u8>) -> Decoded {
    let has_bom = bytes.starts_with(UTF8_BOM);
    if has_bom {
        bytes.drain(..UTF8_BOM.len());
    }
    let (text, lossy) = match String::from_utf8(bytes) {
        Ok(text) => (text, false),
        Err(error) => (String::from_utf8_lossy(error.as_bytes()).into_owned(), true),
    };
    let line_ending = detect_line_ending(&text);
    let text = match normalize_newlines(&text) {
        std::borrow::Cow::Borrowed(_) => text,
        std::borrow::Cow::Owned(normalized) => normalized,
    };
    Decoded {
        text,
        line_ending,
        has_bom,
        lossy,
        stamp: None,
    }
}

/// The line ending most lines of `text` use: CRLF when more than half of the line feeds
/// follow a carriage return, LF otherwise (also for text with no line break at all).
pub(crate) fn detect_line_ending(text: &str) -> LineEnding {
    let bytes = text.as_bytes();
    if memchr::memchr(b'\r', bytes).is_none() {
        return LineEnding::Lf;
    }
    let line_feeds = memchr::memchr_iter(b'\n', bytes).count();
    let crlf = memchr::memmem::find_iter(bytes, b"\r\n").count();
    if crlf * 2 > line_feeds {
        LineEnding::CrLf
    } else {
        LineEnding::Lf
    }
}

/// `text` with every `\r\n` and every lone `\r` turned into `\n`. Borrows when there is no
/// carriage return, which is the common case.
pub(crate) fn normalize_newlines(text: &str) -> std::borrow::Cow<'_, str> {
    let bytes = text.as_bytes();
    if memchr::memchr(b'\r', bytes).is_none() {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for position in memchr::memchr_iter(b'\r', bytes) {
        out.push_str(&text[copied..position]);
        if bytes.get(position + 1) != Some(&b'\n') {
            out.push('\n');
        }
        copied = position + 1;
    }
    out.push_str(&text[copied..]);
    std::borrow::Cow::Owned(out)
}

/// Write `chunks` (text with `\n` line endings) to `writer`, as the bytes a file should
/// hold: an optional byte order mark first, and `\r\n` for every `\n` when asked.
pub(crate) fn write_text<'a>(
    writer: &mut dyn Write,
    chunks: impl Iterator<Item = &'a str>,
    line_ending: LineEnding,
    with_bom: bool,
) -> io::Result<()> {
    if with_bom {
        writer.write_all(UTF8_BOM)?;
    }
    for chunk in chunks {
        let bytes = chunk.as_bytes();
        match line_ending {
            LineEnding::Lf => writer.write_all(bytes)?,
            LineEnding::CrLf => {
                let mut written = 0;
                for position in memchr::memchr_iter(b'\n', bytes) {
                    writer.write_all(&bytes[written..position])?;
                    writer.write_all(b"\r\n")?;
                    written = position + 1;
                }
                writer.write_all(&bytes[written..])?;
            }
        }
    }
    Ok(())
}

/// How many symlinks are followed before giving up (a loop).
const MAX_SYMLINK_DEPTH: usize = 40;

/// The file a path finally names: symlinks are followed, also when the last one points at a
/// file that does not exist yet.
fn resolve_symlinks(path: &Path) -> io::Result<PathBuf> {
    let mut current = path.to_path_buf();
    for _ in 0..MAX_SYMLINK_DEPTH {
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = fs::read_link(&current)?;
                current = match current.parent() {
                    Some(parent) if target.is_relative() => parent.join(target),
                    _ => target,
                };
            }
            _ => return Ok(current),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "too many levels of symbolic links",
    ))
}

/// Distinguishes temporary files made by this process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Create a new, empty temporary file next to `target`. With `private` it starts out
/// readable by the owner only, for the case where the final permissions are set later.
fn create_temp_file(target: &Path, private: bool) -> io::Result<(PathBuf, File)> {
    let directory = match target.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut last_error = None;
    for _ in 0..16 {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_path = directory.join(format!(
            ".{name}.bakedit-{}-{counter}.tmp",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        if private {
            restrict_to_owner(&mut options);
        }
        match options.open(&temp_path) {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last_error = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::other("could not create a temporary file")))
}

#[cfg(unix)]
fn restrict_to_owner(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn restrict_to_owner(_options: &mut OpenOptions) {}

/// Whether replacing `target` by renaming a new file over it would lose something a plain
/// overwrite keeps: other hard links to the file, or its owner and group.
#[cfg(unix)]
fn rename_would_change_identity(existing: &fs::Metadata, temp: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    existing.nlink() > 1 || existing.uid() != temp.uid() || existing.gid() != temp.gid()
}

#[cfg(not(unix))]
fn rename_would_change_identity(_existing: &fs::Metadata, _temp: &fs::Metadata) -> bool {
    false
}

/// Overwrite `target` in place. Not atomic; used only when a rename is not possible or
/// would change who owns the file.
fn write_in_place(
    target: &Path,
    write: &mut dyn FnMut(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(target)?;
    let mut writer = BufWriter::new(file);
    write(&mut writer)?;
    let file = writer.into_inner().map_err(|error| error.into_error())?;
    file.sync_all()
}

/// Write a file so that a crash or a full disk never leaves it half written.
///
/// The contents go to a temporary file in the same directory, which is flushed to disk and
/// then renamed over the target, so readers see either the old file or the new one. The
/// target's permissions are kept. A symlink is followed: the file it points at is replaced
/// and the link stays. A file the user may not write is refused rather than replaced.
///
/// When the directory does not allow creating the temporary file, or renaming would change
/// the file's owner or break its other hard links, the file is overwritten in place
/// instead, which is not atomic.
pub(crate) fn write_atomic(
    path: &Path,
    write: &mut dyn FnMut(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    let target = resolve_symlinks(path)?;
    let existing = match fs::metadata(&target) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if existing.is_some() {
        // Renaming over a read-only file would succeed; opening it for writing says
        // whether the user is actually allowed to change it.
        OpenOptions::new().write(true).open(&target)?;
    }

    let (temp_path, temp_file) = match create_temp_file(&target, existing.is_some()) {
        Ok(created) => created,
        Err(_) => return write_in_place(&target, write),
    };
    let result: io::Result<bool> = (|| {
        if let Some(existing) = &existing {
            if rename_would_change_identity(existing, &temp_file.metadata()?) {
                return Ok(false);
            }
        }
        let mut writer = BufWriter::new(temp_file);
        write(&mut writer)?;
        let file = writer.into_inner().map_err(|error| error.into_error())?;
        if let Some(existing) = &existing {
            fs::set_permissions(&temp_path, existing.permissions())?;
        }
        file.sync_all()?;
        drop(file);
        fs::rename(&temp_path, &target)?;
        Ok(true)
    })();
    match result {
        Ok(true) => {
            sync_directory(&target);
            Ok(())
        }
        Ok(false) => {
            let _ = fs::remove_file(&temp_path);
            write_in_place(&target, write)
        }
        Err(error) => {
            let _ = fs::remove_file(&temp_path);
            Err(error)
        }
    }
}

/// Ask the OS to persist a rename by flushing the directory. Best effort: not every
/// platform or filesystem allows it, and the save itself already succeeded.
fn sync_directory(target: &Path) {
    let directory = match target.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    if let Ok(handle) = File::open(directory) {
        let _ = handle.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_plain_utf8() {
        let decoded = decode(b"hello\nworld\n".to_vec());
        assert_eq!(decoded.text, "hello\nworld\n");
        assert_eq!(decoded.line_ending, LineEnding::Lf);
        assert!(!decoded.has_bom && !decoded.lossy);
    }

    #[test]
    fn decode_crlf_and_lone_cr() {
        let decoded = decode(b"a\r\nb\r\nc".to_vec());
        assert_eq!(decoded.text, "a\nb\nc");
        assert_eq!(decoded.line_ending, LineEnding::CrLf);

        // Mostly LF with one CRLF: LF wins and the stray CR goes.
        let decoded = decode(b"a\nb\nc\r\nd\n".to_vec());
        assert_eq!(decoded.text, "a\nb\nc\nd\n");
        assert_eq!(decoded.line_ending, LineEnding::Lf);

        // Old Mac line endings become line feeds.
        let decoded = decode(b"a\rb\rc".to_vec());
        assert_eq!(decoded.text, "a\nb\nc");
        assert_eq!(decoded.line_ending, LineEnding::Lf);

        let decoded = decode(b"a\r\r\nb\r".to_vec());
        assert_eq!(decoded.text, "a\n\nb\n");
    }

    #[test]
    fn decode_bom_and_invalid_utf8() {
        let decoded = decode(b"\xEF\xBB\xBFhi".to_vec());
        assert_eq!(decoded.text, "hi");
        assert!(decoded.has_bom);

        let decoded = decode(b"ok \xFF\xFE bad".to_vec());
        assert_eq!(decoded.text, "ok \u{fffd}\u{fffd} bad");
        assert!(decoded.lossy);

        let decoded = decode(Vec::new());
        assert_eq!(decoded.text, "");
        assert!(!decoded.has_bom && !decoded.lossy);
    }

    #[test]
    fn line_ending_detection() {
        assert_eq!(detect_line_ending(""), LineEnding::Lf);
        assert_eq!(detect_line_ending("no newline"), LineEnding::Lf);
        assert_eq!(detect_line_ending("a\r\nb"), LineEnding::CrLf);
        assert_eq!(detect_line_ending("a\r\nb\nc\n"), LineEnding::Lf);
        assert_eq!(detect_line_ending("a\r\nb\r\nc\n"), LineEnding::CrLf);
        assert_eq!(
            detect_line_ending("a\r\nb\n"),
            LineEnding::Lf,
            "a tie stays LF"
        );
        assert_eq!(LineEnding::Lf.as_str(), "\n");
        assert_eq!(LineEnding::CrLf.as_str(), "\r\n");
    }

    #[test]
    fn newline_normalisation_borrows_when_it_can() {
        assert!(matches!(
            normalize_newlines("a\nb"),
            std::borrow::Cow::Borrowed(_)
        ));
        assert_eq!(normalize_newlines("a\r\nb\rc\r"), "a\nb\nc\n");
        assert_eq!(normalize_newlines("\r\n\r\n"), "\n\n");
        assert_eq!(normalize_newlines("é\r\n世"), "é\n世");
    }

    #[test]
    fn writing_text_bytes() {
        let mut out = Vec::new();
        write_text(
            &mut out,
            ["a\nb", "\n", "c"].into_iter(),
            LineEnding::Lf,
            false,
        )
        .unwrap();
        assert_eq!(out, b"a\nb\nc");

        let mut out = Vec::new();
        write_text(
            &mut out,
            ["a\nb", "\n", "c\n"].into_iter(),
            LineEnding::CrLf,
            true,
        )
        .unwrap();
        assert_eq!(out, b"\xEF\xBB\xBFa\r\nb\r\nc\r\n");

        let mut out = Vec::new();
        write_text(&mut out, std::iter::empty(), LineEnding::CrLf, false).unwrap();
        assert!(out.is_empty());
    }
}
