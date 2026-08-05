// The `window.hearth.agent`/`window.hearth.permission`/`window.hearth.auth`
// Tauri surface (Chunk 5, spec deanjstone/Hearth#48) — the final piece of
// Phase 3. Ported from the `agent*`/`permission*`/`auth*` handlers in
// electron/main/ipc.ts, backed by the `AgentHostEngine` built in Chunk 2b and
// the real ACP connection built in Chunk 3b.
//
// Command names derive mechanically from Electron's channel strings
// (electron/shared/channels.ts), colons becoming underscores, per spec #48's
// "Command/event naming" decision. Events keep the Electron channel strings
// verbatim, since Tauri events are plain strings with no naming conflict to
// resolve.
//
// **Scope decision**: `agent_prompt` calls `AgentHostEngine::prompt` directly
// rather than routing through `TurnCoordinator` (which wraps every Electron
// turn in self-mod's commit/typecheck machinery). Electron's `agent:prompt`
// handler is literally `turns.runTurn(payload)` — full parity would need that
// wrapping too — but `TurnCoordinator` needs a real `SessionMetaStore`
// (session persistence), which isn't ported to Tauri at all yet (neither is
// `window.hearth.sessions.*`, which the renderer needs regardless to load a
// session before it can prompt). Wiring self-mod-turn integration is deferred
// to whichever future chunk also ports session storage; this chunk is scoped
// to the plain ACP chat surface spec #48 describes. `agent_prompt` therefore
// returns `Result<(), String>` here, not Electron's `SelfModResult | null` —
// there's no self-mod commit to report.

use crate::agents::agent::{
    AgentErrorPayload, AgentKind, AgentUpdatePayload, AuthState, AvailableCommand, BackendStatus,
    ConfigOption, ConfigValue, ModeState, ModelState, PermissionOptionKind, PermissionRequest,
    PermissionRequestPayload, PromptCapabilities, PromptImage, Usage,
};
use crate::agents::agent_host::{AgentHostEngine, HostEvent, HostPromptOptions};
use crate::agents::login_presence;
use crate::selfmod::shell_guard::is_source_mutating_shell;
use serde::Serialize;
use serde_json::Value;
use std::sync::{Arc, OnceLock};
use tauri::{AppHandle, Emitter};

/// Owns the long-lived agent-chat engine for the app's lifetime. Registered
/// via a second `app.manage(...)` call in `lib.rs`'s `setup` hook (separate
/// from `selfmod_commands::AppState` — the two subsystems don't share state,
/// and Tauri supports multiple managed types natively).
pub struct AgentState {
    pub host: Arc<AgentHostEngine>,
}

#[derive(Serialize)]
pub struct AuthCommandDto {
    pub command: String,
}

/// Source-write enforcement (W0b, user story 21): auto-reject a permission
/// ask for shell that mutates repo source, so writes are forced onto the
/// mediated Edit/Write path without bothering the user (ported from ipc.ts's
/// `host.onPermission` handler). Returns `true` when it handled the ask
/// itself — the caller must not also forward it to the renderer.
fn auto_reject_source_mutating_shell(
    engine_cell: &OnceLock<Arc<AgentHostEngine>>,
    request: &PermissionRequest,
) -> bool {
    let source_mutating = request
        .command
        .as_deref()
        .map(is_source_mutating_shell)
        .unwrap_or(false);
    if !source_mutating {
        return false;
    }
    let Some(reject) = request
        .options
        .iter()
        .find(|o| o.kind == PermissionOptionKind::Reject)
    else {
        return false;
    };
    let Some(engine) = engine_cell.get() else {
        return false;
    };
    let engine = engine.clone();
    let id = request.id.clone();
    let option_id = reject.id.clone();
    tauri::async_runtime::spawn(async move {
        let _ = engine.permission_respond(&id, &option_id).await;
    });
    true
}

