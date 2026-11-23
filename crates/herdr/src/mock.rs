//! A scripted stand-in for the herdr server, for tests.
//!
//! [`MockServer`] listens on a unix socket in a temporary directory. A test queues the
//! replies it expects to need, in order, each tied to the method it answers; the server
//! records every request it receives. A request the script did not expect is answered with
//! a `mock_unexpected` error and remembered, so [`MockServer::assert_finished`] can fail the
//! test.
//!
//! This module is compiled for this crate's own tests and, for other crates, behind the
//! `mock` feature.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::client::Client;

/// How long the mock waits for a client to finish sending its request.
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// How often a slow reply checks whether the server is shutting down.
const TICK: Duration = Duration::from_millis(5);

/// What the mock sends back for one request.
#[derive(Debug, Clone)]
pub enum Reply {
    /// A normal reply: `{"id", "result": <value>}`.
    Result(Value),
    /// A refusal: `{"id", "error": {"code", "message"}}`.
    Error {
        /// The machine-readable code, such as `pane_not_found`.
        code: String,
        /// The message for people.
        message: String,
    },
    /// These exact bytes, with nothing added: for malformed replies.
    Raw(Vec<u8>),
    /// Say nothing and keep the connection open this long, then close it.
    Hang(Duration),
    /// Send `bytes` one at a time with `gap` between them, then close.
    Trickle {
        /// The bytes to send.
        bytes: Vec<u8>,
        /// The pause before each byte.
        gap: Duration,
    },
    /// Close the connection without answering.
    Close,
}

/// A request the mock received.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// The request's `id`.
    pub id: String,
    /// The method that was called.
    pub method: String,
    /// The parameters it was called with.
    pub params: Value,
}

struct Step {
    method: String,
    reply: Reply,
}

/// Answers requests the script has nothing queued for. `None` leaves the request unexpected.
type Handler = Box<dyn Fn(&Request) -> Option<Reply> + Send + Sync>;

#[derive(Default)]
struct Shared {
    script: Mutex<VecDeque<Step>>,
    handler: Option<Handler>,
    requests: Mutex<Vec<Request>>,
    unexpected: Mutex<Vec<String>>,
    stop: AtomicBool,
}

