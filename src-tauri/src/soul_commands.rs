// The `window.hearth.personality`/`window.hearth.memory` Tauri surface
// (Phase 7, tracking issue #27). Ported from the `personality:*`/`memory:*`
// handlers in electron/main/ipc.ts, backed by `soul::personality::SoulService`.
// `personality:set`'s commit-and-return-changed-paths step reuses
// `SelfModService::commit_managed`, already ported for exactly this call
// site (see its own doc comment).

use crate::selfmod::git::SelfModKind;
use crate::selfmod_commands::AppState;
use crate::soul::personality::{SoulConfig, DEFAULT_SOUL};
use serde::Serialize;
use std::fs;

const PERSONALITY_REL_PATH: &str = ".hearth/personality.json";

fn personality_path(state: &AppState) -> std::path::PathBuf {
    state.repo_root.join(PERSONALITY_REL_PATH)
}

#[tauri::command]
pub fn personality_get(state: tauri::State<AppState>) -> SoulConfig {
    let path = personality_path(&state);
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<SoulConfig>(&s).ok())
        .unwrap_or(DEFAULT_SOUL)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalitySetResult {
    pub commit: String,
    pub changed_paths: Vec<String>,
}

#[tauri::command]
pub fn personality_set(
    state: tauri::State<AppState>,
    config: SoulConfig,
) -> Result<PersonalitySetResult, String> {
    let path = personality_path(&state);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    fs::write(&path, format!("{json}\n")).map_err(|e| e.to_string())?;

    crate::soul::personality::SoulService::new()
        .set_personality(&config)
        .map_err(|e| e.to_string())?;

    let (commit, changed_paths) = state
        .self_mod
        .commit_managed(
            &[PERSONALITY_REL_PATH.to_string()],
            "update personality",
            SelfModKind::Soul,
        )
        .map_err(|e| e.to_string())?;
    Ok(PersonalitySetResult {
        commit,
        changed_paths,
    })
}

#[tauri::command]
pub fn memory_get() -> String {
    crate::soul::personality::SoulService::new().get_memory("claude")
}

#[tauri::command]
pub fn memory_clear() -> Result<(), String> {
    crate::soul::personality::SoulService::new().set_memory("")
}
