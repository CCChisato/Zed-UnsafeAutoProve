use acp_thread::{SUBAGENT_SESSION_INFO_META_KEY, SubagentSessionInfo};
use agent_client_protocol::schema as acp;
use anyhow::Result;
use gpui::{App, SharedString, Task};
use language_model::LanguageModelToolResultContent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::rc::Rc;
use std::sync::Arc;

use crate::{AgentTool, ThreadEnvironment, ToolCallEventStream, ToolInput};

/// Spawn a sub-agent for a well-scoped task.
///
/// **Non-blocking, fire-and-forget**: This tool dispatches the subagent to run
/// in the background and returns immediately with a `session_id`. Do NOT poll
/// or wait — the subagent's results will be automatically injected into this
/// conversation as a new user message when it completes, waking you up
/// without any explicit polling needed.
///
/// ### How to use
/// 1. Call `spawn_agent` with a `label` and `message` describing the task.
/// 2. The tool returns immediately with a `session_id`.
/// 3. **End your turn immediately** — do NOT keep talking, do NOT poll.
/// 4. The subagent runs in the background independently.
/// 5. When completed, the result is automatically injected into the conversation
///    as a user message, and a new turn starts to let you process the result.
///
/// ### Designing delegated subtasks
/// - Include all relevant context (file paths, requirements, constraints) in the message.
/// - Subtasks must be concrete, well-defined, and self-contained.
/// - Do not duplicate work between your work and delegated subtasks.
/// - Do not use this tool for simple tasks you could do directly with one or two tool calls.
///
/// ### Parallel delegation
/// - Run multiple independent subtasks in parallel by calling this tool multiple times.
/// - Decompose large work into disjoint codebase slices for safe parallel execution.
///
/// ### Follow-up on existing sessions
/// - When sending a follow-up using an existing session_id, the subagent already has
///   the previous context. Send only a short, direct message.
///
/// ### Alternative: Using `terminal` tool with `claw` CLI
/// You can also run `claw subagent spawn "<task>" --output-format json` via
/// the `terminal` tool with a short timeout (e.g. 10000ms). The command returns
/// the session_id immediately. Results are auto-injected the same way.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SpawnAgentToolInput {
    /// Short label displayed in the UI while the agent runs (e.g., "Researching alternatives")
    pub label: String,
    /// The prompt for the agent. For new sessions, include full context needed for the task. For follow-ups (with session_id), you can rely on the agent already having the previous message.
    pub message: String,
    /// Session ID of an existing agent session to continue instead of creating a new one.
    #[serde(default)]
    pub session_id: Option<acp::SessionId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[serde(rename_all = "snake_case")]
pub enum SpawnAgentToolOutput {
    Success {
        session_id: acp::SessionId,
        output: String,
        session_info: SubagentSessionInfo,
    },
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(default)]
        session_id: Option<acp::SessionId>,
        error: String,
        session_info: Option<SubagentSessionInfo>,
    },
}

impl From<SpawnAgentToolOutput> for LanguageModelToolResultContent {
    fn from(output: SpawnAgentToolOutput) -> Self {
        match output {
            SpawnAgentToolOutput::Success {
                session_id,
                output,
                session_info: _, // Don't show this to the model
            } => serde_json::to_string(
                &serde_json::json!({ "session_id": session_id, "output": output }),
            )
            .unwrap_or_else(|e| format!("Failed to serialize spawn_agent output: {e}"))
            .into(),
            SpawnAgentToolOutput::Error {
                session_id,
                error,
                session_info: _, // Don't show this to the model
            } => serde_json::to_string(
                &serde_json::json!({ "session_id": session_id, "error": error }),
            )
            .unwrap_or_else(|e| format!("Failed to serialize spawn_agent output: {e}"))
            .into(),
        }
    }
}

/// Tool that spawns a sub-agent for a well-scoped task.
///
/// **Non-blocking mode**: This tool now returns immediately after dispatching
/// the subagent, providing a `session_id` that can be used with
/// `check_subagent_status` to retrieve results later. The subagent runs
/// in the background and its output is written to the notifications
/// directory (`~/.claw/sessions/notifications/`) when it completes.
///
/// The agent's `run_turn_internal` loop automatically checks for new
/// subagent completions and injects them as user messages, so the LLM
/// will see subagent results on its next turn without needing to
/// explicitly poll.
pub struct SpawnAgentTool {
    environment: Rc<dyn ThreadEnvironment>,
}

impl SpawnAgentTool {
    pub fn new(environment: Rc<dyn ThreadEnvironment>) -> Self {
        Self { environment }
    }
}

