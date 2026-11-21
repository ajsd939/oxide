//! Rust IDE — Tauri backend: project IO, rust-analyzer bridge, cargo builds.
//! Frontend lives in `ui/` (Monaco + vanilla JS), served as static assets.

mod commands;
mod completion;
mod git;
mod lsp;
mod output;
mod term;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(commands::AppState::default())
        .manage(term::TermState::default())
        .setup(|app| {
            commands::init_app(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_root,
            commands::get_tools,
            commands::frontend_log,
            commands::set_root,
            commands::detect_project,
            commands::list_dir,
            commands::walk_tree,
            commands::read_file,
            commands::write_file,
            commands::create_entry,
            commands::delete_entry,
            commands::rename_entry,
            commands::lsp_did_open,
            commands::lsp_did_change,
            commands::lsp_did_close,
            commands::lsp_completion,
            commands::start_build,
            term::term_spawn,
            term::term_write,
            term::term_resize,
            term::term_kill,
            term::term_set_root,
            commands::git_installed,
            commands::git_status,
            commands::git_stage,
            commands::git_stage_all,
            commands::git_unstage,
            commands::git_commit,
            commands::git_push,
            commands::git_pull,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
