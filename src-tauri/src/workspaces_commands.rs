// A deliberately minimal `window.hearth.workspaces` Tauri surface — NOT a
// port of `electron/main/workspaces/registry.ts`'s full CRUD (multiple
// user-added workspaces, git-status polling, persisted registry file). That's
// a separate, larger concern spec #48 doesn't ask for.
//
// This exists only to unblock `src/app/sessions.ts`'s `ensureActiveSession()`,
// whose empty-session-list fallback calls `workspaces.list()` to find the
// Hearth workspace to start a session in — closing Phase 3's exit-criterion
// gap (spec #48: "chat with Claude/Codex works through the Tauri build")
// needs that path to not throw, not a full workspace picker. Always returns
// exactly one entry: the Hearth repo itself, matching
// `registry.ts`'s own `HEARTH_ID`/`isHearth` shape for the one workspace this
// port's dev-only, single-repo `AppState.repo_root` actually has.

use crate::selfmod_commands::AppState;
use serde::Serialize;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub path: String,
    pub is_hearth: bool,
}

#[tauri::command]
pub fn workspaces_list(state: tauri::State<AppState>) -> Vec<Workspace> {
    vec![Workspace {
        id: "hearth".to_string(),
        name: "Hearth".to_string(),
        path: state.repo_root.to_string_lossy().into_owned(),
        is_hearth: true,
    }]
}
