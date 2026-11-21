//! Tauri commands + shared app state: project tree IO, LSP bridge, builds.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::lsp::{file_uri, LspClient, LspStatus};
use crate::output;

/// Directories hidden from the explorer.
const SKIPPED_DIRS: &[&str] = &[".git", "target", "node_modules", ".vscode", "dist", "build"];

pub struct AppState {
    pub root: Mutex<Option<PathBuf>>,
    pub lsp: Mutex<Option<Arc<LspClient>>>,
    pub lsp_status: Mutex<LspStatus>,
    /// Open documents by URI. Only docs the frontend didOpen'd live here, so
    /// didChange/didClose are routed correctly — rust-analyzer only ever
    /// hears about Rust files.
    pub docs: Mutex<HashMap<String, Doc>>,
    pub build_running: AtomicBool,
    /// Bumped on every root change / lsp spawn; stale in-flight spawns whose
    /// generation no longer matches are discarded.
    pub lsp_generation: AtomicU64,
}

/// An open document with its language (Monaco language id).
#[derive(Clone)]
pub struct Doc {
    pub language: String,
    pub text: String,
    pub version: u32,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            root: Mutex::new(None),
            lsp: Mutex::new(None),
            lsp_status: Mutex::new(LspStatus::Off),
            docs: Mutex::new(HashMap::new()),
            build_running: AtomicBool::new(false),
            lsp_generation: AtomicU64::new(0),
        }
    }
}

#[derive(Serialize)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
}

#[derive(Serialize)]
pub struct ToolInfo {
    pub cargo: output::ToolStatus,
    pub rust_analyzer: output::ToolStatus,
}

#[derive(Serialize, Clone)]
pub struct DiagnosticsPayload {
    pub uri: String,
    pub diagnostics: Vec<lsp_types::Diagnostic>,
}

fn emit_lsp_status(app: &AppHandle, status: LspStatus) {
    let s = match status {
        LspStatus::Loading => "loading",
        LspStatus::Ready => "ready",
        LspStatus::Failed => "failed",
        LspStatus::Off => "off",
    };
    let _ = app.emit("lsp-status", s);
}

/// Quick check: does this folder look like a Rust project?
fn has_rust(root: &Path) -> bool {
    if root.join("Cargo.toml").is_file() {
        return true;
    }
    let mut stack = vec![root.to_path_buf()];
    let mut depth = 0;
    while let Some(dir) = stack.pop() {
        depth += 1;
        if depth > 3 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !SKIPPED_DIRS.contains(&name) {
                    stack.push(p);
                }
            } else if p.extension().is_some_and(|e| e == "rs") {
                return true;
            }
        }
    }
    false
}

/// Called once from `setup`: pick the initial root and start the pollers.
pub fn init_app(app: AppHandle) {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(|| std::env::current_dir().ok());

    if let Some(root) = root {
        let state: State<AppState> = app.state();
        *state.root.lock().unwrap() = Some(root.clone());
        let term: State<crate::term::TermState> = app.state();
        *term.root.lock().unwrap() = Some(root.clone());
        maybe_start_lsp(&app, &root);
    }

    // Diagnostics poller: drains rust-analyzer notifications and forwards
    // publishDiagnostics payloads to the frontend.
    let handle = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(300));
        let state: State<AppState> = handle.state();
        let Some(client) = state.lsp.lock().unwrap().clone() else { continue };
        if !client.is_alive() {
            if *state.lsp_status.lock().unwrap() == LspStatus::Ready {
                *state.lsp_status.lock().unwrap() = LspStatus::Failed;
                emit_lsp_status(&handle, LspStatus::Failed);
            }
            continue;
        }
        for n in client.take_notifications() {
            if n.get("method").and_then(|m| m.as_str()) == Some("textDocument/publishDiagnostics") {
                let uri = n
                    .pointer("/params/textDocument/uri")
                    .and_then(|u| u.as_str())
                    .unwrap_or("")
                    .to_string();
                let diagnostics: Vec<lsp_types::Diagnostic> = n
                    .pointer("/params/diagnostics")
                    .and_then(|d| serde_json::from_value(d.clone()).ok())
                    .unwrap_or_default();
                let _ = handle.emit(
                    "diagnostics",
                    DiagnosticsPayload { uri, diagnostics },
                );
            }
        }
    });
}

