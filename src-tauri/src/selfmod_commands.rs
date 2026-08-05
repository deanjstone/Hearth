// The `window.hearth.selfMod.*` IPC surface (Phase 1's "Minimal IPC" per
// Hearth#27). Ported from the `selfModHistory`/`selfModUndo`/`selfModRedo`
// handlers in electron/main/ipc.ts.
//
// Deliberately thin: all three commands are direct pass-throughs to
// `SelfModService`, which already carries its own extensive test coverage
// (chunk 5). The only new logic here is the DTO conversion — `SelfModService`
// (protected-island code) has no reason to know about JSON/serde, so the
// wire shape lives on this, the canvas side of the boundary, matching TS
// keeping self-mod-service.ts itself serialization-agnostic and letting
// ipc.ts's handlers be the (de)serializing boundary.
//
// Field/variant names are `camelCase`-renamed to match the exact JSON shapes
// `git.ts`'s `SelfModLogEntry` and `self-mod-service.ts`'s `StepResult`
// produce, so `electron/preload-tauri/self-mod.ts`'s TS types apply unchanged.

use crate::agents::startup_check::AgentRuntimeStatus;
use crate::reload_driver_tauri::TauriReloadDriver;
use crate::selfmod::boot_watchdog::BootWatchdog;
use crate::selfmod::git::{SelfModKind, SelfModLogEntry};
use crate::selfmod::path_relevance::ReloadKind;
use crate::selfmod::service::{SelfModService, StepResult};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Mutex;

/// Owns the long-lived self-mod + agent-runtime collaborators for the app's
/// lifetime. Registered via `app.manage(...)` in `lib.rs`'s `setup` hook,
/// once the main window (and so the real `ReloadDriver`) exists.
pub struct AppState {
    pub self_mod: SelfModService<TauriReloadDriver>,
    pub boot_watchdog: BootWatchdog,
    /// Resolves the vendored adapter packages for `agent_runtime_recheck`
    /// (agents_commands.rs) — the same repo root `self_mod`/`bridge::start`
    /// already resolve in `lib.rs`'s `setup()`.
    pub repo_root: PathBuf,
    /// Cached result of the eager Node/adapter startup check (Chunk 4, spec
    /// #48). `Mutex`-guarded since "Check again" mutates it in place.
    pub agent_runtime_status: Mutex<AgentRuntimeStatus>,
}

fn kind_str(kind: SelfModKind) -> &'static str {
    match kind {
        SelfModKind::Code => "code",
        SelfModKind::Soul => "soul",
        SelfModKind::Memory => "memory",
    }
}

fn reload_str(kind: ReloadKind) -> &'static str {
    match kind {
        ReloadKind::Hmr => "hmr",
        ReloadKind::FullReload => "full-reload",
        ReloadKind::ProcessRestart => "process-restart",
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelfModLogEntryDto {
    pub hash: String,
    pub subject: String,
    pub conversation_id: Option<String>,
    pub kind: &'static str,
    pub run_id: Option<String>,
    pub subagent: Option<String>,
    pub reverted: bool,
}

impl From<SelfModLogEntry> for SelfModLogEntryDto {
    fn from(e: SelfModLogEntry) -> Self {
        Self {
            hash: e.hash,
            subject: e.subject,
            conversation_id: e.conversation_id,
            kind: kind_str(e.kind),
            run_id: e.run_id,
            subagent: e.subagent,
            reverted: e.reverted,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum StepResultDto {
    #[serde(rename_all = "camelCase")]
    Ok {
        commit: String,
        changed_paths: Vec<String>,
        reload: &'static str,
    },
    Dirty,
    Conflict {
        hash: String,
        files: Vec<String>,
    },
    Noop,
}

impl From<StepResult> for StepResultDto {
    fn from(r: StepResult) -> Self {
        match r {
            StepResult::Ok {
                commit,
                changed_paths,
                reload,
            } => StepResultDto::Ok {
                commit,
                changed_paths,
                reload: reload_str(reload),
            },
            StepResult::Dirty => StepResultDto::Dirty,
            StepResult::Conflict { hash, files } => StepResultDto::Conflict { hash, files },
            StepResult::Noop => StepResultDto::Noop,
        }
    }
}

#[tauri::command]
pub fn self_mod_history(state: tauri::State<AppState>) -> Result<Vec<SelfModLogEntryDto>, String> {
    state
        .self_mod
        .history()
        .map(|entries| entries.into_iter().map(SelfModLogEntryDto::from).collect())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn self_mod_undo(state: tauri::State<AppState>, hash: String) -> Result<StepResultDto, String> {
    state
        .self_mod
        .undo(&hash)
        .map(StepResultDto::from)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn self_mod_redo(state: tauri::State<AppState>, hash: String) -> Result<StepResultDto, String> {
    state
        .self_mod
        .redo(&hash)
        .map(StepResultDto::from)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_result_ok_serializes_to_the_ts_tagged_shape() {
        let dto = StepResultDto::from(StepResult::Ok {
            commit: "abc123".to_string(),
            changed_paths: vec!["src/a.ts".to_string()],
            reload: ReloadKind::Hmr,
        });
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "status": "ok",
                "commit": "abc123",
                "changedPaths": ["src/a.ts"],
                "reload": "hmr",
            })
        );
    }

    #[test]
    fn step_result_dirty_conflict_noop_serialize_to_status_only_or_tagged_fields() {
        assert_eq!(
            serde_json::to_value(StepResultDto::from(StepResult::Dirty)).unwrap(),
            serde_json::json!({ "status": "dirty" })
        );
        assert_eq!(
            serde_json::to_value(StepResultDto::from(StepResult::Noop)).unwrap(),
            serde_json::json!({ "status": "noop" })
        );
        assert_eq!(
            serde_json::to_value(StepResultDto::from(StepResult::Conflict {
                hash: "deadbeef".to_string(),
                files: vec!["src/a.ts".to_string(), "src/b.ts".to_string()],
            }))
            .unwrap(),
            serde_json::json!({
                "status": "conflict",
                "hash": "deadbeef",
                "files": ["src/a.ts", "src/b.ts"],
            })
        );
    }

    #[test]
    fn self_mod_log_entry_serializes_with_camel_case_and_null_optionals() {
        let dto = SelfModLogEntryDto::from(SelfModLogEntry {
            hash: "abc123".to_string(),
            subject: "tweak title".to_string(),
            conversation_id: None,
            kind: SelfModKind::Soul,
            run_id: Some("run-1".to_string()),
            subagent: None,
            reverted: true,
        });
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "hash": "abc123",
                "subject": "tweak title",
                "conversationId": null,
                "kind": "soul",
                "runId": "run-1",
                "subagent": null,
                "reverted": true,
            })
        );
    }
}
