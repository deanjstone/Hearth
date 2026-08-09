// The live ACP connection (Chunk 3b, spec deanjstone/Hearth#48) — spawns the
// adapter subprocess and drives the real wire protocol via the
// `agent-client-protocol` crate, translating through `acp_translate.rs`.
// Ported from `electron/main/agents/acp-client.ts` + `acp-agent.ts`. TS splits
// "shared connection plumbing" (acp-agent.ts, incl. `resolveAdapterBin`) from
// "the SDK-facing client" (acp-client.ts); merged here since Rust has no
// equivalent of TS's thin-subclass inheritance — `claude.rs`/`codex.rs` just
// build an `AcpClient` with a different `resolve_spec` closure and `AgentKind`.
//
// # Extracting a long-lived connection handle
//
// The crate's connection API is builder/dispatch-shaped: everything happens
// inside `Client.builder()...connect_with(transport, |connection| async move
// { ... })`, and that outer future only resolves once the closure's future
// does. But `Agent::new_session`/`AgentSession::prompt`/etc. are independent
// calls made at arbitrary later times — not all inside one synchronous chain.
// `ConnectionTo<Counterpart>` is `Clone` (confirmed by reading the crate
// source — it wraps channel senders and shared registries, not a raw
// transport handle), so `connect()` spawns `connect_with(...)` as a
// background task whose closure sends a *clone* of the connection out via a
// oneshot the instant it's constructed, then blocks on
// `connection.incoming_closed()` for the rest of the connection's lifetime —
// keeping the closure (and therefore the transport it owns) alive without
// blocking `connect()` itself, which returns as soon as it receives that
// clone and completes the ACP handshake.
//
// # Disposal
//
// `dispose()` aborts the background task rather than waiting for
// `incoming_closed()` to resolve naturally. This is what makes a deliberate
// dispose never fire `AgentEvent::Exit` (mirrors TS's `this.child === child`
// guard, which distinguishes an unexpected death from `dispose()` nulling the
// child reference first) — aborting drops the closure's future before it
// reaches the "emit Exit" code after `incoming_closed()`. It should also drop
// the `AcpAgent` transport the closure owns, whose `ChildGuard` SIGKILLs the
// adapter's whole process group on drop (per the crate's own `acp_agent.rs`).
//
// # A spec-text correction, found by reading the crate source directly
//
// Spec #48's Implementation Decisions describe "model switching and mode
// switching both go through the crate's single stable
// SetSessionConfigOptionRequest ... the Rust SDK has no equivalent of the
// TypeScript SDK's separate unstable set_model RPC." The second half is
// true — there is no dedicated model RPC. But the first half doesn't hold:
// the schema crate still has its own dedicated, stable `SetSessionModeRequest`
// (`session/set_mode`, agent.rs:2166 in `agent-client-protocol-schema` 1.5.0)
// — a 1:1 match for TS's `setSessionMode({sessionId, modeId})`. Only
// model-switching was folded into the generic config-option mechanism (see
// `acp_translate.rs`'s module doc for the `SessionModelState` finding this
// pairs with). So `set_mode` here uses `SetSessionModeRequest` directly, and
// only `set_model` goes through `SetSessionConfigOptionRequest`.

use super::acp_translate;
use super::agent::{
    Agent, AgentEvent, AgentExitInfo, AgentSession, AuthMethodInfo, AvailableCommand, ConfigOption,
    ConfigValue, ModeState, ModelState, NewSessionOptions, PromptCapabilities, PromptImage,
    SessionUpdate,
};
use crate::agents::agent::AgentKind;
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Client, ConnectionTo};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex};

/// ACP crate wire types, aliased to keep call sites unambiguous against this
/// module's own (Hearth-shaped) types of the same name.
mod acp {
    pub use agent_client_protocol::schema::v1::*;
}

