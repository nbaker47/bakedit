//! The file operations on their own, without a tree around them.

mod common;

use std::fs;
use std::path::PathBuf;

use bakedit_project::fileops::{self, FileOpError};
use common::Project;

#[test]
fn create_rename_move_copy_and_delete_work_on_plain_paths() {
    let project = Project::new();
    let root = project.root();

    let file = fileops::create_file(root, "a/b/file.txt").unwrap();
    assert_eq!(file, project.path("a/b/file.txt"));
    let dir = fileops::create_dir(root, "target-dir").unwrap();
    assert!(dir.is_dir());
    assert!(fileops::create_file(root, "made/by/slash/")
        .unwrap()
        .is_dir());

    let renamed = fileops::rename(&file, "renamed.txt").unwrap();
    assert_eq!(renamed, project.path("a/b/renamed.txt"));
    let moved = fileops::move_into(&renamed, &dir).unwrap();
    assert_eq!(moved, project.path("target-dir/renamed.txt"));
    let copy = fileops::copy_into(&moved, root).unwrap();
    assert_eq!(copy, project.path("renamed.txt"));
    let duplicate = fileops::duplicate(&copy).unwrap();
    assert_eq!(duplicate, project.path("renamed copy.txt"));

    fileops::delete(&project.path("a")).unwrap();
    fileops::delete(&duplicate).unwrap();
    assert_eq!(
        project.listing(),
        [
            "made/",
            "made/by/",
            "made/by/slash/",
            "renamed.txt",
            "target-dir/",
            "target-dir/renamed.txt",
        ]
    );
}

#[test]
fn nothing_is_ever_overwritten() {
    let project = Project::new();
    project.write("a.txt", "a");
    project.write("b.txt", "b");
    project.write("dir/a.txt", "inside");
    project.mkdir("empty-dir");

    assert!(matches!(
        fileops::rename(&project.path("a.txt"), "b.txt"),
        Err(FileOpError::AlreadyExists(_))
    ));
    // Not even an empty directory is replaced.
    assert!(matches!(
        fileops::rename(&project.path("dir"), "empty-dir"),
        Err(FileOpError::AlreadyExists(_))
    ));
    assert!(matches!(
        fileops::rename(&project.path("a.txt"), "empty-dir"),
        Err(FileOpError::AlreadyExists(_))
    ));
    assert!(matches!(
        fileops::move_into(&project.path("a.txt"), &project.path("dir")),
        Err(FileOpError::AlreadyExists(_))
    ));
    assert!(matches!(
        fileops::create_file(project.root(), "a.txt"),
        Err(FileOpError::AlreadyExists(_))
    ));
    assert!(matches!(
        fileops::create_dir(project.root(), "dir"),
        Err(FileOpError::AlreadyExists(_))
    ));
    assert_eq!(project.read("a.txt"), "a");
    assert_eq!(project.read("b.txt"), "b");
    assert_eq!(project.read("dir/a.txt"), "inside");
    assert_eq!(
        project.listing(),
        ["a.txt", "b.txt", "dir/", "dir/a.txt", "empty-dir/"],
        "no placeholders or temporary files are left behind"
    );
}

#[test]
fn a_failed_operation_leaves_no_new_directories_behind() {
    let project = Project::new();
    project.write("dir/keep.txt", "x");
    project.write("a.txt", "a");
    project.write("taken/b.txt", "b");
    // No file system takes a name this long, so the last step of each of these fails
    // after the directories on the way were made.
    let too_long = "x".repeat(4_000);

    assert!(fileops::create_file(project.root(), &format!("fresh/dirs/{too_long}")).is_err());
    assert!(fileops::create_dir(project.root(), &format!("more/fresh/dirs/{too_long}")).is_err());
    assert!(fileops::create_dir(project.root(), &format!("early/{too_long}/late")).is_err());
    assert!(fileops::rename(&project.path("a.txt"), &format!("brand/new/{too_long}")).is_err());
    // Failures that are caught before anything is created.
    assert!(matches!(
        fileops::rename(&project.path("dir"), "dir/new/place"),
        Err(FileOpError::IntoItself(_))
    ));
    assert!(matches!(
        fileops::rename(&project.path("a.txt"), "taken/b.txt"),
        Err(FileOpError::AlreadyExists(_))
    ));
    assert_eq!(
        project.listing(),
        ["a.txt", "dir/", "dir/keep.txt", "taken/", "taken/b.txt"]
    );
}

