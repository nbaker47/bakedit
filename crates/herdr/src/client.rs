//! The connection to a herdr server.
//!
//! herdr listens on a unix socket and speaks newline-delimited JSON. A client sends one
//! `{"id", "method", "params"}` line; the server answers with one `{"id", "result"}` or
//! `{"id", "error": {"code", "message"}}` line and closes the connection. Every
//! [`Client::call`] therefore opens a connection of its own, which also keeps the client
//! free of shared state: it is `Send + Sync` and can be cloned and used from any thread.
//!
//! # The mutation guard
//!
//! The session a developer works in is also the session their tests would reach if a test
//! built a client from the environment. So a client refuses to send anything that changes
//! herdr (focus, move, resize, close, send text, ...) to the *live* session unless the
//! application said so with [`Client::allow_live_mutations`]. The live session is the default
//! session's socket (`~/.config/herdr/herdr.sock`, and `$XDG_CONFIG_HOME/herdr/herdr.sock`
//! when that variable is set) and whatever `HERDR_SOCKET_PATH` named when the process first
//! built a client. Read-only methods are never refused.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use socket2::{Domain, SockAddr, Socket, Type};

/// How long a call may take when the client is not told otherwise.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

/// The longest timeout a client accepts; longer ones are cut down to this.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(3600);

/// The variable herdr sets inside its panes to the API socket of their session.
pub const SOCKET_ENV_VAR: &str = "HERDR_SOCKET_PATH";

/// The shortest wait worth starting. Socket timeouts are counted in microseconds and one that
/// rounds down to zero means "wait for ever", so a call with less time left than this has
/// timed out.
const SHORTEST_WAIT: Duration = Duration::from_millis(1);

/// The id sent with every request. herdr echoes it and writes it to its log.
const REQUEST_ID: &str = "bakedit";

/// The largest reply accepted. A session snapshot is a few kilobytes; a pane read can be
/// large, but nothing legitimate comes close to this.
const MAX_REPLY_BYTES: usize = 64 * 1024 * 1024;

/// Methods that only report state. Everything else, including any method this crate has
/// never heard of, counts as mutating.
const READ_ONLY_METHODS: &[&str] = &[
    "ping",
    "session.snapshot",
    "workspace.list",
    "workspace.get",
    "worktree.list",
    "tab.list",
    "tab.get",
    "agent.list",
    "agent.get",
    "agent.read",
    "pane.list",
    "pane.current",
    "pane.get",
    "pane.read",
    "pane.layout",
    "pane.process_info",
    "pane.neighbor",
    "pane.edges",
    "layout.export",
    "integration.list",
    "plugin.list",
];

/// Whether `method` changes anything in herdr. Only the methods known to be read-only
/// (`ping`, `session.snapshot`, the `*.list` / `*.get` / `*.read` queries, `pane.layout`,
/// `pane.neighbor`, `pane.edges`, `layout.export`, ...) are not; an unknown method is
/// assumed to mutate.
pub fn is_mutating(method: &str) -> bool {
    !READ_ONLY_METHODS.contains(&method)
}

/// Why a call to herdr failed.
#[derive(Debug, thiserror::Error)]
pub enum HerdrError {
    /// Nothing listens on the socket: the file is missing, or it is left over from a server
    /// that has exited.
    #[error("herdr is not running (no server at {})", .socket.display())]
    NotRunning {
        /// The socket that was tried.
        socket: PathBuf,
    },
    /// The server did not accept the connection or did not finish its reply in time.
    #[error("herdr did not answer `{method}` within {timeout:?}")]
    Timeout {
        /// The method that was called.
        method: String,
        /// The client's timeout.
        timeout: Duration,
    },
    /// The server answered with something that is not a well-formed reply, or a reply lacks
    /// a field this crate needs.
    #[error("unexpected reply to `{method}` from herdr: {detail}")]
    Protocol {
        /// The method that was called.
        method: String,
        /// What was wrong with the reply.
        detail: String,
    },
    /// The server understood the request and refused it.
    #[error("herdr refused `{method}`: {message} ({code})")]
    Server {
        /// The method that was called.
        method: String,
        /// herdr's machine-readable code, such as `pane_not_found` or `invalid_request`.
        code: String,
        /// herdr's message for people.
        message: String,
    },
    /// A mutating method was about to go to the live session without
    /// [`Client::allow_live_mutations`]. Nothing was sent.
    #[error("refusing to send `{method}` to the live herdr session at {}", .socket.display())]
    MutationRefused {
        /// The method that was refused.
        method: String,
        /// The live session's socket.
        socket: PathBuf,
    },
    /// Collapsing needs to remember split sizes, and this client has no state file. Build
    /// the client with [`Client::from_env_or_default`] or give it one with
    /// [`Client::with_state_file`].
    #[error("this herdr client has no collapse state file")]
    NoStateFile,
    /// The socket or the collapse state file could not be used.
    #[error("{context}: {source}")]
    Io {
        /// What was being done.
        context: String,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
}

impl HerdrError {
    /// herdr's error code when the server refused the request (`pane_not_found`,
    /// `tab_not_found`, `invalid_request`, ...), `None` for every other kind of failure.
    pub fn code(&self) -> Option<&str> {
        match self {
            HerdrError::Server { code, .. } => Some(code),
            _ => None,
        }
    }

    /// Whether the failure means no herdr server is there to talk to.
    pub fn is_not_running(&self) -> bool {
        matches!(self, HerdrError::NotRunning { .. })
    }

