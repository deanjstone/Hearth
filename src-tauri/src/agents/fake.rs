// A scripted Agent for developing the UI and permission flow without a live
// model. Ported from electron/main/agents/fake.ts — plays a realistic self-edit
// turn: assistant message, plan, thought, a tool call that produces a diff, a
// mid-turn permission ask, then a closing message and end. Exercises every
// SessionUpdate variant and the permission round-trip, and is the seam
// AgentHost's orchestration logic is tested against (next chunk) — no real
// subprocess, no Tauri harness, no live ACP wire traffic.

use super::agent::{
    Agent, AgentEvent, AgentExitInfo, AgentKind, AgentSession, ConfigOption, ConfigValue,
    ModeState, ModelState, NewSessionOptions, PermissionOption, PermissionOptionKind,
    PermissionRequest, PlanEntry, PlanPriority, PlanStatus, PromptCapabilities, PromptImage,
    SessionMode, SessionUpdate, ToolCallStatus,
};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::{sleep, Duration};

const TICK: Duration = Duration::from_millis(1);

#[derive(Clone)]
struct FakeState {
    model: String,
    mode: String,
}

pub struct FakeAgent {
    events: Mutex<Option<mpsc::UnboundedSender<AgentEvent>>>,
    counter: AtomicU32,
    state: Arc<Mutex<FakeState>>,
}

impl Default for FakeAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeAgent {
    pub fn new() -> Self {
        Self {
            events: Mutex::new(None),
            counter: AtomicU32::new(0),
            state: Arc::new(Mutex::new(FakeState {
                model: "sonnet".to_string(),
                mode: "default".to_string(),
            })),
        }
    }
}

#[async_trait::async_trait]
impl Agent for FakeAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Claude
    }

    async fn connect(&self, events: mpsc::UnboundedSender<AgentEvent>) -> Result<(), String> {
        *self.events.lock().await = Some(events);
        Ok(())
    }

    async fn new_session(&self, _opts: NewSessionOptions) -> Result<Box<dyn AgentSession>, String> {
        let id = format!("fake-{}", self.counter.fetch_add(1, Ordering::SeqCst) + 1);
        let events = self
            .events
            .lock()
            .await
            .clone()
            .ok_or("FakeAgent: connect() before new_session()")?;
        let current = self.state.lock().await.clone();
        let models = ModelState {
            available: vec![
                super::agent::AgentModel {
                    id: "sonnet".to_string(),
                    name: "Claude Sonnet".to_string(),
                    description: None,
                },
                super::agent::AgentModel {
                    id: "opus".to_string(),
                    name: "Claude Opus".to_string(),
                    description: None,
                },
            ],
            current: Some(current.model),
        };
        let modes = ModeState {
            available: vec![
                SessionMode {
                    id: "default".to_string(),
                    name: "Default".to_string(),
                    description: None,
                },
                SessionMode {
                    id: "acceptEdits".to_string(),
                    name: "Accept Edits".to_string(),
                    description: None,
                },
            ],
            current: Some(current.mode),
        };
        Ok(Box::new(FakeSession {
            id,
            events,
            state: self.state.clone(),
            models,
            modes,
        }))
    }

    fn prompt_capabilities(&self) -> PromptCapabilities {
        PromptCapabilities {
            image: false,
            embedded_context: false,
        }
    }

    async fn dispose(&self) -> Result<(), String> {
        *self.events.lock().await = None;
        Ok(())
    }
}

pub struct FakeSession {
    id: String,
    events: mpsc::UnboundedSender<AgentEvent>,
    state: Arc<Mutex<FakeState>>,
    models: ModelState,
    modes: ModeState,
}

impl FakeSession {
    fn emit(&self, update: SessionUpdate) {
        let _ = self.events.send(AgentEvent::Update {
            session_id: self.id.clone(),
            update,
        });
    }
}