#[test]
fn copies_count_up_until_a_name_is_free() {
    let project = Project::new();
    project.write("report.txt", "v1");
    for expected in ["report copy.txt", "report copy 2.txt", "report copy 3.txt"] {
        let copy = fileops::duplicate(&project.path("report.txt")).unwrap();
        assert_eq!(copy, project.path(expected));
    }
    // A gap is filled first.
    project.remove("report copy 2.txt");
    let copy = fileops::duplicate(&project.path("report.txt")).unwrap();
    assert_eq!(copy, project.path("report copy 2.txt"));
    // Copying a copy keeps appending.
    let copy = fileops::duplicate(&project.path("report copy.txt")).unwrap();
    assert_eq!(copy, project.path("report copy copy.txt"));
}

#[test]
fn a_directory_can_be_renamed_by_case_alone() {
    let project = Project::new();
    project.write("docs/guide.md", "text");
    let renamed = fileops::rename(&project.path("docs"), "Docs").unwrap();
    assert_eq!(renamed, project.path("Docs"));
    assert_eq!(project.listing(), ["Docs/", "Docs/guide.md"]);
}

#[test]
fn missing_paths_are_typed_errors() {
    let project = Project::new();
    project.mkdir("dir");
    let ghost = project.path("ghost.txt");
    assert!(matches!(
        fileops::rename(&ghost, "x"),
        Err(FileOpError::NotFound(_))
    ));
    assert!(matches!(
        fileops::move_into(&ghost, &project.path("dir")),
        Err(FileOpError::NotFound(_))
    ));
    assert!(matches!(
        fileops::copy_into(&ghost, &project.path("dir")),
        Err(FileOpError::NotFound(_))
    ));
    assert!(matches!(
        fileops::duplicate(&ghost),
        Err(FileOpError::NotFound(_))
    ));
    assert!(matches!(
        fileops::delete(&ghost),
        Err(FileOpError::NotFound(_))
    ));
    // Checked before the system trash is ever asked.
    assert!(matches!(
        fileops::trash(&ghost),
        Err(FileOpError::NotFound(_))
    ));
    assert!(matches!(
        fileops::copy_into(&project.path("dir"), &ghost),
        Err(FileOpError::NotFound(_))
    ));
    let message = fileops::delete(&ghost).unwrap_err().to_string();
    assert!(message.contains("ghost.txt"), "{message}");
}

#[test]
fn write_atomic_replaces_contents_in_one_step() {
    let project = Project::new();
    let path = project.path("config.toml");

    fileops::write_atomic(&path, b"first").unwrap();
    assert_eq!(project.read("config.toml"), "first");
    fileops::write_atomic(&path, b"second, and longer").unwrap();
    assert_eq!(project.read("config.toml"), "second, and longer");
    fileops::write_atomic(&path, b"").unwrap();
    assert_eq!(project.read("config.toml"), "");
    assert_eq!(
        project.listing(),
        ["config.toml"],
        "no temporary file is left"
    );

    // A missing directory is an error, not a created directory.
    assert!(fileops::write_atomic(&project.path("no/such/dir/file"), b"x").is_err());
    assert_eq!(project.listing(), ["config.toml"]);
}