    pub(crate) fn protocol(method: &str, detail: impl Into<String>) -> HerdrError {
        HerdrError::Protocol {
            method: method.to_string(),
            detail: detail.into(),
        }
    }

    pub(crate) fn io(context: impl Into<String>, source: io::Error) -> HerdrError {
        HerdrError::Io {
            context: context.into(),
            source,
        }
    }
}

/// What a server says about itself in answer to `ping`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ServerInfo {
    /// herdr's version, such as `0.9.5`.
    pub version: String,
    /// The socket API's protocol number.
    pub protocol: u32,
}

/// A handle on one herdr session's API socket.
///
/// Cheap to clone, `Send + Sync`, and stateless: each call connects, asks, reads the answer
/// and disconnects. No call blocks longer than the client's timeout.
#[derive(Debug, Clone)]
pub struct Client {
    socket_path: PathBuf,
    timeout: Duration,
    state_file: Option<PathBuf>,
    allow_live_mutations: bool,
    protected: Vec<PathBuf>,
}

impl Client {
    /// A client for the server listening on `socket_path`, with [`DEFAULT_TIMEOUT`], no
    /// collapse state file and the mutation guard on. Nothing is connected until the first
    /// call, so this never fails.
    pub fn new(socket_path: impl Into<PathBuf>) -> Client {
        // Note where the live session is the first time any client is built, so a later
        // change to the environment cannot move the guard.
        live_sockets();
        Client {
            socket_path: socket_path.into(),
            timeout: DEFAULT_TIMEOUT,
            state_file: None,
            allow_live_mutations: false,
            protected: Vec::new(),
        }
    }

    /// The client the user's scripts would build: the socket named by `HERDR_SOCKET_PATH`,
    /// or `~/.config/herdr/herdr.sock` when that is unset or empty, and the collapse state
    /// file at `$XDG_CACHE_HOME/herdr-collapse/state.json` (`~/.cache/...` when
    /// `XDG_CACHE_HOME` is unset or empty). This is the only constructor that reads the
    /// environment. The mutation guard is on: the application turns it off with
    /// [`Client::allow_live_mutations`].
    pub fn from_env_or_default() -> Client {
        let home = home_dir();
        let socket = socket_path_from(std::env::var_os(SOCKET_ENV_VAR), home.as_deref());
        let mut client = Client::new(socket);
        client.state_file = Some(state_file_from(
            std::env::var_os("XDG_CACHE_HOME"),
            home.as_deref(),
        ));
        client
    }

    /// The same client talking to a different socket, keeping its timeout, state file and
    /// guard settings. For a herdr session other than the default one:
    /// `Client::from_env_or_default().with_socket_path(session.socket_path(&config_dir))`,
    /// with [`crate::launch::Session`].
    pub fn with_socket_path(mut self, socket_path: impl Into<PathBuf>) -> Client {
        self.socket_path = socket_path.into();
        self
    }

    /// The same client with a different timeout for each call. A timeout above
    /// [`MAX_TIMEOUT`] is cut down to it; one under a millisecond makes every call fail with
    /// [`HerdrError::Timeout`] without contacting herdr.
    pub fn with_timeout(mut self, timeout: Duration) -> Client {
        self.timeout = timeout.min(MAX_TIMEOUT);
        self
    }

    /// The same client remembering collapsed split sizes in `path`. The file uses the format
    /// of the `herdr-collapse` script, so pointing both at one file makes them interoperate.
    pub fn with_state_file(mut self, path: impl Into<PathBuf>) -> Client {
        self.state_file = Some(path.into());
        self
    }

    /// The same client with the mutation guard switched off (`true`) or back on (`false`).
    /// Only the running application should pass `true`, once it has decided that this
    /// process is meant to drive the user's session; tests never should.
    pub fn allow_live_mutations(mut self, allow: bool) -> Client {
        self.allow_live_mutations = allow;
        self
    }

    /// The same client treating `socket_path` as a live session too, in addition to the
    /// default socket and the one the environment named. This only ever makes the guard
    /// stricter.
    pub fn with_protected_socket(mut self, socket_path: impl Into<PathBuf>) -> Client {
        self.protected.push(socket_path.into());
        self
    }

    /// The socket this client talks to.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// The longest a single call may take.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Where collapsed split sizes are remembered, if anywhere.
    pub fn state_file(&self) -> Option<&Path> {
        self.state_file.as_deref()
    }

    /// Whether this client points at a session the mutation guard protects: the default
    /// session's socket (under `~/.config/herdr`, and under `$XDG_CONFIG_HOME/herdr` when
    /// that is set), the socket `HERDR_SOCKET_PATH` named when the process first built a
    /// client, or one added with [`Client::with_protected_socket`]. Symlinks and `..` are
    /// resolved before comparing, so another spelling of the same path does not slip through.
    pub fn is_live_session(&self) -> bool {
        let mine = resolve(&self.socket_path);
        live_sockets()
            .iter()
            .chain(self.protected.iter())
            .any(|path| resolve(path) == mine)
    }

    /// Whether a herdr server answers on the socket right now.
    pub fn is_available(&self) -> bool {
        self.ping().is_ok()
    }

    /// Ask the server for its version and protocol number.
    pub fn ping(&self) -> Result<ServerInfo, HerdrError> {
        let reply = self.call("ping", Value::Null)?;
        serde_json::from_value(reply).map_err(|err| HerdrError::protocol("ping", err.to_string()))
    }