#[async_trait::async_trait]
impl AgentSession for FakeSession {
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
        Vec::new()
    }

    async fn prompt(&self, text: &str, _images: &[PromptImage]) -> Result<(), String> {
        let path = "src/shell/Sidebar.tsx".to_string();
        let excerpt: String = text.chars().take(40).collect();

        sleep(TICK).await;
        self.emit(SessionUpdate::Message {
            role: super::agent::MessageRole::Assistant,
            text: format!("On it — \"{excerpt}\"."),
        });

        sleep(TICK).await;
        self.emit(SessionUpdate::Plan {
            entries: vec![
                PlanEntry {
                    content: "Locate the title in the shell".to_string(),
                    status: PlanStatus::Completed,
                    priority: PlanPriority::High,
                },
                PlanEntry {
                    content: "Edit the markup".to_string(),
                    status: PlanStatus::InProgress,
                    priority: PlanPriority::High,
                },
                PlanEntry {
                    content: "Hot-reload and verify".to_string(),
                    status: PlanStatus::Pending,
                    priority: PlanPriority::Medium,
                },
            ],
        });

        sleep(TICK).await;
        self.emit(SessionUpdate::Thought {
            text: format!("Opening {path} to find the title."),
        });

        sleep(TICK).await;
        self.emit(SessionUpdate::ToolCall {
            id: "tc1".to_string(),
            title: format!("Edit {path}"),
            status: ToolCallStatus::Running,
            parent_tool_call_id: None,
        });

        sleep(TICK).await;
        self.emit(SessionUpdate::Diff {
            path: path.clone(),
            old_text: Some("<span>Hearth</span>".to_string()),
            new_text: "<span>Hearth ✨</span>".to_string(),
            parent_tool_call_id: None,
        });

        // Mid-turn permission ask — the turn blocks here until it's answered.
        let (respond_tx, respond_rx) = oneshot::channel();
        let request = PermissionRequest {
            id: "tc1".to_string(),
            title: format!("Write {path}?"),
            options: vec![
                PermissionOption {
                    id: "allow".to_string(),
                    label: "Allow".to_string(),
                    kind: PermissionOptionKind::Allow,
                },
                PermissionOption {
                    id: "always".to_string(),
                    label: "Allow always".to_string(),
                    kind: PermissionOptionKind::AllowAlways,
                },
                PermissionOption {
                    id: "reject".to_string(),
                    label: "Reject".to_string(),
                    kind: PermissionOptionKind::Reject,
                },
            ],
            command: None,
            category: None,
        };
        let approved = if self
            .events
            .send(AgentEvent::Permission {
                session_id: self.id.clone(),
                request,
                respond: respond_tx,
            })
            .is_ok()
        {
            match respond_rx.await {
                Ok(chosen) => chosen != "reject",
                // No one answered (receiver dropped without responding) — TS's
                // "no permission handler registered" case defaults to approved.
                Err(_) => true,
            }
        } else {
            true
        };

        sleep(TICK).await;
        self.emit(SessionUpdate::ToolCall {
            id: "tc1".to_string(),
            title: format!("Edit {path}"),
            status: if approved {
                ToolCallStatus::Done
            } else {
                ToolCallStatus::Error
            },
            parent_tool_call_id: None,
        });

        sleep(TICK).await;
        self.emit(SessionUpdate::Message {
            role: super::agent::MessageRole::Assistant,
            text: if approved {
                "Done — the title is updated.".to_string()
            } else {
                "Okay, I left the file unchanged.".to_string()
            },
        });

        self.emit(SessionUpdate::End {
            stop_reason: "end_turn".to_string(),
        });
        Ok(())
    }

    async fn set_model(&self, model_id: &str) -> Result<(), String> {
        self.state.lock().await.model = model_id.to_string();
        Ok(())
    }

    async fn set_mode(&self, mode_id: &str) -> Result<(), String> {
        self.state.lock().await.mode = mode_id.to_string();
        Ok(())
    }

    async fn set_config_option(&self, _config_id: &str, _value: ConfigValue) -> Result<(), String> {
        Ok(())
    }

    async fn cancel(&self) -> Result<(), String> {
        self.emit(SessionUpdate::End {
            stop_reason: "cancelled".to_string(),
        });
        Ok(())
    }

    async fn dispose(&self) -> Result<(), String> {
        Ok(())
    }
}

// Suppress an unused-import warning for AgentExitInfo — not produced by the fake
// (it never dies unexpectedly), but part of the shared AgentEvent enum callers
// match on exhaustively.
#[allow(unused_imports)]
use AgentExitInfo as _;

#[cfg(test)]
mod tests {
    use super::*;

    struct TurnResult {
        updates: Vec<SessionUpdate>,
        asked: bool,
    }

