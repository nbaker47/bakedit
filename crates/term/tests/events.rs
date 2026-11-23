//! What the terminal tells the application, and what it tells the child when the child asks.

mod common;

use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use bakedit_core::Rgb;
use bakedit_term::{TermColors, TermConfig, TermEvent, Terminal};
use common::{echo, sh, start, TIMEOUT};

fn colors() -> TermColors {
    let mut colors = TermColors {
        fg: Rgb::new(0xa1, 0xb2, 0xc3),
        bg: Rgb::new(0x12, 0x34, 0x56),
        cursor: Rgb::new(0xff, 0x80, 0x00),
        ..TermColors::default()
    };
    colors.ansi[3] = Rgb::new(0x0a, 0x0b, 0x0c);
    colors
}

#[test]
fn queries_are_answered_in_the_order_they_were_asked() {
    // A program asks for the background colour and then for the device attributes, and
    // takes the attributes arriving first to mean "no colour support". The two answers
    // come from different places in the emulator; they must not overtake each other.
    let session = start(TermConfig {
        colors: colors(),
        ..echo("\\x1b]11;?\\x07\\x1b[c")
    });
    session.wait_ready();
    let mut expected = b"\x1b]11;rgb:1212/3434/5656\x07\x1b[?6c".to_vec();
    session.wait_received(&expected);

    let mut ask = |query: &str, answer: &str| {
        session.child_writes(query);
        expected.extend_from_slice(answer.as_bytes());
        session.wait_received(&expected);
    };
    // Colours, with the terminator the question used.
    ask("\x1b]10;?\x1b\\", "\x1b]10;rgb:a1a1/b2b2/c3c3\x1b\\");
    ask("\x1b]12;?\x07", "\x1b]12;rgb:ffff/8080/0000\x07");
    ask("\x1b]4;3;?\x07", "\x1b]4;3;rgb:0a0a/0b0b/0c0c\x07");
    ask("\x1b]4;196;?\x07", "\x1b]4;196;rgb:ffff/0000/0000\x07");
    ask("\x1b]4;244;?\x07", "\x1b]4;244;rgb:8080/8080/8080\x07");
    // Several in one sequence.
    ask(
        "\x1b]10;?;?\x07",
        "\x1b]10;rgb:a1a1/b2b2/c3c3\x07\x1b]11;rgb:1212/3434/5656\x07",
    );
    // A colour the child redefined is reported as redefined, until it resets it.
    ask(
        "\x1b]4;5;rgb:aa/bb/cc\x07\x1b]4;5;?\x07",
        "\x1b]4;5;rgb:aaaa/bbbb/cccc\x07",
    );
    ask(
        "\x1b]11;#102030\x07\x1b]11;?\x07",
        "\x1b]11;rgb:1010/2020/3030\x07",
    );
    ask(
        "\x1b]111\x07\x1b]11;?\x07",
        "\x1b]11;rgb:1212/3434/5656\x07",
    );

    // Status and modes.
    ask("\x1b[5n", "\x1b[0n");
    ask("\x1b[?u", "\x1b[?0u");
    ask("\x1b[?2026$p", "\x1b[?2026;2$y");
    ask("\x1b[?25$p", "\x1b[?25;1$y");
    ask("\x1b[?1004$p", "\x1b[?1004;2$y");
    ask("\x1b[?9999$p", "\x1b[?9999;0$y");
    ask("\x1b[4$p", "\x1b[4;2$y");
    // The text area, in cells and in pixels (200x50 cells of 8x16).
    ask("\x1b[18t", "\x1b[8;50;200t");
    ask("\x1b[14t", "\x1b[4;800;1600t");
    // The cursor position: the child has been printing a line per answer, and is at the
    // start of the next one.
    session.wait_until("the last line to end", |term| {
        term.cursor().is_some_and(|cursor| cursor.col == 0)
    });
    let cursor = session.term.cursor().expect("a visible cursor");
    ask(
        "\x1b[6n",
        &format!("\x1b[{};{}R", cursor.row + 1, cursor.col + 1),
    );
}

#[test]
fn a_theme_change_changes_the_colours_reported() {
    let session = start(TermConfig {
        colors: colors(),
        ..echo("")
    });
    session.wait_ready();
    session.child_writes("\x1b]11;?\x07");
    session.wait_received(b"\x1b]11;rgb:1212/3434/5656\x07");

    let generation = session.term.generation();
    session.term.set_colors(TermColors {
        bg: Rgb::new(0xfa, 0xfb, 0xfc),
        ..colors()
    });
    assert!(session.term.generation() > generation);
    session.child_writes("\x1b]11;?\x07");
    session.wait_received(b"\x1b]11;rgb:1212/3434/5656\x07\x1b]11;rgb:fafa/fbfb/fcfc\x07");
}

