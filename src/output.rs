//! Build & output: detect build tools, run `cargo build`/`cargo run`,
//! streaming combined stdout+stderr into a shared sink that a poller task
//! forwards to the frontend as events.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolStatus {
    Checking,
    Found,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildStatus {
    Idle,
    Running,
    Success,
    Failed,
}

/// Check whether an executable exists on PATH (Windows-aware: tries .exe,
/// .cmd, .bat so npm/python-launcher style shims are found too).
pub fn find_on_path(executable: &str) -> Option<PathBuf> {
    let suffixes: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    let path_var = std::env::var("PATH").ok()?;
    for dir in std::env::split_paths(&path_var) {
        for suffix in suffixes {
            let candidate = dir.join(format!("{executable}{suffix}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// A concrete command to execute in the project root.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunCmd {
    pub program: String,
    pub args: Vec<String>,
}

/// What kind of project lives in the opened folder, and how to build/run it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectInfo {
    /// "rust" | "node" | "python" | "go" | "none"
    pub kind: String,
    pub label: String,
    pub can_build: bool,
    pub can_run: bool,
    pub build: Option<RunCmd>,
    pub run: Option<RunCmd>,
    /// The file a run command targets, if any (for the UI).
    pub entry: Option<String>,
}

fn find_entry_py(root: &Path) -> Option<String> {
    for candidate in ["main.py", "app.py", "run.py", "__main__.py"] {
        if root.join(candidate).is_file() {
            return Some(candidate.to_string());
        }
    }
    // Fall back to the first .py in the root.
    let Ok(entries) = std::fs::read_dir(root) else { return None };
    let mut py_files: Vec<String> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "py"))
        .map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned())
        .collect();
    py_files.sort();
    py_files.into_iter().next()
}

fn find_entry_js(root: &Path) -> Option<String> {
    for candidate in ["index.js", "main.js", "server.js", "app.js"] {
        if root.join(candidate).is_file() {
            return Some(candidate.to_string());
        }
    }
    None
}

/// Detect the project type from marker files / sources in `root`.
pub fn detect_project(root: &Path) -> ProjectInfo {
    // Rust: Cargo.toml
    if root.join("Cargo.toml").is_file() {
        let cargo_ok = find_on_path("cargo").is_some();
        return ProjectInfo {
            kind: "rust".into(),
            label: "Rust · cargo".into(),
            can_build: cargo_ok,
            can_run: cargo_ok,
            build: Some(RunCmd { program: "cargo".into(), args: vec!["build".into()] }),
            run: Some(RunCmd { program: "cargo".into(), args: vec!["run".into()] }),
            entry: None,
        };
    }
    // Go: go.mod
    if root.join("go.mod").is_file() {
        let go_ok = find_on_path("go").is_some();
        return ProjectInfo {
            kind: "go".into(),
            label: "Go".into(),
            can_build: go_ok,
            can_run: go_ok,
            build: Some(RunCmd { program: "go".into(), args: vec!["build".into(), ".".into()] }),
            run: Some(RunCmd { program: "go".into(), args: vec!["run".into(), ".".into()] }),
            entry: None,
        };
    }
    // Node: package.json
    if root.join("package.json").is_file() {
        let npm_ok = find_on_path("npm").is_some();
        let scripts_start = std::fs::read_to_string(root.join("package.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| {
                v.get("scripts")
                    .and_then(|s| s.get("start"))
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string())
            })
            .is_some();
        let entry = find_entry_js(root);
        let run = if npm_ok && scripts_start {
            Some(RunCmd { program: "npm".into(), args: vec!["start".into()] })
        } else if npm_ok && entry.is_some() {
            Some(RunCmd { program: "node".into(), args: vec![entry.clone().unwrap()] })
        } else {
            None
        };
        let build = if npm_ok
            && std::fs::read_to_string(root.join("package.json"))
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| v.get("scripts").and_then(|s| s.get("build")).cloned())
                .is_some()
        {
            Some(RunCmd { program: "npm".into(), args: vec!["run".into(), "build".into()] })
        } else {
            None
        };
        let can_run = run.is_some();
        return ProjectInfo {
            kind: "node".into(),
            label: "Node.js".into(),
            can_build: build.is_some(),
            can_run,
            build,
            run,
            entry,
        };
    }
    // Python: any .py in the root (or a requirements.txt)
    let entry = find_entry_py(root);
    let has_py = entry.is_some() || root.join("requirements.txt").is_file();
    if has_py {
        let python = find_on_path("python").or_else(|| find_on_path("py"));
        let run = match (&python, &entry) {
            (Some(_), Some(entry)) => Some(RunCmd {
                program: "python".into(),
                args: vec![entry.clone()],
            }),
            _ => None,
        };
        let can_run = run.is_some();
        return ProjectInfo {
            kind: "python".into(),
            label: "Python".into(),
            can_build: false,
            can_run,
            build: None,
            run,
            entry,
        };
    }
    ProjectInfo {
        kind: "none".into(),
        label: "no project".into(),
        can_build: false,
        can_run: false,
        build: None,
        run: None,
        entry: None,
    }
}