    /// Call `method` with `params` and return the reply's `result`.
    ///
    /// `params` must be a JSON object; `Value::Null` is sent as `{}` because herdr rejects a
    /// missing or null `params`. The names and shapes are herdr's own: `herdr api schema
    /// --json` prints them.
    ///
    /// Errors: [`HerdrError::MutationRefused`] before anything is sent when the guard
    /// applies; [`HerdrError::NotRunning`] when nothing listens on the socket;
    /// [`HerdrError::Timeout`] when connecting, sending and reading the reply together take
    /// longer than the timeout; [`HerdrError::Server`] with herdr's code and message when
    /// herdr refuses the request; [`HerdrError::Protocol`] when the reply is not a JSON
    /// object with a `result` or an `error`.
    pub fn call(&self, method: &str, params: Value) -> Result<Value, HerdrError> {
        if is_mutating(method) && !self.allow_live_mutations && self.is_live_session() {
            return Err(HerdrError::MutationRefused {
                method: method.to_string(),
                socket: self.socket_path.clone(),
            });
        }
        let params = match params {
            Value::Null => Value::Object(Map::new()),
            other => other,
        };
        let request = json!({ "id": REQUEST_ID, "method": method, "params": params });
        let mut line = serde_json::to_vec(&request).map_err(|err| {
            HerdrError::protocol(method, format!("request is not valid JSON: {err}"))
        })?;
        line.push(b'\n');
        let reply = self.round_trip(method, &line)?;
        parse_reply(method, &reply)
    }

    /// Call `method` and decode field `key` of its result.
    pub(crate) fn call_for<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
        key: &str,
    ) -> Result<T, HerdrError> {
        decode(method, self.call(method, params)?, key)
    }

    /// Send one request line and read the reply line, all before the deadline.
    fn round_trip(&self, method: &str, request: &[u8]) -> Result<Vec<u8>, HerdrError> {
        let deadline = Instant::now() + self.timeout;
        let mut stream = self.connect(method, deadline)?;
        self.send(method, &mut stream, request, deadline)?;
        self.read_line(method, &mut stream, deadline)
    }

    fn connect(&self, method: &str, deadline: Instant) -> Result<Socket, HerdrError> {
        let address = SockAddr::unix(&self.socket_path).map_err(|err| {
            HerdrError::io(format!("herdr socket {}", self.socket_path.display()), err)
        })?;
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None)
            .map_err(|err| HerdrError::io("creating a socket for herdr", err))?;
        let left = self.time_left(method, deadline)?;
        // Bound every later read and write from the start: once the peer has hung up the
        // timeouts can no longer be changed (see `shorten`).
        socket
            .set_read_timeout(Some(left))
            .and_then(|()| socket.set_write_timeout(Some(left)))
            .map_err(|err| HerdrError::io("setting the herdr socket timeouts", err))?;
        match socket.connect_timeout(&address, left) {
            Ok(()) => Ok(socket),
            Err(err) => Err(match err.kind() {
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
                    HerdrError::NotRunning {
                        socket: self.socket_path.clone(),
                    }
                }
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => self.timed_out(method),
                _ => HerdrError::io(
                    format!("connecting to herdr at {}", self.socket_path.display()),
                    err,
                ),
            }),
        }
    }

    fn send(
        &self,
        method: &str,
        stream: &mut Socket,
        mut bytes: &[u8],
        deadline: Instant,
    ) -> Result<(), HerdrError> {
        while !bytes.is_empty() {
            let left = self.time_left(method, deadline)?;
            shorten(stream.set_write_timeout(Some(left)))?;
            match stream.write(bytes) {
                Ok(0) => {
                    return Err(HerdrError::protocol(
                        method,
                        "herdr closed the connection mid-request",
                    ))
                }
                Ok(written) => bytes = &bytes[written..],
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) if is_timeout(&err) => return Err(self.timed_out(method)),
                Err(err) => {
                    return Err(HerdrError::io(format!("sending `{method}` to herdr"), err))
                }
            }
        }
        Ok(())
    }

    /// Read up to the first newline. The server closes the connection after its one reply,
    /// so an end of stream without a newline still yields whatever arrived.
    fn read_line(
        &self,
        method: &str,
        stream: &mut Socket,
        deadline: Instant,
    ) -> Result<Vec<u8>, HerdrError> {
        let mut reply = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let left = self.time_left(method, deadline)?;
            shorten(stream.set_read_timeout(Some(left)))?;
            match stream.read(&mut chunk) {
                Ok(0) => return Ok(reply),
                Ok(read) => {
                    if let Some(end) = chunk[..read].iter().position(|byte| *byte == b'\n') {
                        reply.extend_from_slice(&chunk[..end]);
                        return Ok(reply);
                    }
                    reply.extend_from_slice(&chunk[..read]);
                    if reply.len() > MAX_REPLY_BYTES {
                        return Err(HerdrError::protocol(
                            method,
                            "the reply is larger than 64 MiB",
                        ));
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) if is_timeout(&err) => return Err(self.timed_out(method)),
                Err(err) => {
                    return Err(HerdrError::io(
                        format!("reading the reply to `{method}`"),
                        err,
                    ))
                }
            }
        }
    }

    /// The time left until `deadline`, or the timeout error once it has all but passed.
    fn time_left(&self, method: &str, deadline: Instant) -> Result<Duration, HerdrError> {
        match deadline.checked_duration_since(Instant::now()) {
            Some(left) if left >= SHORTEST_WAIT => Ok(left),
            _ => Err(self.timed_out(method)),
        }
    }

    fn timed_out(&self, method: &str) -> HerdrError {
        HerdrError::Timeout {
            method: method.to_string(),
            timeout: self.timeout,
        }
    }
}

