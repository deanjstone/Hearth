# Research: `agent-client-protocol` Rust crate (v2.0.0) API shape

Resolves: [Hearth#44](https://github.com/deanjstone/Hearth/issues/44) — Phase 3 (Rust/Tauri port)
blocking research on how to port Hearth's ACP client-host logic from the TypeScript SDK
(`@agentclientprotocol/acp`, or `@zed-industries/agent-client-protocol` depending on vintage) to
the published Rust SDK.

## Method

Fetched and extracted the actual published crate sources rather than inferring from names:

- `agent-client-protocol` v2.0.0 tarball, downloaded directly from
  `https://static.crates.io/crates/agent-client-protocol/agent-client-protocol-2.0.0.crate`
  (crates.io's `/api/v1/.../download` endpoint 403'd this session with a bot-policy error; the
  static CDN URL worked). SHA in `.cargo_vcs_info.json`: `ce023279824149008659dd8f4b8b70266a7e8210`,
  `path_in_vcs: "src/agent-client-protocol"` — confirms this is a workspace member of
  `github.com/agentclientprotocol/rust-sdk`.
- `agent-client-protocol-schema` v1.5.0 tarball (the wire-schema crate that
  `agent-client-protocol` 2.0.0 depends on and re-exports as `agent_client_protocol::schema`),
  downloaded the same way. SHA: `d0549a115750a0a25c1c5631dd72e0d248859aa4`,
  `path_in_vcs: "agent-client-protocol-schema"` — this one lives in the **separate** repo
  `github.com/agentclientprotocol/agent-client-protocol` (confirmed via
  `repository =` in its `Cargo.toml`, and via a live fetch of that repo's directory listing).
- Read every relevant `.rs` file directly (not docs.rs prose) for `Cargo.toml` feature
  declarations, trait/struct/enum definitions, `#[cfg(feature = ...)]` gates, and the two
  published runnable examples (`examples/yolo_one_shot_client.rs`,
  `examples/simple_agent.rs`).
- Cross-checked with docs.rs (`docs.rs/agent-client-protocol/2.0.0`,
  `docs.rs/agent-client-protocol-schema/1.5.0`) and live GitHub file/repo listings via WebFetch.

File paths below are relative to the extracted tarballs; GitHub citations use the pinned commit
SHAs above so links stay valid even if `main` moves.

---

## 1. Client/Agent trait split, and how a client-side stdio connection is built

**There are no `trait Agent { async fn prompt(...) }` / `trait Client { async fn requestPermission(...) }` interfaces to implement**, unlike the TS SDK's `Agent`/`Client` interfaces. Version 2.0.0 rewrote the Rust SDK around **unit-struct "roles"** (`Client`, `Agent`, `Proxy`, `Conductor`) plus a **connection-builder / dispatch-handler** architecture. The CHANGELOG is explicit about this being a from-scratch API rewrite while keeping the wire protocol byte-for-byte the same:

> "Version 2.0 keeps the stable ACP v1 wire schema unchanged while making coordinated breaking changes to the Rust SDK APIs and low-level transport boundary."
— `agent-client-protocol-2.0.0/CHANGELOG.md:5-8`
(https://github.com/agentclientprotocol/rust-sdk/blob/ce023279824149008659dd8f4b8b70266a7e8210/src/agent-client-protocol/CHANGELOG.md)

**Roles.** `pub trait Role` defines `type Counterpart`, `role_id()`, `default_handle_dispatch_from()`. The four concrete roles are unit structs:
`Client`, `Agent`, `Proxy`, `Conductor` — `agent-client-protocol-2.0.0/src/role.rs:32-62`,
`agent-client-protocol-2.0.0/src/role/acp.rs:28-350`
(https://github.com/agentclientprotocol/rust-sdk/blob/ce023279824149008659dd8f4b8b70266a7e8210/src/agent-client-protocol/src/role/acp.rs).
Hearth is the **client** role: `Client::Counterpart = Agent`.

**Building a client-side connection.** `Client` (and every role) gets a `.builder()` method
returning a `Builder<Client, NullHandler, NullRun>`
(`agent-client-protocol-2.0.0/src/role/acp.rs:61-108`). The builder is where you register
handlers for incoming requests/notifications (`.on_receive_request(...)`,
`.on_receive_notification(...)`), then call `.connect_with(transport, async |connection| { ... })`
to actually connect and run your driving logic. `connection: ConnectionTo<Agent>` is what you use
to `send_request(...)`/`send_notification(...)` to the agent.

**Stdio + subprocess spawn — the direct equivalent of the TS SDK's `ndJsonStream(output, input)` +
child_process.spawn pattern.** This is `AcpAgent` (`agent-client-protocol-2.0.0/src/acp_agent.rs`,
https://github.com/agentclientprotocol/rust-sdk/blob/ce023279824149008659dd8f4b8b70266a7e8210/src/agent-client-protocol/src/acp_agent.rs).
It implements `ConnectTo<Counterpart>` and, in one call, does everything the TS pattern does by
hand:

- Spawns the child process (`spawn_process()`, using `async-process`), putting it in its own
  process group on Unix so wrapper launchers like `npx`/`uvx` don't orphan the real agent
  (`acp_agent.rs:250-303`).
- Wires `child_stdin`/`child_stdout` into newline-delimited JSON-RPC framing via
  `crate::Lines::new(outgoing_sink, incoming_lines)` — this `Lines` adapter is the direct
  structural analogue of the TS SDK's `ndJsonStream(output, input)` (`acp_agent.rs:667-729`).
- Captures/truncates stderr for error reporting, and installs a `ChildGuard` that SIGKILLs the
  whole process group on drop.
- `AcpAgentConfig::new(command).arg(...).env(...)` configures the launch; `AcpAgent::from_str("python agent.py --flag")`
  parses a shell command string via the `shell-words` crate, and `AcpAgent::from_str(r#"{"command":...}"#)`
  parses JSON config (`acp_agent.rs:861-878`). There are also convenience constructors
  `AcpAgent::claude_agent()` (runs `npx -y @agentclientprotocol/claude-agent-acp@latest`) and
  `AcpAgent::codex()` (runs `npx -y @agentclientprotocol/codex-acp@latest`) —
  `acp_agent.rs:192-206`.

The full idiom, taken verbatim from the crate's own published example
(`agent-client-protocol-2.0.0/examples/yolo_one_shot_client.rs`,
https://github.com/agentclientprotocol/rust-sdk/blob/ce023279824149008659dd8f4b8b70266a7e8210/src/agent-client-protocol/examples/yolo_one_shot_client.rs):

```rust
let agent = AcpAgent::from_str(&cli.command)?; // spawn config; process not yet started

agent_client_protocol::Client
    .builder()
    .on_receive_notification(
        async move |notification: SessionNotification, _cx| { /* session/update */ Ok(()) },
        agent_client_protocol::on_receive_notification!(),
    )
    .on_receive_request(
        async move |request: RequestPermissionRequest, responder, _connection| {
            responder.respond(RequestPermissionResponse::new(/* ... */))
        },
        agent_client_protocol::on_receive_request!(),
    )
    .connect_with(agent, |connection: ConnectionTo<Agent>| async move {
        // connection.send_request(InitializeRequest::new(...)).block_task().await?;
        // connection.send_request(NewSessionRequest::new(cwd)).block_task().await?;
        // connection.send_request(PromptRequest::new(session_id, content)).block_task().await?;
        Ok(())
    })
    .await?;
```

`AcpAgent` spawning the process and framing its stdio is functionally equivalent to: TS's
`spawn(command)` + `ndJsonStream(child.stdin, child.stdout)` + `new ClientSideConnection(handler,
stream)`. There is no separately-named `ClientSideConnection` type in Rust — its role is played
jointly by `Client.builder()...connect_with(...)` (the connection/dispatcher) and `AcpAgent` (the
transport passed as the first arg to `connect_with`).

There is also a lower-level `Stdio` transport (`agent-client-protocol-2.0.0/src/stdio.rs`) for
when *your own process* is the agent/proxy being driven over its own stdin/stdout (used by
`AcpAgent` counterparts, i.e. writing an agent-side binary) — not what Hearth needs, since Hearth
spawns the adapter subprocess rather than being spawned as one.

A convenience sugar layer also exists in `agent-client-protocol-2.0.0/src/session.rs`
(`ConnectionTo::build_session_cwd()`, `SessionBuilder::block_task()`, `ActiveSession::send_prompt()`,
`ActiveSession::read_to_string()`) shown in the crate's top-of-`lib.rs` doc example — a higher-level
helper on top of the same request/response primitives, not a different transport mechanism.

---

## 2. Session lifecycle methods

All defined in `agent-client-protocol-schema-1.5.0/src/v1/agent.rs`
(https://github.com/agentclientprotocol/agent-client-protocol/blob/d0549a115750a0a25c1c5631dd72e0d248859aa4/agent-client-protocol-schema/src/v1/agent.rs),
re-exported through `agent_client_protocol::schema::v1::*`. These are plain request/response
**structs**, not trait methods — you call `connection.send_request(SomeRequest::new(...)).block_task().await?`
(or `.on_receiving_result(...)` for a non-blocking callback style) and get back the typed response.
None of the four core lifecycle calls are feature-gated (all compiled by default, matching the TS
SDK's stable status for these methods):

| Purpose | TS SDK | Rust request struct (send) | Rust response struct (receive) | Wire method | Gate |
|---|---|---|---|---|---|
| Create session | `connection.newSession(...)` | `NewSessionRequest` (`agent.rs:1011`) | `NewSessionResponse` (`agent.rs:1086`) | `session/new` | none (stable) |
| Load/resume with full history replay | `connection.loadSession(...)` | `LoadSessionRequest` (`agent.rs:1171`) | `LoadSessionResponse` (`agent.rs:1248`) | `session/load` | none (stable) |
| Send a prompt (turn) | `connection.prompt(...)` | `PromptRequest` (`agent.rs:3207`) | `PromptResponse` (`agent.rs:3268`) | `session/prompt` | none (stable) |
| Cancel | `connection.cancel(...)` | `CancelNotification` (`agent.rs:5317`) — a notification, not a request; no response type | — | `session/cancel` | none (stable) |

`NewSessionRequest::new(cwd)` takes a working directory (`PathBuf`); `PromptRequest::new(session_id, prompt: Vec<ContentBlock>)`.
`CancelNotification` is sent via `connection.send_notification(...)`, matching the TS SDK's
fire-and-forget `cancel()`.

Confirmed from the same enum that dispatches these on the wire
(`ClientRequest` in `agent.rs:4927-5150`; every arm's `#[cfg(...)]` gate was checked line-by-line):
`NewSessionRequest`, `LoadSessionRequest`, `PromptRequest` all carry **no** `#[cfg(feature = ...)]`
attribute — they compile unconditionally.

**Newly discovered, not present in Hearth's current TS usage** (the Rust SDK schema — v1.5.0 — has
grown beyond what the TS SDK Hearth currently targets, all still stable/ungated):
`session/list` (`ListSessionsRequest`/`Response`), `session/delete` (`DeleteSessionRequest`/`Response`),
`session/resume` (`ResumeSessionRequest`/`Response`, resumes a session context *without* replaying
history, unlike `session/load`), `session/close` (`CloseSessionRequest`/`Response`). Only
`session/fork` (`ForkSessionRequest`/`Response`) is gated, behind `unstable_session_fork`
(`ClientRequest::ForkSessionRequest` arm, `agent.rs:5008-5017`) — matching Hearth's non-use of
session forking.

---

## 3. Streamed session-update notifications

Yes — the crate has the same discriminated-union shape as the TS SDK's `SessionNotification.update.sessionUpdate`, and it is **richer**, not coarser. Defined in
`agent-client-protocol-schema-1.5.0/src/v1/client.rs:99-139`
(https://github.com/agentclientprotocol/agent-client-protocol/blob/d0549a115750a0a25c1c5631dd72e0d248859aa4/agent-client-protocol-schema/src/v1/client.rs#L99-L139):

```rust
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum SessionUpdate {
    UserMessageChunk(ContentChunk),
    AgentMessageChunk(ContentChunk),
    AgentThoughtChunk(ContentChunk),
    ToolCall(ToolCall),
    ToolCallUpdate(ToolCallUpdate),
    Plan(Plan),
    #[cfg(feature = "unstable_plan_operations")] PlanUpdate(PlanUpdate),
    #[cfg(feature = "unstable_plan_operations")] PlanRemoved(PlanRemoved),
    AvailableCommandsUpdate(AvailableCommandsUpdate),
    CurrentModeUpdate(CurrentModeUpdate),
    ConfigOptionUpdate(ConfigOptionUpdate),
    SessionInfoUpdate(SessionInfoUpdate),
    UsageUpdate(UsageUpdate),
}
```

This is a genuine `#[serde(tag = "sessionUpdate", ...)]` discriminated enum (serde-level tagged
union keyed on the wire field `sessionUpdate`, matching the TS discriminant field name exactly),
wrapped in `SessionNotification { session_id, update: SessionUpdate, meta }`
(`client.rs:50-65`) — this is what arrives via the `AgentNotification::SessionNotification`
variant (`client.rs:2513+`), which your `.on_receive_notification(async |n: SessionNotification, _cx| {...}, ...)`
handler receives, exactly as shown in `yolo_one_shot_client.rs:48-54`.

Mapping to your sub-questions' enumerated list — every item you named exists, all stable/ungated
except two:

- message chunks → `UserMessageChunk` / `AgentMessageChunk` — stable
- thought chunks → `AgentThoughtChunk` — stable
- tool-call / tool-call-update → `ToolCall` / `ToolCallUpdate` — stable
- diff → **not** a top-level `SessionUpdate` variant. It's `ToolCallContent::Diff(Diff)`, one of
  the content variants nested *inside* `ToolCall`/`ToolCallUpdate`
  (`agent-client-protocol-schema-1.5.0/src/v1/tool_call.rs:502-523`) — structurally identical to
  how the TS SDK nests diffs inside tool-call content, not as its own discriminant.
- plan → `Plan` — stable. `PlanUpdate`/`PlanRemoved` (incremental plan edits by ID) are new,
  **gated behind `unstable_plan_operations`** — a flag not on your list and, notably, **not
  exposed as a feature of the top-level `agent-client-protocol` crate at all** (see §5).
- available-commands-update → `AvailableCommandsUpdate` — stable
- current-mode-update → `CurrentModeUpdate` — stable
- config-option-update → `ConfigOptionUpdate` — stable (see §5 for how this subsumes model
  switching)
- usage-update → `UsageUpdate` — stable, streamed per-notification context-window/cost figure.
  Distinct from and unrelated to the gated `PromptResponse.usage` end-of-turn total (§5).
- session-info-update → `SessionInfoUpdate` — stable

---

## 4. Permission requests

`session/request_permission` is a **request the agent sends to the client** — in the role/dispatch
model it is a variant of `AgentRequest` (`client.rs:2317-2360` in the schema crate,
https://github.com/agentclientprotocol/agent-client-protocol/blob/d0549a115750a0a25c1c5631dd72e0d248859aa4/agent-client-protocol-schema/src/v1/client.rs#L2317),
**not gated behind any unstable flag**. You surface it by registering a handler on the `Client`
connection builder before calling `.connect_with(...)`:

```rust
.on_receive_request(
    async move |request: RequestPermissionRequest, responder, _connection| {
        let option_id = request.options.first().map(|opt| opt.option_id.clone());
        responder.respond(RequestPermissionResponse::new(
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id.unwrap())),
        ))
    },
    agent_client_protocol::on_receive_request!(),
)
```
— straight from `examples/yolo_one_shot_client.rs:55-71`. `responder` is how you reply (there is
no return-value-based dispatch; you explicitly call `.respond(...)` / `.respond_with_error(...)`).

**Type shape** (`agent-client-protocol-schema-1.5.0/src/v1/client.rs:653-903`):

- `RequestPermissionRequest { session_id, tool_call: ToolCallUpdate, options: Vec<PermissionOption>, meta }`
- `PermissionOption { option_id: PermissionOptionId, name: String, kind: PermissionOptionKind, meta }`
- `PermissionOptionKind` (`client.rs:774-783`) — a plain 4-variant enum, **exactly** matching your
  four kinds:
  ```rust
  pub enum PermissionOptionKind {
      AllowOnce,
      AllowAlways,
      RejectOnce,
      RejectAlways,
  }
  ```
- `RequestPermissionResponse { outcome: RequestPermissionOutcome, meta }`
- `RequestPermissionOutcome` (`client.rs:835-847`) is itself a tagged enum:
  `Cancelled` (used when the client sent `session/cancel` while the permission prompt was
  outstanding — the doc comment states clients *MUST* respond this way to every pending
  `request_permission` in that case) or `Selected(SelectedPermissionOutcome { option_id })`.

This is a 1:1 structural match to the TS SDK's `PermissionOption`/`RequestPermissionOutcome`
shape — no porting surprises here beyond the request/response now being delivered via a
registered async closure instead of a trait method override.

---

## 5. Unstable feature flags

**The top-level `agent-client-protocol` crate exposes exactly six feature flags**, per its
published `Cargo.toml` (`agent-client-protocol-2.0.0/Cargo.toml:44-58`,
https://github.com/agentclientprotocol/rust-sdk/blob/ce023279824149008659dd8f4b8b70266a7e8210/src/agent-client-protocol/Cargo.toml):

```toml
[features]
default = []
unstable = [
    "unstable_auth_methods",
    "unstable_elicitation",
    "unstable_end_turn_token_usage",
    "unstable_mcp_over_acp",
    "unstable_session_fork",
]
unstable_auth_methods = ["agent-client-protocol-schema/unstable_auth_methods"]
unstable_elicitation = ["agent-client-protocol-schema/unstable_elicitation"]
unstable_end_turn_token_usage = ["agent-client-protocol-schema/unstable_end_turn_token_usage"]
unstable_mcp_over_acp = ["agent-client-protocol-schema/unstable_mcp_over_acp"]
unstable_protocol_v2 = ["agent-client-protocol-schema/unstable_protocol_v2"]
unstable_session_fork = ["agent-client-protocol-schema/unstable_session_fork"]
```

Note `unstable_protocol_v2` is deliberately **excluded** from the `unstable` umbrella feature (own
comment in the underlying schema crate: "Protocol v2 is intentionally NOT part of the `unstable`
umbrella. It introduces a parallel `v2` module and (eventually) a different wire version").

The **schema** crate (`agent-client-protocol-schema` 1.5.0) actually defines a superset —
`unstable_llm_providers`, `unstable_nes`, and `unstable_plan_operations` also exist there
(`agent-client-protocol-schema-1.5.0/Cargo.toml.orig:25-46`) — but the top-level
`agent-client-protocol` crate does **not** forward these three as its own Cargo features. A
consumer depending only on `agent-client-protocol = "2.0.0"` (the normal path) cannot turn on
`unstable_llm_providers`/`unstable_nes`/`unstable_plan_operations` through that crate's feature
table at all; they'd need a direct extra dependency on `agent-client-protocol-schema` with
matching version to reach them via Cargo feature unification. None of these three are relevant to
Hearth's current capability set anyway (`providers/*`, NES/next-edit-suggestion endpoints, and
incremental plan-update/-removed notifications are all things Hearth doesn't use).

**Gate-by-gate, checked directly against every `#[cfg(feature = "unstable_...")]` attribute in
`agent-client-protocol-schema-1.5.0/src/v1/{agent,client}.rs`** (not inferred from names):

| Flag | What it gates | Hearth needs it? |
|---|---|---|
| `unstable_auth_methods` | Only the `AuthMethod::EnvVar` and `AuthMethod::Terminal` *variants* (`client.rs`/`agent.rs:568-651`). The base `AuthMethod::Agent { id, name, description }` variant and `InitializeResponse.auth_methods: Vec<AuthMethod>` itself are **not** gated (`agent.rs:145-149`, no `#[cfg]` on the field or on `AuthMethod::Agent`) — same as the TS SDK. | **No.** Hearth only reads `id`/`name`/`description` off `AuthMethod::Agent`. |
| `unstable_elicitation` | `CreateElicitationRequest`/`Response`, `CompleteElicitationNotification`, `ElicitationCapabilities` — the whole "structured user input via a form/URL" subsystem (`client.rs` imports at top, gated `AgentRequest`/`ClientResponse` arms). | No — Hearth doesn't use elicitation. |
| `unstable_end_turn_token_usage` | `PromptResponse.usage: Option<Usage>` field and the `Usage` struct itself (`agent.rs:3268-3467`) — end-of-turn token totals. **Separate** from the always-stable, streamed `UsageUpdate` session notification (§3). | No — Hearth doesn't consume turn-end usage as a distinct feature. |
| `unstable_mcp_over_acp` | `McpServer::Acp` variant, `mcp/connect`/`mcp/message`/`mcp/disconnect` request/response/notification types (`agent.rs:2815-2841`, `client.rs` imports). This is for tunnelling a *separate* MCP server's traffic through the ACP channel itself. The `McpServer::Stdio` variant Hearth actually uses ("All Agents MUST support this transport", `agent.rs:2833-2838`) carries **no** gate at all — fully stable. | **No.** Hearth's `mcpServers` array is plain stdio config, not the MCP-over-ACP tunnel. |
| `unstable_protocol_v2` | An entirely parallel `v2` schema module plus `Client::v2()`/`Agent::v2()` builder variants and a protocol-version negotiator (`role/acp.rs`, gated top to bottom). Ships a different wire version. | No — Hearth targets ACP protocol v1, same as its current TS SDK usage. |
| `unstable_session_fork` | `ForkSessionRequest`/`Response`, the `ClientRequest::ForkSessionRequest` dispatch arm, and `SessionForkCapabilities` (the only session-management capability struct that is gated; `list`/`delete`/`resume`/`close` capability structs are all stable). | No — Hearth doesn't fork sessions. |

**Model switching — the one flag Hearth's TS code needs (`unstable_setSessionModel`) has no Rust
equivalent at all, gated or otherwise, and that's by design, not omission.** Searched the entire
`v1/agent.rs` schema file for `session_model`/`SetSessionModel`/`set_session_model`: zero matches.
Instead, the Rust v1 wire schema **replaces the TS SDK's separate unstable model-switching RPC
with a single, fully-stable, generic mechanism**: `SetSessionConfigOptionRequest`/`Response`
(`agent.rs:2707-2765`, no `#[cfg]` gate) plus `SessionConfigOptionCategory::Model`
(`agent.rs:2481-2489`, also ungated) as one semantic hint among several (`Mode`, `Model`,
`ModelConfig`, `ThoughtLevel`, `Other(String)`). A model switch is just:

```rust
connection.send_request(
    SetSessionConfigOptionRequest::new(session_id, "model", SessionConfigOptionValue::value_id("model-1"))
).block_task().await?;
```

i.e., the **exact same call** Hearth already needs for `setSessionMode` in the TS SDK — Rust
unifies mode-switching and model-switching (and, per the enum, thought/reasoning-level switching)
into one `session/set_config_option` RPC, distinguished only by `configId` and the optional
`SessionConfigOptionCategory` hint. This is a genuine protocol-surface difference from the TS SDK
Hearth currently targets, not just a naming difference — there was a stable, general
config-option API added to the wire schema (v1.5.0) that supersedes the old unstable
`session/set_model` method entirely. **Hearth's Phase 3 Rust code should NOT expect an
`unstable_set_session_model`-style flag; it should call `SetSessionConfigOptionRequest` with
`config_id = "model"` for both mode and model switching, using zero unstable feature flags.**

---

## Answer summary

**1. Client/Agent trait split & stdio construction.** There are no `Agent`/`Client` traits to
implement in the TS-SDK sense. v2.0.0 uses unit-struct roles (`agent_client_protocol::role::acp::{Client, Agent, Proxy, Conductor}`, in `src/role.rs` / `src/role/acp.rs`) plus a connection-builder/dispatch model: `Client.builder().on_receive_request(...).on_receive_notification(...).connect_with(transport, async |connection: ConnectionTo<Agent>| { ... })`. The direct Rust equivalent of the TS SDK's "spawn subprocess → `ndJsonStream(output, input)` → `new ClientSideConnection(handler, stream)`" pattern is `agent_client_protocol::AcpAgent` (`src/acp_agent.rs`): `AcpAgent::from_str("command --args")` or `AcpAgent::new(AcpAgentConfig::new(cmd).arg(...).env(...))` builds a launch config; passing that `AcpAgent` as the `transport` argument to `Client.builder().connect_with(agent, ...)` spawns the subprocess, wires its stdin/stdout through the crate's internal `Lines` ndjson-framing adapter, and drives the connection — all in one call, with process-group cleanup handled automatically. This exact pattern is the crate's own published example, `examples/yolo_one_shot_client.rs`.

**2. Session lifecycle.** All four calls Hearth needs are stable/ungated request or notification structs in `agent_client_protocol::schema::v1`, sent via `connection.send_request(X::new(...)).block_task().await?`: `NewSessionRequest`/`NewSessionResponse` (`session/new`, replaces `connection.newSession()`), `LoadSessionRequest`/`LoadSessionResponse` (`session/load`, replaces `connection.loadSession()`), `PromptRequest`/`PromptResponse` (`session/prompt`, replaces `connection.prompt()`), and `CancelNotification` sent via `connection.send_notification(...)` (`session/cancel`, replaces `connection.cancel()` — this is a fire-and-forget notification with no response type in both SDKs). The schema crate (v1.5.0) additionally exposes stable `session/list`, `session/delete`, `session/resume`, `session/close` that the TS SDK Hearth currently uses does not surface — worth knowing about for Phase 3 even though not currently used. Only `session/fork` requires an unstable flag.

**3. Session-update notifications.** Yes, there is a discriminated enum that mirrors the TS SDK's `SessionNotification.update.sessionUpdate` field-for-field: `agent_client_protocol::schema::v1::SessionUpdate`, a `#[serde(tag = "sessionUpdate", rename_all = "snake_case")]` enum with variants `UserMessageChunk`, `AgentMessageChunk`, `AgentThoughtChunk`, `ToolCall`, `ToolCallUpdate`, `Plan`, `AvailableCommandsUpdate`, `CurrentModeUpdate`, `ConfigOptionUpdate`, `SessionInfoUpdate`, `UsageUpdate` — all stable/ungated and structurally identical to what Hearth already parses in TS. `Diff` is not a top-level variant; it lives inside `ToolCallContent::Diff` nested under `ToolCall`/`ToolCallUpdate`, same nesting the TS SDK uses. Two variants Hearth doesn't currently need are gated behind `unstable_plan_operations` (`PlanUpdate`, `PlanRemoved` — incremental plan edits by ID), a flag that is not even exposed by the top-level crate's Cargo features.

**4. Permission requests.** `session/request_permission` is one variant of the `AgentRequest` enum, delivered to a client-side handler registered via `.on_receive_request(async |request: RequestPermissionRequest, responder, _connection| { responder.respond(RequestPermissionResponse::new(outcome)) }, agent_client_protocol::on_receive_request!())` on the `Client` connection builder — no unstable flag required. `RequestPermissionRequest { session_id, tool_call: ToolCallUpdate, options: Vec<PermissionOption>, meta }`; `PermissionOption { option_id, name, kind: PermissionOptionKind }`; `PermissionOptionKind` is a plain 4-variant enum `AllowOnce | AllowAlways | RejectOnce | RejectAlways`, an exact match for the four kinds you named. The response is `RequestPermissionResponse { outcome: RequestPermissionOutcome }` where `RequestPermissionOutcome` is `Cancelled | Selected(SelectedPermissionOutcome { option_id })` — `Cancelled` is the required response for every pending permission request when the client has sent `session/cancel`.

**5. Unstable feature flags.** The top-level `agent-client-protocol` crate exposes exactly six Cargo features: `unstable_auth_methods`, `unstable_elicitation`, `unstable_end_turn_token_usage`, `unstable_mcp_over_acp`, `unstable_protocol_v2`, `unstable_session_fork` (plus an `unstable` umbrella covering the first five, not `unstable_protocol_v2`). **None of these six need to be enabled for Hearth's current capability set.** Auth-method advertisement (`InitializeResponse.auth_methods`, `AuthMethod::Agent{id,name,description}`) is stable in Rust exactly as it is in TS — only the extra `EnvVar`/`Terminal` auth-method variants are gated. `PromptCapabilities.image`/`embedded_context` are stable. Stdio-based MCP servers (`McpServer::Stdio`) are stable and required of every agent; only the separate MCP-*tunnelled-through-ACP* mechanism (`McpServer::Acp`, `mcp/connect`/`mcp/message`/`mcp/disconnect`) needs `unstable_mcp_over_acp`, and Hearth doesn't use that mechanism. The one genuine surprise: **there is no Rust equivalent of the TS SDK's `unstable_setSessionModel` at all — gated or otherwise.** The Rust v1 wire schema (v1.5.0) replaces that single-purpose unstable RPC with a stable, general `SetSessionConfigOptionRequest`/`session/set_config_option` call (with `SessionConfigOptionCategory::Model` as an optional UX hint), the same stable call Hearth will already be using for `setSessionMode`. **Net: Hearth's Phase 3 Rust code can enable zero `unstable_*` Cargo features on `agent-client-protocol` and implement mode-switching and model-switching through the same single stable `SetSessionConfigOptionRequest` call.**