/// Executable + args that launch the ACP adapter, e.g. the claude-agent-acp
/// bin. Mirrors TS's `AdapterSpec`. `cwd` is the *task* working directory
/// fallback for sessions that don't specify their own (matches TS's
/// `this.cwd = spec.cwd`) — NOT the spawned subprocess's own OS-level
/// current directory: `AcpAgentConfig` (this crate version) has no builder
/// method to set that at all, so the adapter subprocess inherits Hearth's own
/// process cwd. This is a real, disclosed limitation of the crate as
/// published, not an oversight — ACP's actual task cwd is carried
/// per-session via `NewSessionRequest`/`LoadSessionRequest`, which IS fully
/// supported, so this only matters if an adapter's own startup (before any
/// session exists) reads `process.cwd()` for something.
#[derive(Debug, Clone)]
pub struct AdapterSpec {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: HashMap<String, String>,
}

/// Resolve a published adapter's launchable entry from its `package.json`
/// `bin`. These adapters expose only a bin (no main export), so we read
/// `package.json` and follow the bin path — running the vendored adapter, not
/// whatever is on `PATH`. Ported from `resolveAdapterBin` (`acp-agent.ts`);
/// Rust has no `require.resolve` module-resolution algorithm, so this reads
/// `<repo_root>/node_modules/<pkg>/package.json` directly, per the spec's own
/// stated approach.
pub fn resolve_adapter_bin(
    repo_root: &Path,
    pkg_name: &str,
    bin_name: &str,
) -> Result<PathBuf, String> {
    let pkg_json_path = repo_root
        .join("node_modules")
        .join(pkg_name)
        .join("package.json");
    let content = fs::read_to_string(&pkg_json_path).map_err(|e| {
        format!(
            "{pkg_name}: failed to read {}: {e}",
            pkg_json_path.display()
        )
    })?;
    let pkg: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("{pkg_name}: invalid package.json: {e}"))?;
    let bin_rel = match pkg.get("bin") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Object(map)) => map
            .get(bin_name)
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("{pkg_name} exposes no bin \"{bin_name}\" to launch"))?
            .to_string(),
        _ => {
            return Err(format!(
                "{pkg_name} exposes no bin \"{bin_name}\" to launch"
            ))
        }
    };
    let dir = pkg_json_path
        .parent()
        .ok_or_else(|| format!("{pkg_name}: package.json path has no parent directory"))?;
    Ok(dir.join(bin_rel))
}

/// Update kinds that are chat transcript content — suppressed during a
/// `loadSession` replay, since Hearth renders the transcript from its own
/// store (session-state updates like mode/config/usage/commands still flow).
/// Mirrors TS's `CHAT_CONTENT_UPDATES`.
fn is_chat_content(update: &SessionUpdate) -> bool {
    matches!(
        update,
        SessionUpdate::Message { .. }
            | SessionUpdate::Thought { .. }
            | SessionUpdate::ToolCall { .. }
            | SessionUpdate::Diff { .. }
            | SessionUpdate::Plan { .. }
            | SessionUpdate::End { .. }
    )
}

/// `PromptResponse.stop_reason` → the string TS's `stopReason` field already
/// was (the TS ACP SDK types it as a plain string). `StopReason` is
/// `#[non_exhaustive]` upstream, so a future variant falls back to
/// `"end_turn"` rather than failing to compile against a crate bump.
fn stop_reason_label(reason: acp::StopReason) -> String {
    match reason {
        acp::StopReason::EndTurn => "end_turn",
        acp::StopReason::MaxTokens => "max_tokens",
        acp::StopReason::MaxTurnRequests => "max_turn_requests",
        acp::StopReason::Refusal => "refusal",
        acp::StopReason::Cancelled => "cancelled",
        _ => "end_turn",
    }
    .to_string()
}

