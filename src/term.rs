//! Integrated terminal: a real PTY (ConPTY on Windows) running the OS shell,
//! with input/output bridged to the xterm.js frontend via Tauri events.

use std::sync::Mutex;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

/// Pick the best shell for this OS.
fn pick_shell() -> (std::path::PathBuf, Vec<&'static str>) {
    if cfg!(windows) {
        if let Some(pwsh) = crate::output::find_on_path("pwsh") {
            return (pwsh, vec!["-NoLogo"]);
        }
        if let Some(ps) = crate::output::find_on_path("powershell") {
            return (ps, vec!["-NoLogo"]);
        }
        return ("cmd.exe".into(), vec![]);
    }
    // Unix: prefer $SHELL, then bash, then sh.
    if let Ok(shell) = std::env::var("SHELL") {
        if !shell.is_empty() {
            return (shell.into(), vec!["-i"]);
        }
    }
    if let Some(bash) = crate::output::find_on_path("bash") {
        return (bash, vec!["-i"]);
    }
    ("/bin/sh".into(), vec!["-i"])
}

pub struct TermSession {
    writer: Box<dyn std::io::Write + Send>,
    master: Box<dyn MasterPty + Send>,
}

#[derive(Default)]
pub struct TermState {
    pub session: Mutex<Option<TermSession>>,
    pub root: Mutex<Option<std::path::PathBuf>>,
}

#[derive(Serialize, Clone)]
pub struct TermExit {
    pub code: Option<i32>,
}

/// (Re)spawn the shell PTY with cwd = project root.
#[tauri::command]
pub fn term_spawn(app: AppHandle, state: State<'_, TermState>) -> Result<(), String> {
    // Kill any previous session (dropping the master closes the pty).
    *state.session.lock().unwrap() = None;

    let root = state.root.lock().unwrap().clone();
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
        .map_err(|e| e.to_string())?;

    let (shell_path, extra_args) = pick_shell();
    let mut cmd = CommandBuilder::new(shell_path);
    for arg in extra_args {
        cmd.arg(arg);
    }
    if let Some(root) = &root {
        cmd.cwd(root);
    }
    cmd.env("TERM", "xterm-256color");

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("failed to spawn shell: {e}"))?;
    drop(pair.slave); // important: we hold the master only

    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;

    // Reader thread: pty -> frontend.
    let app_reader = app.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let chunk = String::from_utf8_lossy(&buf[..n]).into_owned();
                    if app_reader.emit("term-data", chunk).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = app_reader.emit("term-exit", TermExit { code: None });
    });

    // Watcher thread: report exit code when the shell quits.
    let app_watcher = app.clone();
    std::thread::spawn(move || {
        let code = child.wait().ok().map(|status| status.exit_code() as i32);
        let _ = app_watcher.emit("term-exit", TermExit { code });
    });

    // Keep master so resize works; store writer for input.
    *state.session.lock().unwrap() = Some(TermSession {
        writer,
        master: pair.master,
    });
    Ok(())
}

#[tauri::command]
pub fn term_write(state: State<'_, TermState>, data: String) -> Result<(), String> {
    let mut guard = state.session.lock().unwrap();
    if let Some(session) = guard.as_mut() {
        session
            .writer
            .write_all(data.as_bytes())
            .and_then(|_| session.writer.flush())
            .map_err(|e| e.to_string())
    } else {
        Err("no terminal".into())
    }
}

#[tauri::command]
pub fn term_resize(state: State<'_, TermState>, cols: u16, rows: u16) -> Result<(), String> {
    let guard = state.session.lock().unwrap();
    if let Some(session) = guard.as_ref() {
        session
            .master
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| e.to_string())
    } else {
        Ok(())
    }
}

#[tauri::command]
pub fn term_kill(state: State<'_, TermState>) {
    *state.session.lock().unwrap() = None;
}

/// Inform the terminal of the current project root (used on spawn).
#[tauri::command]
pub fn term_set_root(state: State<'_, TermState>, root: Option<String>) {
    *state.root.lock().unwrap() = root.map(std::path::PathBuf::from);
}

use std::io::Read as _;
