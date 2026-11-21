# OxIDE

A VS Code-like IDE written in Rust. Backend: **Tauri 2** (Rust + WebView2).
Frontend: vanilla HTML/CSS/JS with the **Monaco editor** — the same editor core
VS Code uses — served as static assets (no npm/bundler required).

## Features

- **Explorer** — native folder picker (Ctrl+O / project pill / Open Folder
  buttons), lazy-loading file tree, right-click context menu (New File,
  New Folder, Rename, Delete), inline creation inputs.
- **Editor** — Monaco: selection, multi-cursor, undo/redo, find/replace,
  bracket colorization, smooth caret/scrolling, minimap. Full-text autosave
  to disk with Ctrl+S; dirty dots on tabs; close buttons.
- **Intellisense** — real `rust-analyzer` over a minimal hand-rolled JSON-RPC
  client (stdio threads, `src/lsp.rs`). Completion popup via Monaco's provider
  API; publishDiagnostics become editor squiggles + status-bar counts.
- **Integrated terminal** — a real PTY (ConPTY on Windows / ioctl pty on Unix)
  running the OS shell (PowerShell → cmd fallback on Windows, `$SHELL`/bash on
  Unix), docked at the bottom of the window with xterm.js rendering. Drag the
  divider to resize (persisted), Ctrl+` toggles, restart button for a fresh
  shell. `src/term.rs` + `portable-pty`.
- **Source control** — git status (branch, ahead/behind, staged/changes lists),
  stage/unstage per file or all, commit (Ctrl+Enter), pull, push — via the git
  CLI with argument lists, no shell interpolation (`src/git.rs`). Opens from
  the git icon in the activity bar (Ctrl+Shift+G); auto-refreshes every 3 s.
- **Build & run** — cargo auto-detected on PATH; `cargo build` (Ctrl+Shift+B)
  and `cargo run` (Ctrl+Shift+R) stream output into the OUTPUT tab with status
  chips (running / ok / failed).
- **Quick open** — Ctrl+P fuzzy file finder backed by a Rust tree walk.
- **Themes** — One Dark, One Light, Solarized Dark; Monaco theme definitions +
  a matching CSS-variable shell; Ctrl+K cycles.
- **Tool detection** — cargo / rust-analyzer presence shown in the status bar.

## Keybindings

| Keys | Action |
|---|---|
| Ctrl+S / Ctrl+P / Ctrl+O / Ctrl+W | Save / quick open / open folder / close tab |
| Ctrl+Shift+B / Ctrl+Shift+R | Build / Run |
| Ctrl+` | Toggle bottom dock (terminal / output) |
| Ctrl+Shift+G | Toggle Source Control view |
| Ctrl+K | Cycle theme |
| Monaco defaults | editing, search, multi-cursor, etc. |

## Layout

```
tauri-ide/
  src/            Rust backend
    main.rs       tauri Builder + command registration
    commands.rs   project IO, LSP bridge, builds, git commands, pollers
    lsp.rs        minimal JSON-RPC LSP client (rust-analyzer over stdio)
    term.rs       integrated terminal: PTY (ConPTY) session bridged to xterm.js
    git.rs        git CLI wrappers: status parse, stage, commit, push, pull
    completion.rs completion item model
    output.rs     cargo runner with streamed output sink
  ui/             frontend (served statically by tauri)
    index.html    shell: activity bar, explorer, source control, tabs, dock
    style.css     design system: 3 themes, rounded panels, transitions
    app.js        tree, tabs, Monaco wiring, terminal, git panel, quick-open
    monaco/       vendored monaco-editor 0.52.2 (min build)
    xterm/        vendored @xterm/xterm 5.5.0 + addon-fit
  tauri.conf.json   frontendDist: "ui", withGlobalTauri (no bundler)
  capabilities/     tauri permission set (core + events + dialog)
```

## Run

```cmd
cargo run -- path\to\project
```

Requires: Rust MSVC toolchain, WebView2 runtime (preinstalled on updated
Windows 10), and rust-analyzer on PATH (`rustup component add rust-analyzer`)
for intellisense; cargo for builds.

## Packaging

The vendored frontend assets (Monaco, xterm.js) are gitignored to keep the
repo light — fetch them once after cloning:

```cmd
powershell -ExecutionPolicy Bypass -File scripts\fetch-assets.ps1   # Windows
bash scripts/fetch-assets.sh                                        # Linux/macOS
```

Then build installers:

```cmd
cargo install tauri-cli --version "^2" --locked
cargo tauri build --bundles nsis     :: -> target\release\bundle\nsis\OxIDE_0.3.0_x64-setup.exe
```

On Linux, build the release binary and tar it (needs `libwebkit2gtk-4.1-dev
libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev`):

```bash
cargo build --release && tar -czf oxide-linux-x64.tar.gz -C target/release oxide
```

Pushing to `main` (or the Actions tab, manually) also builds both on GitHub
Actions — artifacts: `oxide-windows-nsis` and `oxide-linux-targz`.

Publishing repo updates works without git.exe via the Git Data API:

```cmd
powershell -ExecutionPolicy Bypass -File scripts\publish-github.ps1 -Message "update"
```

## Notes

- The LSP client polls responses (100 ms ticks, 2 s budget) instead of using
  an async LSP framework — small, debuggable, and plenty for completions.
- Editor text syncs to rust-analyzer as full-text `didChange` (debounced 350 ms).
- Diagnostics map 1:1 into Monaco markers (LSP and Monaco both use 0-based
  lines + UTF-16 columns).
