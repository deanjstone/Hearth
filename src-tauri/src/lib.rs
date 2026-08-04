// Ported piece by piece across the Phase 1 PR sequence (deanjstone/Hearth#27).
// This chunk wires the Phase 1 issue's "Minimal IPC" scope into real app
// boot: `window.hearth.selfMod.{history,undo,redo}` + the frontend-ready
// event + the boot watchdog's revert-on-bricked-boot check. That reaches a
// meaningful slice of `selfmod` (see selfmod_commands.rs, ready.rs,
// lib.rs::run) but not all of it — `capture_turn`'s commit path, the real
// typecheck runner, RunTracker's live subagent attribution, and OverlayClient
// all wait on `turn_coordinator`/`agents` being wired through IPC, which
// needs a real `AgentHost` (Phase 3, ACP agent runtime). Both modules keep
// `#[allow(dead_code)]` until that lands.
#[allow(dead_code)]
mod agents;
mod ready;
mod reload_driver_tauri;
#[allow(dead_code)]
mod selfmod;
mod selfmod_commands;
#[allow(dead_code)]
mod turn_coordinator;

use reload_driver_tauri::TauriReloadDriver;
use selfmod::boot_watchdog::{BootDecision, BootWatchdog};
use selfmod::git;
use selfmod::hmr::HmrController;
use selfmod::service::SelfModService;
use selfmod_commands::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            selfmod_commands::self_mod_history,
            selfmod_commands::self_mod_undo,
            selfmod_commands::self_mod_redo,
            ready::frontend_ready,
        ])
        .setup(|app| {
            // Not yet packaged (bundle.active is false in tauri.conf.json) — dev
            // only for now, so the dev branch of TS's REPO_ROOT resolution
            // (`process.cwd()`) is the whole story; a packaged build's
            // userData-seeded workspace path is future work.
            let repo_root = std::env::current_dir()?;

            // Boot watchdog (W6): if the previous self-mod restart never reached
            // ready, it bricked boot — auto-revert that commit before the window
            // (already pointed at the dev server declaratively, via
            // tauri.conf.json) loads it.
            let marker_path = app
                .path()
                .app_data_dir()?
                .join("pending-self-mod-restart.json");
            let boot_watchdog = BootWatchdog::new(marker_path);
            match boot_watchdog.inspect_boot() {
                BootDecision::Revert { commit, attempt } => {
                    eprintln!("[hearth] boot watchdog: reverting {commit} (attempt {attempt})");
                    if let Err(e) = git::revert_commit(&repo_root, &commit) {
                        eprintln!("[hearth] boot watchdog: revert of {commit} failed: {e}");
                    }
                    // In dev, cargo/tauri already own the process lifecycle — the
                    // revert above is enough; no relaunch (mirrors TS's
                    // `!app.isPackaged` fallthrough in electron/main/index.ts).
                }
                BootDecision::SafeMode { commit } => {
                    eprintln!(
                        "[hearth] boot watchdog: self-mod restart bricked boot repeatedly (commit {commit}); booting current state without further auto-revert"
                    );
                }
                BootDecision::None => {}
            }

            let window = app
                .get_webview_window("main")
                .expect("the \"main\" window is declared in tauri.conf.json");
            let driver = TauriReloadDriver::new(window);
            // Vite serves the renderer in dev — see HmrController's own doc
            // comment for why that means the covered/full-reload morph path is
            // skipped (nothing built one for Tauri yet) in favor of a plain
            // window reload.
            let hmr = HmrController::new(driver, true);
            let self_mod = SelfModService::new(repo_root, hmr);

            app.manage(AppState {
                self_mod,
                boot_watchdog,
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