#[test]
fn the_size_in_pixels_follows_resizes() {
    let session = start(TermConfig {
        cell_width: 9,
        cell_height: 18,
        ..echo("\\x1b[14t")
    });
    session.wait_ready();
    let mut expected = format!("\x1b[4;{};{}t", 50 * 18, 200 * 9).into_bytes();
    session.wait_received(&expected);
    session.term.resize(100, 40);
    session.term.set_cell_size(10, 20);
    session.child_writes("\x1b[14t\x1b[18t");
    expected.extend_from_slice(b"\x1b[4;800;1000t\x1b[8;40;100t");
    session.wait_received(&expected);
}

#[test]
fn titles_are_reported_once_each() {
    let mut session = start(echo(""));
    session.wait_ready();
    assert_eq!(session.term.title(), None);

    session.child_writes("\x1b]0;one\x07");
    assert_eq!(
        session.wait_for_event("the first title", |event| matches!(
            event,
            TermEvent::Title(_)
        )),
        TermEvent::Title("one".to_string())
    );
    assert_eq!(session.term.title().as_deref(), Some("one"));

    // The same title again is not news; a new one is, whichever sequence sets it.
    session.child_writes("\x1b]2;one\x07\x1b]2;two; with a semicolon\x1b\\");
    assert_eq!(
        session.wait_for_event("the second title", |event| matches!(
            event,
            TermEvent::Title(_)
        )),
        TermEvent::Title("two; with a semicolon".to_string())
    );

    // The title stack: push, change, pop.
    session.child_writes("\x1b[22t\x1b]0;three\x07\x1b[23t\x1b]0;last\x07");
    let mut titles = Vec::new();
    while titles.last().map(String::as_str) != Some("last") {
        match session.wait_for_event("the stacked titles", |event| {
            matches!(event, TermEvent::Title(_))
        }) {
            TermEvent::Title(title) => titles.push(title),
            other => unreachable!("{other:?}"),
        }
    }
    assert_eq!(titles, ["three", "two; with a semicolon", "last"]);
    assert_eq!(session.term.title().as_deref(), Some("last"));
    assert!(!session
        .events_so_far()
        .iter()
        .any(|event| matches!(event, TermEvent::Title(_) | TermEvent::ResetTitle)));
}

#[test]
fn a_title_can_be_reset() {
    // Pushed before any title was set, so popping goes back to having none.
    let mut session = start(echo("\\x1b[22t\\x1b]0;temporary\\x07"));
    session.wait_ready();
    session.wait_for_event("the title", |event| {
        *event == TermEvent::Title("temporary".to_string())
    });
    session.child_writes("\x1b[23t");
    session.wait_for_event("the reset", |event| *event == TermEvent::ResetTitle);
    assert_eq!(session.term.title(), None);
}

#[test]
fn the_clipboard_can_be_written_and_read_by_the_child() {
    let mut session = start(echo("\\x1b]52;c;aGVsbG8gd29ybGQ=\\x07"));
    session.wait_ready();
    let stored = session.wait_for_event("the clipboard write", |event| {
        matches!(event, TermEvent::ClipboardStore(_))
    });
    assert_eq!(stored, TermEvent::ClipboardStore("hello world".to_string()));

    // Nothing is waiting for an answer yet.
    assert!(!session.term.respond_clipboard("too early"));

    session.child_writes("\x1b]52;c;?\x07");
    session.wait_for_event("the clipboard request", |event| {
        *event == TermEvent::ClipboardLoad
    });
    assert!(session.term.respond_clipboard("hi"));
    session.wait_received(b"\x1b]52;c;aGk=\x07");
    // One answer per request.
    assert!(!session.term.respond_clipboard("again"));

    // The primary selection is a clipboard too, and the terminator is kept.
    session.child_writes("\x1b]52;p;d29ybGQ=\x07\x1b]52;p;?\x1b\\");
    assert_eq!(
        session.wait_for_event("the selection write", |event| matches!(
            event,
            TermEvent::ClipboardStore(_)
        )),
        TermEvent::ClipboardStore("world".to_string())
    );
    session.wait_for_event("the selection request", |event| {
        *event == TermEvent::ClipboardLoad
    });
    assert!(session.term.respond_clipboard(""));
    session.wait_received(b"\x1b]52;c;aGk=\x07\x1b]52;p;\x1b\\");
}

