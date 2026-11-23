//! The file index on real directories: what gets listed, how it follows changes, and how
//! the finder searches it.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bakedit_project::{parse_query, FileIndex, IndexOptions, IndexSnapshot};
use common::{wait_until, Project};

const TIMEOUT: Duration = Duration::from_secs(20);

fn ready_index(project: &Project) -> FileIndex {
    let index = FileIndex::new(project.root(), IndexOptions::default());
    assert!(index.wait_ready(TIMEOUT), "the first walk completes");
    index
}

fn files(index: &FileIndex) -> Vec<String> {
    index.snapshot().iter().map(str::to_string).collect()
}

/// Tell the index about changed paths and wait until it has dealt with them.
fn update(index: &FileIndex, paths: &[PathBuf]) {
    index.update(paths);
    assert!(index.wait_idle(TIMEOUT), "the update is applied");
}

fn found(index: &FileIndex, query: &str, recent: &[PathBuf]) -> Vec<String> {
    index
        .search(query, 50, recent)
        .into_iter()
        .map(|m| m.path.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn lists_files_relative_to_the_root() {
    let project = Project::with_files(&["src/main.rs", "src/ui/button.rs", "README.md", ".env"]);
    project.mkdir("empty-dir");
    let index = ready_index(&project);
    assert_eq!(
        files(&index),
        [".env", "README.md", "src/main.rs", "src/ui/button.rs"]
    );
    assert_eq!(index.len(), 4);
    assert!(!index.is_empty());
    assert_eq!(index.root(), project.root());
    assert!(index.is_ready());
    assert!(!index.is_scanning());
    assert_eq!(index.generation(), 1);
    assert!(index.snapshot().contains("src/main.rs"));
}

#[test]
fn an_index_is_empty_but_usable_before_it_is_ready() {
    let project = Project::with_files(&["a.txt"]);
    let index = FileIndex::new(project.root(), IndexOptions::default());
    // Whatever the timing, these never block and never panic.
    let _ = index.search("a", 10, &[]);
    let _ = index.len();
    let _ = index.is_scanning();
    assert!(index.wait_ready(TIMEOUT));
    assert_eq!(files(&index), ["a.txt"]);
}

#[test]
fn honours_gitignore_and_dot_ignore_and_never_lists_hidden_names() {
    let project = Project::with_files(&[
        "src/main.rs",
        "build-out/artifact.bin",
        "build-out/deep/more.bin",
        "notes.tmpdata",
        "docs/draft.md",
        "docs/final.md",
        "scratch/x.txt",
        ".git/HEAD",
        ".git/objects/ab/cd",
        ".DS_Store",
        "src/.DS_Store",
        ".github/workflows/ci.yml",
    ]);
    project.write(".gitignore", "build-out/\n*.tmpdata\n");
    project.write(".ignore", "scratch/\n");
    project.write("docs/.gitignore", "draft.md\n");
    let index = ready_index(&project);
    assert_eq!(
        files(&index),
        [
            ".github/workflows/ci.yml",
            ".gitignore",
            ".ignore",
            "docs/.gitignore",
            "docs/final.md",
            "src/main.rs",
        ]
    );
}

#[test]
fn ignored_files_can_be_included() {
    let project = Project::with_files(&["src/main.rs", "out/gen.rs", ".git/HEAD", ".DS_Store"]);
    project.write(".gitignore", "out/\n");
    let index = FileIndex::new(
        project.root(),
        IndexOptions {
            include_ignored: true,
        },
    );
    assert!(index.wait_ready(TIMEOUT));
    assert!(index.include_ignored());
    // Ignored files are listed; always-hidden names still are not.
    assert_eq!(files(&index), [".gitignore", "out/gen.rs", "src/main.rs"]);

    index.set_include_ignored(false);
    assert!(index.wait_idle(TIMEOUT));
    assert_eq!(files(&index), [".gitignore", "src/main.rs"]);
    assert_eq!(index.generation(), 2);

    // Setting it to what it already is does not walk again.
    index.set_include_ignored(false);
    assert!(index.wait_idle(TIMEOUT));
    assert_eq!(index.generation(), 2);
    index.set_include_ignored(true);
    assert!(index.wait_idle(TIMEOUT));
    assert_eq!(files(&index), [".gitignore", "out/gen.rs", "src/main.rs"]);
}

#[test]
fn inside_a_repository_the_rules_stop_at_its_root() {
    let outer = Project::new();
    outer.write(".gitignore", "*.outerdata\n");
    outer.write("repo/.git/HEAD", "ref: refs/heads/main\n");
    outer.write("repo/.gitignore", "*.innerdata\n");
    outer.write("repo/kept.outerdata", "");
    outer.write("repo/dropped.innerdata", "");
    outer.write("repo/src/lib.rs", "");
    let index = FileIndex::new(outer.path("repo"), IndexOptions::default());
    assert!(index.wait_ready(TIMEOUT));
    assert_eq!(
        files(&index),
        [".gitignore", "kept.outerdata", "src/lib.rs"]
    );
}

#[test]
fn a_subdirectory_of_a_repository_inherits_its_rules() {
    let repo = Project::new();
    repo.write(".git/HEAD", "ref: refs/heads/main\n");
    repo.write(".gitignore", "*.gen.rs\n/crates/app/secret.txt\n");
    repo.write("crates/app/src/main.rs", "");
    repo.write("crates/app/src/schema.gen.rs", "");
    repo.write("crates/app/secret.txt", "");
    let index = FileIndex::new(repo.path("crates/app"), IndexOptions::default());
    assert!(index.wait_ready(TIMEOUT));
    assert_eq!(files(&index), ["src/main.rs"]);
}

#[cfg(unix)]
#[test]
fn links_to_files_are_listed_and_links_to_directories_are_not_followed() {
    let project = Project::with_files(&["real/inside.txt", "target.txt"]);
    project.symlink("target.txt", "link-to-file");
    project.symlink("real", "link-to-dir");
    project.symlink("nowhere", "dangling");
    project.symlink("..", "real/up");
    let index = ready_index(&project);
    assert_eq!(
        files(&index),
        ["link-to-file", "real/inside.txt", "target.txt"]
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_root_is_walked() {
    let project = Project::with_files(&["real/src/main.rs", "real/README.md"]);
    let link = project.symlink("real", "link");
    let index = FileIndex::new(&link, IndexOptions::default());
    assert!(index.wait_ready(TIMEOUT));
    assert_eq!(files(&index), ["README.md", "src/main.rs"]);
    assert_eq!(index.root(), link);
}

#[test]
fn a_missing_root_is_an_empty_index() {
    let project = Project::new();
    let index = FileIndex::new(project.path("nope"), IndexOptions::default());
    assert!(index.wait_ready(TIMEOUT));
    assert!(index.is_empty());
    assert!(index.search("x", 10, &[]).is_empty());
}

#[test]
fn created_changed_and_removed_files_are_followed() {
    let project = Project::with_files(&["src/main.rs", "old.txt"]);
    let index = ready_index(&project);
    assert_eq!(index.generation(), 1);

    let created = project.write("src/new.rs", "");
    update(&index, &[created]);
    assert_eq!(files(&index), ["old.txt", "src/main.rs", "src/new.rs"]);
    assert_eq!(index.generation(), 2);

    // A change to a file that is already listed changes nothing.
    project.write("src/new.rs", "edited");
    update(&index, &[project.path("src/new.rs")]);
    assert_eq!(index.generation(), 2);

    project.remove("old.txt");
    update(&index, &[project.path("old.txt")]);
    assert_eq!(files(&index), ["src/main.rs", "src/new.rs"]);
    assert_eq!(index.generation(), 3);

    // A rename is the old path going and the new one appearing.
    std::fs::rename(project.path("src/new.rs"), project.path("src/renamed.rs")).unwrap();
    update(
        &index,
        &[project.path("src/new.rs"), project.path("src/renamed.rs")],
    );
    assert_eq!(files(&index), ["src/main.rs", "src/renamed.rs"]);

    // Relative paths, paths outside the root and paths that never existed are fine.
    project.write("rel.txt", "");
    update(
        &index,
        &[
            PathBuf::from("rel.txt"),
            PathBuf::from("/somewhere/else.txt"),
            project.path("never-existed.txt"),
        ],
    );
    assert_eq!(files(&index), ["rel.txt", "src/main.rs", "src/renamed.rs"]);
}

#[test]
fn whole_directories_come_and_go() {
    let project = Project::with_files(&["keep.txt"]);
    let index = ready_index(&project);

    // Only the directory is reported; what is inside is found by walking it.
    project.write("pkg/src/lib.rs", "");
    project.write("pkg/src/deep/mod.rs", "");
    project.write("pkg/Cargo.toml", "");
    update(&index, &[project.path("pkg")]);
    assert_eq!(
        files(&index),
        [
            "keep.txt",
            "pkg/Cargo.toml",
            "pkg/src/deep/mod.rs",
            "pkg/src/lib.rs"
        ]
    );

    // The directory and its files reported together are not counted twice.
    project.write("pkg/src/extra.rs", "");
    update(
        &index,
        &[
            project.path("pkg/src"),
            project.path("pkg/src/extra.rs"),
            project.path("pkg"),
        ],
    );
    assert_eq!(index.len(), 5);

    // A directory renamed: the old name reported, the new one reported.
    std::fs::rename(project.path("pkg"), project.path("crate")).unwrap();
    update(&index, &[project.path("pkg"), project.path("crate")]);
    assert_eq!(
        files(&index),
        [
            "crate/Cargo.toml",
            "crate/src/deep/mod.rs",
            "crate/src/extra.rs",
            "crate/src/lib.rs",
            "keep.txt",
        ]
    );

    project.remove("crate");
    update(&index, &[project.path("crate")]);
    assert_eq!(files(&index), ["keep.txt"]);

    // A file replaced by a directory of the same name, and back.
    project.remove("keep.txt");
    project.write("keep.txt/inner.txt", "");
    update(&index, &[project.path("keep.txt")]);
    assert_eq!(files(&index), ["keep.txt/inner.txt"]);
    project.remove("keep.txt");
    project.write("keep.txt", "");
    update(&index, &[project.path("keep.txt")]);
    assert_eq!(files(&index), ["keep.txt"]);
}

#[test]
fn updates_respect_ignore_rules_and_hidden_names() {
    let project = Project::with_files(&["src/main.rs"]);
    project.write(".gitignore", "out/\n*.tmpdata\n");
    let index = ready_index(&project);
    let before = index.generation();

    project.write("out/build.log", "");
    project.write("cache.tmpdata", "");
    project.write(".git/index", "");
    project.write("src/.DS_Store", "");
    update(
        &index,
        &[
            project.path("out"),
            project.path("out/build.log"),
            project.path("cache.tmpdata"),
            project.path(".git/index"),
            project.path("src/.DS_Store"),
        ],
    );
    assert_eq!(files(&index), [".gitignore", "src/main.rs"]);
    assert_eq!(index.generation(), before, "nothing listed changed");
}

#[test]
fn a_changed_ignore_file_rebuilds_the_list() {
    let project = Project::with_files(&["src/main.rs", "logs/app.log", "logs/old/x.log"]);
    let index = ready_index(&project);
    assert_eq!(index.len(), 3);

    let gitignore = project.write(".gitignore", "logs/\n");
    update(&index, std::slice::from_ref(&gitignore));
    assert_eq!(files(&index), [".gitignore", "src/main.rs"]);

    // And the worker's own rules were reloaded: a new file under logs/ stays out...
    project.write("logs/new.log", "");
    update(&index, &[project.path("logs/new.log")]);
    assert_eq!(files(&index), [".gitignore", "src/main.rs"]);

    // ...until the rule is lifted again.
    project.write(".gitignore", "");
    update(&index, &[gitignore]);
    assert_eq!(
        files(&index),
        [
            ".gitignore",
            "logs/app.log",
            "logs/new.log",
            "logs/old/x.log",
            "src/main.rs"
        ]
    );
}

#[test]
fn rebuild_walks_everything_again() {
    let project = Project::with_files(&["a.txt"]);
    let index = ready_index(&project);
    project.write("b/c.txt", "");
    project.remove("a.txt");
    // No update was sent, so the list is stale.
    assert_eq!(files(&index), ["a.txt"]);

    index.rebuild();
    assert!(index.wait_idle(TIMEOUT));
    assert_eq!(files(&index), ["b/c.txt"]);
    assert_eq!(index.generation(), 2);

    // A rebuild that finds the same files does not bump the generation.
    index.rebuild();
    assert!(index.wait_idle(TIMEOUT));
    assert_eq!(index.generation(), 2);
}

#[test]
fn a_huge_batch_falls_back_to_a_rebuild() {
    let project = Project::new();
    let index = ready_index(&project);
    let mut paths = Vec::new();
    for n in 0..2_500 {
        paths.push(project.write(&format!("gen/file{n}.txt"), ""));
    }
    update(&index, &paths);
    assert_eq!(index.len(), 2_500);
}

#[test]
fn the_wake_callback_fires_when_the_list_changes() {
    let project = Project::with_files(&["a.txt"]);
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&wakes);
    let index = FileIndex::with_wake(
        project.root(),
        IndexOptions::default(),
        Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    );
    assert!(wait_until(TIMEOUT, || wakes.load(Ordering::SeqCst) == 1));
    assert!(index.is_ready());

    project.write("b.txt", "");
    index.update(&[project.path("b.txt")]);
    assert!(wait_until(TIMEOUT, || wakes.load(Ordering::SeqCst) == 2));
    assert_eq!(index.generation(), 2);

    // No change, no wake.
    index.update(&[project.path("b.txt")]);
    assert!(index.wait_idle(TIMEOUT));
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
}

#[test]
fn dropping_an_index_stops_its_thread_promptly() {
    let project = Project::new();
    for n in 0..3_000 {
        project.write(&format!("d{}/file{n}.txt", n % 50), "");
    }
    let started = Instant::now();
    for _ in 0..20 {
        // Dropped while the first walk is most likely still running.
        let index = FileIndex::new(project.root(), IndexOptions::default());
        index.rebuild();
        index.update(&[project.path("d0")]);
        drop(index);
    }
    assert!(started.elapsed() < Duration::from_secs(30));
}

#[test]
fn search_ranks_file_names_over_directories() {
    let project = Project::with_files(&[
        "crates/core/src/keys.rs",
        "crates/core/src/lib.rs",
        "crates/keys/src/lib.rs",
        "docs/keyboard.md",
        "assets/keymaps/default.json",
    ]);
    let index = ready_index(&project);

    // The file called keys, then the directory called keys, then a looser match.
    assert_eq!(
        found(&index, "keys", &[]),
        [
            "crates/core/src/keys.rs",
            "crates/keys/src/lib.rs",
            "assets/keymaps/default.json"
        ]
    );
    // A name match first, directory matches after it.
    assert_eq!(
        found(&index, "core", &[])[..2],
        ["crates/core/src/lib.rs", "crates/core/src/keys.rs"]
    );
    assert_eq!(
        found(&index, "core/keys", &[])[0],
        "crates/core/src/keys.rs"
    );
    assert_eq!(found(&index, "lib keys", &[]), ["crates/keys/src/lib.rs"]);
    assert!(found(&index, "nothing-like-this", &[]).is_empty());

    let matches = index.search("keys", 1, &[]);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].path, Path::new("crates/core/src/keys.rs"));
    assert_eq!(matches[0].indices, vec![16, 17, 18, 19]);
}

#[test]
fn search_is_smart_case() {
    let project = Project::with_files(&["README.md", "docs/readme-notes.md", "src/Reader.rs"]);
    let index = ready_index(&project);
    assert_eq!(
        found(&index, "readme", &[]),
        ["README.md", "docs/readme-notes.md"]
    );
    assert_eq!(found(&index, "README", &[]), ["README.md"]);
    assert_eq!(found(&index, "Reader", &[]), ["src/Reader.rs"]);
}

#[test]
fn an_empty_query_lists_recent_files_that_still_exist() {
    let project = Project::with_files(&["a.rs", "b.rs", "c.rs", "ignored/secret.env"]);
    project.write(".gitignore", "ignored/\n");
    let index = ready_index(&project);
    let recent = vec![
        project.path("c.rs"),
        PathBuf::from("a.rs"),
        project.path("deleted.rs"),
        PathBuf::from("/outside/the/project.rs"),
        project.path("ignored/secret.env"),
        project.path("c.rs"),
        project.root().to_path_buf(),
    ];
    // In order, without the missing, outside and repeated ones; an ignored file that
    // was opened is still a recent file.
    assert_eq!(
        found(&index, "", &recent),
        ["c.rs", "a.rs", "ignored/secret.env"]
    );
    assert_eq!(found(&index, "   ", &recent).len(), 3);
    assert_eq!(index.search("", 2, &recent).len(), 2);
    assert!(found(&index, "", &[]).is_empty());

    let matches = index.search("", 10, &recent);
    assert!(matches.iter().all(|m| m.score == 0 && m.indices.is_empty()));
}

#[test]
fn recent_files_break_ties_and_reach_ignored_files() {
    let project = Project::with_files(&["a/mod.rs", "b/mod.rs", "c/mod.rs", "out/mod.rs"]);
    project.write(".gitignore", "out/\n");
    let index = ready_index(&project);
    assert_eq!(
        found(&index, "mod", &[]),
        ["a/mod.rs", "b/mod.rs", "c/mod.rs"]
    );
    assert_eq!(
        found(
            &index,
            "mod",
            &[project.path("c/mod.rs"), project.path("out/mod.rs")]
        ),
        ["c/mod.rs", "out/mod.rs", "a/mod.rs", "b/mod.rs"]
    );
}

#[test]
fn a_typed_position_is_split_off_before_searching() {
    let project = Project::with_files(&["src/main.rs", "src/domain.rs"]);
    let index = ready_index(&project);
    let query = parse_query("main.rs:42:7");
    assert_eq!((query.line, query.col), (Some(42), Some(7)));
    assert_eq!(query.path_query, "main.rs");
    assert_eq!(
        found(&index, &query.path_query, &[]),
        ["src/main.rs", "src/domain.rs"]
    );
    // Searching the raw text would look for a file with colons in its name.
    assert!(found(&index, "main.rs:42:7", &[]).is_empty());
}

#[test]
fn a_few_thousand_files_index_and_search_quickly() {
    let project = Project::new();
    for n in 0..4_000 {
        project.write(&format!("pkg{}/mod{}/file{n}.rs", n % 40, n % 7), "");
    }
    project.write("pkg3/the_needle.rs", "");
    let started = Instant::now();
    let index = ready_index(&project);
    let indexed_in = started.elapsed();
    assert_eq!(index.len(), 4_001);

    let started = Instant::now();
    let matches = index.search("theneedle", 10, &[]);
    let searched_in = started.elapsed();
    assert_eq!(matches[0].path, Path::new("pkg3/the_needle.rs"));
    assert!(
        indexed_in < Duration::from_secs(10),
        "indexing took {indexed_in:?}"
    );
    assert!(
        searched_in < Duration::from_millis(500),
        "searching took {searched_in:?}"
    );
}

/// 200,000 paths shaped like a real monorepo.
fn monorepo_paths() -> Vec<String> {
    let areas = ["apps", "crates", "packages", "services", "tools", "vendor"];
    let kinds = ["src", "tests", "benches", "examples", "docs"];
    let stems = [
        "main",
        "lib",
        "mod",
        "index",
        "config",
        "handler",
        "model",
        "view",
        "controller",
        "utils",
        "types",
        "error",
        "client",
        "server",
        "router",
        "schema",
    ];
    let extensions = ["rs", "ts", "tsx", "py", "go", "json", "md", "toml"];
    (0..200_000)
        .map(|n| {
            // About two dozen files to a directory, some 8,000 directories in all.
            let dir = n / 24;
            format!(
                "{}/project_{}/{}/module_{}/{}_{}.{}",
                areas[dir % areas.len()],
                dir % 331,
                kinds[(dir / 7) % kinds.len()],
                dir % 53,
                stems[n % stems.len()],
                n,
                extensions[n % extensions.len()]
            )
        })
        .collect()
}

#[test]
fn two_hundred_thousand_files_search_in_milliseconds() {
    let mut paths = monorepo_paths();
    paths.push("crates/project_7/src/module_3/file_tree.rs".to_string());
    let snapshot = IndexSnapshot::from_paths(paths);
    assert_eq!(snapshot.len(), 200_001);

    let queries = [
        "filetree",
        "main",
        "handler 199",
        "crates/project_7/src",
        "m",
        "zzzzqqq",
    ];
    let mut slowest = Duration::ZERO;
    for query in queries {
        // The first run of each query pays for thread start-up; time the steady state.
        let _ = snapshot.search(query, 50, &[]);
        let started = Instant::now();
        let matches = snapshot.search(query, 50, &[]);
        let elapsed = started.elapsed();
        slowest = slowest.max(elapsed);
        println!("{query:>22}: {:>3} matches in {elapsed:?}", matches.len());
    }
    assert_eq!(
        snapshot.search("filetree", 5, &[])[0].path,
        Path::new("crates/project_7/src/module_3/file_tree.rs")
    );
    // "A few milliseconds" on the machine this was written on; the bound leaves room
    // for a loaded machine and an unoptimised test build.
    assert!(
        slowest < Duration::from_millis(400),
        "slowest query took {slowest:?}"
    );
}

/// Creates 200,000 real files, so it only runs when asked for:
/// `cargo test -p bakedit-project --test index -- --ignored --nocapture`.
#[test]
#[ignore = "writes 200,000 files to a temp directory"]
fn two_hundred_thousand_files_on_disk() {
    let project = Project::new();
    let paths = monorepo_paths();
    for path in &paths {
        project.write(path, "");
    }
    let on_disk = paths.len();

    let started = Instant::now();
    let index = FileIndex::new(project.root(), IndexOptions::default());
    // The caller is not held up while the walk runs.
    let first_search = Instant::now();
    let _ = index.search("main", 50, &[]);
    assert!(first_search.elapsed() < Duration::from_millis(100));
    assert!(index.wait_ready(Duration::from_secs(120)));
    println!("indexed {} files in {:?}", index.len(), started.elapsed());
    assert_eq!(index.len(), on_disk);

    let started = Instant::now();
    let matches = index.search("handler", 50, &[]);
    println!(
        "searched in {:?}, {} matches",
        started.elapsed(),
        matches.len()
    );
    assert_eq!(matches.len(), 50);

    let created = project.write("apps/project_0/src/module_0/brand_new.rs", "");
    let started = Instant::now();
    index.update(&[created]);
    assert!(index.wait_idle(TIMEOUT));
    println!("applied one change in {:?}", started.elapsed());
    assert_eq!(index.len(), on_disk + 1);
}
