/* OxIDE frontend: explorer tree, tabs, Monaco wiring, LSP bridge,
   builds, quick-open, themes. Talks to the Rust backend via Tauri IPC. */

// Bridge — guarded so a missing injection shows a clear message instead of
// silently killing every handler.
const Tauri = window.__TAURI__;
const invoke = Tauri?.core?.invoke?.bind(Tauri.core);
const listen = Tauri?.event?.listen?.bind(Tauri.event);

// Self-reporting: everything notable (and every error) lands in
// %TEMP%\oxide-frontend.log so failures are visible without devtools.
function logFrontend(msg) {
  if (typeof invoke !== "function") return;
  try {
    invoke("frontend_log", { line: `[${new Date().toISOString()}] ${msg}` });
  } catch { /* logging must never break the app */ }
}

window.addEventListener("error", (e) => {
  logFrontend(`ERROR: ${e.message} @ ${e.filename}:${e.lineno}:${e.colno}`);
  showFatal(`${e.message} (${e.filename?.split("/").pop()}:${e.lineno})`);
});
window.addEventListener("unhandledrejection", (e) => {
  logFrontend(`REJECTION: ${e.reason?.message || e.reason}`);
});

function showFatal(msg) {
  let banner = document.getElementById("fatal-banner");
  if (!banner) {
    banner = document.createElement("div");
    banner.id = "fatal-banner";
    banner.style.cssText =
      "position:fixed;bottom:34px;left:50%;transform:translateX(-50%);" +
      "background:#e06c75;color:#fff;padding:8px 14px;border-radius:8px;" +
      "font:12px/1.4 'Segoe UI',sans-serif;z-index:9999;max-width:80%;" +
      "box-shadow:0 6px 24px rgba(0,0,0,.4);white-space:pre-wrap";
    document.body.appendChild(banner);
  }
  banner.textContent = msg;
  logFrontend("FATAL: " + msg);
}

// ---------------------------------------------------------------- helpers

const $ = (id) => document.getElementById(id);