/// The outcome of cutting a socket timeout down to the time that is left.
///
/// macOS refuses to change options on a socket whose peer has hung up (`EINVAL`), and herdr
/// hangs up right after its reply, often before the client gets to read it. That refusal is
/// harmless: such a socket hands over what it has buffered and then reports the end of the
/// stream without ever blocking, and the timeout set before connecting still stands.
fn shorten(changed: io::Result<()>) -> Result<(), HerdrError> {
    match changed {
        Err(err) if err.kind() != io::ErrorKind::InvalidInput => {
            Err(HerdrError::io("setting the herdr socket timeout", err))
        }
        _ => Ok(()),
    }
}

/// A read or write that ran into its socket timeout.
fn is_timeout(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// Turn one reply line into the `result` it carries or the error it reports.
fn parse_reply(method: &str, line: &[u8]) -> Result<Value, HerdrError> {
    if line.iter().all(u8::is_ascii_whitespace) {
        return Err(HerdrError::protocol(
            method,
            "herdr closed the connection without a reply",
        ));
    }
    let reply: Value = serde_json::from_slice(line)
        .map_err(|err| HerdrError::protocol(method, format!("the reply is not JSON: {err}")))?;
    let Value::Object(mut reply) = reply else {
        return Err(HerdrError::protocol(
            method,
            "the reply is not a JSON object",
        ));
    };
    match reply.remove("error") {
        None | Some(Value::Null) => {}
        Some(error) => return Err(server_error(method, error)),
    }
    reply
        .remove("result")
        .ok_or_else(|| HerdrError::protocol(method, "the reply has neither `result` nor `error`"))
}

fn server_error(method: &str, error: Value) -> HerdrError {
    let text = |key: &str| error.get(key).and_then(Value::as_str).map(str::to_string);
    let (code, message) = match &error {
        Value::Object(_) => (
            text("code").unwrap_or_default(),
            text("message").unwrap_or_else(|| error.to_string()),
        ),
        Value::String(message) => (String::new(), message.clone()),
        other => (String::new(), other.to_string()),
    };
    HerdrError::Server {
        method: method.to_string(),
        code,
        message,
    }
}

/// Take field `key` out of a reply's result and decode it.
pub(crate) fn decode<T: DeserializeOwned>(
    method: &str,
    mut result: Value,
    key: &str,
) -> Result<T, HerdrError> {
    let value = result
        .get_mut(key)
        .map(Value::take)
        .ok_or_else(|| HerdrError::protocol(method, format!("the reply has no `{key}`")))?;
    serde_json::from_value(value)
        .map_err(|err| HerdrError::protocol(method, format!("bad `{key}`: {err}")))
}

/// The sockets of the session the user is working in, noted once per process.
fn live_sockets() -> &'static [PathBuf] {
    static LIVE: OnceLock<Vec<PathBuf>> = OnceLock::new();
    LIVE.get_or_init(|| {
        live_sockets_from(
            home_dir().as_deref(),
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os(SOCKET_ENV_VAR),
        )
    })
}

/// The sockets a live session can be on: the default session's socket where the scripts look
/// for it (`<home>/.config/herdr/herdr.sock`), where herdr itself puts it when
/// `XDG_CONFIG_HOME` is set, and the socket the environment named.
fn live_sockets_from(
    home: Option<&Path>,
    xdg_config_home: Option<OsString>,
    named: Option<OsString>,
) -> Vec<PathBuf> {
    let mut sockets = vec![socket_path_from(None, home)];
    if let Some(dir) = crate::launch::config_dir(xdg_config_home.as_deref(), home) {
        sockets.push(crate::launch::Session::Default.socket_path(&dir));
    }
    if let Some(named) = named.filter(|value| !value.is_empty()) {
        sockets.push(PathBuf::from(named));
    }
    sockets.dedup();
    sockets
}

/// `path` with symlinks and `..` resolved. A socket whose server has stopped does not exist,
/// so fall back to resolving its directory, and to the absolute path when that is gone too.
fn resolve(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
        if let Ok(real_parent) = std::fs::canonicalize(parent) {
            return real_parent.join(name);
        }
    }
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The user's home directory: `$HOME`, or the password database when that is unset.
fn home_dir() -> Option<PathBuf> {
    std::env::home_dir().filter(|home| !home.as_os_str().is_empty())
}

/// The socket the scripts would use: `named` when set and not empty, else
/// `<home>/.config/herdr/herdr.sock`. Without a home directory the `~` stays as it is, which
/// is what the scripts end up with too.
pub(crate) fn socket_path_from(named: Option<OsString>, home: Option<&Path>) -> PathBuf {
    match named.filter(|value| !value.is_empty()) {
        Some(named) => PathBuf::from(named),
        None => home
            .unwrap_or(Path::new("~"))
            .join(".config")
            .join("herdr")
            .join("herdr.sock"),
    }
}

