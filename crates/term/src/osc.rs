//! A scanner for the operating system commands alacritty's parser drops.
//!
//! alacritty handles the OSC sequences a terminal needs to draw the screen and ignores the
//! rest. Two kinds of the rest matter to an IDE: the working directory a shell reports
//! (OSC 7), and desktop notifications (OSC 9, OSC 777 and kitty's OSC 99), which is how a
//! coding agent says it is waiting. [`OscScanner`] watches the raw byte stream on its way
//! to the parser and picks those out. It keeps a few bytes of state between reads, so a
//! sequence split across reads is still found.

use std::path::PathBuf;

use crate::event::TermEvent;
use crate::links::percent_decode;

/// Longest payload worth keeping. Anything longer is not a path or a notification.
const MAX_PAYLOAD: usize = 4096;
/// Unfinished kitty notifications remembered at once.
const MAX_PENDING: usize = 8;
/// Longest notification title or body, in bytes.
const MAX_TEXT: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Plain output.
    Ground,
    /// Just saw `ESC`.
    Escape,
    /// Inside `ESC ]`, collecting the payload.
    Osc,
    /// Inside an OSC that is of no interest, waiting for its end.
    Skip,
}

/// A kitty notification whose chunks are still arriving.
#[derive(Debug, Default)]
struct Pending {
    id: String,
    title: String,
    body: String,
}

/// Finds OSC 7, 9, 99 and 777 in a stream of terminal output.
#[derive(Debug)]
pub(crate) struct OscScanner {
    state: State,
    payload: Vec<u8>,
    /// Whether the payload's number has been looked at yet.
    classified: bool,
    pending: Vec<Pending>,
    /// This machine's name, to tell a local working directory from a remote one.
    host: Option<String>,
}

impl OscScanner {
    /// A scanner that accepts working directories reported for `host` (and for no host or
    /// `localhost`). With `None` every host is accepted.
    pub(crate) fn new(host: Option<String>) -> OscScanner {
        OscScanner {
            state: State::Ground,
            payload: Vec::new(),
            classified: false,
            pending: Vec::new(),
            host,
        }
    }

    /// A scanner for this machine.
    pub(crate) fn for_this_host() -> OscScanner {
        OscScanner::new(local_host_name())
    }

    /// Scan the next piece of output, calling `emit` for every complete sequence found.
    pub(crate) fn feed(&mut self, mut bytes: &[u8], emit: &mut dyn FnMut(TermEvent)) {
        while !bytes.is_empty() {
            match self.state {
                State::Ground => match memchr::memchr(0x1b, bytes) {
                    Some(index) => {
                        self.state = State::Escape;
                        bytes = &bytes[index + 1..];
                    }
                    None => return,
                },
                State::Escape => {
                    self.state = match bytes[0] {
                        b']' => {
                            self.payload.clear();
                            self.classified = false;
                            State::Osc
                        }
                        0x1b => State::Escape,
                        _ => State::Ground,
                    };
                    bytes = &bytes[1..];
                }
                State::Osc | State::Skip => {
                    // BEL ends an OSC; so do ESC (normally the first half of ST), CAN and SUB.
                    let end = bytes
                        .iter()
                        .position(|byte| matches!(byte, 0x07 | 0x1b | 0x18 | 0x1a));
                    let chunk = &bytes[..end.unwrap_or(bytes.len())];
                    if self.state == State::Osc {
                        self.collect(chunk);
                    }
                    let Some(end) = end else { return };
                    if self.state == State::Osc {
                        self.dispatch(emit);
                    }
                    self.state = if bytes[end] == 0x1b {
                        State::Escape
                    } else {
                        State::Ground
                    };
                    bytes = &bytes[end + 1..];
                }
            }
        }
    }