function toFileUrl(path) {
  const p = path.replace(/\\/g, "/");
  let out = "";
  for (const ch of p) {
    const code = ch.charCodeAt(0);
    if (/^[a-zA-Z0-9/.\-_~!$&'()*+,;=:@]$/.test(ch)) out += ch;
    else out += "%" + code.toString(16).toUpperCase().padStart(2, "0");
  }
  return "file:///" + out;
}

function fileUrl(path) {
  return toFileUrl(path);
}

const LANGS = {
  rs: "rust", js: "javascript", ts: "typescript", jsx: "javascript",
  tsx: "typescript", html: "html", css: "css", json: "json",
  md: "markdown", py: "python", c: "c", cpp: "cpp", h: "cpp",
  go: "go", java: "java", sh: "shell", yml: "yaml", yaml: "yaml",
  xml: "xml", sql: "sql", toml: "toml", ini: "ini", cfg: "ini",
  bat: "bat", cmd: "bat", ps1: "powershell", lua: "lua", rb: "ruby",
  php: "php", kt: "kotlin", scala: "scala", swift: "swift",
  graphql: "graphql", proto: "protobuf", txt: "plaintext", log: "plaintext",
};

const LANG_NAMES = {
  rust: "Rust", javascript: "JavaScript", typescript: "TypeScript",
  html: "HTML", css: "CSS", json: "JSON", markdown: "Markdown",
  python: "Python", c: "C", cpp: "C++", go: "Go", java: "Java",
  shell: "Shell", yaml: "YAML", xml: "XML", sql: "SQL", toml: "TOML",
  ini: "INI", bat: "Batch", powershell: "PowerShell", lua: "Lua",
  ruby: "Ruby", php: "PHP", kotlin: "Kotlin", scala: "Scala",
  swift: "Swift", graphql: "GraphQL", protobuf: "Protobuf",
  plaintext: "Plain Text",
};

function langFor(path) {
  const ext = path.split(".").pop().toLowerCase();
  return LANGS[ext] || "plaintext";
}

function langName(id) {
  return LANG_NAMES[id] || id.charAt(0).toUpperCase() + id.slice(1);
}

function baseName(p) { return p.split(/[\\/]/).pop(); }
function joinPath(dir, name) {
  const sep = dir.includes("\\") ? "\\" : "/";
  return dir.endsWith(sep) ? dir + name : dir + sep + name;
}

// ---------------------------------------------------------------- state

const state = {
  root: null,
  tabs: [],            // { path, name, model, savedVersionId, dirty }
  activePath: null,
  markers: new Map(),  // uri -> monaco markers
  expanded: new Set(), // dir paths currently expanded in the tree
  themeIndex: 0,
  buildRunning: false,
  lspStatus: "off",
  selectedTreePath: null,
  pendingNew: null,    // { parent, isDir } — inline create input
};

const THEMES = [
  { id: "one-dark", label: "One Dark", body: "dark" },
  { id: "one-light", label: "One Light", body: "light" },
  { id: "solarized-dark", label: "Solarized Dark", body: "solarized" },
];

// ---------------------------------------------------------------- boot

async function boot() {
  logFrontend("boot start");
  if (typeof invoke !== "function" || typeof listen !== "function") {
    showFatal("Tauri bridge missing — frontend cannot reach the backend.");
    return;
  }
  logFrontend("tauri bridge ok");

  // 1) Wire UI handlers FIRST — the app stays usable even if Monaco fails.
  wireEvents();
  logFrontend("ui events wired");

  // 2) Backend event streams.
  await listen("lsp-status", (e) => setLspStatus(e.payload));
  await listen("diagnostics", (e) => onDiagnostics(e.payload));
  await listen("build-output", (e) => appendOutput(e.payload));
  await listen("build-status", (e) => setBuildStatus(e.payload));
  await listen("term-data", (e) => { if (term) term.write(e.payload); });
  await listen("term-exit", (e) => onTermExit(e.payload));
  await listen("project-changed", async (e) => {
    state.root = e.payload;
    state.expanded.clear();
    $("project-label").textContent = baseName(e.payload);
    await renderTree();
    state.quickFiles = (await invokeSafe("walk_tree", { root: e.payload })) || [];
    await refreshProjectInfo();
    // Retarget the terminal + git panel at the new project.
    await invokeSafe("term_set_root", { root: e.payload });
    if (term) { invoke("term_kill"); term.reset(); invoke("term_spawn"); }
    if (!gitViewHidden()) renderGit();
  });
  logFrontend("backend listeners attached");

  // 3) Monaco — deferred so a loader failure can't deaden the shell.
  try {
    await withTimeout(defineMonacoThemes(), 10000, "monaco load timeout");
    initMonaco();
    logFrontend("monaco ready");
  } catch (err) {
    logFrontend("monaco failed: " + (err?.message || err));
    showFatal("Editor failed to load: " + (err?.message || err));
  }

  // 4) Project state.
  try {
    state.root = await invoke("get_root");
  } catch { state.root = null; }
  try {
    const tools = await invoke("get_tools");
    $("status-cargo").textContent = tools.cargo === "found" ? "cargo ✓" : "cargo ✗";
  } catch { /* ignore */ }

  if (state.root) {
    $("project-label").textContent = baseName(state.root);
    state.quickFiles = (await invokeSafe("walk_tree", { root: state.root })) || [];
  }
  await renderTree();
  await refreshProjectInfo();

  // Terminal dock: visible by default at the bottom, sized/persisted by the
  // user via the drag divider. Spawned now so the shell runs in the project.
  $("panel").style.height =
    (parseInt(localStorage.getItem("oxide-panel-h") || "300", 10) || 300) + "px";
  setPanelTab("terminal");
  wireResizer();

  applyTheme();
  logFrontend("boot complete; root=" + state.root);
}

function wireResizer() {
  const resizer = $("panel-resizer");
  const panel = $("panel");
  let dragging = false;
  resizer.addEventListener("mousedown", (e) => {
    dragging = true;
    e.preventDefault();
    resizer.classList.add("dragging");
  });
  window.addEventListener("mousemove", (e) => {
    if (!dragging) return;
    const rect = $("main").getBoundingClientRect();
    const h = Math.min(Math.max(rect.bottom - e.clientY, 120), Math.floor(rect.height * 0.7));
    panel.style.height = h + "px";
    localStorage.setItem("oxide-panel-h", String(h));
    try { termFit?.fit(); } catch { /* terminal not open yet */ }
  });
  window.addEventListener("mouseup", () => {
    if (!dragging) return;
    dragging = false;
    resizer.classList.remove("dragging");
    try { termFit?.fit(); } catch { /* noop */ }
  });
}

function withTimeout(promise, ms, what) {
  return Promise.race([
    promise,
    new Promise((_, reject) => setTimeout(() => reject(new Error(what)), ms)),
  ]);
}

async function invokeSafe(cmd, args) {
  try { return await invoke(cmd, args); } catch (err) {
    logFrontend(`invoke ${cmd} failed: ${err}`);
    return null;
  }
}

function setLspStatus(status) {
  if (!status) return;
  state.lspStatus = status;
  const el = $("status-lsp");
  el.textContent = "rust-analyzer: " + status;
  el.className = "status-item " + status;
}

// ---------------------------------------------------------------- themes

function defineMonacoThemes() {
  return new Promise((resolve, reject) => {
    if (typeof require === "undefined") {
      reject(new Error("monaco loader.js missing (check ui/monaco assets)"));
      return;
    }
    require.config({ paths: { vs: "monaco/vs" } });
    require(
      ["vs/editor/editor.main"],
      () => {
      monaco.editor.defineTheme("one-dark", {
        base: "vs-dark",
        inherit: true,
        rules: [
          { token: "keyword", foreground: "c678dd" },
          { token: "string", foreground: "98c379" },
          { token: "number", foreground: "d19a66" },
          { token: "comment", foreground: "5c6370", fontStyle: "italic" },
          { token: "type", foreground: "e5c07b" },
          { token: "type.identifier", foreground: "e5c07b" },
          { token: "function", foreground: "61afef" },
          { token: "identifier.function", foreground: "61afef" },
          { token: "variable", foreground: "e06c75" },
          { token: "variable.predefined", foreground: "e06c75" },
          { token: "constant", foreground: "d19a66" },
          { token: "operator", foreground: "56b6c2" },
          { token: "delimiter", foreground: "abb2bf" },
          { token: "tag", foreground: "e06c75" },
          { token: "attribute.name", foreground: "d19a66" },
        ],
        colors: {
          "editor.background": "#282c34",
          "editor.lineHighlightBackground": "#2c313c",
          "editorLineNumber.foreground": "#495162",
          "editorLineNumber.activeForeground": "#c8ccd4",
          "editorCursor.foreground": "#61afef",
          "editor.selectionBackground": "#3e4451",
          "editorIndentGuide.background1": "#3b4048",
        },
      });
      monaco.editor.defineTheme("one-light", {
        base: "vs",
        inherit: true,
        rules: [
          { token: "keyword", foreground: "a626a4" },
          { token: "string", foreground: "50a14f" },
          { token: "number", foreground: "b76b01" },
          { token: "comment", foreground: "a0a1a7", fontStyle: "italic" },
          { token: "type", foreground: "c18401" },
          { token: "type.identifier", foreground: "c18401" },
          { token: "function", foreground: "4078f2" },
          { token: "variable", foreground: "e45649" },
          { token: "constant", foreground: "986801" },
          { token: "operator", foreground: "0184bc" },
          { token: "delimiter", foreground: "383a42" },
        ],
        colors: {
          "editor.background": "#fafafa",
          "editor.lineHighlightBackground": "#eeeef2",
          "editorLineNumber.foreground": "#9d9ea6",
          "editorLineNumber.activeForeground": "#383a42",
          "editorCursor.foreground": "#526fff",
          "editor.selectionBackground": "#bfd7ff",
          "editorIndentGuide.background1": "#dcdfe4",
        },
      });
      monaco.editor.defineTheme("solarized-dark", {
        base: "vs-dark",
        inherit: true,
        rules: [
          { token: "keyword", foreground: "859900" },
          { token: "string", foreground: "2aa198" },
          { token: "number", foreground: "d33682" },
          { token: "comment", foreground: "586e75", fontStyle: "italic" },
          { token: "type", foreground: "b58900" },
          { token: "function", foreground: "268bd2" },
          { token: "variable", foreground: "268bd2" },
          { token: "constant", foreground: "d33682" },
          { token: "operator", foreground: "859900" },
          { token: "delimiter", foreground: "93a1a1" },
        ],
        colors: {
          "editor.background": "#00343f",
          "editor.lineHighlightBackground": "#073642",
          "editorLineNumber.foreground": "#4a6570",
          "editorLineNumber.activeForeground": "#b3c4c4",
          "editorCursor.foreground": "#268bd2",
          "editor.selectionBackground": "#073642",
          "editorIndentGuide.background1": "#0a4553",
        },
      });
      resolve();
      },
      (err) => reject(new Error("monaco require failed: " + JSON.stringify(err))),
    );
  });
}

function cycleTheme() {
  state.themeIndex = (state.themeIndex + 1) % THEMES.length;
  applyTheme();
}

function applyTheme() {
  const theme = THEMES[state.themeIndex];
  document.body.dataset.theme = theme.body;
  if (typeof monaco !== "undefined" && monaco.editor) {
    monaco.editor.setTheme(theme.id);
  }
  if (term) term.options.theme = xtermTheme();
  localStorage.setItem("oxide-theme", String(state.themeIndex));
  $("status-theme").textContent = theme.label;
}

// Restore the last-used theme before the first applyTheme runs.
(function () {
  const saved = parseInt(localStorage.getItem("oxide-theme") || "0", 10);
  state.themeIndex = Number.isInteger(saved) && saved >= 0 && saved < THEMES.length ? saved : 0;
})();

function xtermTheme() {
  const themes = {
    dark: {
      background: "#1e222a", foreground: "#c5cdd7", cursor: "#61afef",
      selectionBackground: "#3e4451", black: "#282c34", green: "#98c379",
      yellow: "#d19a66", red: "#e06c75", blue: "#61afef", cyan: "#56b6c2",
    },
    light: {
      background: "#fafafa", foreground: "#383a42", cursor: "#4078f2",
      selectionBackground: "#bfd7ff", black: "#383a42", green: "#50a14f",
      yellow: "#b76b01", red: "#e45649", blue: "#4078f2", cyan: "#0184bc",
    },
    solarized: {
      background: "#002b36", foreground: "#93a1a1", cursor: "#268bd2",
      selectionBackground: "#073642", black: "#002b36", green: "#859900",
      yellow: "#b58900", red: "#d33682", blue: "#268bd2", cyan: "#2aa198",
    },
  };
  return themes[document.body.dataset.theme] || themes.dark;
}

// ---------------------------------------------------------------- terminal

let term = null;
let termFit = null;
let termAlive = false;
let activePanel = "output";

function showPanel() {
  $("panel").classList.remove("hidden");
  setPanelTab(activePanel);
}

function togglePanel() {
  const panel = $("panel");
  panel.classList.toggle("hidden");
  if (!panel.classList.contains("hidden")) {
    setPanelTab(activePanel);
  }
}

function setPanelTab(which) {
  activePanel = which;
  $("ptab-output").classList.toggle("active", which === "output");
  $("ptab-terminal").classList.toggle("active", which === "terminal");
  $("panel-lines").classList.toggle("hidden", which !== "output");
  $("terminal-host").classList.toggle("hidden", which !== "terminal");
  $("btn-term-restart").classList.toggle("hidden", which !== "terminal");
  $("btn-clear-output").classList.toggle("hidden", which !== "output");
  if (which === "terminal") {
    ensureTerminal();
    setTimeout(() => { termFit?.fit(); term?.focus(); }, 30);
  } else {
    $("panel-lines").scrollTop = $("panel-lines").scrollHeight;
  }
}

function ensureTerminal() {
  if (term) {
    if (!termAlive) restartTerminal();
    return;
  }
  if (typeof Terminal === "undefined") {
    showFatal("xterm.js failed to load");
    return;
  }
  term = new Terminal({
    fontFamily: '"Cascadia Mono", "Consolas", monospace',
    fontSize: 12.5,
    lineHeight: 1.2,
    cursorBlink: true,
    theme: xtermTheme(),
    scrollback: 4000,
    allowProposedApi: true,
  });
  termFit = new FitAddon.FitAddon();
  term.loadAddon(termFit);
  term.open($("terminal-host"));
  termFit.fit();
  term.onData((data) => invoke("term_write", { data }).catch(() => {}));
  term.onResize(({ cols, rows }) => invoke("term_resize", { cols, rows }).catch(() => {}));
  new ResizeObserver(() => {
    if (!$("terminal-host").classList.contains("hidden")) {
      try { termFit.fit(); } catch { /* panel hidden mid-resize */ }
    }
  }).observe($("terminal-host"));
  termAlive = true;
  invoke("term_spawn").catch((err) => {
    logFrontend("term_spawn failed: " + err);
    term.write("\r\n\x1b[31mfailed to start shell: " + err + "\x1b[0m\r\n");
  });
  logFrontend("terminal created");
}

function restartTerminal() {
  if (!term) { ensureTerminal(); return; }
  invoke("term_kill");
  term.reset();
  termAlive = true;
  invoke("term_spawn").catch((err) => term.write("\r\n\x1b[31m" + err + "\x1b[0m\r\n"));
}

function onTermExit(payload) {
  termAlive = false;
  if (term) {
    term.write(`\r\n\x1b[2m[shell exited${payload?.code != null ? ` with code ${payload.code}` : ""} — restart button above]\x1b[0m\r\n`);
  }
}

// ---------------------------------------------------------------- source control

function gitViewHidden() {
  return $("git-view").classList.contains("hidden");
}

function showView(which) {
  $("explorer-view").classList.toggle("hidden", which !== "explorer");
  $("git-view").classList.toggle("hidden", which !== "git");
  $("act-explorer").classList.toggle("active", which === "explorer");
  $("act-git").classList.toggle("active", which === "git");
  if (which === "git") renderGit();
}

async function renderGit() {
  const notice = $("git-notice");
  if (!state.root) {
    notice.classList.remove("hidden");
    notice.textContent = "Open a folder to see source control.";
    return;
  }
  const status = await invokeSafe("git_status");
  if (!status) {
    notice.classList.remove("hidden");
    notice.textContent = "This folder is not a git repository. Initialize one from the terminal with `git init`, or open a repository folder.";
    $("git-staged").innerHTML = "";
    $("git-changes").innerHTML = "";
    $("git-branch-name").textContent = "—";
    return;
  }
  notice.classList.add("hidden");
  $("git-branch-name").textContent = status.branch || "(no branch)";
  const ab = [];
  if (status.ahead) ab.push("↑" + status.ahead);
  if (status.behind) ab.push("↓" + status.behind);
  $("git-ahead-behind").textContent = ab.join(" ");

  const staged = status.files.filter((f) => f.staged);
  const changes = status.files.filter((f) => f.unstaged);

  renderGitList($("git-staged"), staged, "staged");
  renderGitList($("git-changes"), changes, "changes");
  $("count-staged").textContent = staged.length ? String(staged.length) : "";
  $("count-changes").textContent = changes.length ? String(changes.length) : "";
}

function badgeClass(xy) {
  if (xy.includes("?")) return "untracked";
  if (xy.includes("A") || xy.includes("C")) return "add";
  if (xy.includes("D")) return "del";
  return "mod";
}

function renderGitList(container, files, kind) {
  container.innerHTML = "";
  if (!files.length) {
    const none = document.createElement("div");
    none.className = "muted small";
    none.style.padding = "4px 6px";
    none.textContent = kind === "staged" ? "Nothing staged" : "No changes";
    container.appendChild(none);
    return;
  }
  for (const f of files) {
    const row = document.createElement("div");
    row.className = "git-file";
    const badge = document.createElement("span");
    badge.className = "badge " + badgeClass(f.status);
    badge.textContent = f.status.trim() || "?";
    const name = document.createElement("span");
    name.className = "fname";
    name.textContent = f.path;
    const action = document.createElement("span");
    action.className = "action";
    action.textContent = kind === "staged" ? "−" : "+";
    action.title = kind === "staged" ? "Unstage" : "Stage";
    action.addEventListener("click", async (e) => {
      e.stopPropagation();
      try {
        if (kind === "staged") await invoke("git_unstage", { paths: [f.path] });
        else await invoke("git_stage", { paths: [f.path] });
        renderGit();
      } catch (err) { appendOutput("git: " + err); showPanel(); }
    });
    row.appendChild(badge);
    row.appendChild(name);
    row.appendChild(action);
    row.addEventListener("click", () => {
      openFile(joinPath(state.root, f.path));
    });
    container.appendChild(row);
  }
}

async function gitCommit() {
  const msg = $("commit-msg").value.trim();
  if (!msg) { $("commit-msg").focus(); return; }
  try {
    const out = await invoke("git_commit", { message: msg });
    $("commit-msg").value = "";
    appendOutput("$ git commit");
    if (out.trim()) appendOutput(out.trim().split("\n")[0]);
    renderGit();
  } catch (err) {
    appendOutput("git commit failed: " + err);
    showPanel();
  }
}

async function gitRun(kind) {
  try {
    const out = kind === "push" ? await invoke("git_push") : await invoke("git_pull");
    appendOutput(`$ git ${kind}`);
    if (out.trim()) appendOutput(out.trim());
    setPanelTab("output");
    renderGit();
  } catch (err) {
    appendOutput(`git ${kind} failed: ` + err);
    showPanel();
  }
}

// ---------------------------------------------------------------- monaco

let editor = null;

function initMonaco() {
  editor = monaco.editor.create($("monaco-host"), {
    value: "",
    language: "plaintext",
    fontFamily: '"Cascadia Mono", "Consolas", monospace',
    fontSize: 14,
    lineHeight: 21,
    minimap: { enabled: true, maxColumn: 80 },
    smoothScrolling: true,
    cursorBlinking: "smooth",
    cursorSmoothCaretAnimation: "on",
    padding: { top: 10 },
    renderLineHighlight: "all",
    scrollBeyondLastLine: false,
    roundedSelection: true,
    theme: "one-dark",
    suggestSelection: "first",
    wordBasedSuggestions: "currentDocument",
    bracketPairColorization: { enabled: true },
    guides: { bracketPairs: true },
  });
  $("monaco-host").classList.add("hidden");
  ensureTomlLanguage();

  editor.onDidChangeCursorPosition((e) => {
    $("status-pos").textContent = `Ln ${e.position.lineNumber}, Col ${e.position.column}`;
  });

  monaco.languages.registerCompletionItemProvider("rust", {
    triggerCharacters: [".", ":", "("],
    async provideCompletionItems(model, position) {
      const word = model.getWordUntilPosition(position);
      const items = await invokeSafe("lsp_completion", {
        uri: model.uri.toString(),
        line: position.lineNumber - 1,
        character: Math.max(0, (word.endColumn || position.column) - 1),
      });
      if (!items || !items.length) return { suggestions: [] };
      const suggestions = items.map((item) => {
        const range = item.textEdit?.range
          ? new monaco.Range(
              item.textEdit.range.start.line + 1,
              item.textEdit.range.start.character + 1,
              item.textEdit.range.end.line + 1,
              item.textEdit.range.end.character + 1,
            )
          : new monaco.Range(
              position.lineNumber,
              word.startColumn,
              position.lineNumber,
              word.endColumn,
            );
        return {
          label: item.label,
          kind: lspKindToMonaco(item.kind),
          detail: item.detail || "",
          documentation: item.documentation || undefined,
          insertText: item.textEdit?.newText || item.insertText || item.label,
          range,
          sortText: item.sortText || undefined,
          filterText: item.filterText || undefined,
        };
      });
      return { suggestions };
    },
  });
}

/** Monaco's min build has no toml basic-language; register a small one. */
function ensureTomlLanguage() {
  try {
    if (monaco.languages.getLanguages().some((l) => l.id === "toml")) return;
    monaco.languages.register({ id: "toml", extensions: [".toml"] });
    monaco.languages.setMonarchTokensProvider("toml", {
      tokenizer: {
        root: [
          [/^\s*\[[^\]]*\]\s*$/, "type.identifier"],
          [/^[A-Za-z0-9_.\-]+(?=\s*=)/, "variable"],
          [/".*?"/, "string"],
          [/'[^']*'/, "string"],
          [/#.*$/, "comment"],
          [/\b(?:true|false)\b/, "keyword"],
          [/\b\d[\d_.]*\b/, "number"],
          [/=/, "delimiter"],
        ],
      },
    });
    monaco.languages.setLanguageConfiguration("toml", {
      comments: { lineComment: "#" },
      brackets: [["{", "}"], ["[", "]"]],
      autoClosingPairs: [
        { open: "{", close: "}" },
        { open: "[", close: "]" },
        { open: '"', close: '"', notIn: ["string"] },
        { open: "'", close: "'", notIn: ["string"] },
      ],
    });
  } catch (err) {
    logFrontend("toml language registration failed: " + err);
  }
}

function lspKindToMonaco(kind) {
  // LSP CompletionItemKind numbers match Monaco's enum almost 1:1.
  const monaco = monaco.languages.CompletionItemKind;
  if (kind == null) return monaco.Text;
  if (kind === 2) return monaco.Method;
  if (kind === 3) return monaco.Function;
  if (kind === 4) return monaco.Constructor;
  if (kind === 5) return monaco.Field;
  if (kind === 6) return monaco.Variable;
  if (kind === 7) return monaco.Class;
  return kind >= 8 ? kind : monaco.Value;
}

// ---------------------------------------------------------------- tabs

async function openFile(path) {
  const existing = state.tabs.find((t) => t.path === path);
  if (existing) { setActiveTab(path); return; }
  const value = await invokeSafe("read_file", { path });
  if (value === null) { appendOutput(`failed to open ${path}`); showPanel(); return; }

  const uri = monaco.Uri.parse(fileUrl(path));
  let model = monaco.editor.getModel(uri);
  if (!model) {
    model = monaco.editor.createModel(value, langFor(path), uri);
    model.savedVersionId = model.getAlternativeVersionId();
    model.onDidChangeContent(() => {
      const tab = state.tabs.find((t) => t.model === model);
      if (!tab) return;
      tab.dirty = model.getAlternativeVersionId() !== model.savedVersionId;
      renderTabs();
      scheduleDidChange(path, model);
    });
  }
  state.tabs.push({ path, name: baseName(path), model, dirty: false });
  setActiveTab(path);

  // Register with the backend for every language; the backend routes LSP
  // traffic to rust-analyzer only for Rust files.
  invoke("lsp_did_open", {
    uri: uri.toString(),
    text: model.getValue(),
    language: langFor(path),
  });
}

let changeTimer = null;
function scheduleDidChange(path, model) {
  clearTimeout(changeTimer);
  changeTimer = setTimeout(() => {
    invoke("lsp_did_change", { uri: fileUrl(path), text: model.getValue() });
  }, 350);
}

function setActiveTab(path) {
  state.activePath = path;
  const tab = state.tabs.find((t) => t.path === path);
  if (tab) {
    editor.setModel(tab.model);
    $("monaco-host").classList.remove("hidden");
    $("welcome").classList.add("hidden");
    editor.focus();
  }
  renderTabs();
  renderTreeSelection();
  updateDiagStatus();
  updateLangStatus();
}

function updateLangStatus() {
  const tab = state.tabs.find((t) => t.path === state.activePath);
  $("status-lang").textContent = tab ? langName(langFor(tab.path)) : "—";
}

function closeTab(path) {
  const idx = state.tabs.findIndex((t) => t.path === path);
  if (idx < 0) return;
  const [tab] = state.tabs.splice(idx, 1);
  invoke("lsp_did_close", { uri: fileUrl(path) });
  if (state.activePath === path) {
    const next = state.tabs[Math.min(idx, state.tabs.length - 1)];
    if (next) setActiveTab(next.path);
    else {
      state.activePath = null;
      editor.setModel(null);
      $("monaco-host").classList.add("hidden");
      $("welcome").classList.remove("hidden");
      renderTabs();
      updateDiagStatus();
      updateLangStatus();
    }
  } else {
    renderTabs();
  }
}

async function saveActive() {
  const tab = state.tabs.find((t) => t.path === state.activePath);
  if (!tab) return;
  await invoke("write_file", { path: tab.path, content: tab.model.getValue() });
  tab.model.savedVersionId = tab.model.getAlternativeVersionId();
  tab.dirty = false;
  renderTabs();
}

function renderTabs() {
  const bar = $("tab-bar");
  bar.innerHTML = "";
  for (const tab of state.tabs) {
    const el = document.createElement("div");
    el.className = "tab" + (tab.path === state.activePath ? " active" : "");
    const name = document.createElement("span");
    name.className = "tab-name";
    name.textContent = tab.name;
    el.appendChild(name);
    if (tab.dirty) {
      const dot = document.createElement("span");
      dot.className = "dot";
      el.appendChild(dot);
    }
    const close = document.createElement("span");
    close.className = "close";
    close.textContent = "✕";
    close.addEventListener("click", (e) => { e.stopPropagation(); closeTab(tab.path); });
    el.appendChild(close);
    el.addEventListener("click", () => setActiveTab(tab.path));
    bar.appendChild(el);
  }
}

// ---------------------------------------------------------------- explorer tree

async function renderTree() {
  const tree = $("file-tree");
  tree.innerHTML = "";
  if (!state.root) {
    const empty = document.createElement("div");
    empty.className = "muted small";
    empty.style.padding = "10px 12px";
    empty.textContent = "No folder open — Ctrl+O or click the project pill.";
    tree.appendChild(empty);
    return;
  }
  const children = await loadChildren(state.root);
  tree.appendChild(buildTreeChildren(state.root, children));
}

async function loadChildren(dir) {
  try { return await invoke("list_dir", { path: dir }); }
  catch { return []; }
}

function buildTreeChildren(parent, entries) {
  const wrap = document.createElement("div");
  wrap.className = "tree-children";
  if (parent === state.root) wrap.className = "";
  for (const entry of entries) {
    wrap.appendChild(buildTreeRow(entry));
  }
  return wrap;
}

function buildTreeRow(entry) {
  const row = document.createElement("div");
  row.className = "tree-row" + (entry.is_dir ? " folder" : "");
  row.dataset.path = entry.path;

  const chev = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  chev.setAttribute("viewBox", "0 0 16 16");
  chev.setAttribute("class", "chev");
  chev.innerHTML = '<path d="M4 6l4 4 4-4" fill="none" stroke="currentColor" stroke-width="1.5"/>';
  if (entry.is_dir) {
    row.appendChild(chev);
    row.classList.add("expanded");
    const icon = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    icon.setAttribute("viewBox", "0 0 16 16");
    icon.setAttribute("class", "icon");
    icon.innerHTML = '<path d="M1.5 3.5h4.5l1.5 2h7v7.5a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1v-9.5a1 1 0 0 1 1-1z" fill="none" stroke="currentColor" stroke-width="1.2"/>';
    row.appendChild(icon);
  } else {
    const spacer = document.createElement("span");
    spacer.style.width = "13px";
    spacer.style.flexShrink = "0";
    row.appendChild(spacer);
    const icon = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    icon.setAttribute("viewBox", "0 0 16 16");
    icon.setAttribute("class", "icon");
    icon.innerHTML = '<path d="M4 1.5h5l3 3v10h-8z" fill="none" stroke="currentColor" stroke-width="1.2"/><path d="M9 1.5v3h3" fill="none" stroke="currentColor" stroke-width="1.2"/>';
    row.appendChild(icon);
  }

  const name = document.createElement("span");
  name.className = "name";
  name.textContent = entry.name;
  row.appendChild(name);

  row.addEventListener("click", async () => {
    if (entry.is_dir) {
      if (state.expanded.has(entry.path)) {
        state.expanded.delete(entry.path);
        row.classList.remove("expanded");
        row.classList.add("collapsed");
        const child = row.nextElementSibling;
        if (child && child.classList.contains("tree-children")) child.remove();
      } else {
        state.expanded.add(entry.path);
        row.classList.add("expanded");
        row.classList.remove("collapsed");
        const kids = await loadChildren(entry.path);
        row.after(buildTreeChildren(entry.path, kids));
      }
    } else {
      openFile(entry.path);
    }
  });

  row.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    showContextMenu(e.clientX, e.clientY, entry);
  });

  if (state.selectedTreePath === entry.path && !entry.is_dir) {
    row.classList.add("selected");
  }
  return row;
}

