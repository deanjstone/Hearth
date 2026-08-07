// Three small, unrelated `window.hearth.*` surfaces (Phase 7, tracking issue
// #27), grouped in one file since each is a one-line OS-integration action
// in the Electron original (`ipc.ts` lines 489-528) — not worth a dedicated
// module apiece.

use crate::selfmod_commands::AppState;
use serde::Serialize;
use std::fs;
use tauri::Manager;

// --- about:info ----------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AboutInfo {
    pub app: String,
    pub tauri: String,
    pub acp_sdk: Option<String>,
    pub claude_adapter: Option<String>,
    pub codex_adapter: Option<String>,
}

fn npm_package_version(repo_root: &std::path::Path, pkg: &str) -> Option<String> {
    let text = fs::read_to_string(
        repo_root
            .join("node_modules")
            .join(pkg)
            .join("package.json"),
    )
    .ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("version")?.as_str().map(|s| s.to_string())
}

#[tauri::command]
pub fn about_info(app: tauri::AppHandle, state: tauri::State<AppState>) -> AboutInfo {
    AboutInfo {
        app: app.package_info().version.to_string(),
        tauri: tauri::VERSION.to_string(),
        acp_sdk: npm_package_version(&state.repo_root, "@agentclientprotocol/sdk"),
        claude_adapter: npm_package_version(&state.repo_root, "@zed-industries/claude-agent-acp"),
        codex_adapter: npm_package_version(&state.repo_root, "@agentclientprotocol/codex-acp"),
    }
}

// --- data:reveal / logs:reveal --------------------------------------------

#[tauri::command]
pub fn data_reveal(app: tauri::AppHandle) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    crate::reveal::reveal_dir(&dir)
}

#[tauri::command]
pub fn logs_reveal(app: tauri::AppHandle) -> Result<(), String> {
    let log_file = app
        .path()
        .app_log_dir()
        .map_err(|e| e.to_string())?
        .join("hearth.log");
    crate::reveal::reveal_file(&log_file)
}

// --- window:zoom-toggle ----------------------------------------------------

/// Double-clicking the title-bar strip zooms the window to fill the screen,
/// and again restores the previous frame — `maximize`/`unmaximize` remembers
/// the prior bounds for us, mirroring Electron's `BrowserWindow` API 1:1.
#[tauri::command]
pub fn window_zoom_toggle(window: tauri::WebviewWindow) -> Result<(), String> {
    if window.is_maximized().map_err(|e| e.to_string())? {
        window.unmaximize().map_err(|e| e.to_string())
    } else {
        window.maximize().map_err(|e| e.to_string())
    }
}
