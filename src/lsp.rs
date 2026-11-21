//! Minimal LSP client: JSON-RPC 2.0 over stdio to `rust-analyzer`.
//!
//! Two std threads (writer, reader) + mutex-protected shared state.
//! `request()` just allocates an id; responses land in a map that callers
//! poll with [`LspClient::take_response`]. No async runtime, no framework.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::Context as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspStatus {
    Loading,
    Ready,
    Failed,
    Off,
}

/// Convert a filesystem path to a `file://` URI as LSP expects it
/// (`file:///C:/foo/bar.rs` — forward slashes, percent-encoded).
pub fn file_uri(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut encoded = String::with_capacity(s.len() + 8);
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'/' | b'-' | b'.' | b'_' | b'~' | b'!'
            | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'=' | b':'
            | b'@' => encoded.push(b as char),
            _ => encoded.push_str(&format!("%{b:02X}")),
        }
    }
    format!("file:///{encoded}")
}

/// A spawned, initialized language server.
pub struct LspClient {
    child: Mutex<Child>,
    /// JSON bodies to write, consumed by the writer thread.
    tx: Option<Mutex<std::sync::mpsc::Sender<String>>>,
    responses: Arc<Mutex<HashMap<i64, serde_json::Value>>>,
    notifications: Arc<Mutex<Vec<serde_json::Value>>>,
    next_id: AtomicI64,
    ready: Arc<Mutex<bool>>,
}

impl LspClient {
    /// Spawn `rust-analyzer --stdio` and run the initialize handshake.
    /// Blocking; call from a background thread/task.
    pub fn spawn_blocking(server: &str, root: &Path) -> anyhow::Result<Self> {
        let mut child = Command::new(server)
            .arg("--stdio")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn {server}"))?;

        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take().context("no stderr")?;

        let (tx, rx) = std::sync::mpsc::channel::<String>();
        thread::spawn(move || writer_loop(stdin, rx));
        // Keep stderr drained or the server can block on a full pipe.
        thread::spawn(move || {
            let mut err = stderr;
            let mut sink = Vec::new();
            let _ = err.read_to_end(&mut sink);
        });

        let client = Self {
            child: Mutex::new(child),
            tx: Some(Mutex::new(tx)),
            responses: Arc::new(Mutex::new(HashMap::new())),
            notifications: Arc::new(Mutex::new(Vec::new())),
            next_id: AtomicI64::new(1),
            ready: Arc::new(Mutex::new(false)),
        };

        let responses = client.responses.clone();
        let notifications = client.notifications.clone();
        let ready = client.ready.clone();
        thread::spawn(move || reader_loop(stdout, responses, notifications, ready));

        // Initialize handshake.
        let root_uri = file_uri(root);
        let id = client.request(
            "initialize",
            serde_json::json!({
                "processId": null,
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "synchronization": { "didSave": true }
                    }
                }
            }),
        );
        for _ in 0..200 {
            if client.take_response(id).is_some() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(100));
        }
        client.notify("initialized", serde_json::json!({}));
        *client.ready.lock().unwrap() = true;
        Ok(client)
    }

    pub fn is_ready(&self) -> bool {
        *self.ready.lock().unwrap()
    }

    /// Send a request; returns its id. Response surfaces in `take_response`.
    pub fn request(&self, method: &str, params: serde_json::Value) -> i64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        id
    }

    pub fn notify(&self, method: &str, params: serde_json::Value) {
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }));
    }

    fn send(&self, body: serde_json::Value) {
        if let Some(tx) = &self.tx {
            if let Ok(sender) = tx.lock() {
                let _ = sender.send(body.to_string());
            }
        }
    }

    /// Take a completed response by id, if it has arrived.
    pub fn take_response(&self, id: i64) -> Option<serde_json::Value> {
        self.responses.lock().unwrap().remove(&id)
    }

    /// Drain queued server notifications (diagnostics, etc.).
    pub fn take_notifications(&self) -> Vec<serde_json::Value> {
        std::mem::take(&mut *self.notifications.lock().unwrap())
    }

    pub fn is_alive(&self) -> bool {
        self.child
            .lock()
            .unwrap()
            .try_wait()
            .map(|status| status.is_none())
            .unwrap_or(false)
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        self.tx = None;
        let _ = self.child.lock().unwrap().kill();
    }
}

fn writer_loop(mut stdin: ChildStdin, rx: std::sync::mpsc::Receiver<String>) {
    while let Ok(body) = rx.recv() {
        let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        if stdin.write_all(frame.as_bytes()).is_err() {
            break;
        }
        let _ = stdin.flush();
    }
}

fn reader_loop(
    stdout: impl Read,
    responses: Arc<Mutex<HashMap<i64, serde_json::Value>>>,
    notifications: Arc<Mutex<Vec<serde_json::Value>>>,
    ready: Arc<Mutex<bool>>,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return, // EOF: server gone
                Ok(_) => {
                    let line = line.trim_end();
                    if line.is_empty() {
                        break;
                    }
                    if let Some(value) = line
                        .strip_prefix("Content-Length:")
                        .map(|v| v.trim().parse().ok())
                        .flatten()
                    {
                        content_length = Some(value);
                    }
                }
            }
        }
        let Some(len) = content_length else { return };

        let mut body = vec![0u8; len];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let Ok(msg) = serde_json::from_slice::<serde_json::Value>(&body) else {
            continue;
        };

        if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
            if msg.get("method").is_some() {
                // Server -> client request we don't support: park it as a
                // notification for visibility.
                notifications.lock().unwrap().push(msg);
            } else {
                responses.lock().unwrap().insert(id, msg);
            }
        } else {
            notifications.lock().unwrap().push(msg);
        }
        let _ = ready;
    }
}