function renderTreeSelection() {
  document.querySelectorAll(".tree-row").forEach((row) => {
    row.classList.toggle("selected", row.dataset.path === state.activePath);
  });
}

// ------------------------------------------------------------- tree actions

function startCreate(isDir) {
  cancelPendingNew();
  const parent = state.root;
  if (!parent) return;
  state.pendingNew = { parent, isDir };

  const input = document.createElement("input");
  input.className = "tree-inline-input";
  input.placeholder = isDir ? "folder name… (Enter)" : "file name… (Enter)";
  input.id = "new-entry-input";
  $("file-tree").prepend(input);
  input.focus();

  input.addEventListener("keydown", async (e) => {
    e.stopPropagation();
    if (e.key === "Escape") { cancelPendingNew(); }
    else if (e.key === "Enter") {
      const name = input.value.trim();
      const pending = state.pendingNew;
      cancelPendingNew();
      if (!name || !pending) return;
      const path = joinPath(pending.parent, name);
      try {
        await invoke("create_entry", { path, isDir: pending.isDir });
        const files = await invokeSafe("walk_tree", { root: state.root });
        state.quickFiles = files;
        if (!pending.isDir) await openFile(path);
        else { state.expanded.add(path); }
        renderTree();
      } catch (err) { appendOutput("create failed: " + err); showPanel(); }
    }
  });
  input.addEventListener("blur", () => setTimeout(cancelPendingNew, 150));
}