/// Composes the adapter subprocess's environment: the current process env,
/// scrubbed of `child_env::INHERITED_CREDENTIAL_VARS` when
/// `HEARTH_SCRUB_INHERITED_KEYS=1` (so a leaked parent-agent credential can't
/// hijack the spawned adapter's gateway — see `child_env.rs`), with `extra`
/// (a backend's own env, e.g. a future BYO-key path) merged over the top.
/// Mirrors TS's `buildChildEnv(process.env, spec.env, {...})`, called inside
/// `AcpClient.connect()` in `acp-client.ts` — not inside `claude.ts`/`codex.ts`'s
/// `resolveAdapter`, which only ever built the small `extra` overlay. Ported
/// the same way here: `claude.rs`/`codex.rs`'s `AdapterSpec.env` is just that
/// overlay; this function does the actual scrub+merge.
///
/// One real divergence from TS, forced by this crate version: Node's
/// `child_process.spawn(cmd, args, { env })` *replaces* the child's whole
/// environment with exactly what's passed, so omitting a credential var from
/// the composed map is enough to keep it from the child. `AcpAgentConfig`
/// (this crate version) has no `env_clear()` equivalent — its `.envs(...)`
/// only adds/overrides entries on top of `std::process::Command`'s normal
/// full-environment inheritance, so simply omitting a key (what
/// `build_child_env`'s scrub does) does nothing: the child still inherits it
/// directly from the OS regardless of what's in this map. So when scrubbing
/// is on, this additionally forces every credential var to an explicit empty
/// string unless `extra` already set it — these CLIs already treat an empty
/// key the same as an absent one, and `.envs()` CAN override an inherited
/// value, just not remove one.
fn child_env_for_adapter(extra: &HashMap<String, String>) -> HashMap<String, String> {
    let base: HashMap<String, String> = std::env::vars().collect();
    let scrub = super::child_env::should_scrub_inherited_keys(&base);
    let env = super::child_env::build_child_env(
        &base,
        extra,
        super::child_env::BuildChildEnvOptions {
            scrub_inherited_keys: scrub,
        },
    );
    force_empty_scrubbed_credentials(env, scrub)
}

/// The "empty-string override" step of `child_env_for_adapter`'s doc comment,
/// split out as a pure function (taking an already-composed env, not reading
/// `std::env::vars()` itself) so it's unit-testable.
fn force_empty_scrubbed_credentials(
    mut env: HashMap<String, String>,
    scrub: bool,
) -> HashMap<String, String> {
    if scrub {
        for key in super::child_env::INHERITED_CREDENTIAL_VARS {
            env.entry((*key).to_string()).or_default();
        }
    }
    env
}

struct ConnectionHandle {
    connection: ConnectionTo<agent_client_protocol::Agent>,
    driver_task: tokio::task::JoinHandle<()>,
    events: mpsc::UnboundedSender<AgentEvent>,
    cwd: String,
    auth_methods: Vec<AuthMethodInfo>,
    prompt_caps: PromptCapabilities,
}

/// The `Agent` implementation shared by both ACP backends (Claude, Codex) —
/// the only real difference between them is which adapter `resolve_spec`
/// resolves and which env/quirks it applies (`claude.rs` / `codex.rs`).
pub struct AcpClient {
    /// Read by the `Agent::kind()` impl below — currently unread from a live
    /// call site (see that trait method's own `#[allow(dead_code)]`).
    #[allow(dead_code)]
    kind: AgentKind,
    resolve_spec: Box<dyn Fn() -> Result<AdapterSpec, String> + Send + Sync>,
    connection: StdMutex<Option<ConnectionHandle>>,
    /// Advertised slash commands / skills — live-updated from streamed
    /// `commands` session updates for a connection's whole lifetime, so this
    /// is `Arc`'d separately from `connection`: the notification-handler
    /// closure (not `&self`) is what writes it.
    commands: Arc<StdMutex<Vec<AvailableCommand>>>,
}

