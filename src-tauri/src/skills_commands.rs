// The `window.hearth.skills` Tauri surface (Phase 7, tracking issue #27).
// Ported from the `skills:*` handlers in electron/main/ipc.ts, backed by
// `skills::list`. `skillsList`'s `commands` half comes from the already-live
// agent host (`agent_commands::agent_commands_get`'s own logic), not from
// this module — mirrors ipc.ts's own `host.advertisedCommands()` call
// alongside `listSkills(cwd)`.

use crate::agents::agent::AvailableCommand;
use crate::selfmod_commands::AppState;
use crate::skills::list::{global_skills_dir, list_skills, set_skill_enabled, SkillInfo};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillsListResult {
    pub skills: Vec<SkillInfo>,
    pub commands: Vec<AvailableCommand>,
}

#[tauri::command]
pub async fn skills_list(
    state: tauri::State<'_, AppState>,
    agent: tauri::State<'_, crate::agent_commands::AgentState>,
    cwd: Option<String>,
) -> Result<SkillsListResult, String> {
    let workspace = cwd
        .map(PathBuf::from)
        .unwrap_or_else(|| state.repo_root.clone());
    Ok(SkillsListResult {
        skills: list_skills(Some(&workspace)),
        commands: agent.host.advertised_commands().await,
    })
}

#[tauri::command]
pub fn skills_reveal() -> Result<(), String> {
    crate::reveal::reveal_dir(&global_skills_dir())
}

#[tauri::command]
pub fn skills_set_enabled(path: String, enabled: bool) -> Result<String, String> {
    set_skill_enabled(&PathBuf::from(path), enabled).map(|p| p.to_string_lossy().into_owned())
}
