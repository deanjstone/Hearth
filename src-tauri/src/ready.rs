// The "new frontend-ready event" called for by Hearth#27 Phase 1's IPC scope.
//
// TS confirms the boot watchdog once the window reaches Electron's
// `did-finish-load` — a native webContents event that fires once the HTML
// document has loaded. That's a coarser signal than we want: it fires once
// the page is *parsed*, not once the renderer has actually mounted and run
// without throwing. A renderer that crashes during React mount would still
// have fired `did-finish-load` moments earlier, so the watchdog would wrongly
// consider the boot healthy.
//
// This command inverts that: the renderer itself calls `frontendReady()`
// once mounted, so a boot that crashes before mounting never confirms —
// exactly the case the watchdog exists to catch.

use crate::selfmod_commands::AppState;

#[tauri::command]
pub fn frontend_ready(state: tauri::State<AppState>) {
    state.boot_watchdog.confirm_ready();
}
