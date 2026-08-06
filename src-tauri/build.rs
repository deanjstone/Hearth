// Declares the app-defined commands (selfmod_commands.rs, ready.rs,
// micro_apps_commands.rs) to tauri-build's ACL generator, so `allow-*`
// permissions exist for capabilities/default.json to grant. Without this,
// Tauri's runtime permission check rejects every invoke of these commands
// with "not allowed... Command not found" — discovered while wiring the
// permanent tauri-driver + WebDriverIO CI suite (Hearth#27 Phase 1,
// e2e-tests/), which is the first thing to actually drive
// window.hearth.selfMod.* end to end, and again in Phase 6
// (e2e-tests/specs/micro-app-csp-proxy.spec.js), the first e2e coverage of
// any command outside that original Phase 1 set.
//
// NOTE: agent_commands.rs/agents_commands.rs/sessions_commands.rs/
// terminal_commands.rs/mcp_commands.rs/workspaces_commands.rs (Phases 2-5)
// are NOT in this list either, and so hit this exact same ACL rejection —
// undiscovered until now because no e2e spec before this one invoked a
// non-Phase-1 command through the real `invoke()` path (bridge.rs's
// eval_js bridge those earlier specs use is a separate loopback HTTP+JS-eval
// mechanism that bypasses Tauri's ACL entirely). Filed as Hearth#54 rather
// than fixed here — out of scope for this phase, and each phase's command
// list is its own file to touch carefully, not a batch edit worth risking
// in the same change as this phase's own fix.
fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "self_mod_history",
            "self_mod_undo",
            "self_mod_redo",
            "frontend_ready",
            "micro_app_create",
            "micro_app_list",
            "micro_app_starters",
            "micro_app_start",
            "micro_app_stop",
            "micro_app_capabilities",
            "micro_app_approve",
            "micro_app_revoke",
        ]),
    ))
    .expect("failed to run tauri-build")
}
