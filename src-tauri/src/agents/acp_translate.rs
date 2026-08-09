// Pure translation between the `agent-client-protocol` crate's wire types and
// Hearth's backend-agnostic `SessionUpdate`/`PermissionRequest` contracts
// (`agent.rs`). Ported from `electron/main/agents/acp-translate.ts`. Kept
// separate from `acp_client.rs` (Chunk 3b, not yet ported — owns the live
// connection) so this, where the protocol-mapping bugs live, stays
// unit-testable without spawning a real adapter subprocess.
//
// One real protocol-shape difference from the TS SDK, discovered by reading
// the crate's actual v1.5.0 schema source rather than assuming the TS port
// carries over unchanged: there is no `SessionModelState` wire type in the
// Rust schema at all. TS's `unstable_setSessionModel` RPC (and its
// model-listing counterpart) were both superseded by the stable, generic
// `SessionConfigOption`/`session/set_config_option` mechanism — a model list
// is just a `SessionConfigOption` whose `category` is
// `SessionConfigOptionCategory::Model`. So `normalize_models` here derives
// Hearth's `ModelState` from the SAME `Vec<SessionConfigOption>` that also
// feeds `normalize_config_options`, rather than from a dedicated field TS's
// version reads — this is the pure-function equivalent of what `acp_client.rs`
// will need to do with `NewSessionResponse.config_options` when it builds an
// `AgentSession`'s `models()`/`config_options()` snapshot.

use super::agent::{
    AgentModel, AvailableCommand, ConfigOption, ConfigSelectOption, Cost, MessageRole, ModeState,
    ModelState, PermissionCategory, PermissionOption,
    PermissionOptionKind as HearthPermissionOptionKind, PermissionRequest, PlanEntry,
    PlanPriority as HearthPlanPriority, PlanStatus as HearthPlanStatus,
    SessionMode as HearthSessionMode, SessionUpdate, ToolCallStatus as HearthToolCallStatus, Usage,
};
use agent_client_protocol::schema::v1 as acp;
use agent_client_protocol::schema::MaybeUndefined;
use std::collections::HashMap;

/// Normalize the crate's `SessionModeState` into Hearth's `{available, current}`.
/// Pure. `None` (an agent that doesn't advertise modes) becomes the empty state.
pub fn normalize_modes(state: Option<&acp::SessionModeState>) -> ModeState {
    match state {
        None => ModeState::default(),
        Some(s) => ModeState {
            available: s
                .available_modes
                .iter()
                .map(|m| HearthSessionMode {
                    id: m.id.to_string(),
                    name: m.name.clone(),
                    description: m.description.clone(),
                })
                .collect(),
            current: Some(s.current_mode_id.to_string()),
        },
    }
}

/// Derive Hearth's `ModelState` from the config-option list — see the module
/// doc for why there's no dedicated wire type to read this from directly. The
/// first (there should only ever be one) option categorized `Model` wins;
/// anything else (no such option, or a non-`select` `Model`-categorized
/// option, which wouldn't make semantic sense) yields the empty state.
pub fn normalize_models(options: Option<&[acp::SessionConfigOption]>) -> ModelState {
    let Some(options) = options else {
        return ModelState::default();
    };
    let model_option = options
        .iter()
        .find(|o| matches!(o.category, Some(acp::SessionConfigOptionCategory::Model)));
    match model_option.map(|o| &o.kind) {
        Some(acp::SessionConfigKind::Select(select)) => ModelState {
            available: flatten_select_options(&select.options)
                .into_iter()
                .map(|o| AgentModel {
                    id: o.value,
                    name: o.name,
                    description: o.description,
                })
                .collect(),
            current: Some(select.current_value.to_string()),
        },
        _ => ModelState::default(),
    }
}

/// Normalize the crate's `SessionConfigOption`s into Hearth's `ConfigOption`s.
/// Pure. Grouped selects are flattened into a single option list (adapters use
/// flat options today; flattening is forward-safe), mirroring
/// `normalizeConfigOptions`'s TS behavior exactly.
pub fn normalize_config_options(options: Option<&[acp::SessionConfigOption]>) -> Vec<ConfigOption> {
    let Some(options) = options else {
        return Vec::new();
    };
    options
        .iter()
        .filter_map(normalize_one_config_option)
        .collect()
}

