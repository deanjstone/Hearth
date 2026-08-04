// Declares the app-defined commands (selfmod_commands.rs, ready.rs) to
// tauri-build's ACL generator, so `allow-*` permissions exist for
// capabilities/default.json to grant. Without this, Tauri's runtime
// permission check rejects every invoke of these commands with "not
// allowed... Plugin not found" — discovered while wiring the permanent
// tauri-driver + WebDriverIO CI suite (Hearth#27 Phase 1, e2e-tests/), which
// is the first thing to actually drive window.hearth.selfMod.* end to end.
fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "self_mod_history",
            "self_mod_undo",
            "self_mod_redo",
            "frontend_ready",
        ]),
    ))
    .expect("failed to run tauri-build")
}
