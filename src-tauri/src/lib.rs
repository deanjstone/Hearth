// Ported piece by piece across the Phase 1 PR sequence (deanjstone/Hearth#27).
// Closing Phase 3's exit-criterion gap (spec #48): `agent_prompt` now routes
// through `TurnCoordinator` (self-mod's commit/typecheck wrapping) backed by
// a real `SessionStore`, so `capture_turn`'s commit path, the real typecheck
// runner, and `OverlayClient` are all live — `RunTracker`'s live subagent
// attribution (self-mod:activity) is the one remaining TS behavior with no
// Rust wiring yet (self_mod.ts's own onActivity/onValidation preload stubs
// still no-op).
mod agent_commands;
mod agents;
mod agents_commands;
mod bridge;
mod fs_commands;
mod git_commands;
mod git_panel;
mod mcp;
mod mcp_commands;
mod micro_apps;
mod micro_apps_commands;
mod misc_commands;
mod ready;
mod reload_driver_tauri;
mod reveal;
mod routines;
mod routines_commands;
#[allow(dead_code)]
mod selfmod;
mod selfmod_commands;
mod sessions;
mod sessions_commands;
mod skills;
mod skills_commands;
mod soul;
mod soul_commands;
mod terminal;
mod terminal_commands;
mod turn_coordinator;
mod webview_hardening;
mod workspaces_commands;

