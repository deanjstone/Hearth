// The `window.hearth.microApps` Tauri surface (Phase 6, tracking issue #27).
// Ported from the `micro-app:*` handlers in electron/main/ipc.ts, backed by
// micro_apps/{server,csp_proxy,capabilities,scaffold,broker}.rs. Command
// names derive mechanically from Electron's channel strings (colons ->
// underscores), same convention terminal_commands.rs/mcp_commands.rs
// already established.
//
// The one real architectural departure from the TS handlers: `microAppStart`
// there hands the renderer Vite's own URL directly (Electron enforces CSP
// via a session-wide `webRequest` hook instead). Here, `micro_app_start`
// starts (or reuses) a `csp_proxy::CspProxy` in front of that same Vite
// server and hands back the PROXY's URL — see csp_proxy.rs's own header
// comment for why a proxy is the enforcement point on Tauri/WebKitGTK.

use crate::micro_apps::broker::CredentialBroker;
use crate::micro_apps::capabilities::{AppCapabilities, CapabilityStore};
use crate::micro_apps::csp_proxy::{self, CspProxy, CspProxyDeps};
use crate::micro_apps::scaffold::{list_starters, scaffold_micro_app, ScaffoldResult, StarterInfo};
use crate::micro_apps::server::{MicroAppInfo, MicroAppServer};
use crate::selfmod_commands::AppState;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Registered via `app.manage(...)` in `lib.rs`'s `setup()`, alongside
/// `AppState`/`AgentState`/`TerminalState`/`McpState` — each subsystem gets
/// its own managed struct.
pub struct MicroAppsState {
    // `pub`, matching McpState's/TerminalState's own managed-struct
    // convention (mcp_commands.rs, terminal_commands.rs) — each is a
    // shared, already-Arc'd handle a caller outside this file could
    // legitimately want direct access to (none does yet).
    pub servers: Arc<MicroAppServer>,
    pub capabilities: Arc<CapabilityStore>,
    pub broker: Arc<CredentialBroker>,
    /// One CSP proxy per currently-embedded app, keyed by name. Not folded
    /// into `MicroAppServer` — that module owns the Vite process lifecycle
    /// only; this is the orchestration layer pairing a running Vite server
    /// with its own proxy. Kept private (unlike the fields above): it's
    /// mutated in place behind its own lock, not a plain shared handle, so
    /// only this file's own commands should ever touch it.
    proxies: Mutex<HashMap<String, CspProxy>>,
}

impl MicroAppsState {
    pub fn new(
        servers: Arc<MicroAppServer>,
        capabilities: Arc<CapabilityStore>,
        broker: Arc<CredentialBroker>,
    ) -> Self {
        Self {
            servers,
            capabilities,
            broker,
            proxies: Mutex::new(HashMap::new()),
        }
    }

    /// Mirrors `terminal_commands.rs`'s `TerminalState`/`AgentState`
    /// teardown called from `lib.rs`'s `ExitRequested` handler.
    pub fn stop_all(&self) {
        self.servers.stop_all();
        let mut proxies = self.proxies.lock().unwrap();
        for (_, proxy) in proxies.drain() {
            proxy.stop();
        }
    }
}

#[tauri::command]
pub fn micro_app_create(
    name: String,
    starter: Option<String>,
    state: tauri::State<AppState>,
) -> Result<ScaffoldResult, String> {
    scaffold_micro_app(&state.repo_root, &name, starter.as_deref())
}

#[tauri::command]
pub fn micro_app_list(
    state: tauri::State<AppState>,
    micro_apps: tauri::State<MicroAppsState>,
) -> Vec<MicroAppInfo> {
    micro_apps.servers.list(&state.repo_root)
}

#[tauri::command]
pub fn micro_app_starters(state: tauri::State<AppState>) -> Vec<StarterInfo> {
    list_starters(&state.repo_root)
}