#[test]
fn the_bell_rings_but_not_in_a_flood() {
    let mut session = start(echo(""));
    session.wait_ready();
    session.child_writes("\x07");
    session.wait_for_event("the bell", |event| *event == TermEvent::Bell);

    std::thread::sleep(Duration::from_millis(150));
    session.child_writes("\x07".repeat(200));
    session.wait_for_event("a bell from the flood", |event| *event == TermEvent::Bell);
    std::thread::sleep(Duration::from_millis(300));
    let bells = session
        .events_so_far()
        .iter()
        .filter(|event| **event == TermEvent::Bell)
        .count();
    assert!(bells <= 3, "{bells} more bells from one burst");
}

#[test]
fn the_working_directory_and_notifications_are_passed_on() {
    let mut session = start(echo("\\x1b]7;file:///tmp/some%20dir\\x07"));
    session.wait_ready();
    let cwd = session.wait_for_event("the working directory", |event| {
        matches!(event, TermEvent::Cwd(_))
    });
    assert_eq!(cwd, TermEvent::Cwd(PathBuf::from("/tmp/some dir")));

    let note = |title: &str, body: &str| TermEvent::Notification {
        title: title.to_string(),
        body: body.to_string(),
    };
    let mut expect = |sequence: &str, wanted: TermEvent| {
        session.child_writes(sequence);
        let got = session.wait_for_event("a notification", |event| {
            matches!(event, TermEvent::Notification { .. } | TermEvent::Cwd(_))
        });
        assert_eq!(got, wanted, "after {sequence:?}");
    };
    expect("\x1b]9;Build finished\x07", note("", "Build finished"));
    expect(
        "\x1b]777;notify;Agent;Needs your input\x1b\\",
        note("Agent", "Needs your input"),
    );
    expect(
        "\x1b]99;i=1:d=0;Tests\x1b\\\x1b]99;i=1:p=body;12 passed\x1b\\",
        note("Tests", "12 passed"),
    );
    expect(
        "\x1b]7;file://localhost/var/tmp\x07",
        TermEvent::Cwd(PathBuf::from("/var/tmp")),
    );

    // A progress report and a directory on another machine are neither.
    session.child_writes(
        "\x1b]9;4;1;50\x07\x1b]7;file://some-other-host.invalid/home/me\x07\x1b]9;done\x07",
    );
    let got = session.wait_for_event("the next notification", |event| {
        matches!(event, TermEvent::Notification { .. } | TermEvent::Cwd(_))
    });
    assert_eq!(got, note("", "done"));
    // None of it was shown as text.
    assert!(!session.term.screen_text().contains("Build finished"));
}

#[test]
fn a_burst_of_output_wakes_the_application_a_few_times_not_thousands() {
    let interval = Duration::from_millis(50);
    let started = Instant::now();
    let mut session = start(TermConfig {
        wakeup_interval: interval,
        ..sh("seq 1 200000; exec cat")
    });

    // Count wakeups until one arrives with the output complete. That one has to come: the
    // last change is always followed by a wakeup, however many were folded together.
    let mut wakeups = 0u32;
    loop {
        assert!(
            started.elapsed() < TIMEOUT,
            "no wakeup after the last line; {wakeups} before it"
        );
        if let Some(TermEvent::Wakeup) = session.next_event(Duration::from_millis(500)) {
            wakeups += 1;
            if session.term.row_text(22) == "200000" {
                break;
            }
        }
    }
    let allowed = started.elapsed().as_millis() / interval.as_millis() + 2;
    assert!(
        u128::from(wakeups) <= allowed,
        "{wakeups} wakeups in {:?}, at most {allowed} expected",
        started.elapsed()
    );
    assert!(session.term.history_len() > 0);

    // And with nothing happening there are none.
    std::thread::sleep(interval * 3);
    session.events_so_far();
    assert_eq!(session.next_event(Duration::from_millis(300)), None);
}