/// Lock a mutex, carrying on with the data if another thread panicked while holding it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A fake herdr server on a temporary socket. Dropping it stops the server thread and
/// removes the socket and its directory.
pub struct MockServer {
    dir: tempfile::TempDir,
    socket: PathBuf,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl MockServer {
    /// Start a server with an empty script.
    ///
    /// # Panics
    ///
    /// Panics when the temporary directory or the socket cannot be created, which in a test
    /// is the right way to fail.
    pub fn start() -> MockServer {
        MockServer::launch(None)
    }

    /// Start a server that asks `handler` about every request the script has no reply queued
    /// for. The handler returns the reply to send, or `None` to treat the request as
    /// unexpected. This is how a test plays a herdr that keeps state, such as a tab whose
    /// split ratios really change.
    ///
    /// # Panics
    ///
    /// Panics when the temporary directory or the socket cannot be created.
    pub fn with_handler(
        handler: impl Fn(&Request) -> Option<Reply> + Send + Sync + 'static,
    ) -> MockServer {
        MockServer::launch(Some(Box::new(handler)))
    }

    fn launch(handler: Option<Handler>) -> MockServer {
        let dir = tempfile::tempdir().expect("a temporary directory for the mock herdr socket");
        let socket = dir.path().join("herdr.sock");
        let listener = UnixListener::bind(&socket).expect("binding the mock herdr socket");
        let shared = Arc::new(Shared {
            handler,
            ..Shared::default()
        });
        let thread = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || serve(listener, &shared))
        };
        MockServer {
            dir,
            socket,
            shared,
            thread: Some(thread),
        }
    }

    /// The socket the mock listens on.
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// A path inside the mock's temporary directory for a collapse state file. The file does
    /// not exist until something writes it.
    pub fn state_file(&self) -> PathBuf {
        self.dir
            .path()
            .join("cache")
            .join("herdr-collapse")
            .join("state.json")
    }

    /// A client for this server whose collapse state file is [`MockServer::state_file`].
    pub fn client(&self) -> Client {
        Client::new(&self.socket).with_state_file(self.state_file())
    }

    /// Queue `reply` for the next request, which must be a call to `method`.
    pub fn expect(&self, method: &str, reply: Reply) -> &MockServer {
        lock(&self.shared.script).push_back(Step {
            method: method.to_string(),
            reply,
        });
        self
    }

    /// Queue a normal reply carrying `result` for a call to `method`.
    pub fn ok(&self, method: &str, result: Value) -> &MockServer {
        self.expect(method, Reply::Result(result))
    }

    /// Queue a refusal with `code` and `message` for a call to `method`.
    pub fn fail(&self, method: &str, code: &str, message: &str) -> &MockServer {
        self.expect(
            method,
            Reply::Error {
                code: code.to_string(),
                message: message.to_string(),
            },
        )
    }

    /// Every request received so far, in order, including unexpected ones.
    pub fn requests(&self) -> Vec<Request> {
        lock(&self.shared.requests).clone()
    }

    /// The methods of every request received so far, in order.
    pub fn methods(&self) -> Vec<String> {
        lock(&self.shared.requests)
            .iter()
            .map(|request| request.method.clone())
            .collect()
    }

    /// How many queued replies have not been used yet.
    pub fn remaining(&self) -> usize {
        lock(&self.shared.script).len()
    }

    /// Check that the conversation went exactly as scripted.
    ///
    /// # Panics
    ///
    /// Panics when a queued reply was never asked for, or a request arrived that the script
    /// did not expect at that point.
    pub fn assert_finished(&self) {
        let unexpected = lock(&self.shared.unexpected).clone();
        assert!(
            unexpected.is_empty(),
            "the mock herdr received unexpected requests: {unexpected:?}"
        );
        let unused: Vec<String> = lock(&self.shared.script)
            .iter()
            .map(|step| step.method.clone())
            .collect();
        assert!(
            unused.is_empty(),
            "the mock herdr was never asked for: {unused:?}"
        );
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // The server thread may be blocked in accept: a connection wakes it.
        drop(UnixStream::connect(&self.socket));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(listener: UnixListener, shared: &Shared) {
    for connection in listener.incoming() {
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        if let Ok(stream) = connection {
            answer(stream, shared);
        }
    }
}

/// Read one request, record it, and send whatever the script says comes next.
fn answer(mut stream: UnixStream, shared: &Shared) {
    let Some(line) = read_request_line(&mut stream) else {
        return;
    };
    let Ok(request) = serde_json::from_slice::<Value>(&line) else {
        write_error(
            &mut stream,
            "",
            "invalid_request",
            "invalid request: not JSON",
        );
        return;
    };
    let id = request
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let params = request.get("params").cloned().unwrap_or(Value::Null);
    let request = Request {
        id: id.clone(),
        method: method.clone(),
        params,
    };
    lock(&shared.requests).push(request.clone());

    match next_reply(&request, shared) {
        Some(Reply::Result(result)) => {
            write_line(&mut stream, &json!({ "id": id, "result": result }))
        }
        Some(Reply::Error { code, message }) => write_error(&mut stream, &id, &code, &message),
        Some(Reply::Raw(bytes)) => {
            let _ = stream.write_all(&bytes);
        }
        Some(Reply::Hang(duration)) => wait(duration, shared),
        Some(Reply::Trickle { bytes, gap }) => {
            for byte in bytes {
                wait(gap, shared);
                if shared.stop.load(Ordering::SeqCst) || stream.write_all(&[byte]).is_err() {
                    break;
                }
            }
        }
        Some(Reply::Close) => {}
        None => write_error(
            &mut stream,
            &id,
            "mock_unexpected",
            &format!("the mock did not expect {method} here"),
        ),
    }
}

/// The reply for `request`: the next scripted one when it is for this method, else whatever
/// the handler says. `None`, after noting the request as unexpected, when neither has one.
fn next_reply(request: &Request, shared: &Shared) -> Option<Reply> {
    let expected = {
        let mut script = lock(&shared.script);
        match script.front() {
            Some(step) if step.method == request.method => {
                return script.pop_front().map(|step| step.reply)
            }
            Some(step) => format!("the script expected {}", step.method),
            None => "the script was finished".to_string(),
        }
    };
    let handled = shared.handler.as_ref().and_then(|handler| handler(request));
    if handled.is_none() {
        lock(&shared.unexpected).push(format!("{} ({expected})", request.method));
    }
    handled
}

/// The request line, or `None` when the client connected and left without sending one.
fn read_request_line(stream: &mut UnixStream) -> Option<Vec<u8>> {
    let _ = stream.set_read_timeout(Some(REQUEST_READ_TIMEOUT));
    let mut line = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                if let Some(end) = chunk[..read].iter().position(|byte| *byte == b'\n') {
                    line.extend_from_slice(&chunk[..end]);
                    break;
                }
                line.extend_from_slice(&chunk[..read]);
            }
        }
    }
    (!line.is_empty()).then_some(line)
}