function cancelPendingNew() {
  state.pendingNew = null;
  document.getElementById("new-entry-input")?.remove();
}

async function refreshTree() {
  const files = await invokeSafe("walk_tree", { root: state.root });
  state.quickFiles = files;
  renderTree();
}

// -------------------------------------------------------------- context menu

function showContextMenu(x, y, entry) {
  const menu = $("context-menu");
  menu.innerHTML = "";
  const items = [
    ...(entry.is_dir
      ? [
          { label: "New File", action: () => startCreateUnder(entry.path, false) },
          { label: "New Folder", action: () => startCreateUnder(entry.path, true) },
        ]
      : [
          { label: "Open", action: () => openFile(entry.path) },
        ]),
    { sep: true },
    { label: "Rename", action: () => startRename(entry) },
    { label: "Delete", danger: true, action: () => deleteEntry(entry) },
  ];
  for (const item of items) {
    if (item.sep) {
      const sep = document.createElement("div");
      sep.className = "ctx-sep";
      menu.appendChild(sep);
      continue;
    }
    const el = document.createElement("div");
    el.className = "ctx-item" + (item.danger ? " danger" : "");
    el.textContent = item.label;
    el.addEventListener("click", () => {
      hideContextMenu();
      item.action();
    });
    menu.appendChild(el);
  }
  menu.classList.remove("hidden");
  const rect = menu.getBoundingClientRect();
  menu.style.left = Math.min(x, window.innerWidth - rect.width - 8) + "px";
  menu.style.top = Math.min(y, window.innerHeight - rect.height - 8) + "px";
}

