// The startup Node/adapter-runtime check's Tauri surface (Chunk 4, spec #48;
// decision from grilling ticket #47). No Electron equivalent exists to
// mirror here — Electron never needs this check at all (it spawns the
// adapter through `ELECTRON_RUN_AS_NODE`, using its own binary as a Node
// interpreter), so this is new behavior specific to the Tauri port, where a
// real system Node install is required instead (spec #26's Implementation
// Decisions). `electron/preload/index.ts`'s `agentRuntime` field exists too,
// but only as an always-`ok` constant, so the renderer's gating component
// needs no Electron/Tauri branching of its own.

use crate::agents::startup_check::{self, AgentRuntimeStatus};
use crate::selfmod_commands::AppState;
use serde::Serialize;

#[derive(Serialize, Clone)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum AgentRuntimeStatusDto {
    Ok,
    NodeMissing,
    AdapterMissing { package: String },
}

impl From<AgentRuntimeStatus> for AgentRuntimeStatusDto {
    fn from(s: AgentRuntimeStatus) -> Self {
        match s {
            AgentRuntimeStatus::Ok => Self::Ok,
            AgentRuntimeStatus::NodeMissing => Self::NodeMissing,
            AgentRuntimeStatus::AdapterMissing { package } => Self::AdapterMissing { package },
        }
    }
}

/// The result of the eager check `lib.rs`'s `setup()` ran once at boot.
#[tauri::command]
pub fn agent_runtime_status(state: tauri::State<AppState>) -> AgentRuntimeStatusDto {
    state
        .agent_runtime_status
        .lock()
        .expect("agent_runtime_status mutex poisoned")
        .clone()
        .into()
}

/// "Check again" (user story 32): re-run the same check in place, update the
/// cache, and return the fresh result — no app reload needed to recover.
#[tauri::command]
pub fn agent_runtime_recheck(state: tauri::State<AppState>) -> AgentRuntimeStatusDto {
    let path_var = std::env::var("PATH").unwrap_or_default();
    let fresh = startup_check::check(&state.repo_root, &path_var);
    *state
        .agent_runtime_status
        .lock()
        .expect("agent_runtime_status mutex poisoned") = fresh.clone();
    fresh.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dto_serializes_to_the_ts_tagged_shape() {
        assert_eq!(
            serde_json::to_value(AgentRuntimeStatusDto::from(AgentRuntimeStatus::Ok)).unwrap(),
            serde_json::json!({ "status": "ok" })
        );
        assert_eq!(
            serde_json::to_value(AgentRuntimeStatusDto::from(AgentRuntimeStatus::NodeMissing))
                .unwrap(),
            serde_json::json!({ "status": "node-missing" })
        );
        assert_eq!(
            serde_json::to_value(AgentRuntimeStatusDto::from(
                AgentRuntimeStatus::AdapterMissing {
                    package: "@zed-industries/claude-agent-acp".to_string()
                }
            ))
            .unwrap(),
            serde_json::json!({ "status": "adapter-missing", "package": "@zed-industries/claude-agent-acp" })
        );
    }
}
