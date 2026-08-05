// The backend-agnostic agent contract — data shapes only in this chunk. Mirrors
// electron/main/agents/agent.ts + the ACP-related types electron/shared/protocol.ts
// defines (SessionUpdate, PermissionRequest, ModelState, ModeState, ConfigOption,
// Usage, PromptCapabilities, PromptImage, PlanEntry, AuthMethodInfo,
// AvailableCommand), so the renderer's existing TypeScript types apply unchanged.
//
// These are wire-shaped by design (a 1:1 mirror of a wire-contract TS file, not an
// internal domain model with its own natural shape), so — unlike self-mod's
// protected-island types — they carry serde derives directly rather than routing
// through a separate DTO layer at the Tauri-command boundary.
//
// The `Agent`/`AgentSession` async traits below are the live-connection surface —
// the Rust analog of `agent.ts`'s interfaces. TS registers `onUpdate`/`onPermission`/
// `onExit` callbacks on the connection; Rust reshapes that as a single `AgentEvent`
// channel handed to `connect()`, since a live event stream is the more idiomatic
// Rust equivalent of callback registration, and it composes cleanly with
// `tokio::select!` on the consuming side (`AgentHost`, next chunk). A permission
// ask is a request-response, not fire-and-forget, so its event variant carries a
// `oneshot::Sender` the asker awaits directly — the same shape the map's permission
// round-trip standing decision already uses for the outward (Rust → renderer) leg.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

/// Which ACP backend. Mirrors `shared/protocol.ts`'s `AgentKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
}

/// Subscription-only in this phase (Phase 3 standing decision — no `api-key`/
/// secrets-backed path; see spec #48's Out of Scope). Modeled as an enum, not a
/// bare unit, so a later phase can add an `ApiKey` variant without reshaping
/// every caller that matches on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentAuth {
    Subscription,
}

/// Per-backend spawn config. Mirrors `agent.ts`'s `AgentConfig`. `kind`/`auth`
/// are set at construction (lib.rs's factory closure) but not read back off
/// this struct anywhere yet — each backend's `AcpClient` gets its `kind`
/// directly (not via this config), and subscription auth needs no further
/// branching. Kept on the struct for wire/API parity with `agent.ts` and for
/// whichever later phase adds a real api-key path.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub kind: AgentKind,
    /// Working directory the agent operates in (the Hearth repo, for self-mod).
    pub cwd: String,
    pub auth: AgentAuth,
}

/// A selectable model exposed by the active backend (mirrors ACP `ModelInfo`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModel {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The models a backend offers + which is current (mirrors ACP `SessionModelState`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelState {
    pub available: Vec<AgentModel>,
    pub current: Option<String>,
}

/// A permission/operating mode the backend advertises (mirrors ACP `SessionMode`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMode {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The modes a backend offers + which is current (mirrors ACP `SessionModeState`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeState {
    pub available: Vec<SessionMode>,
    pub current: Option<String>,
}

/// One selectable value of a `select` config option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSelectOption {
    pub value: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A generic agent-advertised session config option (mirrors ACP
/// `SessionConfigOption`). `category` is a UX hint (`mode | model | thought_level |
/// <custom>`); the renderer already surfaces mode + model via dedicated controls.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ConfigOption {
    #[serde(rename = "select")]
    Select {
        id: String,
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        category: Option<String>,
        current: String,
        options: Vec<ConfigSelectOption>,
    },
    #[serde(rename = "boolean")]
    Boolean {
        id: String,
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        category: Option<String>,
        current: bool,
    },
}

/// Cumulative session cost, when the adapter reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    pub amount: f64,
    pub currency: String,
}

/// Context-window + cost usage for a session (mirrors ACP `usage_update`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// Tokens currently in the context window.
    pub used: u64,
    /// Total context-window size in tokens.
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<Cost>,
}

/// What the active backend accepts in a prompt beyond text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCapabilities {
    pub image: bool,
    pub embedded_context: bool,
}

/// An image attached to a prompt — base64 data + mime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptImage {
    pub data: String,
    pub mime_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanPriority {
    High,
    Medium,
    Low,
}

/// A single plan task (mirrors ACP `PlanEntry`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanEntry {
    pub content: String,
    pub status: PlanStatus,
    pub priority: PlanPriority,
}