/// The collapse state file the `herdr-collapse` script uses: `herdr-collapse/state.json`
/// under `xdg_cache_home` when set and not empty, else under `<home>/.cache`.
pub(crate) fn state_file_from(xdg_cache_home: Option<OsString>, home: Option<&Path>) -> PathBuf {
    let cache = match xdg_cache_home.filter(|value| !value.is_empty()) {
        Some(cache) => PathBuf::from(cache),
        None => home.unwrap_or(Path::new("~")).join(".cache"),
    };
    cache.join("herdr-collapse").join("state.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{MockServer, Reply};

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn client_and_error_are_send_and_sync() {
        assert_send_sync::<Client>();
        assert_send_sync::<HerdrError>();
    }

    #[test]
    fn call_sends_one_json_line_and_returns_the_result() {
        let server = MockServer::start();
        server.ok(
            "pane.get",
            json!({"type": "pane_info", "pane": {"pane_id": "w1:p1"}}),
        );
        let result = server
            .client()
            .call("pane.get", json!({"pane_id": "w1:p1"}))
            .unwrap();
        assert_eq!(result["pane"]["pane_id"], "w1:p1");
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "pane.get");
        assert_eq!(requests[0].params, json!({"pane_id": "w1:p1"}));
        assert_eq!(requests[0].id, "bakedit");
        server.assert_finished();
    }

    #[test]
    fn null_params_are_sent_as_an_empty_object() {
        let server = MockServer::start();
        server.ok(
            "ping",
            json!({"type": "pong", "version": "0.9.5", "protocol": 22}),
        );
        server.client().call("ping", Value::Null).unwrap();
        assert_eq!(server.requests()[0].params, json!({}));
    }

    #[test]
    fn ping_reports_version_and_protocol() {
        let server = MockServer::start();
        server.ok(
            "ping",
            json!({"type": "pong", "version": "0.9.5", "protocol": 22, "capabilities": {}}),
        );
        server.ok(
            "ping",
            json!({"type": "pong", "version": "0.9.5", "protocol": 22}),
        );
        let client = server.client();
        assert_eq!(
            client.ping().unwrap(),
            ServerInfo {
                version: "0.9.5".into(),
                protocol: 22
            }
        );
        assert!(client.is_available());
    }

    #[test]
    fn a_missing_socket_means_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("herdr.sock");
        let client = Client::new(&socket);
        let err = client.call("ping", Value::Null).unwrap_err();
        assert!(
            matches!(&err, HerdrError::NotRunning { socket: s } if s == &socket),
            "{err:?}"
        );
        assert!(err.is_not_running());
        assert!(!client.is_available());
    }

    #[test]
    fn a_socket_file_nobody_listens_on_means_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("herdr.sock");
        // Binding and dropping the listener leaves the file behind, like a crashed server.
        drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
        assert!(socket.exists());
        let err = Client::new(&socket).call("ping", Value::Null).unwrap_err();
        assert!(err.is_not_running(), "{err:?}");
    }

    #[test]
    fn a_path_too_long_for_a_socket_is_an_io_error() {
        let long = format!("/tmp/{}/herdr.sock", "x".repeat(200));
        let err = Client::new(long).call("ping", Value::Null).unwrap_err();
        assert!(matches!(err, HerdrError::Io { .. }), "{err:?}");
    }

    #[test]
    fn a_server_error_carries_code_and_message() {
        let server = MockServer::start();
        server.fail("pane.get", "pane_not_found", "pane w9:p9 not found");
        let err = server
            .client()
            .call("pane.get", json!({"pane_id": "w9:p9"}))
            .unwrap_err();
        assert_eq!(err.code(), Some("pane_not_found"));
        assert_eq!(
            err.to_string(),
            "herdr refused `pane.get`: pane w9:p9 not found (pane_not_found)"
        );
        assert!(!err.is_not_running());
    }

    #[test]
    fn errors_read_well_as_messages() {
        let socket = PathBuf::from("/run/herdr.sock");
        let not_running = HerdrError::NotRunning {
            socket: socket.clone(),
        };
        assert_eq!(
            not_running.to_string(),
            "herdr is not running (no server at /run/herdr.sock)"
        );
        let timeout = HerdrError::Timeout {
            method: "pane.focus".into(),
            timeout: Duration::from_secs(2),
        };
        assert_eq!(
            timeout.to_string(),
            "herdr did not answer `pane.focus` within 2s"
        );
        let refused = HerdrError::MutationRefused {
            method: "pane.close".into(),
            socket,
        };
        assert_eq!(
            refused.to_string(),
            "refusing to send `pane.close` to the live herdr session at /run/herdr.sock"
        );
        let protocol = HerdrError::protocol("ping", "the reply is not JSON");
        assert_eq!(
            protocol.to_string(),
            "unexpected reply to `ping` from herdr: the reply is not JSON"
        );
        let io = HerdrError::io("writing /x/state.json", io::Error::other("disk full"));
        assert_eq!(io.to_string(), "writing /x/state.json: disk full");
        assert_eq!(
            HerdrError::NoStateFile.to_string(),
            "this herdr client has no collapse state file"
        );
        for err in [
            not_running,
            timeout,
            refused,
            protocol,
            io,
            HerdrError::NoStateFile,
        ] {
            assert_eq!(err.code(), None);
        }
    }

    #[test]
    fn odd_error_bodies_still_become_server_errors() {
        let server = MockServer::start();
        server.expect(
            "ping",
            Reply::Raw(b"{\"id\":\"bakedit\",\"error\":\"boom\"}\n".to_vec()),
        );
        server.expect(
            "ping",
            Reply::Raw(b"{\"id\":\"bakedit\",\"error\":{\"detail\":1}}\n".to_vec()),
        );
        server.expect(
            "ping",
            Reply::Raw(b"{\"id\":\"bakedit\",\"error\":7}\n".to_vec()),
        );
        let client = server.client();
        let err = client.call("ping", Value::Null).unwrap_err();
        assert!(
            matches!(&err, HerdrError::Server { code, message, .. } if code.is_empty() && message == "boom")
        );
        let err = client.call("ping", Value::Null).unwrap_err();
        assert!(
            matches!(&err, HerdrError::Server { code, message, .. } if code.is_empty() && message.contains("detail"))
        );
        let err = client.call("ping", Value::Null).unwrap_err();
        assert!(matches!(&err, HerdrError::Server { message, .. } if message == "7"));
    }

    #[test]
    fn a_null_error_next_to_a_result_is_not_an_error() {
        let server = MockServer::start();
        server.expect(
            "ping",
            Reply::Raw(b"{\"id\":\"bakedit\",\"error\":null,\"result\":{\"ok\":true}}\n".to_vec()),
        );
        assert_eq!(
            server.client().call("ping", Value::Null).unwrap(),
            json!({"ok": true})
        );
    }

    #[test]
    fn malformed_replies_are_protocol_errors() {
        let server = MockServer::start();
        server.expect("ping", Reply::Raw(b"not json\n".to_vec()));
        server.expect("ping", Reply::Raw(b"[1, 2]\n".to_vec()));
        server.expect("ping", Reply::Raw(b"{\"id\":\"bakedit\"}\n".to_vec()));
        server.expect("ping", Reply::Close);
        server.expect("ping", Reply::Raw(b"\n".to_vec()));
        let client = server.client();
        for expected in [
            "not JSON",
            "not a JSON object",
            "neither `result` nor `error`",
            "without a reply",
            "without a reply",
        ] {
            let err = client.call("ping", Value::Null).unwrap_err();
            assert!(
                matches!(&err, HerdrError::Protocol { detail, .. } if detail.contains(expected)),
                "{err:?}"
            );
        }
    }

    #[test]
    fn a_reply_without_a_trailing_newline_is_still_read() {
        let server = MockServer::start();
        server.expect(
            "ping",
            Reply::Raw(b"{\"id\":\"bakedit\",\"result\":{\"type\":\"ok\"}}".to_vec()),
        );
        assert_eq!(
            server.client().call("ping", Value::Null).unwrap(),
            json!({"type": "ok"})
        );
    }

    #[test]
    fn only_the_first_line_of_a_reply_is_used() {
        let server = MockServer::start();
        server.expect(
            "ping",
            Reply::Raw(b"{\"result\":{\"n\":1}}\n{\"result\":{\"n\":2}}\n".to_vec()),
        );
        assert_eq!(
            server.client().call("ping", Value::Null).unwrap(),
            json!({"n": 1})
        );
    }

    #[test]
    fn a_large_reply_arrives_whole() {
        let server = MockServer::start();
        let text = "x".repeat(300_000);
        server.ok("pane.read", json!({"read": {"text": text}}));
        let result = server
            .client()
            .call("pane.read", json!({"pane_id": "w1:p1", "source": "recent"}))
            .unwrap();
        assert_eq!(result["read"]["text"].as_str().unwrap().len(), 300_000);
    }

    #[test]
    fn a_silent_server_times_out_within_the_timeout() {
        let server = MockServer::start();
        server.expect("ping", Reply::Hang(Duration::from_secs(5)));
        let client = server.client().with_timeout(Duration::from_millis(150));
        let started = Instant::now();
        let err = client.call("ping", Value::Null).unwrap_err();
        let took = started.elapsed();
        assert!(
            matches!(&err, HerdrError::Timeout { method, timeout } if method == "ping" && *timeout == Duration::from_millis(150)),
            "{err:?}"
        );
        assert!(
            took >= Duration::from_millis(140),
            "returned too early: {took:?}"
        );
        assert!(took < Duration::from_millis(1500), "blocked for {took:?}");
    }

    #[test]
    fn a_reply_that_trickles_in_is_cut_off_at_the_deadline() {
        let server = MockServer::start();
        server.expect(
            "ping",
            Reply::Trickle {
                bytes: b"{\"result\":{}}".to_vec(),
                gap: Duration::from_millis(60),
            },
        );
        let client = server.client().with_timeout(Duration::from_millis(200));
        let started = Instant::now();
        let err = client.call("ping", Value::Null).unwrap_err();
        assert!(matches!(err, HerdrError::Timeout { .. }), "{err:?}");
        assert!(started.elapsed() < Duration::from_millis(1500));
    }

    #[test]
    fn a_zero_timeout_fails_at_once() {
        let server = MockServer::start();
        let err = server
            .client()
            .with_timeout(Duration::ZERO)
            .call("ping", Value::Null)
            .unwrap_err();
        assert!(matches!(err, HerdrError::Timeout { .. }), "{err:?}");
        assert!(server.requests().is_empty());
    }

    #[test]
    fn a_timeout_too_short_to_count_fails_at_once_instead_of_waiting_for_ever() {
        // Less than a microsecond would reach the socket as "no timeout at all".
        let server = MockServer::start();
        server.expect("ping", Reply::Hang(Duration::from_secs(3)));
        for timeout in [
            Duration::from_nanos(1),
            Duration::from_nanos(999),
            Duration::from_micros(999),
        ] {
            let started = Instant::now();
            let err = server
                .client()
                .with_timeout(timeout)
                .call("ping", Value::Null)
                .unwrap_err();
            assert!(matches!(err, HerdrError::Timeout { .. }), "{err:?}");
            assert!(started.elapsed() < Duration::from_millis(500));
        }
        assert!(server.requests().is_empty());
    }

    #[test]
    fn timeouts_are_capped() {
        let client = Client::new("/nowhere/herdr.sock").with_timeout(Duration::MAX);
        assert_eq!(client.timeout(), MAX_TIMEOUT);
        // The capped deadline can be computed: the call fails for the right reason.
        assert!(client
            .call("ping", Value::Null)
            .unwrap_err()
            .is_not_running());
    }

    #[test]
    fn read_only_and_mutating_methods_are_told_apart() {
        for method in [
            "ping",
            "session.snapshot",
            "pane.current",
            "pane.layout",
            "layout.export",
            "pane.neighbor",
            "tab.list",
        ] {
            assert!(!is_mutating(method), "{method}");
        }
        for method in [
            "pane.focus",
            "pane.focus_direction",
            "pane.swap",
            "pane.move",
            "pane.zoom",
            "pane.resize",
            "pane.scroll",
            "pane.send_text",
            "pane.split",
            "pane.close",
            "layout.set_split_ratio",
            "tab.focus",
            "tab.create",
            "workspace.focus",
            "workspace.create",
            "server.stop",
            "something.new",
            "",
        ] {
            assert!(is_mutating(method), "{method}");
        }
    }

    #[test]
    fn mutations_are_refused_on_a_protected_socket_before_anything_is_sent() {
        let server = MockServer::start();
        let client = server.client().with_protected_socket(server.socket_path());
        assert!(client.is_live_session());
        let err = client
            .call("pane.focus", json!({"pane_id": "w1:p1"}))
            .unwrap_err();
        assert!(
            matches!(&err, HerdrError::MutationRefused { method, socket } if method == "pane.focus" && socket == server.socket_path()),
            "{err:?}"
        );
        assert!(
            server.requests().is_empty(),
            "the refused request reached the server"
        );
    }

    #[test]
    fn read_only_calls_pass_the_guard() {
        let server = MockServer::start();
        server.ok("pane.current", json!({"pane": {}}));
        let client = server.client().with_protected_socket(server.socket_path());
        client.call("pane.current", Value::Null).unwrap();
        assert_eq!(server.requests().len(), 1);
    }

    #[test]
    fn the_application_can_allow_live_mutations() {
        let server = MockServer::start();
        server.ok("pane.focus", json!({"pane": {}}));
        let client = server
            .client()
            .with_protected_socket(server.socket_path())
            .allow_live_mutations(true);
        client
            .call("pane.focus", json!({"pane_id": "w1:p1"}))
            .unwrap();
        assert_eq!(server.requests().len(), 1);
        // And take the permission back.
        let err = client
            .allow_live_mutations(false)
            .call("pane.focus", json!({"pane_id": "w1:p1"}))
            .unwrap_err();
        assert!(matches!(err, HerdrError::MutationRefused { .. }));
    }

    #[test]
    fn the_guard_sees_through_other_spellings_of_the_socket_path() {
        let server = MockServer::start();
        let dir = server.socket_path().parent().unwrap();
        let name = server.socket_path().file_name().unwrap();
        let dotted = dir.join("sub").join("..").join(name);
        std::fs::create_dir(dir.join("sub")).unwrap();
        let link = dir.join("alias.sock");
        std::os::unix::fs::symlink(server.socket_path(), &link).unwrap();
        for spelling in [dotted, link] {
            let client = Client::new(&spelling).with_protected_socket(server.socket_path());
            assert!(client.is_live_session(), "{}", spelling.display());
            let err = client
                .call("pane.close", json!({"pane_id": "w1:p1"}))
                .unwrap_err();
            assert!(matches!(err, HerdrError::MutationRefused { .. }), "{err:?}");
        }
        assert!(server.requests().is_empty());
    }

    #[test]
    fn the_guard_covers_a_socket_that_does_not_exist_yet() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("later.sock");
        let client =
            Client::new(dir.path().join(".").join("later.sock")).with_protected_socket(&socket);
        assert!(client.is_live_session());
        assert!(matches!(
            client
                .call("pane.focus", json!({"pane_id": "w1:p1"}))
                .unwrap_err(),
            HerdrError::MutationRefused { .. }
        ));
    }

    #[test]
    fn an_unprotected_socket_is_not_live() {
        let server = MockServer::start();
        assert!(!server.client().is_live_session());
    }

    // The two tests below point a client at the user's real session socket. They are safe by
    // construction: the guard answers before any connection is made, and the request they try
    // names a pane that cannot exist, so even a broken guard could not change anything.
    const NO_SUCH_PANE: &str = "bakedit-guard-test:no-such-pane";

    #[test]
    fn the_default_socket_is_protected() {
        let Some(home) = home_dir() else { return };
        let client = Client::new(socket_path_from(None, Some(&home)));
        assert!(client.is_live_session());
        let err = client
            .call("pane.focus", json!({"pane_id": NO_SUCH_PANE}))
            .unwrap_err();
        assert!(matches!(err, HerdrError::MutationRefused { .. }), "{err:?}");
    }

    #[test]
    fn the_socket_named_by_the_environment_is_protected() {
        let Some(named) = std::env::var_os(SOCKET_ENV_VAR).filter(|value| !value.is_empty()) else {
            return;
        };
        let client = Client::new(PathBuf::from(named));
        assert!(client.is_live_session());
        let err = client
            .call("pane.focus", json!({"pane_id": NO_SUCH_PANE}))
            .unwrap_err();
        assert!(matches!(err, HerdrError::MutationRefused { .. }), "{err:?}");
        // The client the application would build is guarded the same way.
        let client = Client::from_env_or_default();
        assert!(client.is_live_session());
        let err = client
            .call("pane.focus", json!({"pane_id": NO_SUCH_PANE}))
            .unwrap_err();
        assert!(matches!(err, HerdrError::MutationRefused { .. }), "{err:?}");
    }

    #[test]
    fn the_guard_knows_every_place_a_live_socket_can_be() {
        let home = Path::new("/home/nat");
        assert_eq!(
            live_sockets_from(Some(home), None, None),
            vec![PathBuf::from("/home/nat/.config/herdr/herdr.sock")]
        );
        assert_eq!(
            live_sockets_from(
                Some(home),
                Some("/x/config".into()),
                Some("/run/outer.sock".into())
            ),
            vec![
                PathBuf::from("/home/nat/.config/herdr/herdr.sock"),
                PathBuf::from("/x/config/herdr/herdr.sock"),
                PathBuf::from("/run/outer.sock"),
            ]
        );
        assert_eq!(
            live_sockets_from(Some(home), Some("".into()), Some("".into())),
            vec![PathBuf::from("/home/nat/.config/herdr/herdr.sock")]
        );
        // What this process really noted includes the socket its environment names.
        if let Some(named) = std::env::var_os(SOCKET_ENV_VAR).filter(|value| !value.is_empty()) {
            assert!(live_sockets().contains(&PathBuf::from(named)));
        }
    }

    #[test]
    fn socket_path_follows_the_scripts() {
        let home = Path::new("/home/nat");
        assert_eq!(
            socket_path_from(None, Some(home)),
            Path::new("/home/nat/.config/herdr/herdr.sock")
        );
        assert_eq!(
            socket_path_from(Some("".into()), Some(home)),
            Path::new("/home/nat/.config/herdr/herdr.sock")
        );
        assert_eq!(
            socket_path_from(Some("/run/h.sock".into()), Some(home)),
            Path::new("/run/h.sock")
        );
        assert_eq!(
            socket_path_from(None, None),
            Path::new("~/.config/herdr/herdr.sock")
        );
    }

    #[test]
    fn state_file_follows_the_collapse_script() {
        let home = Path::new("/home/nat");
        assert_eq!(
            state_file_from(None, Some(home)),
            Path::new("/home/nat/.cache/herdr-collapse/state.json")
        );
        assert_eq!(
            state_file_from(Some("".into()), Some(home)),
            Path::new("/home/nat/.cache/herdr-collapse/state.json")
        );
        assert_eq!(
            state_file_from(Some("/x/cache".into()), Some(home)),
            Path::new("/x/cache/herdr-collapse/state.json")
        );
    }

    #[test]
    fn from_env_or_default_uses_the_environment() {
        let client = Client::from_env_or_default();
        let home = home_dir();
        assert_eq!(
            client.socket_path(),
            socket_path_from(std::env::var_os(SOCKET_ENV_VAR), home.as_deref())
        );
        assert_eq!(
            client.state_file(),
            Some(state_file_from(std::env::var_os("XDG_CACHE_HOME"), home.as_deref()).as_path())
        );
        assert_eq!(client.timeout(), DEFAULT_TIMEOUT);
    }

    #[test]
    fn a_new_client_has_no_state_file_until_given_one() {
        let client = Client::new("/nowhere/herdr.sock");
        assert_eq!(client.state_file(), None);
        let client = client.with_state_file("/tmp/x/state.json");
        assert_eq!(client.state_file(), Some(Path::new("/tmp/x/state.json")));
        assert_eq!(client.socket_path(), Path::new("/nowhere/herdr.sock"));
    }

    #[test]
    fn the_socket_can_be_swapped_while_everything_else_stays() {
        let client = Client::new("/nowhere/a.sock")
            .with_state_file("/tmp/x/state.json")
            .with_timeout(Duration::from_millis(250))
            .with_socket_path("/nowhere/b.sock");
        assert_eq!(client.socket_path(), Path::new("/nowhere/b.sock"));
        assert_eq!(client.state_file(), Some(Path::new("/tmp/x/state.json")));
        assert_eq!(client.timeout(), Duration::from_millis(250));
        assert!(!client.is_live_session());
    }

    #[test]
    fn swapping_in_the_live_socket_turns_the_guard_on() {
        let server = MockServer::start();
        let client = Client::new("/nowhere/a.sock")
            .with_protected_socket(server.socket_path())
            .with_socket_path(server.socket_path());
        assert!(client.is_live_session());
        assert!(matches!(
            client
                .call("pane.focus", json!({"pane_id": "w1:p1"}))
                .unwrap_err(),
            HerdrError::MutationRefused { .. }
        ));
        assert!(server.requests().is_empty());
    }

    #[test]
    fn many_threads_can_share_one_client() {
        let server =
            MockServer::with_handler(|request| Some(Reply::Result(request.params.clone())));
        let client = server.client();
        std::thread::scope(|scope| {
            for worker in 0..8 {
                let client = &client;
                scope.spawn(move || {
                    for round in 0..20 {
                        let sent = json!({"worker": worker, "round": round});
                        assert_eq!(client.call("echo", sent.clone()).unwrap(), sent);
                    }
                });
            }
        });
        assert_eq!(server.requests().len(), 160);
    }

    #[test]
    fn decode_reports_missing_and_misshapen_fields() {
        let err = decode::<String>("pane.get", json!({"other": 1}), "pane").unwrap_err();
        assert!(
            matches!(&err, HerdrError::Protocol { detail, .. } if detail.contains("no `pane`"))
        );
        let err = decode::<String>("pane.get", json!({"pane": 1}), "pane").unwrap_err();
        assert!(
            matches!(&err, HerdrError::Protocol { detail, .. } if detail.contains("bad `pane`"))
        );
        assert_eq!(
            decode::<String>("pane.get", json!({"pane": "x"}), "pane").unwrap(),
            "x"
        );
    }
}