/// Translate one `HostEvent` into the matching Electron-channel-named Tauri
/// event, applying source-write enforcement (W0b, user story 21) ahead of
/// forwarding a permission ask to the renderer. Wired as the `emit` callback
/// passed to `AgentHostEngine::new` in `lib.rs`'s `setup()`.
///
/// `engine_cell` exists to break the construction cycle: this closure is
/// built *before* `AgentHostEngine::new` returns the `Arc<AgentHostEngine>`
/// it needs to call `permission_respond` on for an auto-rejected ask, so
/// `lib.rs` hands in a shared, initially-empty cell and fills it immediately
/// after construction — nothing invokes `emit` before `new` returns, so it's
/// always populated by the time this ever runs.
pub fn emit_host_event(
    app: &AppHandle,
    engine_cell: &OnceLock<Arc<AgentHostEngine>>,
    event: HostEvent,
) {
    match event {
        HostEvent::Update {
            session_key,
            update,
        } => {
            let _ = app.emit(
                "agent:update",
                AgentUpdatePayload {
                    session_id: session_key,
                    update,
                },
            );
        }
        HostEvent::Permission {
            session_key,
            request,
        } => {
            if auto_reject_source_mutating_shell(engine_cell, &request) {
                return;
            }
            let _ = app.emit(
                "permission:request",
                PermissionRequestPayload {
                    session_id: session_key,
                    req: request,
                },
            );
        }
        HostEvent::Exit {
            session_keys,
            message,
        } => {
            // A background/routine failure must attribute to the session(s)
            // whose turns were actually in flight, not whatever session
            // happens to be foreground; an idle death (no in-flight turns)
            // still surfaces once, globally.
            let targets: Vec<Option<String>> = if session_keys.is_empty() {
                vec![None]
            } else {
                session_keys.into_iter().map(Some).collect()
            };
            for session_key in targets {
                let _ = app.emit(
                    "agent:error",
                    AgentErrorPayload {
                        session_key,
                        message: message.clone(),
                    },
                );
            }
        }
        HostEvent::ModelsChanged(state) => {
            let _ = app.emit("agent:models:changed", state);
        }
        HostEvent::ModeChanged(state) => {
            let _ = app.emit("agent:mode:changed", state);
        }
        HostEvent::ConfigChanged(options) => {
            let _ = app.emit("agent:config:changed", options);
        }
        HostEvent::UsageChanged(usage) => {
            let _ = app.emit("agent:usage:changed", usage);
        }
        HostEvent::CommandsChanged(commands) => {
            let _ = app.emit("agent:commands:changed", commands);
        }
    }
}

// --- Prompt / cancel --------------------------------------------------