    /// Add payload bytes, giving up on the sequence as soon as it cannot be one we want.
    fn collect(&mut self, chunk: &[u8]) {
        for &byte in chunk {
            // The parser ignores the other C0 controls inside an OSC, so they are no payload.
            if byte < 0x20 {
                continue;
            }
            if self.payload.len() >= MAX_PAYLOAD {
                return self.skip();
            }
            self.payload.push(byte);
            if self.classified {
                continue;
            }
            if byte == b';' {
                self.classified = true;
                let number = &self.payload[..self.payload.len() - 1];
                if !matches!(number, b"7" | b"9" | b"99" | b"777") {
                    return self.skip();
                }
            } else if self.payload.len() > 3 {
                return self.skip();
            }
        }
    }

    fn skip(&mut self) {
        self.payload.clear();
        self.state = State::Skip;
    }

    fn dispatch(&mut self, emit: &mut dyn FnMut(TermEvent)) {
        let payload = String::from_utf8_lossy(&self.payload).into_owned();
        self.payload.clear();
        let Some((number, rest)) = payload.split_once(';') else {
            return;
        };
        let event = match number {
            "7" => parse_cwd(rest, self.host.as_deref()).map(TermEvent::Cwd),
            "9" => growl_notification(rest),
            "777" => rxvt_notification(rest),
            "99" => self.kitty_notification(rest),
            _ => None,
        };
        if let Some(event) = event {
            emit(event);
        }
    }

    /// OSC 99: `metadata ; payload`, where a notification may arrive in several chunks that
    /// share an `i=` identifier and end with the chunk that does not say `d=0`.
    fn kitty_notification(&mut self, rest: &str) -> Option<TermEvent> {
        let (metadata, payload) = rest.split_once(';').unwrap_or((rest, ""));
        let mut id = "";
        let mut done = true;
        let mut part = "title";
        let mut encoded = false;
        for (key, value) in metadata.split(':').filter_map(|pair| pair.split_once('=')) {
            match key {
                "i" => id = value,
                "d" => done = value != "0",
                "p" => part = value,
                "e" => encoded = value == "1",
                _ => {}
            }
        }
        let index = self.pending.iter().position(|pending| pending.id == id);
        if !matches!(part, "title" | "body") {
            // Closing a notification forgets its chunks; queries and icons are not for us.
            if part == "close" {
                if let Some(index) = index {
                    self.pending.remove(index);
                }
            }
            return None;
        }
        let text = if encoded {
            String::from_utf8_lossy(&base64_decode(payload)?).into_owned()
        } else {
            payload.to_string()
        };
        let index = index.unwrap_or_else(|| {
            if self.pending.len() >= MAX_PENDING {
                self.pending.remove(0);
            }
            self.pending.push(Pending {
                id: id.to_string(),
                ..Pending::default()
            });
            self.pending.len() - 1
        });
        let pending = &mut self.pending[index];
        let target = if part == "title" {
            &mut pending.title
        } else {
            &mut pending.body
        };
        if target.len() + text.len() <= MAX_TEXT {
            target.push_str(&text);
        }
        if !done {
            return None;
        }
        let finished = self.pending.remove(index);
        notification(&finished.title, &finished.body)
    }
}

/// A notification event, or `None` when there is nothing to say.
fn notification(title: &str, body: &str) -> Option<TermEvent> {
    let title = clean(title);
    let body = clean(body);
    (!title.is_empty() || !body.is_empty()).then_some(TermEvent::Notification { title, body })
}

/// Notification text is shown to the user as it is: no control characters.
fn clean(text: &str) -> String {
    let spaced: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    spaced.trim().to_string()
}

/// OSC 9: `9 ; message`, the iTerm2 notification. ConEmu uses the same number for
/// unrelated commands (`9;4;...` is the progress bar), which are not notifications.
fn growl_notification(rest: &str) -> Option<TermEvent> {
    let command = rest.split(';').next().unwrap_or("");
    let conemu = (rest.contains(';') || command.len() == rest.len())
        && matches!(command.parse::<u8>(), Ok(1..=12));
    if conemu {
        return None;
    }
    notification("", rest)
}