impl AcpClient {
    pub fn new(
        kind: AgentKind,
        resolve_spec: impl Fn() -> Result<AdapterSpec, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            resolve_spec: Box::new(resolve_spec),
            connection: StdMutex::new(None),
            commands: Arc::new(StdMutex::new(Vec::new())),
        }
    }

    fn live_connection(
        &self,
    ) -> Result<
        (
            ConnectionTo<agent_client_protocol::Agent>,
            mpsc::UnboundedSender<AgentEvent>,
            String,
            PromptCapabilities,
        ),
        String,
    > {
        self.connection
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| {
                (
                    c.connection.clone(),
                    c.events.clone(),
                    c.cwd.clone(),
                    c.prompt_caps,
                )
            })
            .ok_or_else(|| "not connected — call connect() first".to_string())
    }
}

#[async_trait::async_trait]
impl Agent for AcpClient {
    fn kind(&self) -> AgentKind {
        self.kind
    }

    async fn connect(&self, events: mpsc::UnboundedSender<AgentEvent>) -> Result<(), String> {
        let spec = (self.resolve_spec)()?;
        let cwd = spec.cwd.clone();
        let transport = AcpAgent::new(
            AcpAgentConfig::new(spec.command)
                .args(spec.args)
                .envs(child_env_for_adapter(&spec.env)),
        );

        let (conn_tx, conn_rx) = oneshot::channel();
        let commands = self.commands.clone();
        let events_for_notif = events.clone();
        let events_for_permission = events.clone();
        let events_for_exit = events.clone();
        let titles: Arc<TokioMutex<HashMap<String, String>>> =
            Arc::new(TokioMutex::new(HashMap::new()));
        // ACP session ids currently replaying prior history via `loadSession`.
        // See `is_chat_content`'s doc.
        let replaying: Arc<StdMutex<HashSet<String>>> = Arc::new(StdMutex::new(HashSet::new()));

        let driver_task = tokio::spawn(async move {
            let outcome = Client
                .builder()
                .on_receive_notification(
                    move |notification: acp::SessionNotification, _cx| {
                        let commands = commands.clone();
                        let events = events_for_notif.clone();
                        let titles = titles.clone();
                        let replaying = replaying.clone();
                        async move {
                            let acp_session_id = notification.session_id.to_string();
                            let is_replaying = replaying.lock().unwrap().contains(&acp_session_id);
                            let updates = {
                                let mut titles = titles.lock().await;
                                acp_translate::translate_update(&notification.update, &mut titles)
                            };
                            for update in updates {
                                if let SessionUpdate::Commands { commands: cmds } = &update {
                                    *commands.lock().unwrap() = cmds.clone();
                                }
                                if is_replaying && is_chat_content(&update) {
                                    continue;
                                }
                                let _ = events.send(AgentEvent::Update {
                                    session_id: acp_session_id.clone(),
                                    update,
                                });
                            }
                            Ok(())
                        }
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .on_receive_request(
                    move |request: acp::RequestPermissionRequest,
                          responder: agent_client_protocol::Responder<
                        acp::RequestPermissionResponse,
                    >,
                          _cx| {
                        let events = events_for_permission.clone();
                        async move {
                            let acp_session_id = request.session_id.to_string();
                            let translated = acp_translate::translate_permission(&request);
                            let (respond_tx, respond_rx) = oneshot::channel();
                            let sent = events.send(AgentEvent::Permission {
                                session_id: acp_session_id,
                                request: translated,
                                respond: respond_tx,
                            });
                            // No handler ever answers (send failed, or the
                            // asker gave up without responding) — tell the
                            // agent to stop waiting rather than hang the turn.
                            let outcome = if sent.is_ok() {
                                match respond_rx.await {
                                    Ok(option_id) => acp::RequestPermissionOutcome::Selected(
                                        acp::SelectedPermissionOutcome::new(option_id),
                                    ),
                                    Err(_) => acp::RequestPermissionOutcome::Cancelled,
                                }
                            } else {
                                acp::RequestPermissionOutcome::Cancelled
                            };
                            responder.respond(acp::RequestPermissionResponse::new(outcome))
                        }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_with(
                    transport,
                    move |connection: ConnectionTo<agent_client_protocol::Agent>| {
                        let conn_tx = conn_tx;
                        async move {
                            // Hand a clone out so `connect()` (awaiting
                            // `conn_rx` below) can use it independently of
                            // this closure — see the module doc.
                            let _ = conn_tx.send(connection.clone());
                            // Keep this closure (and therefore this
                            // `connect_with` future, and the transport it
                            // owns) alive for the connection's whole
                            // lifetime; only return once the adapter's stdio
                            // genuinely closes.
                            connection.incoming_closed().await;
                            Ok(())
                        }
                    },
                )
                .await;
            if let Err(err) = outcome {
                eprintln!("[hearth] acp connection ended: {err}");
            }
            // Only reached on a genuine transport close (see above) — never
            // on a deliberate dispose, which aborts this task instead of
            // letting it run to completion. Mirrors TS's `this.child ===
            // child` guard in `acp-client.ts`'s `exit` handler.
            let _ = events_for_exit.send(AgentEvent::Exit(AgentExitInfo {
                code: None,
                signal: None,
            }));
        });

        let connection = conn_rx
            .await
            .map_err(|_| "adapter connection closed before the handshake completed".to_string())?;

        // No client fs capability: the agent writes directly to disk and the
        // git self-mod layer observes after (see ARCHITECTURE.md) — matches
        // TS's explicit `{ fs: { readTextFile: false, writeTextFile: false } }`,
        // which is also `ClientCapabilities::default()`'s value.
        let init = connection
            .send_request(acp::InitializeRequest::new(
                agent_client_protocol::schema::ProtocolVersion::V1,
            ))
            .block_task()
            .await
            .map_err(|e| e.to_string())?;

        let auth_methods = init
            .auth_methods
            .iter()
            .filter_map(|m| match m {
                acp::AuthMethod::Agent(a) => Some(AuthMethodInfo {
                    id: a.id.to_string(),
                    name: a.name.clone(),
                    description: a.description.clone(),
                }),
                // The other AuthMethod variants (EnvVar/Terminal) are behind
                // `unstable_auth_methods`, not compiled into this build.
                #[allow(unreachable_patterns)]
                _ => None,
            })
            .collect();
        let pc = init.agent_capabilities.prompt_capabilities;
        let prompt_caps = PromptCapabilities {
            image: pc.image,
            embedded_context: pc.embedded_context,
        };

        *self.connection.lock().unwrap() = Some(ConnectionHandle {
            connection,
            driver_task,
            events,
            cwd,
            auth_methods,
            prompt_caps,
        });
        Ok(())
    }

    async fn new_session(&self, opts: NewSessionOptions) -> Result<Box<dyn AgentSession>, String> {
        let (connection, events, default_cwd, prompt_caps) = self.live_connection()?;
        let cwd = opts.cwd.unwrap_or(default_cwd);
        let response = connection
            .send_request(acp::NewSessionRequest::new(cwd))
            .block_task()
            .await
            .map_err(|e| e.to_string())?;
        Ok(Box::new(AcpSession::from_response(
            response.session_id.to_string(),
            response.modes.as_ref(),
            response.config_options.as_deref(),
            connection,
            events,
            prompt_caps,
        )))
    }

    async fn resume_session(
        &self,
        acp_session_id: &str,
        opts: NewSessionOptions,
    ) -> Result<Box<dyn AgentSession>, String> {
        let (connection, events, default_cwd, prompt_caps) = self.live_connection()?;
        let cwd = opts.cwd.unwrap_or(default_cwd);
        let response = connection
            .send_request(acp::LoadSessionRequest::new(
                acp_session_id.to_string(),
                cwd,
            ))
            .block_task()
            .await
            .map_err(|e| e.to_string())?;
        Ok(Box::new(AcpSession::from_response(
            acp_session_id.to_string(),
            response.modes.as_ref(),
            response.config_options.as_deref(),
            connection,
            events,
            prompt_caps,
        )))
    }

    fn prompt_capabilities(&self) -> PromptCapabilities {
        self.connection
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| c.prompt_caps)
            .unwrap_or_default()
    }

    fn auth_methods(&self) -> Vec<AuthMethodInfo> {
        self.connection
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| c.auth_methods.clone())
            .unwrap_or_default()
    }

    fn advertised_commands(&self) -> Vec<AvailableCommand> {
        self.commands.lock().unwrap().clone()
    }

    async fn dispose(&self) -> Result<(), String> {
        let handle = self.connection.lock().unwrap().take();
        if let Some(handle) = handle {
            handle.driver_task.abort();
        }
        Ok(())
    }
}

struct AcpSession {
    id: String,
    connection: ConnectionTo<agent_client_protocol::Agent>,
    events: mpsc::UnboundedSender<AgentEvent>,
    models: ModelState,
    modes: ModeState,
    config_options: Vec<ConfigOption>,
    /// The config-option id backing the model selector, if the backend
    /// advertises one (see `acp_translate.rs`'s module doc — there's no
    /// dedicated model RPC, so `set_model` addresses whatever id the
    /// `Model`-categorized `SessionConfigOption` actually used, rather than
    /// assuming a fixed literal like `"model"`).
    model_config_id: Option<String>,
    prompt_caps: PromptCapabilities,
}

impl AcpSession {
    fn from_response(
        id: String,
        modes: Option<&acp::SessionModeState>,
        config_options: Option<&[acp::SessionConfigOption]>,
        connection: ConnectionTo<agent_client_protocol::Agent>,
        events: mpsc::UnboundedSender<AgentEvent>,
        prompt_caps: PromptCapabilities,
    ) -> Self {
        let model_config_id = config_options.and_then(|opts| {
            opts.iter()
                .find(|o| matches!(o.category, Some(acp::SessionConfigOptionCategory::Model)))
                .map(|o| o.id.to_string())
        });
        Self {
            id,
            models: acp_translate::normalize_models(config_options),
            modes: acp_translate::normalize_modes(modes),
            config_options: acp_translate::normalize_config_options(config_options),
            model_config_id,
            connection,
            events,
            prompt_caps,
        }
    }
}

#[async_trait::async_trait]
impl AgentSession for AcpSession {
    fn id(&self) -> &str {
        &self.id
    }
    fn models(&self) -> ModelState {
        self.models.clone()
    }
    fn modes(&self) -> ModeState {
        self.modes.clone()
    }
    fn config_options(&self) -> Vec<ConfigOption> {
        self.config_options.clone()
    }

    async fn prompt(&self, text: &str, images: &[PromptImage]) -> Result<(), String> {
        let mut blocks = vec![acp::ContentBlock::Text(acp::TextContent::new(text))];
        // Gate here too (defense in depth): never send an image to a backend
        // that didn't advertise `promptCapabilities.image`.
        if self.prompt_caps.image {
            for img in images {
                blocks.push(acp::ContentBlock::Image(acp::ImageContent::new(
                    img.data.clone(),
                    img.mime_type.clone(),
                )));
            }
        }
        let response = self
            .connection
            .send_request(acp::PromptRequest::new(self.id.clone(), blocks))
            .block_task()
            .await
            .map_err(|e| e.to_string())?;
        // The turn's "end" isn't a streamed session/update notification — the
        // agent reports it as this RPC's own response — so it's synthesized
        // here rather than in acp_translate.rs's translate_update, matching
        // TS's AcpClient doing the same after `connection.prompt()` resolves.
        let _ = self.events.send(AgentEvent::Update {
            session_id: self.id.clone(),
            update: SessionUpdate::End {
                stop_reason: stop_reason_label(response.stop_reason),
            },
        });
        Ok(())
    }

    async fn set_model(&self, model_id: &str) -> Result<(), String> {
        let Some(config_id) = &self.model_config_id else {
            return Ok(()); // backend advertises no model selector — no-op
        };
        self.connection
            .send_request(acp::SetSessionConfigOptionRequest::new(
                self.id.clone(),
                config_id.clone(),
                acp::SessionConfigOptionValue::value_id(model_id.to_string()),
            ))
            .block_task()
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn set_mode(&self, mode_id: &str) -> Result<(), String> {
        // No-op if the backend exposes no modes at all (matches `set_model`'s
        // guard above and the trait's own doc comment) — unlike TS's
        // `connection.setSessionMode?.(...)`, where the optional chain checks
        // whether the SDK method exists at all (this crate's
        // `SetSessionModeRequest` always exists at the wire-protocol level),
        // so the equivalent guard here is "does this session advertise any
        // modes", not "does the RPC exist".
        if self.modes.available.is_empty() {
            return Ok(());
        }
        self.connection
            .send_request(acp::SetSessionModeRequest::new(
                self.id.clone(),
                mode_id.to_string(),
            ))
            .block_task()
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn set_config_option(&self, config_id: &str, value: ConfigValue) -> Result<(), String> {
        let acp_value = match value {
            ConfigValue::Str(s) => acp::SessionConfigOptionValue::value_id(s),
            ConfigValue::Bool(b) => acp::SessionConfigOptionValue::boolean(b),
        };
        self.connection
            .send_request(acp::SetSessionConfigOptionRequest::new(
                self.id.clone(),
                config_id.to_string(),
                acp_value,
            ))
            .block_task()
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn cancel(&self) -> Result<(), String> {
        self.connection
            .send_notification(acp::CancelNotification::new(self.id.clone()))
            .map_err(|e| e.to_string())
    }

    async fn dispose(&self) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn resolve_adapter_bin_follows_a_string_bin_field() {
        let dir = tempfile::tempdir().unwrap();
        let pkg_dir = dir
            .path()
            .join("node_modules")
            .join("@zed-industries")
            .join("claude-agent-acp");
        write(
            &pkg_dir.join("package.json"),
            r#"{"name":"@zed-industries/claude-agent-acp","bin":"bin/cli.js"}"#,
        );
        let bin = resolve_adapter_bin(
            dir.path(),
            "@zed-industries/claude-agent-acp",
            "claude-agent-acp",
        )
        .unwrap();
        assert_eq!(bin, pkg_dir.join("bin/cli.js"));
    }

    #[test]
    fn resolve_adapter_bin_follows_the_named_entry_in_an_object_bin_field() {
        let dir = tempfile::tempdir().unwrap();
        let pkg_dir = dir
            .path()
            .join("node_modules")
            .join("@agentclientprotocol")
            .join("codex-acp");
        write(
            &pkg_dir.join("package.json"),
            r#"{"name":"@agentclientprotocol/codex-acp","bin":{"codex-acp":"dist/index.js","other":"x"}}"#,
        );
        let bin =
            resolve_adapter_bin(dir.path(), "@agentclientprotocol/codex-acp", "codex-acp").unwrap();
        assert_eq!(bin, pkg_dir.join("dist/index.js"));
    }

    #[test]
    fn resolve_adapter_bin_errors_when_the_named_bin_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let pkg_dir = dir.path().join("node_modules").join("some-pkg");
        write(&pkg_dir.join("package.json"), r#"{"bin":{"other":"x"}}"#);
        let err = resolve_adapter_bin(dir.path(), "some-pkg", "some-pkg").unwrap_err();
        assert!(err.contains("exposes no bin"));
    }

    #[test]
    fn resolve_adapter_bin_errors_when_the_package_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_adapter_bin(dir.path(), "not-installed", "not-installed").unwrap_err();
        assert!(err.contains("not-installed"));
    }

    #[test]
    fn chat_content_updates_are_exactly_the_transcript_variants() {
        assert!(is_chat_content(&SessionUpdate::Message {
            role: super::super::agent::MessageRole::Assistant,
            text: "hi".to_string(),
        }));
        assert!(is_chat_content(&SessionUpdate::End {
            stop_reason: "end_turn".to_string()
        }));
        assert!(!is_chat_content(&SessionUpdate::Mode {
            current: "default".to_string()
        }));
        assert!(!is_chat_content(&SessionUpdate::Commands {
            commands: vec![]
        }));
    }

    #[test]
    fn stop_reason_label_matches_the_wire_snake_case_strings() {
        assert_eq!(stop_reason_label(acp::StopReason::EndTurn), "end_turn");
        assert_eq!(stop_reason_label(acp::StopReason::MaxTokens), "max_tokens");
        assert_eq!(stop_reason_label(acp::StopReason::Cancelled), "cancelled");
    }

    #[test]
    fn scrubbing_forces_credential_vars_to_empty_when_not_already_set_by_extra() {
        let env = force_empty_scrubbed_credentials(HashMap::new(), true);
        for key in super::super::child_env::INHERITED_CREDENTIAL_VARS {
            assert_eq!(env.get(*key).map(String::as_str), Some(""));
        }
    }

    #[test]
    fn scrubbing_never_overrides_a_value_extra_already_set() {
        let mut composed = HashMap::new();
        composed.insert("ANTHROPIC_API_KEY".to_string(), "users-own-key".to_string());
        let env = force_empty_scrubbed_credentials(composed, true);
        assert_eq!(env.get("ANTHROPIC_API_KEY").unwrap(), "users-own-key");
    }

    #[test]
    fn without_scrubbing_credential_vars_are_left_exactly_as_composed() {
        let env = force_empty_scrubbed_credentials(HashMap::new(), false);
        assert!(env.is_empty());
    }

    /// True if any process's command line contains `needle` — used to check
    /// the real adapter subprocess's liveness from outside the crate (which
    /// doesn't expose the child's pid through `ConnectionTo`).
    fn any_process_matches(needle: &str) -> bool {
        std::process::Command::new("pgrep")
            .args(["-f", needle])
            .output()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false)
    }

    // Manual, not run by default (`cargo test -- --ignored` to run it): spawns
    // the REAL vendored claude-agent-acp adapter against this repo and proves
    // the whole live-connection design actually works end to end, not just
    // "compiles against the crate's types" — connect() completes the ACP
    // handshake, new_session() gets a real session id and a real advertised
    // model/mode/config-option snapshot back, and dispose() genuinely kills
    // the adapter's OS process (checked via `pgrep`, not inferred). Never
    // sends a prompt, so it costs no model-usage quota. Requires `node` on
    // PATH and `node_modules/@zed-industries/claude-agent-acp` installed
    // (both true in this repo).
    #[tokio::test]
    #[ignore]
    async fn live_smoke_test_against_the_real_claude_adapter() {
        let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let config = super::super::agent::AgentConfig {
            kind: AgentKind::Claude,
            cwd: repo_root.to_string_lossy().into_owned(),
            auth: super::super::agent::AgentAuth::Subscription,
        };
        let client = super::super::claude::new_agent(config, repo_root);

        let (events_tx, _events_rx) = mpsc::unbounded_channel();
        client
            .connect(events_tx)
            .await
            .expect("connect should complete the ACP handshake");
        assert!(
            any_process_matches("claude-agent-acp"),
            "the real adapter subprocess should be running after connect()"
        );

        let session = client
            .new_session(NewSessionOptions::default())
            .await
            .expect("new_session should succeed against a real adapter");
        assert!(!session.id().is_empty());
        println!(
            "live smoke test: session {} models={:?} modes={:?}",
            session.id(),
            session.models(),
            session.modes()
        );

        // No prompt is ever sent — this only proves the connection/session
        // lifecycle, not turn behavior (which would cost real quota).

        client.dispose().await.unwrap();
        assert!(
            client.connection.lock().unwrap().is_none(),
            "dispose() should clear the connection handle"
        );
        // Give the aborted task's Drop (the ChildGuard's process-group
        // SIGKILL) a moment to actually run before checking.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(
            !any_process_matches("claude-agent-acp"),
            "dispose() should have killed the adapter subprocess"
        );
    }
}