#[tauri::command]
pub async fn micro_app_start(
    name: String,
    state: tauri::State<'_, AppState>,
    micro_apps: tauri::State<'_, MicroAppsState>,
) -> Result<String, String> {
    let vite_url = micro_apps
        .servers
        .ensure_started(&state.repo_root, &name)
        .await?;
    let vite_parsed = url::Url::parse(&vite_url)
        .map_err(|e| format!("micro-app {name}: unparseable dev URL {vite_url}: {e}"))?;
    // Whatever host Vite itself reported (usually "localhost"), not a
    // hardcoded "127.0.0.1" — Vite (no --host flag) binds whatever
    // "localhost" resolves to on this machine, which on some CI runners is
    // ::1 (IPv6-only). csp_proxy::start connects by this name so tokio's own
    // resolver matches whatever Vite actually bound to.
    let upstream_host = vite_parsed
        .host_str()
        .ok_or_else(|| format!("micro-app {name}: dev URL {vite_url} has no host"))?
        .to_string();
    let upstream_port = vite_parsed
        .port()
        .ok_or_else(|| format!("micro-app {name}: dev URL {vite_url} has no port"))?;

    // Lock scoped to just the lookup so it's released before the `.await`
    // below (a std Mutex guard can't be held across one). A cached proxy
    // whose upstream host:port no longer matches is stale — Vite crashed
    // and was respawned on a different port (server.rs's own reap-on-read)
    // since this proxy was created — so it's dropped rather than reused; a
    // proxy silently forwarding to whatever now occupies its old upstream
    // port would be worse than the extra restart.
    let existing_port = micro_apps.proxies.lock().unwrap().get(&name).and_then(|p| {
        (p.upstream_host() == upstream_host && p.upstream_port() == upstream_port).then(|| p.port())
    });
    if existing_port.is_none() {
        if let Some(stale) = micro_apps.proxies.lock().unwrap().remove(&name) {
            stale.stop();
        }
    }
    let proxy_port = match existing_port {
        Some(port) => port,
        None => {
            let capabilities = micro_apps.capabilities.clone();
            let broker = micro_apps.broker.clone();
            let broker_origin: Arc<dyn Fn() -> Option<String> + Send + Sync> =
                Arc::new(move || broker.origin());
            let proxy = csp_proxy::start(
                upstream_host,
                upstream_port,
                CspProxyDeps {
                    app_name: name.clone(),
                    capabilities,
                    broker_origin,
                },
            )
            .await?;
            let port = proxy.port();
            micro_apps
                .proxies
                .lock()
                .unwrap()
                .insert(name.clone(), proxy);
            port
        }
    };

    // Hand the frame its per-app broker token + origin so it can make
    // authed calls without ever holding the secret (mirrors ipc.ts's own
    // microAppStart handler). Passed as query params the app reads from
    // location.search.
    let mut url = url::Url::parse(&format!("http://127.0.0.1:{proxy_port}"))
        .expect("well-formed loopback URL");
    if let Some(broker_origin) = micro_apps.broker.origin() {
        let token = micro_apps.broker.token_for(&name);
        url.query_pairs_mut()
            .append_pair("__hearthBroker", &broker_origin)
            .append_pair("__hearthToken", &token);
    }
    Ok(url.to_string())
}

#[tauri::command]
pub fn micro_app_stop(name: String, micro_apps: tauri::State<MicroAppsState>) {
    micro_apps.servers.stop(&name);
    if let Some(proxy) = micro_apps.proxies.lock().unwrap().remove(&name) {
        proxy.stop();
    }
}

#[tauri::command]
pub fn micro_app_capabilities(
    name: String,
    state: tauri::State<AppState>,
    micro_apps: tauri::State<MicroAppsState>,
) -> AppCapabilities {
    micro_apps
        .capabilities
        .capabilities(&state.repo_root, &name)
}

#[tauri::command]
pub fn micro_app_approve(
    name: String,
    hosts: Vec<String>,
    micro_apps: tauri::State<MicroAppsState>,
) -> Result<(), String> {
    micro_apps.capabilities.approve(&name, &hosts)
}

#[tauri::command]
pub fn micro_app_revoke(
    name: String,
    host: Option<String>,
    micro_apps: tauri::State<MicroAppsState>,
) -> Result<(), String> {
    micro_apps.capabilities.revoke(&name, host.as_deref())
}
