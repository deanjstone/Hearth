// Ported piece by piece across the Phase 1 PR sequence (deanjstone/Hearth#27);
// not yet wired into the app boot sequence, so unused until later chunks land.
#[allow(dead_code)]
mod selfmod;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