fn maybe_start_lsp(app: &AppHandle, root: &Path) {
    let state: State<AppState> = app.state();
    if state.lsp.lock().unwrap().is_some() {
        return;
    }
    if !has_rust(root) {
        *state.lsp_status.lock().unwrap() = LspStatus::Off;
        emit_lsp_status(app, LspStatus::Off);
        return;
    }
    // Generation guard: if the user switches projects while rust-analyzer is
    // still handshaking, the stale result is discarded instead of clobbering
    // the new project's state.
    let generation = state.lsp_generation.fetch_add(1, Ordering::SeqCst) + 1;
    *state.lsp_status.lock().unwrap() = LspStatus::Loading;
    emit_lsp_status(app, LspStatus::Loading);
    let root = root.to_path_buf();
    let app = app.clone();
    std::thread::spawn(move || {
        let state: State<AppState> = app.state();
        let result = LspClient::spawn_blocking("rust-analyzer", &root);
        if state.lsp_generation.load(Ordering::SeqCst) != generation {
            // Stale spawn: the project changed while we were starting.
            drop(result);
            return;
        }
        match result {
            Ok(client) => {
                *state.lsp.lock().unwrap() = Some(Arc::new(client));
                *state.lsp_status.lock().unwrap() = LspStatus::Ready;
                emit_lsp_status(&app, LspStatus::Ready);
                // Open any Rust docs that were collected before the server
                // was ready, so they get diagnostics + completions.
                flush_open_docs(&state);
            }
            Err(err) => {
                eprintln!("failed to start rust-analyzer: {err:#}");
                *state.lsp_status.lock().unwrap() = LspStatus::Failed;
                emit_lsp_status(&app, LspStatus::Failed);
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_root(state: State<AppState>) -> Option<String> {
    state
        .root
        .lock()
        .unwrap()
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Frontend diagnostics: appends a line to %TEMP%\rustide-frontend.log.
#[tauri::command]
pub fn frontend_log(line: String) {
    use std::io::Write as _;
    let path = std::env::temp_dir().join("rustide-frontend.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
}

#[tauri::command]
pub fn get_tools() -> ToolInfo {
    ToolInfo {
        cargo: output::tool_status("cargo"),
        rust_analyzer: output::tool_status("rust-analyzer"),
    }
}

#[tauri::command]
pub fn set_root(app: AppHandle, state: State<AppState>, path: String) -> Result<(), String> {
    let path = PathBuf::from(&path);
    if !path.is_dir() {
        return Err(format!("not a directory: {path:?}"));
    }
    *state.root.lock().unwrap() = Some(path.clone());
    let term: State<crate::term::TermState> = app.state();
    *term.root.lock().unwrap() = Some(path.clone());
    state.docs.lock().unwrap().clear();
    // Drop any old language server and invalidate in-flight spawns; maybe_start_lsp
    // spawns a fresh one if this project is Rust.
    *state.lsp.lock().unwrap() = None;
    *state.lsp_status.lock().unwrap() = LspStatus::Off;
    state.lsp_generation.fetch_add(1, Ordering::SeqCst);
    maybe_start_lsp(&app, &path);
    let _ = app.emit("project-changed", path.to_string_lossy().into_owned());
    Ok(())
}

#[tauri::command]
pub fn list_dir(path: String) -> Result<Vec<Entry>, String> {
    let dir = Path::new(&path);
    let mut entries: Vec<Entry> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if p.is_dir() {
                !SKIPPED_DIRS.contains(&name)
            } else {
                !name.starts_with('.')
            }
        })
        .map(|p| Entry {
            name: p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("???")
                .to_string(),
            path: p.to_string_lossy().into_owned(),
            is_dir: p.is_dir(),
        })
        .collect();
    entries.sort_by(|a, b| (!b.is_dir).cmp(&(!a.is_dir)).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(entries)
}

#[tauri::command]
pub fn read_file(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|e| e.to_string())
}

/// All files under `root` (skipping ignored dirs), for quick-open.
#[tauri::command]
pub fn walk_tree(root: String) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::from(&root)];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let p = entry.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            if p.is_dir() {
                if !SKIPPED_DIRS.contains(&name.as_str()) && out.len() < 20_000 {
                    stack.push(p);
                }
            } else if !name.starts_with('.') && out.len() < 20_000 {
                out.push(p.to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out
}

#[tauri::command]
pub fn write_file(app: AppHandle, state: State<AppState>, path: String, content: String) -> Result<(), String> {
    std::fs::write(&path, &content).map_err(|e| e.to_string())?;
    // Keep the language server in sync if this doc is open.
    let uri = file_uri(Path::new(&path));
    let mut docs = state.docs.lock().unwrap();
    if let Some(doc) = docs.get_mut(&uri) {
        doc.text = content.clone();
        doc.version += 1;
        let (v, lang) = (doc.version, doc.language.clone());
        drop(docs);
        if lang == "rust" {
            if let Some(client) = state.lsp.lock().unwrap().clone() {
                if client.is_ready() {
                    client.notify(
                        "textDocument/didChange",
                        serde_json::json!({
                            "textDocument": { "uri": uri, "version": v },
                            "contentChanges": [{ "text": content }],
                        }),
                    );
                }
            }
        }
    }
    let _ = app;
    Ok(())
}

#[tauri::command]
pub fn create_entry(path: String, is_dir: bool) -> Result<(), String> {
    let p = Path::new(&path);
    if p.exists() {
        return Err("already exists".into());
    }
    if is_dir {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())
    } else {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(p, "").map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub fn delete_entry(path: String) -> Result<(), String> {
    let p = Path::new(&path);
    if p.is_dir() {
        std::fs::remove_dir_all(p).map_err(|e| e.to_string())
    } else {
        std::fs::remove_file(p).map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub fn rename_entry(from: String, to: String) -> Result<(), String> {
    std::fs::rename(&from, &to).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn lsp_did_open(state: State<AppState>, uri: String, text: String, language: Option<String>) {
    let lang = language.unwrap_or_else(|| "plaintext".into());
    state.docs.lock().unwrap().insert(
        uri.clone(),
        Doc { language: lang.clone(), text: text.clone(), version: 1 },
    );
    // rust-analyzer only understands Rust; other languages get no LSP (yet).
    if lang != "rust" {
        return;
    }
    notify_did_open(&state, &uri, &text);
}

/// Send didOpen for one stored Rust doc, if the server is ready.
fn notify_did_open(state: &State<AppState>, uri: &str, text: &str) {
    if let Some(client) = state.lsp.lock().unwrap().clone() {
        if client.is_ready() {
            client.notify(
                "textDocument/didOpen",
                serde_json::json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": "rust",
                        "version": 1,
                        "text": text,
                    }
                }),
            );
        }
    }
}

/// When rust-analyzer becomes ready, open every Rust doc collected before
/// the handshake finished (fixes "opened .rs while RA was starting").
pub fn flush_open_docs(state: &State<AppState>) {
    let docs: Vec<(String, String)> = state
        .docs
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, d)| d.language == "rust")
        .map(|(uri, d)| (uri.clone(), d.text.clone()))
        .collect();
    for (uri, text) in docs {
        notify_did_open(state, &uri, &text);
    }
}

#[tauri::command]
pub fn lsp_did_change(state: State<AppState>, uri: String, text: String) {
    let mut docs = state.docs.lock().unwrap();
    // Only docs we actually opened get LSP traffic; everything else is just
    // tracked locally (and ignored by rust-analyzer).
    let Some(doc) = docs.get_mut(&uri) else { return };
    doc.text = text.clone();
    doc.version += 1;
    let (v, lang) = (doc.version, doc.language.clone());
    drop(docs);
    if lang != "rust" {
        return;
    }
    if let Some(client) = state.lsp.lock().unwrap().clone() {
        if client.is_ready() {
            client.notify(
                "textDocument/didChange",
                serde_json::json!({
                    "textDocument": { "uri": uri, "version": v },
                    "contentChanges": [{ "text": text }],
                }),
            );
        }
    }
}

#[tauri::command]
pub fn lsp_did_close(state: State<AppState>, uri: String) {
    let doc = state.docs.lock().unwrap().remove(&uri);
    if let Some(doc) = doc {
        if doc.language == "rust" {
            if let Some(client) = state.lsp.lock().unwrap().clone() {
                client.notify(
                    "textDocument/didClose",
                    serde_json::json!({ "textDocument": { "uri": uri } }),
                );
            }
        }
    }
}

/// Completion via rust-analyzer. Async command; polls the response for up to
/// ~2s on a blocking thread. Returns raw LSP CompletionItem JSON for JS to map.
#[tauri::command]
pub async fn lsp_completion(
    state: State<'_, AppState>,
    uri: String,
    line: u32,
    character: u32,
) -> Result<Vec<serde_json::Value>, String> {
    let Some(client) = state.lsp.lock().unwrap().clone() else {
        return Ok(Vec::new());
    };
    if !client.is_ready() || !client.is_alive() {
        return Ok(Vec::new());
    }
    let id = client.request(
        "textDocument/completion",
        serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
        }),
    );

    let result = tauri::async_runtime::spawn_blocking(move || {
        for _ in 0..20 {
            if let Some(response) = client.take_response(id) {
                return response;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        serde_json::Value::Null
    })
    .await
    .map_err(|e| e.to_string())?;

    if result.is_null() {
        return Ok(Vec::new());
    }
    let items = if result.is_array() {
        result
    } else {
        result.get("items").cloned().unwrap_or_default()
    };
    Ok(items.as_array().cloned().unwrap_or_default())
}

/// What kind of project is open (drives the Build/Run buttons).
#[tauri::command]
pub fn detect_project(state: State<AppState>) -> Result<output::ProjectInfo, String> {
    let root = state
        .root
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "no folder open".to_string())?;
    Ok(output::detect_project(&root))
}

/// Build or run the detected project type. `run=false` means build.
#[tauri::command]
pub fn start_build(app: AppHandle, state: State<AppState>, run: bool) -> Result<(), String> {
    if state.build_running.swap(true, Ordering::SeqCst) {
        return Err("a task is already running".into());
    }
    let Some(root) = state.root.lock().unwrap().clone() else {
        state.build_running.store(false, Ordering::SeqCst);
        return Err("no folder open".into());
    };

    let info = output::detect_project(&root);
    let chosen = if run { info.run.clone() } else { info.build.clone() };
    let Some(cmd) = chosen else {
        state.build_running.store(false, Ordering::SeqCst);
        let msg = if run {
            format!(
                "Nothing to run in this folder — detected: {}. Looking for Cargo.toml, package.json, go.mod, or a Python entry (main.py / app.py / …).",
                info.label
            )
        } else {
            format!("Nothing to build for {} — use Run instead.", info.label)
        };
        let _ = app.emit("build-output", msg);
        let _ = app.emit("build-status", "idle");
        return Ok(());
    };

    if output::find_on_path(&cmd.program).is_none() {
        state.build_running.store(false, Ordering::SeqCst);
        let _ = app.emit(
            "build-output",
            format!(
                "`{}` not found on PATH — install it and restart the IDE.",
                cmd.program
            ),
        );
        let _ = app.emit("build-status", "failed");
        return Ok(());
    }

    let _ = app.emit(
        "build-output",
        format!("$ {} {}  (in {})", cmd.program, cmd.args.join(" "), root.display()),
    );
    let _ = app.emit("build-status", "running");

    let sink = Arc::new(Mutex::new(Vec::new()));
    let result_slot: Arc<Mutex<Option<anyhow::Result<i32>>>> = Arc::new(Mutex::new(None));

    {
        let root = root.clone();
        let sink = sink.clone();
        let result_slot = result_slot.clone();
        let cmd = cmd.clone();
        std::thread::spawn(move || {
            let r = output::run_command_blocking(&root, &cmd, sink);
            *result_slot.lock().unwrap() = Some(r);
        });
    }

    let app2 = app.clone();
    std::thread::spawn(move || {
        loop {
            let lines: Vec<String> = std::mem::take(&mut *sink.lock().unwrap());
            for line in lines {
                let _ = app2.emit("build-output", line);
            }
            let done = result_slot.lock().unwrap().is_some();
            if done {
                let result = result_slot.lock().unwrap().take().unwrap();
                match result {
                    Ok(0) => {
                        let _ = app2.emit("build-output", "process finished with exit code 0 — ok");
                        let _ = app2.emit("build-status", "success");
                    }
                    Ok(code) => {
                        let _ = app2.emit("build-output", format!("process finished with exit code {code}"));
                        let _ = app2.emit("build-status", "failed");
                    }
                    Err(err) => {
                        let _ = app2.emit("build-output", format!("failed to start: {err:#}"));
                        let _ = app2.emit("build-status", "failed");
                    }
                }
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        let state: State<AppState> = app2.state();
        state.build_running.store(false, Ordering::SeqCst);
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Git commands
// ---------------------------------------------------------------------------

fn with_root<F, T>(state: &State<AppState>, f: F) -> Result<T, String>
where
    F: FnOnce(&Path) -> Result<T, String>,
{
    let root = state
        .root
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "no folder open".to_string())?;
    f(&root)
}

#[tauri::command]
pub fn git_installed() -> bool {
    crate::git::is_installed()
}

#[tauri::command]
pub fn git_status(state: State<AppState>) -> Result<crate::git::GitStatus, String> {
    with_root(&state, |root| crate::git::status(root))
}

#[tauri::command]
pub fn git_stage(state: State<AppState>, paths: Vec<String>) -> Result<(), String> {
    with_root(&state, |root| crate::git::stage(root, &paths))
}

#[tauri::command]
pub fn git_stage_all(state: State<AppState>) -> Result<(), String> {
    with_root(&state, |root| crate::git::stage_all(root))
}

#[tauri::command]
pub fn git_unstage(state: State<AppState>, paths: Vec<String>) -> Result<(), String> {
    with_root(&state, |root| crate::git::unstage(root, &paths))
}

#[tauri::command]
pub fn git_commit(state: State<AppState>, message: String) -> Result<String, String> {
    with_root(&state, |root| crate::git::commit(root, &message))
}

#[tauri::command]
pub fn git_push(state: State<AppState>) -> Result<String, String> {
    with_root(&state, |root| crate::git::push(root))
}

#[tauri::command]
pub fn git_pull(state: State<AppState>) -> Result<String, String> {
    with_root(&state, |root| crate::git::pull(root))
}
