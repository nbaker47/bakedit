//! Talking to a running bakedit: `bakedit file.rs:42` from any shell (or from an agent's
//! hook) opens the file in the window that is already up, the way `zed file` does.
//!
//! Each running window listens on a unix socket in `~/.local/state/bakedit/instances/` and
//! keeps a small JSON file next to it saying which project it has open and when it last had
//! the focus. A client reads those files, picks a window and sends one JSON line.

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// A file to open, with an optional 1-based position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenTarget {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Open these files; the last one ends up in front.
    Open { targets: Vec<OpenTarget> },
    /// Run a named action, as if a key bound to it had been pressed.
    Action {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<serde_json::Value>,
    },
    Ping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What a running window publishes about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceInfo {
    pub pid: u32,
    pub root: PathBuf,
    pub socket: PathBuf,
    /// Milliseconds since the epoch when this window last gained the focus.
    pub focused_at: u64,
}

/// `$XDG_STATE_HOME/bakedit`, else `~/.local/state/bakedit`.
pub fn state_dir() -> PathBuf {
    let base = match std::env::var_os("XDG_STATE_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".local").join("state"),
    };
    base.join("bakedit")
}

pub fn instances_dir() -> PathBuf {
    state_dir().join("instances")
}

fn now_millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn write_info(path: &Path, info: &InstanceInfo) -> io::Result<()> {
    // Write to a temp name and rename so a reader never sees half a file.
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec(info)?)?;
    fs::rename(&tmp, path)
}

/// The listening side, owned by a running window. Dropping it removes its files.
pub struct Server {
    info: InstanceInfo,
    info_path: PathBuf,
    stop: Arc<AtomicBool>,
}

impl Server {
    /// Start listening in `dir` for a window whose project is `root`. `on_request` is called
    /// on the listener thread for every request; it should hand the request to the UI
    /// thread and return quickly.
    pub fn start(dir: &Path, root: &Path, on_request: Box<dyn Fn(Request) + Send>) -> io::Result<Server> {
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        let pid = std::process::id();
        let socket = dir.join(format!("{pid}.sock"));
        let info_path = dir.join(format!("{pid}.json"));
        // A stale socket from a previous process that had our pid.
        let _ = fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket)?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
        let info = InstanceInfo { pid, root: root.to_path_buf(), socket: socket.clone(), focused_at: now_millis() };
        write_info(&info_path, &info)?;

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        std::thread::Builder::new().name("bakedit-ipc".into()).spawn(move || {
            for stream in listener.incoming() {
                if thread_stop.load(Ordering::Relaxed) {
                    break;
                }
                if let Ok(stream) = stream {
                    serve(stream, &on_request);
                }
            }
        })?;
        Ok(Server { info, info_path, stop })
    }

    pub fn info(&self) -> &InstanceInfo {
        &self.info
    }

    /// Record that this window just got the focus, so clients prefer it.
    pub fn touch_focus(&mut self) {
        self.info.focused_at = now_millis();
        let _ = write_info(&self.info_path, &self.info);
    }

    /// Record a change of project directory.
    pub fn set_root(&mut self, root: &Path) {
        self.info.root = root.to_path_buf();
        let _ = write_info(&self.info_path, &self.info);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Unblock the accept loop so the thread sees the flag and ends.
        let _ = UnixStream::connect(&self.info.socket);
        let _ = fs::remove_file(&self.info.socket);
        let _ = fs::remove_file(&self.info_path);
    }
}

fn serve(stream: UnixStream, on_request: &(dyn Fn(Request) + Send)) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    // One request per connection, at most 1 MiB.
    match reader.by_ref().take(1 << 20).read_line(&mut line) {
        Ok(0) | Err(_) => return,
        Ok(_) => {}
    }
    let response = match serde_json::from_str::<Request>(line.trim()) {
        Ok(request) => {
            if request != Request::Ping {
                on_request(request);
            }
            Response { ok: true, error: None }
        }
        Err(err) => Response { ok: false, error: Some(format!("bad request: {err}")) },
    };
    if let Ok(mut bytes) = serde_json::to_vec(&response) {
        bytes.push(b'\n');
        let _ = (&stream).write_all(&bytes);
    }
}

/// Send one request to a window and wait for its acknowledgement.
pub fn send(socket: &Path, request: &Request) -> io::Result<Response> {
    let stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut bytes = serde_json::to_vec(request)?;
    bytes.push(b'\n');
    (&stream).write_all(&bytes)?;
    let mut line = String::new();
    BufReader::new(&stream).take(1 << 16).read_line(&mut line)?;
    serde_json::from_str(line.trim()).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Every window that answers, most recently focused first. Files left behind by windows
/// that are gone are removed along the way.
pub fn live_instances(dir: &Path) -> Vec<InstanceInfo> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut live = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let info = fs::read(&path).ok().and_then(|bytes| serde_json::from_slice::<InstanceInfo>(&bytes).ok());
        match info {
            Some(info) if info.pid != std::process::id() && send(&info.socket, &Request::Ping).is_ok_and(|r| r.ok) => {
                live.push(info);
            }
            Some(info) if info.pid == std::process::id() => {}
            Some(info) => {
                let _ = fs::remove_file(&info.socket);
                let _ = fs::remove_file(&path);
            }
            None => {
                let _ = fs::remove_file(&path);
            }
        }
    }
    live.sort_by(|a, b| b.focused_at.cmp(&a.focused_at));
    live
}

