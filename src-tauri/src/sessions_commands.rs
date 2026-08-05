// The `window.hearth.sessions.*` Tauri surface, closing a gap in Phase 3's
// exit criterion (spec deanjstone/Hearth#48: "chat with Claude/Codex works
// through the Tauri build") — the renderer's `ChatView.tsx`/`sessions.ts`
// (`ensureActiveSession`, `sessions.get`) need this before `agent_prompt`
// (agent_commands.rs) is even reachable. Ported from the `sessions*` handlers
// in electron/main/ipc.ts, backed by `sessions::store::SessionStore`.
//
// Command names derive mechanically from Electron's channel strings
// (electron/shared/channels.ts), colons/hyphens becoming underscores, per
// spec #48's "Command/event naming" decision (established in Chunk 5's
// agent_commands.rs, applied here for the first time to a non-agent domain).

use crate::sessions::store::{
    CreateSessionInput, SessionDetail, SessionMeta, SessionSearchHit, SessionStore,
};
use std::path::Path;
use std::sync::Arc;

/// Owns the long-lived session store for the app's lifetime. Registered via
/// its own `app.manage(...)` call in `lib.rs`'s `setup` hook (separate from
/// `AppState`/`AgentState` — matches `AgentState`'s own precedent comment for
/// why this project uses one managed struct per subsystem rather than a
/// single monolithic state type).
pub struct SessionState {
    pub store: Arc<SessionStore>,
}

#[tauri::command]
pub fn sessions_list(state: tauri::State<SessionState>) -> Vec<SessionMeta> {
    state.store.list()
}

#[tauri::command]
pub fn sessions_search(state: tauri::State<SessionState>, query: String) -> Vec<SessionSearchHit> {
    state.store.search(&query)
}

/// Workspace-kind inference (ipc.ts's `sessionsCreate` handler-level logic,
/// not `SessionStore.create`'s own narrower self-only default): an explicit
/// `kind` wins; otherwise a `self` session (Hearth's own repo) or any cwd
/// containing a `.git` dir is `code`, everything else `knowledge`.
fn infer_kind(input: &CreateSessionInput) -> crate::sessions::store::WorkspaceKind {
    if input.is_self || Path::new(&input.cwd).join(".git").exists() {
        crate::sessions::store::WorkspaceKind::Code
    } else {
        crate::sessions::store::WorkspaceKind::Knowledge
    }
}

#[tauri::command]
pub fn sessions_create(
    state: tauri::State<SessionState>,
    input: CreateSessionInput,
) -> Result<SessionMeta, String> {
    let kind = Some(input.kind.unwrap_or_else(|| infer_kind(&input)));
    state.store.create(CreateSessionInput { kind, ..input })
}

#[tauri::command]
pub fn sessions_get(state: tauri::State<SessionState>, id: String) -> Option<SessionDetail> {
    state.store.get(&id)
}

#[tauri::command]
pub fn sessions_append(
    state: tauri::State<SessionState>,
    id: String,
    entries: Vec<crate::sessions::store::TranscriptEntry>,
) -> Result<(), String> {
    state.store.append(&id, &entries)
}

#[tauri::command]
pub fn sessions_rename(
    state: tauri::State<SessionState>,
    id: String,
    title: String,
) -> Result<Option<SessionMeta>, String> {
    state.store.rename(&id, &title)
}

#[tauri::command]
pub fn sessions_set_kind(
    state: tauri::State<SessionState>,
    id: String,
    kind: crate::sessions::store::WorkspaceKind,
) -> Result<Option<SessionMeta>, String> {
    state.store.set_kind(&id, kind)
}

#[tauri::command]
pub fn sessions_archive(state: tauri::State<SessionState>, id: String) -> Result<(), String> {
    state.store.archive(&id)
}

#[tauri::command]
pub fn sessions_delete(state: tauri::State<SessionState>, id: String) -> Result<(), String> {
    state.store.remove(&id)
}

#[tauri::command]
pub fn sessions_duplicate(
    state: tauri::State<SessionState>,
    id: String,
) -> Result<Option<SessionMeta>, String> {
    state.store.duplicate(&id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::store::WorkspaceKind;

    fn input(cwd: &str, is_self: bool) -> CreateSessionInput {
        CreateSessionInput {
            title: None,
            workspace_id: "ws".to_string(),
            cwd: cwd.to_string(),
            is_self,
            kind: None,
        }
    }

    #[test]
    fn infer_kind_treats_a_self_session_as_code() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            infer_kind(&input(dir.path().to_str().unwrap(), true)),
            WorkspaceKind::Code
        );
    }

    #[test]
    fn infer_kind_treats_a_git_dir_as_code_even_when_not_self() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        assert_eq!(
            infer_kind(&input(dir.path().to_str().unwrap(), false)),
            WorkspaceKind::Code
        );
    }

    #[test]
    fn infer_kind_falls_back_to_knowledge_without_git_or_self() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            infer_kind(&input(dir.path().to_str().unwrap(), false)),
            WorkspaceKind::Knowledge
        );
    }
}