/// A slash command / skill the agent advertises.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableCommand {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// An auth method the backend's ACP adapter advertises in its initialize response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthMethodInfo {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Current-backend status (main → renderer on the backend-changed event).
/// Mirrors `shared/protocol.ts`'s `BackendStatus`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendStatus {
    pub kind: AgentKind,
    /// Present if the backend failed to connect after a switch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What the renderer needs to show truthful per-backend auth state. Mirrors
/// `shared/protocol.ts`'s `AuthState`. Phase 3 is subscription-only (see
/// `AgentAuth`), so `mode` is always `"subscription"` and `key_source` is
/// always `None` here — both fields still exist on the wire so a later
/// phase's api-key path doesn't need to reshape this struct.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthState {
    pub kind: AgentKind,
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_source: Option<String>,
    /// The ACP handshake completed (the adapter spawned + initialized). Only
    /// the active backend has a live adapter to report this.
    pub connected: bool,
    /// Connect error, when the adapter failed to come up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// For the INACTIVE backend: whether the CLI's own stored login is
    /// present (presence-only — never the token value).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login_present: Option<bool>,
    pub methods: Vec<AuthMethodInfo>,
}

/// Payload shape for the `agent:update` event (Rust → renderer).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUpdatePayload {
    pub session_id: String,
    pub update: SessionUpdate,
}

/// Payload shape for the `permission:request` event (Rust → renderer).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRequestPayload {
    pub session_id: String,
    pub req: PermissionRequest,
}

/// Payload shape for the `agent:error` event (Rust → renderer). `session_key`
/// attributes the failure to the renderer session whose turn was in flight
/// when the adapter died; `None` means a global failure.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentErrorPayload {
    pub session_key: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionOptionKind {
    #[serde(rename = "allow")]
    Allow,
    #[serde(rename = "allow-always")]
    AllowAlways,
    #[serde(rename = "reject")]
    Reject,
}

/// One option the user can pick when answering a permission ask.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub id: String,
    pub label: String,
    pub kind: PermissionOptionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionCategory {
    Execute,
    Edit,
    Other,
}

/// Permission ask raised mid-turn. The UI must answer or the agent hangs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRequest {
    pub id: String,
    pub title: String,
    pub options: Vec<PermissionOption>,
    /// Raw shell command when this is an execute ask — used to auto-reject
    /// source-mutating shell so writes are forced onto the mediated path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<PermissionCategory>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageRole {
    #[serde(rename = "assistant")]
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallStatus {
    Pending,
    Running,
    Done,
    Error,
}

/// A single streamed update from a turn — mirrors ACP `session/update`, translated
/// into Hearth's own shape (see `acp_translate.rs` for that translation).
///
/// `parent_tool_call_id` (on `ToolCall` and `Diff`) is the id of the parent Task
/// tool-call when the update originates inside a subagent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SessionUpdate {
    #[serde(rename = "message")]
    Message { role: MessageRole, text: String },
    #[serde(rename = "thought")]
    Thought { text: String },
    #[serde(rename = "tool-call")]
    ToolCall {
        id: String,
        title: String,
        status: ToolCallStatus,
        #[serde(skip_serializing_if = "Option::is_none", rename = "parentToolCallId")]
        parent_tool_call_id: Option<String>,
    },
    #[serde(rename = "diff")]
    Diff {
        path: String,
        #[serde(rename = "oldText")]
        old_text: Option<String>,
        #[serde(rename = "newText")]
        new_text: String,
        #[serde(skip_serializing_if = "Option::is_none", rename = "parentToolCallId")]
        parent_tool_call_id: Option<String>,
    },
    #[serde(rename = "plan")]
    Plan { entries: Vec<PlanEntry> },
    #[serde(rename = "commands")]
    Commands { commands: Vec<AvailableCommand> },
    #[serde(rename = "mode")]
    Mode { current: String },
    #[serde(rename = "config")]
    Config { options: Vec<ConfigOption> },
    #[serde(rename = "usage")]
    Usage { usage: Usage },
    #[serde(rename = "info")]
    Info { title: String },
    #[serde(rename = "end")]
    End {
        #[serde(rename = "stopReason")]
        stop_reason: String,
    },
}

/// How the adapter subprocess died (mirrors `child_process`'s `'exit'` event).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentExitInfo {
    pub code: Option<i32>,
    pub signal: Option<String>,
}