function hideContextMenu() {
  $("context-menu").classList.add("hidden");
}

async function startCreateUnder(dir, isDir) {
  // Ensure visible: expand the dir
  state.expanded.add(dir);
  await renderTree();
  startCreate(isDir);
  // Point the pending parent at the requested dir
  state.pendingNew = { parent: dir, isDir };
}

async function startRename(entry) {
  const newName = prompt("New name:", entry.name);
  if (!newName || newName === entry.name) return;
  const newPath = joinPath(entry.path.split(/[\\/]/).slice(0, -1).join("\\"), newName);
  try {
    await invoke("rename_entry", { from: entry.path, to: newPath });
    // Re-open if it was open
    const tab = state.tabs.find((t) => t.path === entry.path);
    if (tab) {
      closeTab(entry.path);
      await openFile(newPath);
    }
    refreshTree();
  } catch (err) {
    appendOutput("rename failed: " + err);
    showPanel();
  }
}

async function deleteEntry(entry) {
  if (!confirm(`Delete ${entry.is_dir ? "folder" : "file"} "${entry.name}"?`)) return;
  try {
    await invoke("delete_entry", { path: entry.path });
    if (!entry.is_dir) closeTab(entry.path);
    refreshTree();
  } catch (err) {
    appendOutput("delete failed: " + err);
    showPanel();
  }
}