/// Spawn an arbitrary command in `root`, streaming stdout+stderr lines into
/// `sink`. Blocking; call from the background executor. Returns the exit code.
pub fn run_command_blocking(
    root: &Path,
    cmd: &RunCmd,
    sink: Arc<Mutex<Vec<String>>>,
) -> anyhow::Result<i32> {
    let program = find_on_path(&cmd.program).unwrap_or_else(|| PathBuf::from(&cmd.program));
    let mut child: Child = Command::new(program)
        .args(&cmd.args)
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let stdout: Box<dyn std::io::Read + Send> =
        Box::new(child.stdout.take().expect("no stdout"));
    let stderr: Box<dyn std::io::Read + Send> =
        Box::new(child.stderr.take().expect("no stderr"));
    for stream in [stdout, stderr] {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);

    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(150)) {
            Ok(line) => sink.lock().unwrap().push(line),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if child.try_wait()?.is_some() {
                    while let Ok(line) = rx.try_recv() {
                        sink.lock().unwrap().push(line);
                    }
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let code = child.wait()?.code().unwrap_or(-1);
    Ok(code)
}

/// Detect a tool's presence as a serializable status.
pub fn tool_status(executable: &str) -> ToolStatus {
    if find_on_path(executable).is_some() {
        ToolStatus::Found
    } else {
        ToolStatus::Missing
    }
}

/// Spawn `cargo build` (or `run`), streaming stdout+stderr lines into `sink`.
/// Blocking; call from the background executor. Returns the exit code.
pub fn run_cargo_blocking(
    root: &Path,
    run: bool,
    sink: Arc<Mutex<Vec<String>>>,
) -> anyhow::Result<i32> {
    let cargo = find_on_path("cargo").unwrap_or_else(|| "cargo".into());
    let mut child: Child = Command::new(cargo)
        .arg(if run { "run" } else { "build" })
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let stdout: Box<dyn std::io::Read + Send> =
        Box::new(child.stdout.take().expect("no stdout"));
    let stderr: Box<dyn std::io::Read + Send> =
        Box::new(child.stderr.take().expect("no stderr"));
    for stream in [stdout, stderr] {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);

    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(150)) {
            Ok(line) => sink.lock().unwrap().push(line),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if child.try_wait()?.is_some() {
                    while let Ok(line) = rx.try_recv() {
                        sink.lock().unwrap().push(line);
                    }
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let code = child.wait()?.code().unwrap_or(-1);
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_detection_does_not_panic() {
        let _ = tool_status("cargo");
        let _ = find_on_path("definitely-not-a-tool-xyz");
    }

    #[test]
    fn detects_python_project() {
        let dir = std::env::temp_dir().join(format!("oxide-detect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.py"), "print('hi')").unwrap();
        let info = detect_project(&dir);
        assert_eq!(info.kind, "python");
        assert_eq!(info.entry.as_deref(), Some("main.py"));
        assert!(!info.can_build);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn detects_empty_folder_as_none() {
        let dir = std::env::temp_dir().join(format!("oxide-detect-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let info = detect_project(&dir);
        assert_eq!(info.kind, "none");
        assert!(!info.can_run);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