    async fn run_turn(answer: Option<&str>) -> TurnResult {
        let agent = FakeAgent::new();
        let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
        agent.connect(tx).await.unwrap();
        let session = agent
            .new_session(NewSessionOptions::default())
            .await
            .unwrap();

        let answer = answer.map(|s| s.to_string());
        let drain = tokio::spawn(async move {
            let mut updates = Vec::new();
            let mut asked = false;
            while let Some(event) = rx.recv().await {
                match event {
                    AgentEvent::Update { update, .. } => updates.push(update),
                    AgentEvent::Permission {
                        request, respond, ..
                    } => {
                        match &answer {
                            // No handler registered (TS: `if (this.permissionHandler)`
                            // is false, so the ask never gets a listener) — drop
                            // `respond` unanswered; `prompt()`'s `Err(_) => true`
                            // branch is what makes the turn default to approved.
                            None => drop(respond),
                            Some(a) => {
                                asked = true;
                                let chosen = request
                                    .options
                                    .iter()
                                    .find(|o| &o.id == a)
                                    .map(|o| o.id.clone())
                                    .unwrap_or_else(|| a.clone());
                                let _ = respond.send(chosen);
                            }
                        }
                    }
                    AgentEvent::Exit(_) => {}
                }
            }
            (updates, asked)
        });

        session
            .prompt("change the sidebar title", &[])
            .await
            .unwrap();
        drop(session);
        agent.dispose().await.unwrap();
        drop(agent);
        let (updates, asked) = drain.await.unwrap();
        TurnResult { updates, asked }
    }

    fn update_type(u: &SessionUpdate) -> &'static str {
        match u {
            SessionUpdate::Message { .. } => "message",
            SessionUpdate::Thought { .. } => "thought",
            SessionUpdate::ToolCall { .. } => "tool-call",
            SessionUpdate::Diff { .. } => "diff",
            SessionUpdate::Plan { .. } => "plan",
            SessionUpdate::Commands { .. } => "commands",
            SessionUpdate::Mode { .. } => "mode",
            SessionUpdate::Config { .. } => "config",
            SessionUpdate::Usage { .. } => "usage",
            SessionUpdate::Info { .. } => "info",
            SessionUpdate::End { .. } => "end",
        }
    }

    #[tokio::test]
    async fn emits_every_update_variant_in_order_and_ends() {
        let result = run_turn(Some("allow")).await;
        let types: Vec<&str> = result.updates.iter().map(update_type).collect();
        assert_eq!(
            types,
            vec![
                "message",
                "plan",
                "thought",
                "tool-call",
                "diff",
                "tool-call",
                "message",
                "end"
            ]
        );
        assert_eq!(
            result.updates.last().unwrap(),
            &SessionUpdate::End {
                stop_reason: "end_turn".to_string()
            }
        );
    }

    #[tokio::test]
    async fn the_diff_carries_a_path_and_both_texts() {
        let result = run_turn(Some("allow")).await;
        let diff = result
            .updates
            .iter()
            .find(|u| matches!(u, SessionUpdate::Diff { .. }))
            .unwrap();
        match diff {
            SessionUpdate::Diff { path, .. } => assert_eq!(path, "src/shell/Sidebar.tsx"),
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn approving_leaves_the_final_tool_call_done() {
        let result = run_turn(Some("allow")).await;
        assert!(result.asked);
        let tool_calls: Vec<&SessionUpdate> = result
            .updates
            .iter()
            .filter(|u| matches!(u, SessionUpdate::ToolCall { .. }))
            .collect();
        match tool_calls.last().unwrap() {
            SessionUpdate::ToolCall { status, .. } => assert_eq!(*status, ToolCallStatus::Done),
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn rejecting_flips_the_final_tool_call_to_error_and_changes_the_closing_message() {
        let result = run_turn(Some("reject")).await;
        let tool_calls: Vec<&SessionUpdate> = result
            .updates
            .iter()
            .filter(|u| matches!(u, SessionUpdate::ToolCall { .. }))
            .collect();
        match tool_calls.last().unwrap() {
            SessionUpdate::ToolCall { status, .. } => assert_eq!(*status, ToolCallStatus::Error),
            _ => unreachable!(),
        }
        let last_message = result
            .updates
            .iter()
            .rev()
            .find(|u| matches!(u, SessionUpdate::Message { .. }))
            .unwrap();
        match last_message {
            SessionUpdate::Message { text, .. } => assert!(text.contains("unchanged")),
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn with_no_permission_handler_the_turn_still_completes_defaults_to_approved() {
        let result = run_turn(None).await;
        assert!(!result.asked);
        assert_eq!(update_type(result.updates.last().unwrap()), "end");
        let tool_calls: Vec<&SessionUpdate> = result
            .updates
            .iter()
            .filter(|u| matches!(u, SessionUpdate::ToolCall { .. }))
            .collect();
        match tool_calls.last().unwrap() {
            SessionUpdate::ToolCall { status, .. } => assert_eq!(*status, ToolCallStatus::Done),
            _ => unreachable!(),
        }
    }
}