/// `None` for a config-option payload shape this build doesn't know about —
/// `SessionConfigKind` is `#[non_exhaustive]` upstream, so a future variant
/// (compiled against an older `agent-client-protocol` than the adapter speaks)
/// is skipped rather than guessed at, mirroring `normalizeConfigOptions`'s own
/// "unknown shapes are skipped" behavior.
fn normalize_one_config_option(o: &acp::SessionConfigOption) -> Option<ConfigOption> {
    let id = o.id.to_string();
    let name = o.name.clone();
    let description = o.description.clone();
    let category = o.category.as_ref().map(config_category_label);
    match &o.kind {
        acp::SessionConfigKind::Boolean(b) => Some(ConfigOption::Boolean {
            id,
            name,
            description,
            category,
            current: b.current_value,
        }),
        acp::SessionConfigKind::Select(s) => Some(ConfigOption::Select {
            id,
            name,
            description,
            category,
            current: s.current_value.to_string(),
            options: flatten_select_options(&s.options),
        }),
        _ => None,
    }
}

fn flatten_select_options(options: &acp::SessionConfigSelectOptions) -> Vec<ConfigSelectOption> {
    match options {
        acp::SessionConfigSelectOptions::Ungrouped(opts) => {
            opts.iter().map(to_select_option).collect()
        }
        acp::SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|g| g.options.iter())
            .map(to_select_option)
            .collect(),
        // A future grouping shape this build predates — no options to list.
        _ => Vec::new(),
    }
}

fn to_select_option(o: &acp::SessionConfigSelectOption) -> ConfigSelectOption {
    ConfigSelectOption {
        value: o.value.to_string(),
        name: o.name.clone(),
        description: o.description.clone(),
    }
}

/// TS's `SessionConfigOption.category` is already a plain string; the crate
/// models it as an enum (with an open `Other(String)` catch-all), so this maps
/// back to the same snake_case labels the renderer already expects.
fn config_category_label(c: &acp::SessionConfigOptionCategory) -> String {
    match c {
        acp::SessionConfigOptionCategory::Mode => "mode".to_string(),
        acp::SessionConfigOptionCategory::Model => "model".to_string(),
        acp::SessionConfigOptionCategory::ModelConfig => "model_config".to_string(),
        acp::SessionConfigOptionCategory::ThoughtLevel => "thought_level".to_string(),
        acp::SessionConfigOptionCategory::Other(label) => label.clone(),
        // A future named category this build predates.
        _ => "other".to_string(),
    }
}

/// ACP tool-call status → our coarser lifecycle. `None` covers both a bare
/// `ToolCall` whose status was never sent on the wire (defaults to `Pending`
/// upstream by serde, so this only sees `None` for a `ToolCallUpdate` that
/// isn't touching status) and a `ToolCallUpdateFields.status` that's genuinely
/// absent — mirrors `mapToolStatus`'s `status: ... | null | undefined` param.
pub fn map_tool_status(
    status: Option<acp::ToolCallStatus>,
    is_update: bool,
) -> HearthToolCallStatus {
    match status {
        Some(acp::ToolCallStatus::Pending) => HearthToolCallStatus::Pending,
        Some(acp::ToolCallStatus::InProgress) => HearthToolCallStatus::Running,
        Some(acp::ToolCallStatus::Completed) => HearthToolCallStatus::Done,
        Some(acp::ToolCallStatus::Failed) => HearthToolCallStatus::Error,
        // A bare tool_call with no status is just starting; an update with no
        // status (or, defensively, a future status variant this build
        // predates) is mid-flight.
        _ if is_update => HearthToolCallStatus::Running,
        _ => HearthToolCallStatus::Pending,
    }
}

/// ACP permission-option kind → our three-way allow/allow-always/reject.
pub fn map_permission_kind(kind: acp::PermissionOptionKind) -> HearthPermissionOptionKind {
    match kind {
        acp::PermissionOptionKind::AllowOnce => HearthPermissionOptionKind::Allow,
        acp::PermissionOptionKind::AllowAlways => HearthPermissionOptionKind::AllowAlways,
        acp::PermissionOptionKind::RejectOnce | acp::PermissionOptionKind::RejectAlways => {
            HearthPermissionOptionKind::Reject
        }
        // A future permission-option kind this build predates — deny by
        // default rather than silently allowing an operation we don't
        // recognize.
        _ => HearthPermissionOptionKind::Reject,
    }
}