#[tauri::command]
pub async fn agent_prompt(
    state: tauri::State<'_, AgentState>,
    session_id: String,
    cwd: Option<String>,
    text: String,
    images: Option<Vec<PromptImage>>,
) -> Result<(), String> {
    state
        .host
        .prompt(
            &text,
            HostPromptOptions {
                key: Some(session_id),
                cwd,
                images: images.unwrap_or_default(),
                resume_id: None,
            },
        )
        .await
        // The ACP session id `prompt` returns has nowhere to persist yet (no
        // session store) — see this file's header comment.
        .map(|_acp_session_id| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn agent_cancel(
    state: tauri::State<'_, AgentState>,
    session_id: Option<String>,
) -> Result<(), String> {
    state.host.cancel(session_id).await
}

// --- Backend switcher ---------------------------------------------------

#[tauri::command]
pub async fn agent_backend_get(state: tauri::State<'_, AgentState>) -> Result<AgentKind, String> {
    Ok(state.host.kind().await)
}

#[tauri::command]
pub async fn agent_backend_set(
    app: AppHandle,
    state: tauri::State<'_, AgentState>,
    kind: AgentKind,
) -> Result<BackendStatus, String> {
    let error = state.host.switch_to(kind).await.err();
    let status = BackendStatus {
        kind: state.host.kind().await,
        error,
    };
    let _ = app.emit("agent:backend:changed", &status);
    Ok(status)
}

// --- Models --------------------------------------------------------------

#[tauri::command]
pub async fn agent_models_get(state: tauri::State<'_, AgentState>) -> Result<ModelState, String> {
    Ok(state.host.models().await)
}

#[tauri::command]
pub async fn agent_model_set(
    state: tauri::State<'_, AgentState>,
    model_id: String,
) -> Result<(), String> {
    state.host.set_model(&model_id).await
}

// --- Modes -----------------------------------------------------------

#[tauri::command]
pub async fn agent_modes_get(state: tauri::State<'_, AgentState>) -> Result<ModeState, String> {
    Ok(state.host.modes().await)
}

#[tauri::command]
pub async fn agent_mode_set(
    state: tauri::State<'_, AgentState>,
    mode_id: String,
) -> Result<(), String> {
    state.host.set_mode(&mode_id).await
}

// --- Generic config options -----------------------------------------

#[tauri::command]
pub async fn agent_config_get(
    state: tauri::State<'_, AgentState>,
) -> Result<Vec<ConfigOption>, String> {
    Ok(state.host.config_options().await)
}

#[tauri::command]
pub async fn agent_config_set(
    state: tauri::State<'_, AgentState>,
    config_id: String,
    value: Value,
) -> Result<(), String> {
    let value = match value {
        Value::String(s) => ConfigValue::Str(s),
        Value::Bool(b) => ConfigValue::Bool(b),
        _ => return Err("config value must be a string or boolean".to_string()),
    };
    state.host.set_config_option(&config_id, value).await
}

// --- Usage -----------------------------------------------------------

#[tauri::command]
pub async fn agent_usage_get(state: tauri::State<'_, AgentState>) -> Result<Option<Usage>, String> {
    Ok(state.host.usage().await)
}

// --- Prompt capabilities / commands ----------------------------------

#[tauri::command]
pub async fn agent_prompt_caps_get(
    state: tauri::State<'_, AgentState>,
) -> Result<PromptCapabilities, String> {
    Ok(state.host.prompt_capabilities().await)
}

#[tauri::command]
pub async fn agent_commands_get(
    state: tauri::State<'_, AgentState>,
) -> Result<Vec<AvailableCommand>, String> {
    Ok(state.host.advertised_commands().await)
}

// --- Permission round-trip --------------------------------------------

#[tauri::command]
pub async fn permission_respond(
    state: tauri::State<'_, AgentState>,
    id: String,
    option_id: String,
) -> Result<(), String> {
    state.host.permission_respond(&id, &option_id).await
}

// --- Auth ---------------------------------------------------------------
//
// Subscription-only this phase (spec #48's Out of Scope — no api-key path),
// so this is a simplified pass-through compared to ipc.ts's `authStatusFor`:
// no `resolveAuth`/secrets branching, just presence-checking the inactive
// backend's stored login and reading the active backend's live connection
// state.

#[tauri::command]
pub async fn auth_status(
    state: tauri::State<'_, AgentState>,
    kind: AgentKind,
    reconnect: Option<bool>,
) -> Result<AuthState, String> {
    let host = state.host.clone();
    let base = AuthState {
        kind,
        mode: "subscription".to_string(),
        key_source: None,
        connected: false,
        error: None,
        login_present: None,
        methods: Vec::new(),
    };
    if kind != host.kind().await {
        return Ok(AuthState {
            login_present: Some(login_presence::has_stored_login_default(kind)),
            ..base
        });
    }
    if reconnect.unwrap_or(false) {
        // Best-effort, matching ipc.ts: a reconnect failure is surfaced via
        // the connect attempt below instead.
        let _ = host.reconnect().await;
    }
    match host.connect().await {
        Ok(_) => Ok(AuthState {
            connected: host.is_connected().await,
            methods: host.auth_methods().await,
            ..base
        }),
        Err(err) => Ok(AuthState {
            error: Some(err),
            ..base
        }),
    }
}

/// The login command the user runs themselves (in Hearth's terminal or their
/// own). We render no OAuth and store no subscription token.
#[tauri::command]
pub fn auth_login(kind: AgentKind) -> AuthCommandDto {
    AuthCommandDto {
        command: if kind == AgentKind::Codex {
            "codex login"
        } else {
            "claude login"
        }
        .to_string(),
    }
}

/// Subscription credential is the CLI's, not ours to delete — hand back the
/// command for the user to run. No `cleared` branch (unlike ipc.ts's
/// api-key-mode path): there's no stored secret here to clear.
#[tauri::command]
pub fn auth_logout(kind: AgentKind) -> AuthCommandDto {
    AuthCommandDto {
        command: if kind == AgentKind::Codex {
            "codex logout"
        } else {
            "claude logout"
        }
        .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_login_and_logout_report_the_right_cli_command_per_backend() {
        assert_eq!(auth_login(AgentKind::Claude).command, "claude login");
        assert_eq!(auth_login(AgentKind::Codex).command, "codex login");
        assert_eq!(auth_logout(AgentKind::Claude).command, "claude logout");
        assert_eq!(auth_logout(AgentKind::Codex).command, "codex logout");
    }
}