/// Per-session options. `cwd` is the task working directory (the workspace).
#[derive(Debug, Clone, Default)]
pub struct NewSessionOptions {
    pub cwd: Option<String>,
}

/// A generic config option's new value — mirrors TS's `string | boolean` union.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigValue {
    Str(String),
    Bool(bool),
}

/// Pushed from a live `Agent` connection to whatever's driving it (`AgentHost`,
/// next chunk) for the lifetime of the connection.
pub enum AgentEvent {
    Update {
        session_id: String,
        update: SessionUpdate,
    },
    /// A mid-turn permission ask. The asker blocks on `respond` resolving.
    Permission {
        session_id: String,
        request: PermissionRequest,
        respond: oneshot::Sender<String>,
    },
    /// Fired only on an UNEXPECTED adapter death — never for a deliberate dispose.
    Exit(AgentExitInfo),
}

/// One live ACP session. Mirrors `agent.ts`'s `AgentSession` interface.
/// `models`/`modes`/`config_options` are snapshots captured at session creation —
/// they don't change out from under a live session; `AgentHost` maintains its own
/// live cache from streamed `SessionUpdate`s (see `acp_translate.rs`, next chunk).
#[async_trait]
pub trait AgentSession: Send + Sync {
    fn id(&self) -> &str;
    fn models(&self) -> ModelState;
    fn modes(&self) -> ModeState;
    fn config_options(&self) -> Vec<ConfigOption>;
    /// Send a turn. `images` are attached as ACP image blocks when the backend
    /// advertises the capability (ignored otherwise).
    async fn prompt(&self, text: &str, images: &[PromptImage]) -> Result<(), String>;
    /// Switch the model for this session (no-op if the backend exposes none).
    async fn set_model(&self, model_id: &str) -> Result<(), String>;
    /// Switch the permission mode (no-op if the backend exposes none).
    async fn set_mode(&self, mode_id: &str) -> Result<(), String>;
    async fn set_config_option(&self, config_id: &str, value: ConfigValue) -> Result<(), String>;
    async fn cancel(&self) -> Result<(), String>;
    /// No caller yet — `AgentHostEngine` disposes at the `Agent` (connection)
    /// level, not per-session; a future per-session teardown path (e.g. the
    /// renderer explicitly closing one conversation) is the natural caller.
    #[allow(dead_code)]
    async fn dispose(&self) -> Result<(), String>;
}

/// A live ACP backend connection. Mirrors `agent.ts`'s `Agent` interface.
#[async_trait]
pub trait Agent: Send + Sync {
    /// No caller yet — `AgentHostEngine` tracks the current kind itself
    /// rather than reading it back off a live connection.
    #[allow(dead_code)]
    fn kind(&self) -> AgentKind;
    /// Spawn the ACP adapter subprocess and complete the ACP handshake. `events`
    /// carries every update/permission-ask/exit for the connection's lifetime.
    async fn connect(&self, events: mpsc::UnboundedSender<AgentEvent>) -> Result<(), String>;
    async fn new_session(&self, opts: NewSessionOptions) -> Result<Box<dyn AgentSession>, String>;
    /// Resume a prior conversation by its ACP session id. Rejects if the backend
    /// can't load it (the caller falls back to a fresh session). Default: no
    /// backends implementing this trait support resume unless they override it.
    async fn resume_session(
        &self,
        acp_session_id: &str,
        opts: NewSessionOptions,
    ) -> Result<Box<dyn AgentSession>, String> {
        let _ = (acp_session_id, opts);
        Err("resume not supported".to_string())
    }
    /// Prompt capabilities the adapter advertised at initialize.
    fn prompt_capabilities(&self) -> PromptCapabilities {
        PromptCapabilities {
            image: false,
            embedded_context: false,
        }
    }
    /// Auth methods the adapter advertised at initialize (empty until connected).
    fn auth_methods(&self) -> Vec<AuthMethodInfo> {
        Vec::new()
    }
    /// Slash commands / skills the agent has advertised this connection.
    fn advertised_commands(&self) -> Vec<AvailableCommand> {
        Vec::new()
    }
    async fn dispose(&self) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_kind_serializes_lowercase() {
        assert_eq!(
            serde_json::to_value(AgentKind::Claude).unwrap(),
            serde_json::json!("claude")
        );
        assert_eq!(
            serde_json::to_value(AgentKind::Codex).unwrap(),
            serde_json::json!("codex")
        );
    }