// -------------------------------------------------------------- output panel

function appendOutput(line) {
  const lines = $("panel-lines");
  const el = document.createElement("div");
  el.className = "line";
  if (line.startsWith("$ ")) el.classList.add("cmd");
  if (/^error/i.test(line) || line.includes("error[")) el.classList.add("err");
  el.textContent = line;
  lines.appendChild(el);
  while (lines.children.length > 1500) lines.firstChild.remove();
  lines.scrollTop = lines.scrollHeight;
}

function setBuildStatus(status) {
  state.buildRunning = status === "running";
  const chip = $("panel-chip");
  const statusEl = $("status-build");
  const label = { running: "building…", success: "build ok", failed: "build failed", idle: "ready to build" }[status] || status;
  chip.textContent = label;
  chip.className = "chip " + status;
  statusEl.textContent = label;
  statusEl.className = "status-item " + status;
  if (status !== "running") showPanel();
}

async function runBuild(build) {
  if (state.buildRunning) return;
  if (!state.root) {
    showPanel();
    setPanelTab("output");
    appendOutput("Open a folder first (Ctrl+O).");
    return;
  }
  const info = state.project || { kind: "none", can_build: false, can_run: false, label: "no project" };
  // Interpreted languages have no build step — treat Build as Run.
  if (build && !info.can_build && info.can_run) build = false;
  showPanel();
  setPanelTab("output");
  const nothingDetected =
    "No runnable project detected in this folder — looking for Cargo.toml, package.json, go.mod, or Python files (main.py / app.py / …).";
  if (build && !info.can_build) {
    appendOutput(info.kind === "none" ? nothingDetected : `Nothing to build for ${info.label} — use Run instead.`);
    return;
  }
  if (!build && !info.can_run) {
    appendOutput(info.kind === "none" ? nothingDetected : `Nothing to run — ${info.label} has no run command.`);
    return;
  }
  try { await invoke("start_build", { run: !build }); }
  catch (err) { appendOutput(err); }
}

