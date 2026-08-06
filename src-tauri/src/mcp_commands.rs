// The `window.hearth.mcp` Tauri surface (Phase 5, tracking issue #27).
// Ported from the `mcp:*`/`connectors:active` handlers in electron/main/ipc.ts,
// backed by `mcp::registry::McpRegistry`/`mcp::probe`/`mcp::active_connectors`.
// Command names derive mechanically from Electron's channel strings, per the
// naming convention established in Chunk 5's agent_commands.rs and reused by
// every phase since.

use crate::mcp::active_connectors::{read_active_connectors, ActiveConnectors};
use crate::mcp::probe::{probe_server, ProbeResult, RealMcpProbe};
use crate::mcp::registry::{McpRegistry, McpServerConfig, McpServerInput, McpServerPatch};
use crate::mcp::to_acp::SecretLookup;
use crate::selfmod_commands::AppState;
use crate::terminal::login_path::{LoginPathResolver, RealShellQuery};
use std::sync::Arc;

/// Owns the long-lived MCP server registry for the app's lifetime, plus a
/// long-lived `LoginPathResolver` for `connectors_active`'s `cli_resolves`
/// checks. Registered via its own `app.manage(...)` call in `lib.rs`'s
/// `setup` hook, matching `SessionState`/`TerminalState`'s
/// one-managed-struct-per-subsystem precedent. The resolver is shared (not
/// built fresh per call) so its login-shell PATH cache actually helps —
/// see `active_connectors::read_active_connectors`'s own doc comment.
pub struct McpState {
    pub registry: Arc<McpRegistry>,
    pub login_resolver: LoginPathResolver<RealShellQuery>,
}

/// Stand-in `SecretLookup`: secrets storage (`safeStorage` -> `keyring`) is
/// out of scope for this MVP (spec #26's Out of Scope), the same standing
/// gap Phase 3 already left in the auth path. Every `secretKey`-bound env
/// var resolves as missing until that follow-on lands — an honest "missing
/// secret" report rather than a silent bypass. See `to_acp.rs`'s own header
/// comment.
struct NullSecretLookup;
impl SecretLookup for NullSecretLookup {
    fn get(&self, _key: &str) -> Option<String> {
        None
    }
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        Err("invalid MCP server: needs a non-empty string name".to_string())
    } else {
        Ok(())
    }
}

#[tauri::command]
pub fn mcp_list(state: tauri::State<McpState>) -> Vec<McpServerConfig> {
    state.registry.list()
}

#[tauri::command]
pub fn mcp_add(
    state: tauri::State<McpState>,
    input: McpServerInput,
) -> Result<McpServerConfig, String> {
    validate_name(&input.name)?;
    state.registry.add(input)
}

#[tauri::command]
pub fn mcp_update(
    state: tauri::State<McpState>,
    id: String,
    patch: McpServerPatch,
) -> Result<Option<McpServerConfig>, String> {
    if let Some(name) = &patch.name {
        validate_name(name)?;
    }
    state.registry.update(&id, patch)
}

#[tauri::command]
pub fn mcp_remove(state: tauri::State<McpState>, id: String) -> Result<(), String> {
    // No secret cleanup here (unlike ipc.ts's A5 step): there's no secrets
    // store yet to clean up (see NullSecretLookup's comment above).
    state.registry.remove(&id)
}

#[tauri::command]
pub fn mcp_set_enabled(
    state: tauri::State<McpState>,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    state.registry.set_enabled(&id, enabled)
}

#[tauri::command]
pub async fn mcp_test(
    state: tauri::State<'_, McpState>,
    id: String,
) -> Result<ProbeResult, String> {
    let Some(cfg) = state.registry.get(&id) else {
        return Ok(ProbeResult {
            ok: false,
            error: Some("Server not found".to_string()),
            ..Default::default()
        });
    };
    Ok(probe_server(&cfg, &NullSecretLookup, &RealMcpProbe).await)
}

/// A2: read-only view of the connectors each backend loads from its own CLI
/// config. `cwd` scopes Claude's local/project servers; defaults to the repo
/// root.
#[tauri::command]
pub fn connectors_active(
    state: tauri::State<AppState>,
    mcp: tauri::State<McpState>,
    cwd: Option<String>,
) -> ActiveConnectors {
    let resolved = cwd
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| state.repo_root.clone());
    read_active_connectors(&resolved, &mcp.login_resolver)
}