    #[test]
    fn tool_call_update_serializes_to_the_ts_tagged_shape() {
        let update = SessionUpdate::ToolCall {
            id: "tc1".to_string(),
            title: "Edit file".to_string(),
            status: ToolCallStatus::Running,
            parent_tool_call_id: None,
        };
        assert_eq!(
            serde_json::to_value(&update).unwrap(),
            serde_json::json!({ "type": "tool-call", "id": "tc1", "title": "Edit file", "status": "running" })
        );
    }

    #[test]
    fn diff_update_includes_parent_tool_call_id_only_when_present() {
        let update = SessionUpdate::Diff {
            path: "src/a.ts".to_string(),
            old_text: Some("old".to_string()),
            new_text: "new".to_string(),
            parent_tool_call_id: Some("parent1".to_string()),
        };
        assert_eq!(
            serde_json::to_value(&update).unwrap(),
            serde_json::json!({
                "type": "diff",
                "path": "src/a.ts",
                "oldText": "old",
                "newText": "new",
                "parentToolCallId": "parent1",
            })
        );
    }

    #[test]
    fn message_update_uses_assistant_role() {
        let update = SessionUpdate::Message {
            role: MessageRole::Assistant,
            text: "hi".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&update).unwrap(),
            serde_json::json!({ "type": "message", "role": "assistant", "text": "hi" })
        );
    }

    #[test]
    fn end_update_serializes_stop_reason() {
        let update = SessionUpdate::End {
            stop_reason: "end_turn".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&update).unwrap(),
            serde_json::json!({ "type": "end", "stopReason": "end_turn" })
        );
    }

    #[test]
    fn plan_entry_serializes_snake_case_status_and_lowercase_priority() {
        let entry = PlanEntry {
            content: "do thing".to_string(),
            status: PlanStatus::InProgress,
            priority: PlanPriority::High,
        };
        assert_eq!(
            serde_json::to_value(&entry).unwrap(),
            serde_json::json!({ "content": "do thing", "status": "in_progress", "priority": "high" })
        );
    }

    #[test]
    fn config_option_select_and_boolean_serialize_to_tagged_shapes() {
        let select = ConfigOption::Select {
            id: "reasoning".to_string(),
            name: "Reasoning".to_string(),
            description: None,
            category: Some("thought_level".to_string()),
            current: "medium".to_string(),
            options: vec![ConfigSelectOption {
                value: "medium".to_string(),
                name: "Medium".to_string(),
                description: None,
            }],
        };
        assert_eq!(
            serde_json::to_value(&select).unwrap(),
            serde_json::json!({
                "type": "select",
                "id": "reasoning",
                "name": "Reasoning",
                "category": "thought_level",
                "current": "medium",
                "options": [{ "value": "medium", "name": "Medium" }],
            })
        );

        let boolean = ConfigOption::Boolean {
            id: "verbose".to_string(),
            name: "Verbose".to_string(),
            description: None,
            category: None,
            current: true,
        };
        assert_eq!(
            serde_json::to_value(&boolean).unwrap(),
            serde_json::json!({ "type": "boolean", "id": "verbose", "name": "Verbose", "current": true })
        );
    }

    #[test]
    fn permission_option_kind_serializes_hyphenated_allow_always() {
        assert_eq!(
            serde_json::to_value(PermissionOptionKind::AllowAlways).unwrap(),
            serde_json::json!("allow-always")
        );
    }

    #[test]
    fn permission_request_omits_absent_optionals() {
        let req = PermissionRequest {
            id: "p1".to_string(),
            title: "Run command?".to_string(),
            options: vec![PermissionOption {
                id: "allow".to_string(),
                label: "Allow".to_string(),
                kind: PermissionOptionKind::Allow,
            }],
            command: None,
            category: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("command").is_none());
        assert!(json.get("category").is_none());
    }

    #[test]
    fn model_state_and_mode_state_default_to_empty() {
        assert_eq!(
            ModelState::default(),
            ModelState {
                available: vec![],
                current: None
            }
        );
        assert_eq!(
            ModeState::default(),
            ModeState {
                available: vec![],
                current: None
            }
        );
    }
}