function buildOrRun() {
  const info = state.project;
  runBuild(info ? info.can_build : true);
}

async function refreshProjectInfo() {
  const info = await invokeSafe("detect_project");
  state.project = info;
  if (!info) return;
  $("status-build").textContent = info.kind === "none" ? "no project detected" : info.label;
  $("status-build-btn").textContent = info.can_build ? "▶ Build" : "▶ Run";
  $("btn-build").classList.toggle("hidden", !info.can_build);
  $("btn-run").classList.toggle("hidden", !info.can_run);
  const entryNote = info.entry ? ` · ${info.entry}` : "";
  $("status-build").title = info.kind === "none"
    ? "No runnable project detected"
    : `${info.label}${entryNote}`;
}

// -------------------------------------------------------------- diagnostics

function onDiagnostics(payload) {
  const { uri, diagnostics } = payload;
  const mapped = diagnostics.map((d) => ({
    severity: ({ 1: 8, 2: 4, 3: 2, 4: 1 }[d.severity]) || 8,
    message: d.message,
    source: d.source || "rust-analyzer",
    startLineNumber: d.range.start.line + 1,
    startColumn: d.range.start.character + 1,
    endLineNumber: d.range.end.line + 1,
    endColumn: d.range.end.character + 1,
  }));
  state.markers.set(uri, mapped);
  const model = monaco.editor.getModel(monaco.Uri.parse(uri));
  if (model) {
    monaco.editor.setModelMarkers(model, "rust-analyzer", mapped);
  }
  updateDiagStatus();
}

function updateDiagStatus() {
  const tab = state.tabs.find((t) => t.path === state.activePath);
  let errors = 0;
  let warnings = 0;
  if (tab) {
    const markers = state.markers.get(fileUrl(tab.path)) || [];
    for (const m of markers) {
      if (m.severity >= 8) errors++;
      else if (m.severity >= 4) warnings++;
    }
  }
  $("diag-errors").textContent = "✖ " + errors;
  $("diag-warnings").textContent = "⚠ " + warnings;
}

// -------------------------------------------------------------- quick open

async function showQuickOpen() {
  if (!state.root) return;
  if (!state.quickFiles || !state.quickFiles.length) {
    state.quickFiles = (await invokeSafe("walk_tree", { root: state.root })) || [];
  }
  $("quick-open").classList.remove("hidden");
  const input = $("quick-input");
  input.value = "";
  renderQuickList("");
  input.focus();
}

