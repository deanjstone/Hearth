// The `window.hearth.git` Tauri surface (Phase 7, tracking issue #27).
// Ported from the `git:*` handlers in electron/main/ipc.ts, backed by
// `git_panel`. Only `state.repo_root` is supported as a workspace root —
// same simplification `fs_commands.rs`/`workspaces_commands.rs` already
// make, since the multi-workspace registry `at(cwd)` resolves against isn't
// ported.

use crate::git_panel::{self, BranchInfo, DiffSummary, GitStatus, PrResult};
use crate::selfmod_commands::AppState;

#[tauri::command]
pub fn git_diff(
    state: tauri::State<AppState>,
    cwd: Option<String>,
    rev: Option<String>,
) -> Result<DiffSummary, String> {
    let _ = cwd;
    git_panel::get_diff(&state.repo_root, rev.as_deref())
}

#[tauri::command]
pub fn git_status(state: tauri::State<AppState>, cwd: Option<String>) -> Result<GitStatus, String> {
    let _ = cwd;
    git_panel::status(&state.repo_root)
}

#[tauri::command]
pub fn git_stage(
    state: tauri::State<AppState>,
    paths: Vec<String>,
    cwd: Option<String>,
) -> Result<(), String> {
    let _ = cwd;
    git_panel::stage(&state.repo_root, &paths)
}

#[tauri::command]
pub fn git_unstage(
    state: tauri::State<AppState>,
    paths: Vec<String>,
    cwd: Option<String>,
) -> Result<(), String> {
    let _ = cwd;
    git_panel::unstage(&state.repo_root, &paths)
}

#[tauri::command]
pub fn git_commit(
    state: tauri::State<AppState>,
    message: String,
    cwd: Option<String>,
) -> Result<String, String> {
    let _ = cwd;
    git_panel::commit(&state.repo_root, &message)
}

#[tauri::command]
pub fn git_branches(
    state: tauri::State<AppState>,
    cwd: Option<String>,
) -> Result<BranchInfo, String> {
    let _ = cwd;
    git_panel::branches(&state.repo_root)
}

#[tauri::command]
pub fn git_switch_branch(
    state: tauri::State<AppState>,
    name: String,
    create: bool,
    cwd: Option<String>,
) -> Result<(), String> {
    let _ = cwd;
    git_panel::switch_branch(&state.repo_root, &name, create)
}

#[tauri::command]
pub fn git_create_pr(
    state: tauri::State<AppState>,
    title: String,
    body: String,
    cwd: Option<String>,
) -> PrResult {
    let _ = cwd;
    git_panel::create_pr(&state.repo_root, &title, &body)
}
