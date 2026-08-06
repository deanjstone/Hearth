// The `window.hearth.terminal` Tauri surface (Phase 4, tracking issue #27).
// Ported from the `terminal:*` handlers in electron/main/ipc.ts, backed by
// `TerminalManager` (src-tauri/src/terminal/pty.rs). Command names derive
// mechanically from Electron's channel strings (colons -> underscores),
// carrying forward the naming convention spec #48 established for Phase 3
// (no Phase-4-specific spec superseded it — tracking issue #27's own Phase 4
// entry is the source of truth here). Events keep the channel strings
// verbatim: `terminal:data`/`terminal:exit`.

use crate::selfmod_commands::AppState;
use crate::terminal::login_path::{LoginPathResolver, RealShellQuery};
use crate::terminal::pty::{RealPtySpawner, TerminalManager};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};

pub type RealTerminalManager = TerminalManager<RealPtySpawner, RealShellQuery>;

/// Registered via `app.manage(...)` in `lib.rs`'s `setup()`, alongside
/// `AppState`/`AgentState`/`SessionState` — each subsystem gets its own
/// managed struct.
pub struct TerminalState {
    pub manager: RealTerminalManager,
}

#[derive(Serialize, Clone)]
struct TerminalDataPayload<'a> {
    id: &'a str,
    data: &'a str,
}

#[derive(Serialize, Clone)]
struct TerminalExitPayload<'a> {
    id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
}

/// Build a `TerminalManager` wired to emit `terminal:data`/`terminal:exit`
/// on `app`. Constructed once in `lib.rs`'s `setup()`.
pub fn new_terminal_manager(app: AppHandle) -> RealTerminalManager {
    let base_env: HashMap<String, String> = std::env::vars().collect();
    let data_app = app.clone();
    let exit_app = app;
    TerminalManager::new(
        RealPtySpawner,
        LoginPathResolver::new(RealShellQuery, cfg!(target_os = "windows")),
        base_env,
        move |id, data| {
            let _ = data_app.emit("terminal:data", TerminalDataPayload { id, data });
        },
        move |id| {
            // Natural process exit — no reason string. `terminal_create`'s
            // own error path below emits a *different* `terminal:exit` with
            // a reason, for spawn-time failures the manager never got a
            // chance to track.
            let _ = exit_app.emit("terminal:exit", TerminalExitPayload { id, reason: None });
        },
    )
}

/// Resolve a caller-supplied cwd the same way `ipc.ts`'s `at(cwd)` helper
/// does: an absent cwd defaults to the repo root; a cwd outside the
/// registered workspace is rejected, so a compromised renderer can't spawn a
/// shell in an arbitrary directory. Simplified vs `registry.ts`'s real
/// `contains()` (no `resolve()`/canonicalization, since `workspaces_commands.rs`'s
/// stub has exactly one workspace to check against — see its own header
/// comment for why a full registry isn't ported here).
fn resolve_cwd(repo_root: &Path, cwd: Option<&str>) -> Result<PathBuf, String> {
    match cwd {
        None => Ok(repo_root.to_path_buf()),
        Some(cwd) => {
            let target = PathBuf::from(cwd);
            if target.starts_with(repo_root) {
                Ok(target)
            } else {
                Err(format!("cwd is not a registered workspace: {cwd}"))
            }
        }
    }
}

#[tauri::command]
pub fn terminal_create(
    id: String,
    cwd: Option<String>,
    cols: u16,
    rows: u16,
    app: AppHandle,
    state: tauri::State<AppState>,
    terminal: tauri::State<TerminalState>,
) {
    let resolved = match resolve_cwd(&state.repo_root, cwd.as_deref()) {
        Ok(p) => p,
        Err(reason) => {
            let _ = app.emit(
                "terminal:exit",
                TerminalExitPayload {
                    id: &id,
                    reason: Some(&reason),
                },
            );
            return;
        }
    };
    if let Err(reason) = terminal.manager.create(&id, &resolved, cols, rows) {
        // A dead pane must explain itself (matching ipc.ts's own comment):
        // log the cause and give the renderer a reason to print instead of a
        // blank exited terminal.
        eprintln!(
            "[hearth] terminal create failed (id {id}, cwd {}): {reason}",
            resolved.display()
        );
        let _ = app.emit(
            "terminal:exit",
            TerminalExitPayload {
                id: &id,
                reason: Some(&reason),
            },
        );
    }
}

#[tauri::command]
pub fn terminal_write(id: String, data: String, terminal: tauri::State<TerminalState>) {
    terminal.manager.write(&id, &data);
}

#[tauri::command]
pub fn terminal_resize(id: String, cols: u16, rows: u16, terminal: tauri::State<TerminalState>) {
    terminal.manager.resize(&id, cols, rows);
}

#[tauri::command]
pub fn terminal_kill(id: String, terminal: tauri::State<TerminalState>) {
    terminal.manager.kill(&id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_cwd_defaults_to_repo_root_when_absent() {
        let repo_root = PathBuf::from("/repo");
        assert_eq!(
            resolve_cwd(&repo_root, None).unwrap(),
            PathBuf::from("/repo")
        );
    }

    #[test]
    fn resolve_cwd_allows_the_repo_root_itself() {
        let repo_root = PathBuf::from("/repo");
        assert_eq!(
            resolve_cwd(&repo_root, Some("/repo")).unwrap(),
            PathBuf::from("/repo")
        );
    }

    #[test]
    fn resolve_cwd_allows_a_subdirectory_of_the_repo_root() {
        let repo_root = PathBuf::from("/repo");
        assert_eq!(
            resolve_cwd(&repo_root, Some("/repo/src-tauri")).unwrap(),
            PathBuf::from("/repo/src-tauri")
        );
    }

    #[test]
    fn resolve_cwd_rejects_a_path_outside_the_repo_root() {
        let repo_root = PathBuf::from("/repo");
        assert!(resolve_cwd(&repo_root, Some("/etc")).is_err());
    }

    #[test]
    fn resolve_cwd_rejects_a_sibling_directory_with_a_matching_prefix() {
        // A naive string-prefix check would wrongly accept "/repo-evil" as
        // being "inside" "/repo" — Path::starts_with is component-aware and
        // correctly rejects it.
        let repo_root = PathBuf::from("/repo");
        assert!(resolve_cwd(&repo_root, Some("/repo-evil")).is_err());
    }
}