/// Translate an ACP permission request into our UI-facing shape. The tool
/// call's id doubles as the permission id — it's stable and ties the answer
/// back to the tool that asked.
pub fn translate_permission(req: &acp::RequestPermissionRequest) -> PermissionRequest {
    // Surface the raw shell command (when present) so the policy layer can
    // auto-reject source-mutating shell. ACP carries it in the tool call's
    // raw input.
    let command = req
        .tool_call
        .fields
        .raw_input
        .as_ref()
        .and_then(|v| v.get("command"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    // Coarse category for the renderer's Command-approval tiers. A present raw
    // command means shell regardless of how the kind is labelled; otherwise
    // map the ACP tool kind. Unknown/missing kinds fall to `Other`
    // (auto-approvable).
    let kind = req.tool_call.fields.kind;
    let category = if command.is_some() || kind == Some(acp::ToolKind::Execute) {
        PermissionCategory::Execute
    } else if matches!(
        kind,
        Some(acp::ToolKind::Edit) | Some(acp::ToolKind::Delete) | Some(acp::ToolKind::Move)
    ) {
        PermissionCategory::Edit
    } else {
        PermissionCategory::Other
    };
    PermissionRequest {
        id: req.tool_call.tool_call_id.to_string(),
        title: req
            .tool_call
            .fields
            .title
            .clone()
            .unwrap_or_else(|| "Permission requested".to_string()),
        options: req
            .options
            .iter()
            .map(|o| PermissionOption {
                id: o.option_id.to_string(),
                label: o.name.clone(),
                kind: map_permission_kind(o.kind),
            })
            .collect(),
        command,
        category: Some(category),
    }
}

/// The parent Task tool-call id when this update came from inside a subagent.
/// The Claude adapter stashes it at `_meta.claudeCode.parentToolUseId`; ACP's
/// `_meta` is an open record so we read it defensively. Codex doesn't
/// populate it (returns nothing → treated as the main thread).
fn parent_tool_call_id(meta: &Option<acp::Meta>) -> Option<String> {
    meta.as_ref()?
        .get("claudeCode")?
        .get("parentToolUseId")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn diffs_from_content(
    content: &[acp::ToolCallContent],
    parent: Option<&str>,
) -> Vec<SessionUpdate> {
    content
        .iter()
        .filter_map(|item| match item {
            acp::ToolCallContent::Diff(d) => Some(SessionUpdate::Diff {
                path: d.path.to_string_lossy().into_owned(),
                old_text: d.old_text.clone(),
                new_text: d.new_text.clone(),
                parent_tool_call_id: parent.map(|s| s.to_string()),
            }),
            _ => None,
        })
        .collect()
}

fn map_plan_status(status: &acp::PlanEntryStatus) -> HearthPlanStatus {
    match status {
        acp::PlanEntryStatus::Pending => HearthPlanStatus::Pending,
        acp::PlanEntryStatus::InProgress => HearthPlanStatus::InProgress,
        acp::PlanEntryStatus::Completed => HearthPlanStatus::Completed,
        // A future plan-entry status this build predates.
        _ => HearthPlanStatus::Pending,
    }
}

fn map_plan_priority(priority: &acp::PlanEntryPriority) -> HearthPlanPriority {
    match priority {
        acp::PlanEntryPriority::High => HearthPlanPriority::High,
        acp::PlanEntryPriority::Medium => HearthPlanPriority::Medium,
        acp::PlanEntryPriority::Low => HearthPlanPriority::Low,
        // A future plan-entry priority this build predates.
        _ => HearthPlanPriority::Medium,
    }
}

/// Translate one ACP session update into zero or more Hearth updates. A
/// single tool call can yield a status update plus one diff per modified
/// file, so this returns a `Vec`.
///
/// `titles` caches tool-call titles by id: ACP `tool_call_update`
/// notifications usually omit the title, so we backfill from the originating
/// `tool_call`. The map is read and written here by design — pass a
/// per-session map (mirrors TS's `translateUpdate(update, titles)`).
pub fn translate_update(
    update: &acp::SessionUpdate,
    titles: &mut HashMap<String, String>,
) -> Vec<SessionUpdate> {
    match update {
        acp::SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
            acp::ContentBlock::Text(t) => vec![SessionUpdate::Message {
                role: MessageRole::Assistant,
                text: t.text.clone(),
            }],
            _ => Vec::new(),
        },
        acp::SessionUpdate::AgentThoughtChunk(chunk) => match &chunk.content {
            acp::ContentBlock::Text(t) => vec![SessionUpdate::Thought {
                text: t.text.clone(),
            }],
            _ => Vec::new(),
        },
        acp::SessionUpdate::ToolCall(tc) => {
            let id = tc.tool_call_id.to_string();
            titles.insert(id.clone(), tc.title.clone());
            let parent = parent_tool_call_id(&tc.meta);
            let mut out = vec![SessionUpdate::ToolCall {
                id,
                title: tc.title.clone(),
                status: map_tool_status(Some(tc.status), false),
                parent_tool_call_id: parent.clone(),
            }];
            out.extend(diffs_from_content(&tc.content, parent.as_deref()));
            out
        }
        acp::SessionUpdate::ToolCallUpdate(upd) => {
            let id = upd.tool_call_id.to_string();
            if let Some(title) = &upd.fields.title {
                titles.insert(id.clone(), title.clone());
            }
            let title = upd
                .fields
                .title
                .clone()
                .or_else(|| titles.get(&id).cloned())
                .unwrap_or_else(|| "Tool call".to_string());
            let parent = parent_tool_call_id(&upd.meta);
            let mut out = vec![SessionUpdate::ToolCall {
                id,
                title,
                status: map_tool_status(upd.fields.status, true),
                parent_tool_call_id: parent.clone(),
            }];
            if let Some(content) = &upd.fields.content {
                out.extend(diffs_from_content(content, parent.as_deref()));
            }
            out
        }
        acp::SessionUpdate::Plan(plan) => vec![SessionUpdate::Plan {
            entries: plan
                .entries
                .iter()
                .map(|e| PlanEntry {
                    content: e.content.clone(),
                    status: map_plan_status(&e.status),
                    priority: map_plan_priority(&e.priority),
                })
                .collect(),
        }],
        acp::SessionUpdate::AvailableCommandsUpdate(u) => vec![SessionUpdate::Commands {
            commands: u
                .available_commands
                .iter()
                .map(|c| AvailableCommand {
                    name: c.name.clone(),
                    description: if c.description.is_empty() {
                        None
                    } else {
                        Some(c.description.clone())
                    },
                })
                .collect(),
        }],
        acp::SessionUpdate::CurrentModeUpdate(u) => vec![SessionUpdate::Mode {
            current: u.current_mode_id.to_string(),
        }],
        acp::SessionUpdate::ConfigOptionUpdate(u) => vec![SessionUpdate::Config {
            options: normalize_config_options(Some(&u.config_options)),
        }],
        acp::SessionUpdate::UsageUpdate(u) => vec![SessionUpdate::Usage {
            usage: Usage {
                used: u.used,
                size: u.size,
                cost: u.cost.as_ref().map(|c| Cost {
                    amount: c.amount,
                    currency: c.currency.clone(),
                }),
            },
        }],
        // Agent-supplied session title (W9). Only emit when a non-empty title
        // is set; the renderer uses it to auto-title the session.
        acp::SessionUpdate::SessionInfoUpdate(u) => match &u.title {
            MaybeUndefined::Value(t) if !t.is_empty() => {
                vec![SessionUpdate::Info { title: t.clone() }]
            }
            _ => Vec::new(),
        },
        // The echo of the user's own message (UserMessageChunk) isn't part of
        // the surfaces Hearth drives, and this enum is `#[non_exhaustive]` in
        // the crate (future variants — none compiled in with default
        // features today — must also fall through here).
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_chunk(text: &str) -> acp::ContentChunk {
        acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(text)))
    }

    #[test]
    fn agent_message_chunk_becomes_an_assistant_message() {
        let update = acp::SessionUpdate::AgentMessageChunk(text_chunk("hi there"));
        let mut titles = HashMap::new();
        assert_eq!(
            translate_update(&update, &mut titles),
            vec![SessionUpdate::Message {
                role: MessageRole::Assistant,
                text: "hi there".to_string()
            }]
        );
    }

    #[test]
    fn agent_thought_chunk_becomes_a_thought() {
        let update = acp::SessionUpdate::AgentThoughtChunk(text_chunk("thinking..."));
        let mut titles = HashMap::new();
        assert_eq!(
            translate_update(&update, &mut titles),
            vec![SessionUpdate::Thought {
                text: "thinking...".to_string()
            }]
        );
    }

    #[test]
    fn user_message_chunk_is_not_forwarded() {
        let update = acp::SessionUpdate::UserMessageChunk(text_chunk("echo"));
        let mut titles = HashMap::new();
        assert_eq!(translate_update(&update, &mut titles), Vec::new());
    }

    #[test]
    fn tool_call_caches_its_title_and_yields_a_pending_status_by_default() {
        let update = acp::SessionUpdate::ToolCall(acp::ToolCall::new("tc1", "Edit file"));
        let mut titles = HashMap::new();
        assert_eq!(
            translate_update(&update, &mut titles),
            vec![SessionUpdate::ToolCall {
                id: "tc1".to_string(),
                title: "Edit file".to_string(),
                status: HearthToolCallStatus::Pending,
                parent_tool_call_id: None,
            }]
        );
        assert_eq!(titles.get("tc1"), Some(&"Edit file".to_string()));
    }

    #[test]
    fn tool_call_with_a_diff_yields_both_the_status_and_the_diff() {
        let update = acp::SessionUpdate::ToolCall(
            acp::ToolCall::new("tc1", "Edit file")
                .status(acp::ToolCallStatus::InProgress)
                .content(vec![acp::ToolCallContent::Diff(acp::Diff::new(
                    "src/a.ts", "new",
                ))]),
        );
        let mut titles = HashMap::new();
        let out = translate_update(&update, &mut titles);
        assert_eq!(
            out,
            vec![
                SessionUpdate::ToolCall {
                    id: "tc1".to_string(),
                    title: "Edit file".to_string(),
                    status: HearthToolCallStatus::Running,
                    parent_tool_call_id: None,
                },
                SessionUpdate::Diff {
                    path: "src/a.ts".to_string(),
                    old_text: None,
                    new_text: "new".to_string(),
                    parent_tool_call_id: None,
                },
            ]
        );
    }

    #[test]
    fn tool_call_update_backfills_the_title_from_the_cache_when_omitted() {
        let mut titles = HashMap::new();
        titles.insert("tc1".to_string(), "Edit file".to_string());
        let update = acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            "tc1",
            acp::ToolCallUpdateFields::new().status(acp::ToolCallStatus::Completed),
        ));
        assert_eq!(
            translate_update(&update, &mut titles),
            vec![SessionUpdate::ToolCall {
                id: "tc1".to_string(),
                title: "Edit file".to_string(),
                status: HearthToolCallStatus::Done,
                parent_tool_call_id: None,
            }]
        );
    }

    #[test]
    fn tool_call_update_with_no_status_and_no_cached_title_defaults_to_running_and_a_generic_title()
    {
        let mut titles = HashMap::new();
        let update = acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            "tc1",
            acp::ToolCallUpdateFields::new(),
        ));
        assert_eq!(
            translate_update(&update, &mut titles),
            vec![SessionUpdate::ToolCall {
                id: "tc1".to_string(),
                title: "Tool call".to_string(),
                status: HearthToolCallStatus::Running,
                parent_tool_call_id: None,
            }]
        );
    }

    #[test]
    fn a_failed_tool_call_status_maps_to_error() {
        assert_eq!(
            map_tool_status(Some(acp::ToolCallStatus::Failed), true),
            HearthToolCallStatus::Error
        );
    }

    #[test]
    fn plan_entries_translate_status_and_priority() {
        let update = acp::SessionUpdate::Plan(acp::Plan::new(vec![acp::PlanEntry::new(
            "do the thing",
            acp::PlanEntryPriority::High,
            acp::PlanEntryStatus::InProgress,
        )]));
        let mut titles = HashMap::new();
        assert_eq!(
            translate_update(&update, &mut titles),
            vec![SessionUpdate::Plan {
                entries: vec![PlanEntry {
                    content: "do the thing".to_string(),
                    status: HearthPlanStatus::InProgress,
                    priority: HearthPlanPriority::High,
                }]
            }]
        );
    }

    #[test]
    fn available_commands_update_maps_empty_description_to_none() {
        let update =
            acp::SessionUpdate::AvailableCommandsUpdate(acp::AvailableCommandsUpdate::new(vec![
                acp::AvailableCommand::new("plan", ""),
            ]));
        let mut titles = HashMap::new();
        assert_eq!(
            translate_update(&update, &mut titles),
            vec![SessionUpdate::Commands {
                commands: vec![AvailableCommand {
                    name: "plan".to_string(),
                    description: None,
                }]
            }]
        );
    }

    #[test]
    fn current_mode_update_maps_to_a_mode_update() {
        let update = acp::SessionUpdate::CurrentModeUpdate(acp::CurrentModeUpdate::new("agent"));
        let mut titles = HashMap::new();
        assert_eq!(
            translate_update(&update, &mut titles),
            vec![SessionUpdate::Mode {
                current: "agent".to_string()
            }]
        );
    }

    #[test]
    fn usage_update_carries_cost_when_present() {
        let update = acp::SessionUpdate::UsageUpdate(
            acp::UsageUpdate::new(10, 100).cost(acp::Cost::new(0.5, "USD")),
        );
        let mut titles = HashMap::new();
        assert_eq!(
            translate_update(&update, &mut titles),
            vec![SessionUpdate::Usage {
                usage: Usage {
                    used: 10,
                    size: 100,
                    cost: Some(Cost {
                        amount: 0.5,
                        currency: "USD".to_string()
                    })
                }
            }]
        );
    }

    #[test]
    fn session_info_update_is_dropped_when_the_title_is_absent_or_empty() {
        let mut titles = HashMap::new();
        assert_eq!(
            translate_update(
                &acp::SessionUpdate::SessionInfoUpdate(acp::SessionInfoUpdate::new()),
                &mut titles
            ),
            Vec::new()
        );
        assert_eq!(
            translate_update(
                &acp::SessionUpdate::SessionInfoUpdate(acp::SessionInfoUpdate::new().title("")),
                &mut titles
            ),
            Vec::new()
        );
    }

    #[test]
    fn session_info_update_with_a_title_becomes_an_info_update() {
        let mut titles = HashMap::new();
        assert_eq!(
            translate_update(
                &acp::SessionUpdate::SessionInfoUpdate(
                    acp::SessionInfoUpdate::new().title("New chat title")
                ),
                &mut titles
            ),
            vec![SessionUpdate::Info {
                title: "New chat title".to_string()
            }]
        );
    }

    #[test]
    fn permission_kind_maps_both_reject_variants_to_reject() {
        assert_eq!(
            map_permission_kind(acp::PermissionOptionKind::RejectOnce),
            HearthPermissionOptionKind::Reject
        );
        assert_eq!(
            map_permission_kind(acp::PermissionOptionKind::RejectAlways),
            HearthPermissionOptionKind::Reject
        );
        assert_eq!(
            map_permission_kind(acp::PermissionOptionKind::AllowAlways),
            HearthPermissionOptionKind::AllowAlways
        );
    }

    #[test]
    fn translate_permission_extracts_the_raw_shell_command_and_categorizes_as_execute() {
        let tool_call = acp::ToolCallUpdate::new(
            "tc1",
            acp::ToolCallUpdateFields::new()
                .title("Run it?")
                .raw_input(serde_json::json!({ "command": "rm -rf /" })),
        );
        let req = acp::RequestPermissionRequest::new(
            "sess1",
            tool_call,
            vec![acp::PermissionOption::new(
                "allow",
                "Allow",
                acp::PermissionOptionKind::AllowOnce,
            )],
        );
        let translated = translate_permission(&req);
        assert_eq!(translated.id, "tc1");
        assert_eq!(translated.title, "Run it?");
        assert_eq!(translated.command, Some("rm -rf /".to_string()));
        assert_eq!(translated.category, Some(PermissionCategory::Execute));
        assert_eq!(translated.options.len(), 1);
        assert_eq!(
            translated.options[0].kind,
            HearthPermissionOptionKind::Allow
        );
    }

    #[test]
    fn translate_permission_falls_back_to_a_generic_title_and_other_category() {
        let tool_call = acp::ToolCallUpdate::new("tc1", acp::ToolCallUpdateFields::new());
        let req = acp::RequestPermissionRequest::new("sess1", tool_call, vec![]);
        let translated = translate_permission(&req);
        assert_eq!(translated.title, "Permission requested");
        assert_eq!(translated.command, None);
        assert_eq!(translated.category, Some(PermissionCategory::Other));
    }

    #[test]
    fn translate_permission_categorizes_edit_kind_as_edit() {
        let tool_call = acp::ToolCallUpdate::new(
            "tc1",
            acp::ToolCallUpdateFields::new().kind(acp::ToolKind::Edit),
        );
        let req = acp::RequestPermissionRequest::new("sess1", tool_call, vec![]);
        assert_eq!(
            translate_permission(&req).category,
            Some(PermissionCategory::Edit)
        );
    }

    #[test]
    fn normalize_modes_reports_the_empty_state_when_the_agent_advertises_none() {
        assert_eq!(normalize_modes(None), ModeState::default());
    }

    #[test]
    fn normalize_modes_maps_available_and_current() {
        let state = acp::SessionModeState::new(
            "default",
            vec![acp::SessionMode::new("default", "Default")],
        );
        let modes = normalize_modes(Some(&state));
        assert_eq!(modes.current, Some("default".to_string()));
        assert_eq!(modes.available.len(), 1);
        assert_eq!(modes.available[0].id, "default");
    }

    #[test]
    fn normalize_models_derives_from_the_model_categorized_config_option() {
        let options = vec![acp::SessionConfigOption::new(
            "model",
            "Model",
            acp::SessionConfigKind::Select(acp::SessionConfigSelect::new(
                "sonnet",
                vec![
                    acp::SessionConfigSelectOption::new("sonnet", "Sonnet"),
                    acp::SessionConfigSelectOption::new("opus", "Opus"),
                ],
            )),
        )
        .category(acp::SessionConfigOptionCategory::Model)];
        let models = normalize_models(Some(&options));
        assert_eq!(models.current, Some("sonnet".to_string()));
        assert_eq!(models.available.len(), 2);
        assert_eq!(models.available[0].id, "sonnet");
    }

    #[test]
    fn normalize_models_is_empty_when_no_option_is_categorized_model() {
        let options = vec![acp::SessionConfigOption::new(
            "verbose",
            "Verbose",
            acp::SessionConfigKind::Boolean(acp::SessionConfigBoolean::new(true)),
        )];
        assert_eq!(normalize_models(Some(&options)), ModelState::default());
    }

    #[test]
    fn normalize_config_options_flattens_grouped_selects() {
        let options = vec![acp::SessionConfigOption::new(
            "reasoning",
            "Reasoning",
            acp::SessionConfigKind::Select(acp::SessionConfigSelect::new(
                "medium",
                vec![acp::SessionConfigSelectGroup::new(
                    "levels",
                    "Levels",
                    vec![acp::SessionConfigSelectOption::new("medium", "Medium")],
                )],
            )),
        )
        .category(acp::SessionConfigOptionCategory::ThoughtLevel)];
        let out = normalize_config_options(Some(&options));
        assert_eq!(out.len(), 1);
        match &out[0] {
            ConfigOption::Select {
                category, options, ..
            } => {
                assert_eq!(category.as_deref(), Some("thought_level"));
                assert_eq!(options.len(), 1);
                assert_eq!(options[0].value, "medium");
            }
            _ => panic!("expected a Select config option"),
        }
    }

    #[test]
    fn normalize_config_options_maps_boolean_options() {
        let options = vec![acp::SessionConfigOption::new(
            "verbose",
            "Verbose",
            acp::SessionConfigKind::Boolean(acp::SessionConfigBoolean::new(true)),
        )];
        let out = normalize_config_options(Some(&options));
        assert_eq!(
            out,
            vec![ConfigOption::Boolean {
                id: "verbose".to_string(),
                name: "Verbose".to_string(),
                description: None,
                category: None,
                current: true,
            }]
        );
    }
}