impl AgentTool for SpawnAgentTool {
    type Input = SpawnAgentToolInput;
    type Output = SpawnAgentToolOutput;

    const NAME: &'static str = "spawn_agent";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Other
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(i) => format!("Spawn subagent: {}", i.label).into(),
            Err(value) => value
                .get("label")
                .and_then(|v| v.as_str())
                .map(|s| SharedString::from(s.to_owned()))
                .unwrap_or_else(|| "Spawning agent".into()),
        }
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        cx.spawn(async move |cx| {
            let input = input
                .recv()
                .await
                .map_err(|e| SpawnAgentToolOutput::Error {
                    session_id: None,
                    error: format!("Failed to receive tool input: {e}"),
                    session_info: None,
                })?;

            // Clone AsyncApp for background use BEFORE cx.update consumes the input
            // We need to get it from the current cx before we move things into closures.
            let async_app_clone = cx.clone();

            let (subagent, subagent_session_id) = cx.update(|cx| {
                let subagent = if let Some(session_id) = input.session_id.clone() {
                    self.environment.resume_subagent(session_id, cx)
                } else {
                    self.environment.create_subagent(input.label.clone(), cx)
                };
                let subagent = subagent.map_err(|err| SpawnAgentToolOutput::Error {
                    session_id: None,
                    error: err.to_string(),
                    session_info: None,
                })?;
                let session_id = subagent.id();

                event_stream.subagent_spawned(session_id.clone());

                Ok::<_, SpawnAgentToolOutput>((subagent, session_id))
            })?;

            // NON-BLOCKING: Immediately return the session_id to the LLM.
            // Use foreground_executor().spawn() to run the subagent
            // without blocking the current turn loop. This is the same
            // thread (gpui main thread), so Send is not required.
            let bg_session_id = subagent_session_id.clone();
            let bg_message = input.message.clone();
            let bg_subagent = subagent.clone();
            let bg_cx = cx.foreground_executor().clone();

            bg_cx
                .spawn(async move {
                    let start = std::time::Instant::now();

                    let send_result = bg_subagent.send(bg_message, &async_app_clone).await;
                    let elapsed = start.elapsed();

                    let (status, output_text, error_text) = match send_result {
                        Ok(output) => ("completed", Some(output), None),
                        Err(e) => ("error", None, Some(e.to_string())),
                    };

                    // Write notification file
                    let notif = serde_json::json!({
                        "session_id": bg_session_id,
                        "status": status,
                        "duration_ms": elapsed.as_millis(),
                        "completed_at_ms": std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis(),
                        "source": "zed_native",
                        "output": output_text,
                        "error": error_text,
                    });

                    let home = std::env::var("HOME")
                        .or_else(|_| std::env::var("USERPROFILE"))
                        .unwrap_or_else(|_| ".".to_string());
                    let notif_dir = std::path::PathBuf::from(home)
                        .join(".claw")
                        .join("sessions")
                        .join("notifications");
                    let _ = std::fs::create_dir_all(&notif_dir);
                    let notif_path = notif_dir.join(format!("{}.native.json", bg_session_id));
                    let _ = std::fs::write(
                        &notif_path,
                        serde_json::to_string_pretty(&notif).unwrap_or_default(),
                    );
                })
                .detach();

            // Return immediately with just the session_id
            Ok(SpawnAgentToolOutput::Success {
                session_id: subagent_session_id.clone(),
                output: format!(
                    "Subagent dispatched. session_id={}\n\
                    The subagent is running in the background. \
                    Use `check_subagent_status` with this session_id \
                    to retrieve results when ready.",
                    subagent_session_id
                ),
                session_info: SubagentSessionInfo {
                    session_id: subagent_session_id,
                    message_start_index: 0,
                    message_end_index: None,
                },
            })
        })
    }

    fn replay(
        &self,
        _input: Self::Input,
        output: Self::Output,
        event_stream: ToolCallEventStream,
        _cx: &mut App,
    ) -> Result<()> {
        let (content, session_info) = match output {
            SpawnAgentToolOutput::Success {
                output,
                session_info,
                ..
            } => (output.into(), Some(session_info)),
            SpawnAgentToolOutput::Error {
                error,
                session_info,
                ..
            } => (error.into(), session_info),
        };

        let meta = session_info.map(|session_info| {
            acp::Meta::from_iter([(
                SUBAGENT_SESSION_INFO_META_KEY.into(),
                serde_json::json!(&session_info),
            )])
        });
        event_stream.update_fields_with_meta(
            acp::ToolCallUpdateFields::new().content(vec![content]),
            meta,
        );

        Ok(())
    }
}