fn write_line(stream: &mut UnixStream, value: &Value) {
    let mut bytes = value.to_string().into_bytes();
    bytes.push(b'\n');
    let _ = stream.write_all(&bytes);
}

fn write_error(stream: &mut UnixStream, id: &str, code: &str, message: &str) {
    write_line(
        stream,
        &json!({ "id": id, "error": { "code": code, "message": message } }),
    );
}

/// Sleep for `duration`, giving up early when the server is being dropped.
fn wait(duration: Duration, shared: &Shared) {
    let until = Instant::now() + duration;
    while !shared.stop.load(Ordering::SeqCst) {
        let Some(left) = until
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
        else {
            break;
        };
        std::thread::sleep(left.min(TICK));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_come_back_in_script_order_and_requests_are_recorded() {
        let server = MockServer::start();
        server
            .ok("a.one", json!({"n": 1}))
            .ok("a.two", json!({"n": 2}));
        let client = server.client();
        assert_eq!(
            client.call("a.one", json!({"x": 1})).unwrap(),
            json!({"n": 1})
        );
        assert_eq!(server.remaining(), 1);
        assert_eq!(client.call("a.two", Value::Null).unwrap(), json!({"n": 2}));
        assert_eq!(server.methods(), vec!["a.one", "a.two"]);
        assert_eq!(server.requests()[0].params, json!({"x": 1}));
        server.assert_finished();
    }

    #[test]
    fn an_unexpected_method_is_refused_and_leaves_the_script_alone() {
        let server = MockServer::start();
        server.ok("a.one", json!({}));
        let client = server.client();
        let err = client.call("a.other", Value::Null).unwrap_err();
        assert_eq!(err.code(), Some("mock_unexpected"));
        assert_eq!(server.remaining(), 1);
        client.call("a.one", Value::Null).unwrap();
        assert_eq!(server.remaining(), 0);
        // One more than scripted is unexpected too.
        assert_eq!(
            client.call("a.one", Value::Null).unwrap_err().code(),
            Some("mock_unexpected")
        );
    }

    #[test]
    #[should_panic(expected = "unexpected requests")]
    fn assert_finished_fails_on_an_unexpected_request() {
        let server = MockServer::start();
        let _ = server.client().call("a.one", Value::Null);
        server.assert_finished();
    }

    #[test]
    #[should_panic(expected = "never asked for")]
    fn assert_finished_fails_on_an_unused_reply() {
        let server = MockServer::start();
        server.ok("a.one", json!({}));
        server.assert_finished();
    }

    #[test]
    fn the_socket_and_its_directory_go_away_on_drop() {
        let server = MockServer::start();
        let socket = server.socket_path().to_path_buf();
        let dir = socket.parent().unwrap().to_path_buf();
        assert!(socket.exists());
        drop(server);
        assert!(!dir.exists());
    }

    #[test]
    fn dropping_a_server_that_is_hanging_does_not_wait_for_it() {
        let server = MockServer::start();
        server.expect("slow", Reply::Hang(Duration::from_secs(30)));
        let client = server.client().with_timeout(Duration::from_millis(50));
        assert!(client.call("slow", Value::Null).is_err());
        let started = Instant::now();
        drop(server);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_client_that_connects_and_leaves_is_not_recorded() {
        let server = MockServer::start();
        drop(UnixStream::connect(server.socket_path()).unwrap());
        server.ok("a.one", json!({}));
        server.client().call("a.one", Value::Null).unwrap();
        assert_eq!(server.methods(), vec!["a.one"]);
    }

    #[test]
    fn a_handler_answers_what_the_script_does_not() {
        let server = MockServer::with_handler(|request| match request.method.as_str() {
            "echo" => Some(Reply::Result(request.params.clone())),
            _ => None,
        });
        server.ok("echo", json!("scripted"));
        let client = server.client();
        // The scripted reply wins while it is next in line.
        assert_eq!(
            client.call("echo", json!({"n": 1})).unwrap(),
            json!("scripted")
        );
        assert_eq!(
            client.call("echo", json!({"n": 2})).unwrap(),
            json!({"n": 2})
        );
        server.assert_finished();
        assert_eq!(
            client.call("other", Value::Null).unwrap_err().code(),
            Some("mock_unexpected")
        );
    }

    #[test]
    fn the_client_keeps_its_state_file_inside_the_mock_directory() {
        let server = MockServer::start();
        let client = server.client();
        let state = client.state_file().unwrap();
        assert!(state.starts_with(server.socket_path().parent().unwrap()));
        assert!(!state.exists());
    }
}