#[test]
fn a_single_change_wakes_the_application_at_once() {
    let mut session = start(echo(""));
    session.wait_ready();
    std::thread::sleep(Duration::from_millis(100));
    session.events_so_far();

    let sent = Instant::now();
    assert!(session.term.send_text("x"));
    session.wait_for_event("a wakeup for the echo", |event| *event == TermEvent::Wakeup);
    // Not exact science on a busy machine, but far from a polling interval.
    assert!(sent.elapsed() < Duration::from_secs(5));
    session.wait_received(b"x");
}

#[test]
fn an_unfinished_synchronized_update_is_shown_after_a_moment() {
    // The child starts a synchronized update and never ends it. The emulator gives up
    // waiting after 150 ms and shows what it has, long before the child's five seconds.
    let started = Instant::now();
    let session = start(sh(
        "printf '\\033[?2026hpending'; sleep 5; printf '\\033[?2026l'; exec cat",
    ));
    session.wait_for_text("pending");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn the_event_callback_may_use_the_terminal() {
    // The callback is not run with the terminal locked, so calling back into it works.
    let slot: Arc<Mutex<Option<Arc<Terminal>>>> = Arc::new(Mutex::new(None));
    let (sender, receiver) = mpsc::channel();
    let callback_slot = slot.clone();
    let on_event = Box::new(move |event: TermEvent| {
        if let TermEvent::Title(title) = event {
            let term = callback_slot.lock().unwrap().clone();
            if let Some(term) = term {
                let report = (
                    title,
                    term.title(),
                    term.modes().alt_screen,
                    term.screen_text().contains("READY"),
                    term.cursor().is_some(),
                );
                assert!(term.send_text("from the callback"));
                let _ = sender.send(report);
            }
        }
    });
    let term = Arc::new(Terminal::spawn(echo(""), on_event).expect("the terminal starts"));
    *slot.lock().unwrap() = Some(term.clone());

    let deadline = Instant::now() + TIMEOUT;
    while !term.screen_text().contains("READY") {
        assert!(Instant::now() < deadline, "the child never got ready");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(term.send_text("!1b5d303b68656c6c6f07"));
    let report = receiver
        .recv_timeout(TIMEOUT)
        .expect("the callback ran to its end");
    assert_eq!(
        report,
        (
            "hello".to_string(),
            Some("hello".to_string()),
            false,
            true,
            true
        )
    );
    // Break the cycle the test built, so the terminal is dropped.
    *slot.lock().unwrap() = None;
}

#[test]
fn a_callback_that_panics_does_not_stop_the_events() {
    let (sender, receiver) = mpsc::channel();
    let on_event = Box::new(move |event: TermEvent| {
        if event == TermEvent::Bell {
            panic!("a bug in the application's callback");
        }
        let _ = sender.send(event);
    });
    let term = Terminal::spawn(echo(""), on_event).expect("the terminal starts");
    let deadline = Instant::now() + TIMEOUT;
    while !term.screen_text().contains("READY") {
        assert!(Instant::now() < deadline, "the child never got ready");
        std::thread::sleep(Duration::from_millis(5));
    }
    // A bell, which makes the callback panic, then a title and a question.
    assert!(term.send_text(&format!(
        "!{}",
        common::hex("\x07\x1b]0;still here\x07\x1b[c")
    )));
    loop {
        match receiver
            .recv_timeout(TIMEOUT)
            .expect("events keep coming after the panic")
        {
            TermEvent::Title(title) => {
                assert_eq!(title, "still here");
                break;
            }
            _ => continue,
        }
    }
    // The child still gets its answers, too.
    let deadline = Instant::now() + TIMEOUT;
    while !term
        .screen_text()
        .replace('\n', "")
        .contains(&common::hex("\x1b[?6c"))
    {
        assert!(
            Instant::now() < deadline,
            "no answer to the query\n{}",
            term.screen_text()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn no_events_arrive_after_the_terminal_is_dropped() {
    let (sender, receiver) = mpsc::channel();
    let on_event = Box::new(move |event: TermEvent| {
        let _ = sender.send((Instant::now(), event));
    });
    let term = Terminal::spawn(sh("while :; do echo tick; sleep 0.01; done"), on_event)
        .expect("the terminal starts");
    receiver.recv_timeout(TIMEOUT).expect("a first event");
    drop(term);
    let dropped = Instant::now();
    // One event may have been on its way; after that, silence, and the callback is freed.
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok((at, event)) => assert!(
                at < dropped + Duration::from_millis(250),
                "{event:?} arrived {:?} after the drop",
                at - dropped
            ),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                assert!(Instant::now() < deadline, "the callback was never released")
            }
        }
    }
}
