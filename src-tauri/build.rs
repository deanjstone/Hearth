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
// Fixed for Hearth#54 (Phase 7): agent_commands.rs/agents_commands.rs/
// sessions_commands.rs/terminal_commands.rs/mcp_commands.rs/
// workspaces_commands.rs (Phases 2-5) were missing from this list, so every
// real invoke() of those commands was rejected the same way — undiscovered
// until Phase 6's e2e spec because no e2e spec before it invoked a non-Phase-1
// command through the real `invoke()` path (bridge.rs's eval_js bridge those
// earlier specs use is a separate loopback HTTP+JS-eval mechanism that
// bypasses Tauri's ACL entirely).
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
            "agent_runtime_status",
            "agent_runtime_recheck",
            "agent_prompt",
            "agent_cancel",
            "agent_backend_get",
            "agent_backend_set",
            "agent_models_get",
            "agent_model_set",
            "agent_modes_get",
            "agent_mode_set",
            "agent_config_get",
            "agent_config_set",
            "agent_usage_get",
            "agent_prompt_caps_get",
            "agent_commands_get",
            "permission_respond",
            "auth_status",
            "auth_login",
            "auth_logout",
            "sessions_list",
            "sessions_search",
            "sessions_create",
            "sessions_get",
            "sessions_append",
            "sessions_rename",
            "sessions_set_kind",
            "sessions_archive",
            "sessions_delete",
            "sessions_duplicate",
            "workspaces_list",
            "terminal_create",
            "terminal_write",
            "terminal_resize",
            "terminal_kill",
            "mcp_list",
            "mcp_add",
            "mcp_update",
            "mcp_remove",
            "mcp_set_enabled",
            "mcp_test",
            "connectors_active",
            // Phase 7 (tracking issue #27): files/skills/personality/memory/
            // routines/about/data/win/git — the remaining IPC surface the
            // regression pass against spec #26's user stories needs.
            "fs_list",
            "fs_read",
            "fs_write",
            "skills_list",
            "skills_reveal",
            "skills_set_enabled",
            "personality_get",
            "personality_set",
            "memory_get",
            "memory_clear",
            "routines_list",
            "routines_create",
            "routines_update",
            "routines_set_enabled",
            "routines_delete",
            "routines_run_now",
            "about_info",
            "data_reveal",
            "logs_reveal",
            "window_zoom_toggle",
            "git_diff",
            "git_status",
            "git_stage",
            "git_unstage",
            "git_commit",
            "git_branches",
            "git_switch_branch",
            "git_create_pr",
        ]),
    ))
    .expect("failed to run tauri-build")
}
