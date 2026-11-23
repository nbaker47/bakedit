//! Sizes and concurrency a day of real use reaches: long path lists, large diffs, and
//! status running while the index changes.

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use bakedit_git::{DiffTarget, HunkAction, StageState};
use common::TestRepo;

#[test]
fn more_paths_than_fit_on_a_command_line() {
    let test = TestRepo::with_commit();
    // Long names: together the paths come to over two megabytes, more than an operating
    // system accepts as the arguments of one command.
    let directory = format!(
        "{}/{}",
        "a-long-directory-name-".repeat(8),
        "nested-".repeat(20)
    );
    let paths: Vec<PathBuf> = (0..4_000)
        .map(|number| {
            PathBuf::from(format!(
                "{directory}/file-{number:05}-{}.txt",
                "with-a-long-name-".repeat(12)
            ))
        })
        .collect();
    let total: usize = paths.iter().map(|path| path.as_os_str().len()).sum();
    assert!(total > 2_100_000, "only {total} bytes of paths");
    std::fs::create_dir_all(test.path(&directory)).unwrap();
    for path in &paths {
        std::fs::write(test.root().join(path), "x\n").unwrap();
    }

    test.repo.stage(&paths).unwrap();
    let status = test.repo.status().unwrap();
    assert_eq!(status.entries.len(), paths.len());
    assert!(status.entries.iter().all(|entry| entry.is_fully_staged()));

    // All but the first hundred back out again.
    test.repo.unstage(&paths[100..]).unwrap();
    let status = test.repo.status().unwrap();
    assert_eq!(status.counts().staged, 100);
    assert_eq!(status.counts().untracked, paths.len() - 100);

    let outcome = test.repo.restore(&paths).unwrap();
    assert!(outcome.restored.is_empty());
    assert_eq!(outcome.untracked.len(), paths.len() - 100);
}

#[test]
fn a_diff_of_a_hundred_thousand_lines_and_its_hunk() {
    let test = TestRepo::new();
    let lines = 100_000;
    let old: String = (0..lines)
        .map(|line| format!("old line {line}\n"))
        .collect();
    let new: String = (0..lines)
        .map(|line| format!("new line {line}\n"))
        .collect();
    test.write("big.txt", &old);
    test.commit_all("big");
    test.write("big.txt", &new);

    let diff = test
        .repo
        .diff_file("big.txt", DiffTarget::IndexToWorktree)
        .unwrap()
        .unwrap();
    assert_eq!(diff.hunks.len(), 1);
    assert_eq!((diff.additions(), diff.deletions()), (lines, lines));
    let hunk = &diff.hunks[0];
    assert_eq!(hunk.lines.len(), 2 * lines);
    assert_eq!(
        hunk.lines[lines - 1].text,
        format!("old line {}", lines - 1)
    );
    assert_eq!(hunk.lines[lines - 1].old_no, Some(lines as u32));
    assert_eq!(hunk.lines[2 * lines - 1].new_no, Some(lines as u32));

    // The patch for that one hunk is megabytes on git's standard input.
    test.repo.apply_hunk(&diff, 0, HunkAction::Stage).unwrap();
    assert_eq!(test.repo.index_text("big.txt").unwrap().unwrap(), new);
    assert_eq!(test.repo.head_text("big.txt").unwrap().unwrap(), old);
}

#[test]
fn status_keeps_answering_while_the_index_is_being_changed() {
    let test = TestRepo::with_commit();
    for number in 0..40 {
        test.write(&format!("file-{number}.txt"), "content\n");
    }
    let done = Arc::new(AtomicBool::new(false));

    let readers: Vec<_> = (0..4)
        .map(|_| {
            let repo = test.repo.clone();
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                let mut answers = 0;
                while !done.load(Ordering::Relaxed) {
                    let status = repo.status().expect("status never fails or waits");
                    assert_eq!(status.entries.len(), 40);
                    assert_eq!(status.branch.as_deref(), Some("main"));
                    answers += 1;
                }
                answers
            })
        })
        .collect();

    // One writer, as the IDE has: stage everything, unstage everything, again.
    for _ in 0..15 {
        test.repo.stage_all().unwrap();
        test.repo.unstage_all().unwrap();
    }
    done.store(true, Ordering::Relaxed);
    for reader in readers {
        assert!(reader.join().unwrap() > 0);
    }

    let status = test.repo.status().unwrap();
    assert!(status
        .entries
        .iter()
        .all(|entry| entry.stage_state() == StageState::Unstaged));
}
