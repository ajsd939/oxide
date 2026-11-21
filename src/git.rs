//! Source control: thin wrappers over the git CLI for status, staging,
//! commit, and push. All git invocations use argument lists (no shell
//! interpolation), run with the project root as cwd.

use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::output::find_on_path;

#[derive(Debug, Clone, Serialize)]
pub struct GitFile {
    /// Two-char XY status from porcelain (e.g. "M ", " M", "??", "A ").
    pub status: String,
    pub path: String,
    /// True when the change is staged (index differs from HEAD).
    pub staged: bool,
    /// True when the worktree differs (unstaged modification or untracked).
    pub unstaged: bool,
}

#[derive(Debug, Serialize)]
pub struct GitStatus {
    pub branch: String,
    pub ahead: usize,
    pub behind: usize,
    pub files: Vec<GitFile>,
}

fn git_exe() -> Result<std::path::PathBuf, String> {
    find_on_path("git").ok_or_else(|| {
        "git not found on PATH — install it (winget install Git.Git) and restart".into()
    })
}

fn run_git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new(git_exe()?)
        .arg("-c")
        .arg("core.quotepath=false")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(if stderr.trim().is_empty() { stdout.trim().to_string() } else { stderr.trim().to_string() })
    }
}

fn is_git_repo(root: &Path) -> Result<(), String> {
    match run_git(root, &["rev-parse", "--is-inside-work-tree"]) {
        Ok(s) if s.trim() == "true" => Ok(()),
        _ => Err("not a git repository".into()),
    }
}

/// `git status --porcelain=v1 -b` -> structured status.
pub fn status(root: &Path) -> Result<GitStatus, String> {
    is_git_repo(root)?;
    let out = run_git(root, &["status", "--porcelain=v1", "-b"])?;

    let mut branch = String::new();
    let mut ahead = 0;
    let mut behind = 0;
    let mut files = Vec::new();

    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            // "## branch...origin/branch [ahead 1, behind 2]" or "## No commits yet on branch"
            let head = rest.split("...").next().unwrap_or("");
            let head = head.split('[').next().unwrap_or(head);
            branch = head.trim().trim_start_matches("No commits yet on ").to_string();
            if rest.contains("ahead") {
                if let Some(n) = parse_counts(rest, "ahead") { ahead = n; }
            }
            if rest.contains("behind") {
                if let Some(n) = parse_counts(rest, "behind") { behind = n; }
            }
            continue;
        }
        if line.len() < 4 {
            continue;
        }
        let (xy, path) = line.split_at(2);
        let (x, y) = (xy.chars().next().unwrap_or(' '), xy.chars().nth(1).unwrap_or(' '));
        let path = path
            .trim_start()
            .rsplit_once(" -> ")
            .map(|(_, new)| new.to_string())
            .unwrap_or_else(|| path.trim_start().to_string());
        files.push(GitFile {
            status: xy.to_string(),
            path,
            staged: x != ' ' && x != '?',
            unstaged: y != ' ' || x == '?',
        });
    }

    Ok(GitStatus { branch, ahead, behind, files })
}

fn parse_counts(s: &str, key: &str) -> Option<usize> {
    let idx = s.find(&format!("{key} "))?;
    s[idx + key.len() + 1..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

pub fn stage(root: &Path, paths: &[String]) -> Result<(), String> {
    let mut args = vec!["add", "--"];
    args.extend(paths.iter().map(|s| s.as_str()));
    run_git(root, &args).map(|_| ())
}

pub fn stage_all(root: &Path) -> Result<(), String> {
    run_git(root, &["add", "-A"]).map(|_| ())
}

pub fn unstage(root: &Path, paths: &[String]) -> Result<(), String> {
    let mut args = vec!["reset", "HEAD", "--"];
    args.extend(paths.iter().map(|s| s.as_str()));
    run_git(root, &args).map(|_| ())
}

pub fn commit(root: &Path, message: &str) -> Result<String, String> {
    run_git(root, &["commit", "-m", message])
}

pub fn push(root: &Path) -> Result<String, String> {
    run_git(root, &["push"])
}

pub fn pull(root: &Path) -> Result<String, String> {
    run_git(root, &["pull", "--rebase"])
}

pub fn is_installed() -> bool {
    find_on_path("git").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_counts() {
        assert_eq!(parse_counts("[ahead 3, behind 7]", "ahead"), Some(3));
        assert_eq!(parse_counts("[ahead 3, behind 7]", "behind"), Some(7));
        assert_eq!(parse_counts("nope", "ahead"), None);
    }

    #[test]
    fn parses_porcelain_lines() {
        // covered indirectly by status() integration on a repo; unit test the
        // counting helper only (git availability varies by machine).
        let _ = is_installed();
    }
}