/// OSC 777: `777 ; notify ; title ; body`, from rxvt-unicode.
fn rxvt_notification(rest: &str) -> Option<TermEvent> {
    let rest = rest.strip_prefix("notify;")?;
    let (title, body) = rest.split_once(';').unwrap_or((rest, ""));
    notification(title, body)
}

/// OSC 7: `file://host/path`, percent-encoded. kitty's `kitty-shell-cwd://host/path` (not
/// encoded) and a bare absolute path are accepted too. A directory on another machine is
/// of no use here and comes back as `None`.
fn parse_cwd(value: &str, local_host: Option<&str>) -> Option<PathBuf> {
    if value.starts_with('/') {
        return Some(PathBuf::from(value));
    }
    let (scheme, rest) = value.split_once("://")?;
    let (host, path) = rest.split_at(rest.find('/')?);
    if !is_local(host, local_host) {
        return None;
    }
    let path = if scheme.eq_ignore_ascii_case("file") {
        percent_decode(path)
    } else {
        path.to_string()
    };
    Some(PathBuf::from(path))
}

/// Whether `host` names this machine. Names are compared without their domain, because a
/// shell may report either form.
fn is_local(host: &str, local_host: Option<&str>) -> bool {
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let short = |name: &str| name.split('.').next().unwrap_or("").to_ascii_lowercase();
    match local_host {
        Some(local) => short(host) == short(local),
        None => true,
    }
}

/// This machine's host name, if it can be read.
fn local_host_name() -> Option<String> {
    let mut buffer = [0u8; 256];
    // SAFETY: gethostname writes at most `len` bytes into the buffer it is given, and the
    // buffer is a live local array of exactly that length.
    let status = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if status != 0 {
        return None;
    }
    let len = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    let name = String::from_utf8_lossy(&buffer[..len]).into_owned();
    (!name.is_empty()).then_some(name)
}