#[cfg(unix)]
#[test]
fn write_atomic_keeps_permissions_and_writes_through_links() {
    use std::os::unix::fs::PermissionsExt;
    let project = Project::new();
    project.write("real/script.sh", "old");
    fs::set_permissions(
        project.path("real/script.sh"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let link = project.symlink("real/script.sh", "link.sh");

    fileops::write_atomic(&link, b"new").unwrap();
    assert_eq!(project.read("real/script.sh"), "new");
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the link stays"
    );
    let mode = fs::metadata(project.path("real/script.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);

    // A read-only file in a writable directory can still be replaced, and stays read-only.
    fs::set_permissions(
        project.path("real/script.sh"),
        fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    fileops::write_atomic(&project.path("real/script.sh"), b"newer").unwrap();
    assert_eq!(project.read("real/script.sh"), "newer");
    let mode = fs::metadata(project.path("real/script.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o444);
}

#[cfg(unix)]
#[test]
fn a_failed_atomic_write_leaves_the_old_file() {
    use std::os::unix::fs::PermissionsExt;
    let project = Project::new();
    project.write("locked/file.txt", "old");
    fs::set_permissions(project.path("locked"), fs::Permissions::from_mode(0o555)).unwrap();
    let result = fileops::write_atomic(&project.path("locked/file.txt"), b"new");
    fs::set_permissions(project.path("locked"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(result.is_err());
    assert_eq!(project.read("locked/file.txt"), "old");
    assert_eq!(project.listing(), ["locked/", "locked/file.txt"]);
}

#[cfg(unix)]
#[test]
fn sockets_are_not_copied() {
    use std::os::unix::net::UnixListener;
    // Socket paths have a short length limit, so this project sits directly in /tmp-like
    // territory: the temp directory's own name is short enough.
    let project = Project::new();
    project.write("d/data.txt", "data");
    let socket_path = project.path("d/s");
    let Ok(_listener) = UnixListener::bind(&socket_path) else {
        // The temp directory's path is too long for a socket on this system.
        return;
    };

    // Inside a directory, the socket is left out and everything else is copied.
    let copy = fileops::duplicate(&project.path("d")).unwrap();
    assert_eq!(copy, project.path("d copy"));
    assert_eq!(project.read("d copy/data.txt"), "data");
    assert!(!project.exists("d copy/s"));

    // On its own it is refused, and nothing is left behind.
    assert!(matches!(
        fileops::duplicate(&socket_path),
        Err(FileOpError::Unsupported(path)) if path == socket_path
    ));
    assert_eq!(
        project.listing(),
        ["d copy/", "d copy/data.txt", "d/", "d/data.txt", "d/s"]
    );
}

#[cfg(unix)]
#[test]
fn a_copy_that_fails_half_way_leaves_nothing_behind() {
    use std::os::unix::fs::PermissionsExt;
    let project = Project::new();
    project.write("src-dir/readable.txt", "ok");
    project.write("src-dir/secret/hidden.txt", "no");
    fs::set_permissions(
        project.path("src-dir/secret"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    let unreadable = fs::read_dir(project.path("src-dir/secret")).is_err();

    let result = fileops::copy_into(&project.path("src-dir"), &project.mkdir("dest"));
    fs::set_permissions(
        project.path("src-dir/secret"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    if !unreadable {
        // Running as a user who can read anything (root): nothing to check.
        return;
    }
    assert!(result.is_err());
    assert!(
        fs::read_dir(project.path("dest")).unwrap().next().is_none(),
        "the partial copy was removed"
    );
    assert_eq!(
        project.read("src-dir/readable.txt"),
        "ok",
        "the source is untouched"
    );
}

#[test]
fn copy_names_are_available_to_the_ui() {
    assert_eq!(fileops::copy_name("a.txt", false, 0), "a.txt");
    assert_eq!(fileops::copy_name("a.txt", false, 1), "a copy.txt");
    assert_eq!(fileops::copy_name("folder", true, 2), "folder copy 2");
}

/// Puts a file in the real system trash, so it only runs when asked for:
/// `cargo test -p bakedit-project --test fileops -- --ignored`.
#[test]
#[ignore = "moves a file to the real system trash"]
fn the_system_trash_takes_a_file() {
    let project = Project::new();
    let file = project.write("bakedit-trash-test.txt", "safe to delete");
    let dir = project.write("bakedit-trash-test-dir/inner.txt", "safe to delete");
    let dir = dir.parent().map(PathBuf::from).unwrap();

    fileops::trash(&file).unwrap();
    fileops::trash(&dir).unwrap();
    assert!(!file.exists());
    assert!(!dir.exists());
    assert!(project.listing().is_empty());
}
