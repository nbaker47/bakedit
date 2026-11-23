//! The child starts with default signal handling and its own controlling terminal, whatever
//! the process that started it does with its signals. This test changes how the whole test
//! process treats SIGINT, so it is alone in its file.

mod common;

use bakedit_core::{Key, KeyEvent, Mods};
use common::{sh, start};

#[test]
fn ctrl_c_interrupts_the_foreground_program() {
    // An application may well ignore the interrupt signal for itself. Ignored signals are
    // inherited across exec, and a shell cannot trap a signal that was ignored when it
    // started, so without a reset in the child this test never sees "interrupted".
    // SAFETY: changes this test process's disposition for SIGINT; no handler is installed.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
    }

    let script = "trap 'echo interrupted' INT; tty; ps -o pid= -o pgid= -o tpgid= -p $$; \
                  echo sleeping; sleep 100; echo \"after $?\"; exec cat";
    let session = start(sh(script));
    session.wait_for_text("sleeping");
    let term = &session.term;

    // The pty is the child's controlling terminal, and the child leads the process group
    // in its foreground: that is what makes the keyboard signals reach it.
    assert!(
        term.row_text(0).starts_with("/dev/"),
        "{}",
        term.row_text(0)
    );
    let ids: Vec<u32> = term
        .row_text(1)
        .split_whitespace()
        .map(|id| id.parse().expect("a number"))
        .collect();
    assert_eq!(
        ids,
        [term.pid(), term.pid(), term.pid()],
        "pid, process group and foreground group"
    );

    // ctrl-c is byte 3; the terminal driver turns it into SIGINT for the foreground group.
    assert!(term.send_key(&KeyEvent::new(Key::Char('c'), Mods::CTRL)));
    session.wait_for_text("interrupted");
    // sleep was killed by the signal (128 + 2), and the shell carried on.
    session.wait_for_text("after 130");
    assert!(term.is_running());
}