/// Decode standard or URL-safe base64, with or without padding. `None` on a character that
/// is not base64.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut bits: u32 = 0;
    let mut count = 0;
    for byte in input.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_with(host: Option<&str>, pieces: &[&[u8]]) -> Vec<TermEvent> {
        let mut scanner = OscScanner::new(host.map(str::to_string));
        let mut events = Vec::new();
        for piece in pieces {
            scanner.feed(piece, &mut |event| events.push(event));
        }
        events
    }

    fn scan(bytes: &[u8]) -> Vec<TermEvent> {
        scan_with(Some("mac.local"), &[bytes])
    }

    fn cwd(path: &str) -> TermEvent {
        TermEvent::Cwd(PathBuf::from(path))
    }

    fn note(title: &str, body: &str) -> TermEvent {
        TermEvent::Notification {
            title: title.to_string(),
            body: body.to_string(),
        }
    }

    #[test]
    fn working_directory_with_either_terminator() {
        assert_eq!(
            scan(b"\x1b]7;file://mac.local/Users/me/Code\x07"),
            vec![cwd("/Users/me/Code")]
        );
        assert_eq!(
            scan(b"\x1b]7;file://mac.local/Users/me/Code\x1b\\"),
            vec![cwd("/Users/me/Code")]
        );
        assert_eq!(scan(b"\x1b]7;file:///tmp\x07"), vec![cwd("/tmp")]);
        assert_eq!(scan(b"\x1b]7;file://localhost/tmp\x07"), vec![cwd("/tmp")]);
        assert_eq!(scan(b"\x1b]7;file://MAC/tmp\x07"), vec![cwd("/tmp")]);
        assert_eq!(scan(b"\x1b]7;/plain/path\x07"), vec![cwd("/plain/path")]);
    }

    #[test]
    fn working_directory_is_percent_decoded() {
        assert_eq!(
            scan(b"\x1b]7;file:///Users/me/My%20Notes/caf%C3%A9\x07"),
            vec![cwd("/Users/me/My Notes/café")]
        );
        // kitty's variant is not encoded, so a literal percent sign stays.
        assert_eq!(
            scan(b"\x1b]7;kitty-shell-cwd://mac.local/tmp/100%25\x07"),
            vec![cwd("/tmp/100%25")]
        );
    }

    #[test]
    fn a_remote_working_directory_is_ignored() {
        assert_eq!(
            scan(b"\x1b]7;file://server.example.com/home/me\x07"),
            vec![]
        );
        // With no local name to compare against, everything is taken.
        assert_eq!(
            scan_with(None, &[b"\x1b]7;file://server/home/me\x07"]),
            vec![cwd("/home/me")]
        );
        assert_eq!(scan(b"\x1b]7;relative/path\x07"), vec![]);
        assert_eq!(scan(b"\x1b]7;file://host-without-path\x07"), vec![]);
        assert_eq!(scan(b"\x1b]7;\x07"), vec![]);
        assert_eq!(scan(b"\x1b]7\x07"), vec![]);
    }

    #[test]
    fn growl_notifications() {
        assert_eq!(
            scan(b"\x1b]9;Build finished\x07"),
            vec![note("", "Build finished")]
        );
        assert_eq!(scan(b"\x1b]9;a;b;c\x1b\\"), vec![note("", "a;b;c")]);
        assert_eq!(
            scan("\x1b]9;完了 ✓\x07".as_bytes()),
            vec![note("", "完了 ✓")]
        );
        assert_eq!(
            scan(b"\x1b]9;13 tests failed\x07"),
            vec![note("", "13 tests failed")]
        );
        assert_eq!(scan(b"\x1b]9;42\x07"), vec![note("", "42")]);
    }

    #[test]
    fn conemu_commands_are_not_notifications() {
        assert_eq!(scan(b"\x1b]9;4;1;50\x07"), vec![]);
        assert_eq!(scan(b"\x1b]9;4;0\x07"), vec![]);
        assert_eq!(scan(b"\x1b]9;9;C:\\Users\x07"), vec![]);
        assert_eq!(scan(b"\x1b]9;12\x07"), vec![]);
        assert_eq!(scan(b"\x1b]9;\x07"), vec![]);
    }

    #[test]
    fn rxvt_notifications() {
        assert_eq!(
            scan(b"\x1b]777;notify;Claude;Waiting for input\x07"),
            vec![note("Claude", "Waiting for input")]
        );
        assert_eq!(
            scan(b"\x1b]777;notify;Title;a;b\x07"),
            vec![note("Title", "a;b")]
        );
        assert_eq!(
            scan(b"\x1b]777;notify;Only a title\x07"),
            vec![note("Only a title", "")]
        );
        assert_eq!(scan(b"\x1b]777;notify;;\x07"), vec![]);
        assert_eq!(scan(b"\x1b]777;precmd\x07"), vec![]);
        assert_eq!(scan(b"\x1b]777;preexec;ls\x07"), vec![]);
    }

    #[test]
    fn kitty_notifications() {
        assert_eq!(
            scan(b"\x1b]99;;Hello world\x1b\\"),
            vec![note("Hello world", "")]
        );
        assert_eq!(
            scan(b"\x1b]99;i=1:p=body;Just a body\x07"),
            vec![note("", "Just a body")]
        );
        // Title and body as two chunks with the same identifier.
        assert_eq!(
            scan(b"\x1b]99;i=1:d=0;Title\x1b\\\x1b]99;i=1:d=1:p=body;Body\x1b\\"),
            vec![note("Title", "Body")]
        );
        // A title split over chunks, interleaved with another notification.
        let bytes = b"\x1b]99;i=a:d=0;Hel\x07\x1b]99;i=b;Other\x07\x1b]99;i=a:d=0;lo\x07\x1b]99;i=a:p=body;there\x07";
        assert_eq!(scan(bytes), vec![note("Other", ""), note("Hello", "there")]);
        // Base64 payloads.
        assert_eq!(
            scan(b"\x1b]99;e=1;SGVsbG8gd29ybGQ=\x07"),
            vec![note("Hello world", "")]
        );
        assert_eq!(
            scan(b"\x1b]99;i=x:e=1:p=body;w6lsw6g\x07"),
            vec![note("", "élè")]
        );
        assert_eq!(scan(b"\x1b]99;e=1;not base64!\x07"), vec![]);
    }

    #[test]
    fn kitty_queries_and_closes_are_not_notifications() {
        assert_eq!(scan(b"\x1b]99;i=1:p=?;\x07"), vec![]);
        assert_eq!(scan(b"\x1b]99;i=1:p=icon;data\x07"), vec![]);
        // A close drops the chunks collected so far.
        assert_eq!(
            scan(b"\x1b]99;i=1:d=0;Half\x07\x1b]99;i=1:p=close;\x07\x1b]99;i=1;Fresh\x07"),
            vec![note("Fresh", "")]
        );
        assert_eq!(scan(b"\x1b]99;;\x07"), vec![]);
    }

    #[test]
    fn unfinished_kitty_notifications_are_bounded() {
        let mut scanner = OscScanner::new(None);
        let mut events = Vec::new();
        for i in 0..100 {
            let chunk = format!("\x1b]99;i={i}:d=0;part\x07");
            scanner.feed(chunk.as_bytes(), &mut |event| events.push(event));
        }
        assert!(events.is_empty());
        assert_eq!(scanner.pending.len(), MAX_PENDING);
        // The oldest were dropped, the newest can still be finished.
        scanner.feed(b"\x1b]99;i=99;!\x07", &mut |event| events.push(event));
        assert_eq!(events, vec![note("part!", "")]);
        // An endless title stops growing.
        for _ in 0..100 {
            scanner.feed(
                format!("\x1b]99;i=big:d=0;{}\x07", "x".repeat(100)).as_bytes(),
                &mut |_| {},
            );
        }
        let big = scanner
            .pending
            .iter()
            .find(|pending| pending.id == "big")
            .unwrap();
        assert!(big.title.len() <= MAX_TEXT);
    }

    #[test]
    fn sequences_split_across_reads_are_found() {
        let whole = b"before\x1b]7;file:///tmp/dir\x1b\\middle\x1b]9;hi\x07after";
        let expected = vec![cwd("/tmp/dir"), note("", "hi")];
        assert_eq!(scan(whole), expected);
        for split in 1..whole.len() {
            let (a, b) = whole.split_at(split);
            assert_eq!(
                scan_with(Some("mac"), &[a, b]),
                expected,
                "split at {split}"
            );
        }
        let bytes: Vec<&[u8]> = whole.chunks(1).collect();
        assert_eq!(scan_with(Some("mac"), &bytes), expected);
    }

    #[test]
    fn other_output_is_left_alone() {
        assert_eq!(scan(b""), vec![]);
        assert_eq!(scan(b"plain text\r\n"), vec![]);
        assert_eq!(scan(b"\x1b[31mred\x1b[0m \x1b(B \x1bM"), vec![]);
        assert_eq!(scan(b"\x1b]0;a title\x07\x1b]2;another\x1b\\"), vec![]);
        assert_eq!(
            scan(b"\x1b]52;c;aGk=\x07\x1b]8;;https://example.com\x07link\x1b]8;;\x07"),
            vec![]
        );
        assert_eq!(
            scan(b"\x1b]133;A\x07\x1b]1337;CurrentDir=/tmp\x07\x1b]10;?\x07\x1b]104\x07"),
            vec![]
        );
        // Numbers that merely start like ours.
        assert_eq!(
            scan(b"\x1b]70;x\x07\x1b]90;x\x07\x1b]7777;x\x07\x1b]998;x\x07"),
            vec![]
        );
        // A text that looks like an OSC without the escape.
        assert_eq!(scan(b"]9;not a sequence\x07"), vec![]);
    }

    #[test]
    fn a_sequence_can_follow_any_other_escape() {
        assert_eq!(
            scan(b"\x1b\x1b]9;double escape\x07"),
            vec![note("", "double escape")]
        );
        assert_eq!(
            scan(b"\x1b[1m\x1b]9;after csi\x07"),
            vec![note("", "after csi")]
        );
        // An OSC cut short by a new escape sequence still counts, as it does for the parser.
        assert_eq!(
            scan(b"\x1b]9;cut\x1b]9;next\x07"),
            vec![note("", "cut"), note("", "next")]
        );
        // The title before it is skipped, then the scanner is ready again.
        assert_eq!(
            scan(b"\x1b]0;title\x1b\\\x1b]9;then this\x1b\\"),
            vec![note("", "then this")]
        );
        // CAN and SUB end a sequence too.
        assert_eq!(
            scan(b"\x1b]9;cancelled\x18\x1b]9;ok\x07"),
            vec![note("", "cancelled"), note("", "ok")]
        );
    }

    #[test]
    fn control_characters_never_reach_the_text() {
        assert_eq!(scan(b"\x1b]9;a\x01b\tc\x07"), vec![note("", "abc")]);
        assert_eq!(
            scan(b"\x1b]777;notify; padded ;\x7fbody\x07"),
            vec![note("padded", "body")]
        );
        assert_eq!(scan(b"\x1b]99;e=1;YQpi\x07"), vec![note("a b", "")]);
    }

    #[test]
    fn oversized_payloads_are_dropped_without_losing_sync() {
        let mut bytes = b"\x1b]9;".to_vec();
        bytes.extend(std::iter::repeat_n(b'x', MAX_PAYLOAD * 3));
        bytes.extend_from_slice(b"\x07\x1b]9;small\x07");
        assert_eq!(scan(&bytes), vec![note("", "small")]);
        // An OSC of another kind, however long, is never buffered.
        let mut scanner = OscScanner::new(None);
        let mut long = b"\x1b]52;c;".to_vec();
        long.extend(std::iter::repeat_n(b'A', 1 << 20));
        scanner.feed(&long, &mut |_| {});
        assert!(scanner.payload.capacity() < 64);
        assert_eq!(scanner.state, State::Skip);
        let mut events = Vec::new();
        scanner.feed(b"\x07\x1b]9;back\x07", &mut |event| events.push(event));
        assert_eq!(events, vec![note("", "back")]);
    }

    #[test]
    fn invalid_utf8_is_replaced_not_fatal() {
        assert_eq!(scan(b"\x1b]9;caf\xff\x07"), vec![note("", "caf\u{fffd}")]);
        assert_eq!(
            scan(b"\x1b]7;file:///tmp/%ff\x07"),
            vec![cwd("/tmp/\u{fffd}")]
        );
    }

    #[test]
    fn base64_forms() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVsbG8").unwrap(), b"hello");
        assert_eq!(base64_decode("aGk").unwrap(), b"hi");
        assert_eq!(base64_decode("").unwrap(), b"");
        assert_eq!(
            base64_decode("_-8=").unwrap(),
            base64_decode("/+8=").unwrap()
        );
        assert_eq!(base64_decode("aG Vs\nbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("a*b"), None);
    }

    #[test]
    fn this_host_has_a_name() {
        // Not every sandbox has one, but when it does it is not empty.
        if let Some(name) = local_host_name() {
            assert!(!name.is_empty());
            assert!(is_local(&name, Some(&name)));
        }
        assert!(is_local("", Some("mac")));
        assert!(is_local("LocalHost", Some("mac")));
        assert!(is_local("mac.lan", Some("MAC.local")));
        assert!(!is_local("other", Some("mac")));
    }
}