use agent_commands::{AgentState, BoxedOverlayClient};
use agents::agent::{Agent, AgentAuth, AgentConfig, AgentKind};
use agents::agent_host::{AgentFactory, AgentHostBridge, AgentHostEngine};
use agents::startup_check;
use mcp::registry::McpRegistry;
use mcp_commands::McpState;
use micro_apps::broker::{CredentialBroker, NullSecretLookup};
use micro_apps::capabilities::CapabilityStore;
use micro_apps::server::MicroAppServer;
use micro_apps_commands::MicroAppsState;
use reload_driver_tauri::TauriReloadDriver;
use selfmod::boot_watchdog::{BootDecision, BootWatchdog};
use selfmod::git;
use selfmod::hmr::HmrController;
use selfmod::overlay_client::OverlayClient;
use selfmod::service::SelfModService;
use selfmod_commands::AppState;
use sessions::store::SessionStore;
use sessions_commands::SessionState;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{Emitter, Manager};
use turn_coordinator::TurnCoordinator;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            selfmod_commands::self_mod_history,
            selfmod_commands::self_mod_undo,
            selfmod_commands::self_mod_redo,
            ready::frontend_ready,
            agents_commands::agent_runtime_status,
            agents_commands::agent_runtime_recheck,
            agent_commands::agent_prompt,
            agent_commands::agent_cancel,
            agent_commands::agent_backend_get,
            agent_commands::agent_backend_set,
            agent_commands::agent_models_get,
            agent_commands::agent_model_set,
            agent_commands::agent_modes_get,
            agent_commands::agent_mode_set,
            agent_commands::agent_config_get,
            agent_commands::agent_config_set,
            agent_commands::agent_usage_get,
            agent_commands::agent_prompt_caps_get,
            agent_commands::agent_commands_get,
            agent_commands::permission_respond,
            agent_commands::auth_status,
            agent_commands::auth_login,
            agent_commands::auth_logout,
            sessions_commands::sessions_list,
            sessions_commands::sessions_search,
            sessions_commands::sessions_create,
            sessions_commands::sessions_get,
            sessions_commands::sessions_append,
            sessions_commands::sessions_rename,
            sessions_commands::sessions_set_kind,
            sessions_commands::sessions_archive,
            sessions_commands::sessions_delete,
            sessions_commands::sessions_duplicate,
            workspaces_commands::workspaces_list,
            terminal_commands::terminal_create,
            terminal_commands::terminal_write,
            terminal_commands::terminal_resize,
            terminal_commands::terminal_kill,
            mcp_commands::mcp_list,
            mcp_commands::mcp_add,
            mcp_commands::mcp_update,
            mcp_commands::mcp_remove,
            mcp_commands::mcp_set_enabled,
            mcp_commands::mcp_test,
            mcp_commands::connectors_active,
            fs_commands::fs_list,
            fs_commands::fs_read,
            fs_commands::fs_write,
            skills_commands::skills_list,
            skills_commands::skills_reveal,
            skills_commands::skills_set_enabled,
            soul_commands::personality_get,
            soul_commands::personality_set,
            soul_commands::memory_get,
            soul_commands::memory_clear,
            routines_commands::routines_list,
            routines_commands::routines_create,
            routines_commands::routines_update,
            routines_commands::routines_set_enabled,
            routines_commands::routines_delete,
            routines_commands::routines_run_now,
            misc_commands::about_info,
            misc_commands::data_reveal,
            misc_commands::logs_reveal,
            misc_commands::window_zoom_toggle,
            git_commands::git_diff,
            git_commands::git_status,
            git_commands::git_stage,
            git_commands::git_unstage,
            git_commands::git_commit,
            git_commands::git_branches,
            git_commands::git_switch_branch,
            git_commands::git_create_pr,
            micro_apps_commands::micro_app_create,
            micro_apps_commands::micro_app_list,
            micro_apps_commands::micro_app_starters,
            micro_apps_commands::micro_app_start,
            micro_apps_commands::micro_app_stop,
            micro_apps_commands::micro_app_capabilities,
            micro_apps_commands::micro_app_approve,
            micro_apps_commands::micro_app_revoke,
        ])
        .setup(|app| {
            // Not yet packaged (bundle.active is false in tauri.conf.json) — dev
            // only for now, so a packaged build's userData-seeded workspace
            // path is future work. Anchor on CARGO_MANIFEST_DIR (baked in at
            // compile time as this crate's own directory, i.e. `src-tauri/`)
            // rather than `std::env::current_dir()`: the Phase 7 cutover
            // made `pnpm dev` run `cd src-tauri && cargo tauri dev`, so the
            // process cwd is `src-tauri/` itself, not the repo root — using
            // it here silently pointed self-mod, the agent adapter
            // resolver, and the agent's own session cwd at a directory with
            // no `.git` and no `node_modules`, breaking all three.
            let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("src-tauri crate always has a parent directory (the repo root)")
                .to_path_buf();

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

            // W2 (Phase 6, tracking issue #27) — see
            // webview_hardening.rs's own header comment for why every
            // WebviewWindow needs this call, not just this one.
            webview_hardening::deny_all_permissions(&window)?;

            let driver = TauriReloadDriver::new(window);
            // Vite serves the renderer in dev — see HmrController's own doc
            // comment for why that means the covered/full-reload morph path is
            // skipped (nothing built one for Tauri yet) in favor of a plain
            // window reload.
            let hmr = HmrController::new(driver, true);
            let bridge_repo_root = repo_root.clone();
            let self_mod = SelfModService::new(repo_root.clone(), hmr);

            // Startup Node/adapter check (Chunk 4, spec #48): eager, once,
            // cached — see startup_check.rs's doc comment for why (agent-chat
            // only, not a whole-app gate). PATH is read once here rather than
            // inside `check()` itself so the pure function stays fixture-testable.
            let path_var = std::env::var("PATH").unwrap_or_default();
            let agent_runtime_status = startup_check::check(&repo_root, &path_var);
            if agent_runtime_status != startup_check::AgentRuntimeStatus::Ok {
                eprintln!("[hearth] agent runtime unavailable at startup: {agent_runtime_status:?}");
            }

            // --- Agent chat (Chunk 5, spec #48): construct the real
            // AgentHostEngine, wiring HostEvent -> Tauri events via
            // agent_commands::emit_host_event. The OnceLock breaks the
            // construction cycle (emit_host_event needs the engine handle
            // itself, for permission auto-reject, but the closure is built
            // before AgentHostEngine::new returns it) — see
            // emit_host_event's doc comment. Claude is the initial backend,
            // matching Electron's own default.
            let agent_repo_root = repo_root.clone();
            let engine_cell: Arc<OnceLock<Arc<AgentHostEngine>>> = Arc::new(OnceLock::new());
            let emit_cell = engine_cell.clone();
            let emit_handle = app.handle().clone();
            let factory: AgentFactory = Box::new(move |kind| {
                let cwd = agent_repo_root.to_string_lossy().into_owned();
                let config = AgentConfig { kind, cwd, auth: AgentAuth::Subscription };
                match kind {
                    AgentKind::Claude => {
                        Arc::new(agents::claude::new_agent(config, agent_repo_root.clone())) as Arc<dyn Agent>
                    }
                    AgentKind::Codex => {
                        Arc::new(agents::codex::new_agent(config, agent_repo_root.clone())) as Arc<dyn Agent>
                    }
                }
            });
            let agent_host = AgentHostEngine::new(factory, AgentKind::Claude, move |event| {
                agent_commands::emit_host_event(&emit_handle, &emit_cell, event);
            });
            engine_cell.set(agent_host.clone()).ok();

            // --- Session persistence + self-mod-wrapped turns: closes
            // Phase 3's exit-criterion gap (spec #48: "chat with Claude/Codex
            // works through the Tauri build"). `SessionStore` lives in its
            // own app-scoped data dir (Tauri's `app_data_dir`, like the boot
            // watchdog marker above) — separate from Electron's `userData`
            // sessions, since the two builds have different app identifiers;
            // no session continuity between them is expected during
            // development. `OverlayClient`'s dev URL is the same fixed
            // `http://localhost:5173` `tauri.conf.json` already declares
            // (HmrController's own `vite_served: true` above makes the same
            // dev-only assumption).
            let sessions_dir = app.path().app_data_dir()?.join("sessions");
            let session_store = Arc::new(SessionStore::new(sessions_dir));
            app.manage(SessionState { store: session_store });

            let agent_bridge = Arc::new(AgentHostBridge::new(agent_host.clone()));
            let overlay: Arc<BoxedOverlayClient> = Arc::new(OverlayClient::new(
                Box::new(|| Some("http://localhost:5173".to_string())) as Box<dyn Fn() -> Option<String> + Send + Sync>,
            ));
            app.manage(AgentState {
                host: agent_host,
                bridge: agent_bridge,
                turn_coordinator: Arc::new(TurnCoordinator::new()),
                overlay,
            });

            app.manage(AppState {
                self_mod,
                boot_watchdog,
                repo_root,
                agent_runtime_status: Mutex::new(agent_runtime_status),
            });

            // Terminal (Phase 4, tracking issue #27): a real PTY per panel,
            // output streamed to the renderer keyed by id. Mirrors
            // electron/main/ipc.ts's own `new TerminalManager(...)` +
            // app.manage() pattern already used for AgentState/SessionState
            // above — its own managed struct, not folded into AppState.
            app.manage(terminal_commands::TerminalState {
                manager: terminal_commands::new_terminal_manager(app.handle().clone()),
            });

            // MCP registry (Phase 5, tracking issue #27): user-configured
            // MCP servers, JSON-persisted in their own app-scoped file —
            // mirrors electron/main/index.ts's `new McpRegistry(join(dataDir,
            // 'mcp-servers.json'))` + app.manage() pattern above. The
            // `LoginPathResolver` here is its own long-lived instance (not
            // shared with `TerminalState`'s) so its login-shell PATH cache
            // actually pays off across repeated `connectors_active` calls.
            let mcp_path = app.path().app_data_dir()?.join("mcp-servers.json");
            app.manage(McpState {
                registry: Arc::new(McpRegistry::new(mcp_path)),
                login_resolver: terminal::login_path::LoginPathResolver::new(
                    terminal::login_path::RealShellQuery,
                    cfg!(target_os = "windows"),
                ),
            });

            // The agent's view_app/read_ui/click/fill/eval_js bridge
            // (Hearth#27 Phase 2) — a loopback HTTP server, same shape as
            // electron/main/agent-bridge.ts. Needs the "main" window to
            // already exist (it's used for both the default snapshot target
            // and eval_js), so this runs after the window above is created.
            bridge::start(app.handle().clone(), bridge_repo_root);

            // Micro-app sandbox (Phase 6, tracking issue #27): egress
            // capability grants (W6), JSON-persisted in their own
            // app-scoped file mirroring McpRegistry's own pattern just
            // above; the credential broker (W7), started eagerly like
            // bridge::start so its loopback origin is stable for the whole
            // app lifetime; and the Vite-dev-server + CSP-proxy
            // orchestrator (micro_apps_commands::MicroAppsState) the
            // `micro_app_*` commands operate on. `NullSecretLookup`: real
            // secrets-backed credential injection is out of scope for this
            // MVP (spec #26's Out of Scope), same standing gap
            // mcp/to_acp.rs's own `NullSecretLookup` already carries — every
            // `microapp.<origin>`-keyed credential honestly reports absent
            // rather than silently bypassing auth.
            let capabilities_path = app.path().app_data_dir()?.join("micro-app-capabilities.json");
            let capabilities = Arc::new(CapabilityStore::new(capabilities_path));
            let broker = Arc::new(CredentialBroker::new(capabilities.clone(), Arc::new(NullSecretLookup)));
            if let Err(e) = broker.start() {
                eprintln!("[hearth] micro-app credential broker failed to start: {e}");
            }
            app.manage(MicroAppsState::new(Arc::new(MicroAppServer::new()), capabilities, broker));

            // Routines (Phase 7, tracking issue #27): scheduled/automated
            // agent runs, JSON-persisted in their own app-scoped file
            // mirroring McpRegistry's/CapabilityStore's own pattern above.
            // `on_due` pushes over the same `routines:due` channel string
            // Electron's `scheduler.ts` construction site sends on.
            let routines_dir = app.path().app_data_dir()?.join("routines");
            let routine_store = Arc::new(routines::store::RoutineStore::new(routines_dir));
            let due_handle = app.handle().clone();
            let routine_scheduler = Arc::new(routines::scheduler::RoutineScheduler::new(
                routine_store.clone(),
                move |routine| {
                    let _ = due_handle.emit("routines:due", routine);
                },
            ));
            routines::scheduler::spawn_ticker(routine_scheduler.clone(), std::time::Duration::from_secs(30));
            app.manage(routines_commands::RoutinesState {
                store: routine_store,
                scheduler: routine_scheduler,
            });

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // Clean shutdown teardown (spec #48's Permission round-trip
            // decision: "AgentHost::teardown() called by backend switch,
            // reconnect, and shutdown") — settles any outstanding permission
            // asks/in-flight turns with a clean error instead of leaving them
            // hanging, and aborts the event-drain task before the process
            // exits.
            if let tauri::RunEvent::ExitRequested { .. } = event {
                // Mirrors electron/main/ipc.ts's `before-quit` handler:
                // `terminals.disposeAll()` then `host.dispose()`.
                if let Some(state) = app_handle.try_state::<terminal_commands::TerminalState>() {
                    state.manager.dispose_all();
                }
                if let Some(state) = app_handle.try_state::<AgentState>() {
                    let host = state.host.clone();
                    tauri::async_runtime::block_on(host.dispose());
                }
                // Every running micro-app's Vite server + CSP proxy (Phase
                // 6, tracking issue #27) — mirrors ipc.ts's own before-quit
                // handler not leaking child processes/listeners past app
                // exit.
                if let Some(state) = app_handle.try_state::<MicroAppsState>() {
                    state.stop_all();
                }
            }
        });
}