/// The window that should open `paths`: the one whose project contains the first path (the
/// deepest such project if several do), else the most recently focused. `instances` must be
/// ordered most recently focused first, as [`live_instances`] returns them.
pub fn pick_instance<'a>(instances: &'a [InstanceInfo], paths: &[PathBuf]) -> Option<&'a InstanceInfo> {
    let containing = paths.first().and_then(|path| {
        instances
            .iter()
            .filter(|instance| path.starts_with(&instance.root))
            .max_by_key(|instance| instance.root.components().count())
    });
    containing.or_else(|| instances.first())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn info(pid: u32, root: &str, focused_at: u64) -> InstanceInfo {
        InstanceInfo { pid, root: PathBuf::from(root), socket: PathBuf::from("/nowhere"), focused_at }
    }

    #[test]
    fn requests_round_trip_as_json_lines() {
        let request = Request::Open {
            targets: vec![OpenTarget { path: "/a/b.rs".into(), line: Some(3), column: None }],
        };
        let text = serde_json::to_string(&request).unwrap();
        assert_eq!(text, r#"{"op":"open","targets":[{"path":"/a/b.rs","line":3}]}"#);
        assert_eq!(serde_json::from_str::<Request>(&text).unwrap(), request);
        assert_eq!(serde_json::from_str::<Request>(r#"{"op":"ping"}"#).unwrap(), Request::Ping);
        let action: Request = serde_json::from_str(r#"{"op":"action","name":"workspace::Save"}"#).unwrap();
        assert_eq!(action, Request::Action { name: "workspace::Save".into(), args: None });
    }

    #[test]
    fn picks_the_window_whose_project_contains_the_file() {
        let instances = vec![
            info(1, "/code/other", 300),
            info(2, "/code/app", 200),
            info(3, "/code/app/packages/web", 100),
        ];
        let pick = |path: &str| pick_instance(&instances, &[PathBuf::from(path)]).map(|i| i.pid);
        assert_eq!(pick("/code/app/src/main.rs"), Some(2));
        assert_eq!(pick("/code/app/packages/web/index.ts"), Some(3), "the deepest project wins");
        assert_eq!(pick("/elsewhere/x"), Some(1), "otherwise the most recently focused");
        assert_eq!(pick_instance(&instances, &[]).map(|i| i.pid), Some(1));
        assert_eq!(pick_instance(&[], &[PathBuf::from("/x")]), None);
        // A sibling whose name merely starts the same is not "inside".
        assert_eq!(pick("/code/app2/x.rs"), Some(1));
    }

    #[test]
    fn server_receives_requests_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let mut server = Server::start(dir.path(), Path::new("/code/app"), Box::new(move |r| tx.send(r).unwrap())).unwrap();
        let socket = server.info().socket.clone();

        assert_eq!(send(&socket, &Request::Ping).unwrap(), Response { ok: true, error: None });
        assert!(rx.try_recv().is_err(), "a ping is answered without bothering the app");

        let request = Request::Action { name: "workspace::Save".into(), args: None };
        assert!(send(&socket, &request).unwrap().ok);
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), request);

        // Garbage gets an error reply, not a hang or a crash.
        let stream = UnixStream::connect(&socket).unwrap();
        (&stream).write_all(b"not json\n").unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let reply: Response = serde_json::from_str(line.trim()).unwrap();
        assert!(!reply.ok);

        let before = server.info().focused_at;
        std::thread::sleep(Duration::from_millis(5));
        server.touch_focus();
        assert!(server.info().focused_at > before);

        let info_path = dir.path().join(format!("{}.json", std::process::id()));
        assert!(info_path.exists());
        drop(server);
        assert!(!socket.exists());
        assert!(!info_path.exists());
    }

    #[test]
    fn dead_instances_are_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let dead = InstanceInfo {
            pid: 1,
            root: "/code/app".into(),
            socket: dir.path().join("1.sock"),
            focused_at: 5,
        };
        fs::write(dir.path().join("1.json"), serde_json::to_vec(&dead).unwrap()).unwrap();
        fs::write(dir.path().join("2.json"), b"not json").unwrap();
        fs::write(dir.path().join("unrelated.txt"), b"keep me").unwrap();
        assert_eq!(live_instances(dir.path()), Vec::new());
        assert!(!dir.path().join("1.json").exists());
        assert!(!dir.path().join("2.json").exists());
        assert!(dir.path().join("unrelated.txt").exists());
        assert_eq!(live_instances(&dir.path().join("missing")), Vec::new());
    }
}