function hideQuickOpen() {
  $("quick-open").classList.add("hidden");
}

function renderQuickList(filter) {
  const list = $("quick-list");
  list.innerHTML = "";
  const files = state.quickFiles || [];
  const q = filter.toLowerCase().replace(/\s/g, "");
  let matches = files;
  if (q) {
    matches = files.filter((p) => p.toLowerCase().replace(/[\\/]/g, "").includes(q));
  }
  const base = state.root ? state.root.length + 1 : 0;
  matches.slice(0, 100).forEach((path, i) => {
    const el = document.createElement("div");
    el.className = "quick-item" + (i === 0 ? " focused" : "");
    const name = document.createElement("span");
    name.textContent = baseName(path);
    const dir = document.createElement("span");
    dir.className = "path";
    dir.textContent = path.slice(base);
    el.appendChild(name);
    el.appendChild(dir);
    el.addEventListener("click", () => { hideQuickOpen(); openFile(path); });
    list.appendChild(el);
  });
  state.quickMatches = matches.slice(0, 100);
}

// ---------------------------------------------------------------- wiring

function wireEvents() {
  // Explorer buttons
  $("btn-new-file").addEventListener("click", () => startCreate(false));
  $("btn-new-folder").addEventListener("click", () => startCreate(true));
  $("btn-refresh").addEventListener("click", refreshTree);
  $("btn-open-folder").addEventListener("click", openFolderDialog);
  $("project-name").addEventListener("click", openFolderDialog);
  $("welcome-open").addEventListener("click", openFolderDialog);
  $("welcome-quick").addEventListener("click", showQuickOpen);

  // Activity bar
  $("act-theme").addEventListener("click", cycleTheme);
  $("act-build").addEventListener("click", buildOrRun);
  $("act-git").addEventListener("click", () => showView(gitViewHidden() ? "git" : "explorer"));
  $("act-explorer").addEventListener("click", () => showView("explorer"));

  // Panel tabs + terminal
  $("ptab-output").addEventListener("click", () => setPanelTab("output"));
  $("ptab-terminal").addEventListener("click", () => setPanelTab("terminal"));
  $("btn-term-restart").addEventListener("click", restartTerminal);

  // Source control
  $("btn-git-refresh").addEventListener("click", renderGit);
  $("btn-git-stage-all").addEventListener("click", async () => {
    try { await invoke("git_stage_all"); renderGit(); }
    catch (err) { appendOutput("git: " + err); showPanel(); }
  });
  $("btn-git-pull").addEventListener("click", () => gitRun("pull"));
  $("btn-git-push").addEventListener("click", () => gitRun("push"));
  $("btn-commit").addEventListener("click", gitCommit);
  $("commit-msg").addEventListener("keydown", (e) => {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); gitCommit(); }
    e.stopPropagation();
  });

  // Build buttons
  $("btn-build").addEventListener("click", () => runBuild(true));
  $("btn-run").addEventListener("click", () => runBuild(false));
  $("status-build-btn").addEventListener("click", buildOrRun);
  $("btn-clear-output").addEventListener("click", () => { $("panel-lines").innerHTML = ""; });
  $("btn-panel-close").addEventListener("click", togglePanel);

  // Quick open
  $("quick-input").addEventListener("input", (e) => renderQuickList(e.target.value));
  $("quick-input").addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.key === "Escape") hideQuickOpen();
    else if (e.key === "Enter") {
      const first = $("quick-list").firstChild;
      if (first) { hideQuickOpen(); openFile(state.quickMatches[0]); }
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      const items = [...$("quick-list").children];
      const idx = items.findIndex((el) => el.classList.contains("focused"));
      if (idx >= 0) {
        items[idx].classList.remove("focused");
        const next = items[(idx + (e.key === "ArrowDown" ? 1 : items.length - 1)) % items.length];
        next.classList.add("focused");
        next.scrollIntoView({ block: "nearest" });
      }
    }
  });
  $("quick-open").addEventListener("mousedown", (e) => {
    if (e.target === $("quick-open")) hideQuickOpen();
  });

  // Context menu dismiss
  window.addEventListener("mousedown", (e) => {
    if (!$("context-menu").contains(e.target)) hideContextMenu();
  });

  // Global keybindings (capture phase so the webview doesn't swallow them)
  window.addEventListener("keydown", (e) => {
    const ctrl = e.ctrlKey || e.metaKey;
    if (!ctrl) return;
    const k = e.key.toLowerCase();
    if (k === "s") { e.preventDefault(); saveActive(); }
    else if (k === "o") { e.preventDefault(); openFolderDialog(); }
    else if (k === "p") { e.preventDefault(); showQuickOpen(); }
    else if (k === "`") { e.preventDefault(); togglePanel(); }
    else if (k === "b" && e.shiftKey) { e.preventDefault(); buildOrRun(); }
    else if (k === "r" && e.shiftKey) { e.preventDefault(); runBuild(false); }
    else if (k === "g" && e.shiftKey) { e.preventDefault(); showView(gitViewHidden() ? "git" : "explorer"); }
    else if (k === "k" && !e.shiftKey) { e.preventDefault(); cycleTheme(); }
    else if (k === "w") { e.preventDefault(); if (state.activePath) closeTab(state.activePath); }
  }, true);

  // Refresh the source-control view while it's visible.
  setInterval(() => { if (!gitViewHidden() && state.root) renderGit(); }, 3000);

  applyTheme();
}

async function openFolderDialog() {
  try {
    const picked = await window.__TAURI__.dialog.open({ directory: true, multiple: false });
    if (!picked) return;
    await invoke("set_root", { path: picked });
    closeAllTabs();
  } catch (err) {
    appendOutput("open folder failed: " + err);
    showPanel();
  }
}

function closeAllTabs() {
  for (const tab of [...state.tabs]) closeTab(tab.path);
}

// ---------------------------------------------------------------- go

boot();
