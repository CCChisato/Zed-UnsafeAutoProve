use agent_client_protocol::schema as acp;
use anyhow::Result;
use gpui::{App, SharedString, Task};
use language_model::LanguageModelToolResultContent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use crate::{AgentTool, ThreadEnvironment, ToolCallEventStream, ToolInput};

/// Check the status of a previously spawned subagent.
///
/// Supports both:
/// 1. **Zed native subagents** (spawned via `spawn_agent` tool) — checks
///    `~/.claw/sessions/notifications/<session_id>.native.json`
/// 2. **Claw subagents** (spawned via `claw subagent spawn` in terminal) —
///    runs `claw subagent status <session_id>`
///
/// For claw subagents with structured reports:
/// When spawned without `--raw`, the output includes a `##SUBAGENT_REPORT##`
/// section with fields: task, status, summary, key_findings, raw_output.
/// The `report` field contains these parsed values when available.
///
/// ## Notification directory
/// If `session_id` is `"check_all"`, the tool will scan the notification
/// directory (`~/.claw/sessions/notifications/`) for any new completion
/// notifications (both .native.json and .notification.json) and return
/// all of them.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CheckSubagentStatusToolInput {
    /// The session_id returned by `claw subagent spawn`.
    /// Use "check_all" to scan the notification directory for new completions.
    pub session_id: String,
    /// Working directory (project root) where claw was called from
    pub cd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckSubagentStatusToolOutput {
    pub session_id: String,
    pub status: String,
    pub output: Option<String>,
    pub error: Option<String>,
    pub duration_ms: Option<u64>,
    /// Parsed `##SUBAGENT_REPORT##` section, if available.
    /// Contains fields like: task, status, summary, key_findings, raw_output.
    pub report: Option<serde_json::Value>,
    /// When session_id is "check_all", contains all pending notifications.
    pub all_notifications: Option<Vec<serde_json::Value>>,
}

impl From<CheckSubagentStatusToolOutput> for LanguageModelToolResultContent {
    fn from(output: CheckSubagentStatusToolOutput) -> Self {
        serde_json::to_string(&output)
            .unwrap_or_else(|e| format!("Failed to serialize: {e}"))
            .into()
    }
}

pub struct CheckSubagentStatusTool {
    environment: Rc<dyn ThreadEnvironment>,
}

impl CheckSubagentStatusTool {
    pub fn new(environment: Rc<dyn ThreadEnvironment>) -> Self {
        Self { environment }
    }

    /// Build the notifications directory path.
    fn notifications_dir() -> PathBuf {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join(".claw")
            .join("sessions")
            .join("notifications")
    }
}

impl AgentTool for CheckSubagentStatusTool {
    type Input = CheckSubagentStatusToolInput;
    type Output = CheckSubagentStatusToolOutput;

    const NAME: &'static str = "check_subagent_status";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Read
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(i) => format!("Check subagent {}", i.session_id).into(),
            Err(_) => "Check subagent status".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        cx.spawn(async move |cx| {
            let input = input
                .recv()
                .await
                .map_err(|e| CheckSubagentStatusToolOutput {
                    session_id: String::new(),
                    status: "error".into(),
                    output: None,
                    error: Some(format!("Failed to receive tool input: {e}")),
                    duration_ms: None,
                    report: None,
                    all_notifications: None,
                })?;

            // Special case: "check_all" scans the notification directory
            if input.session_id == "check_all" {
                let notif_dir = Self::notifications_dir();
                let mut notifications = Vec::new();
                if let Ok(entries) = fs::read_dir(&notif_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|e| e.to_str()) == Some("json") {
                            if let Ok(content) = fs::read_to_string(&path) {
                                if let Ok(notif) =
                                    serde_json::from_str::<serde_json::Value>(&content)
                                {
                                    notifications.push(notif);
                                    // Remove notification file after reading
                                    let _ = fs::remove_file(&path);
                                }
                            }
                        }
                    }
                }
                return Ok(CheckSubagentStatusToolOutput {
                    session_id: "check_all".to_string(),
                    status: if notifications.is_empty() {
                        "no_notifications"
                    } else {
                        "notifications_found"
                    }
                    .to_string(),
                    output: None,
                    error: None,
                    duration_ms: None,
                    report: None,
                    all_notifications: Some(notifications),
                });
            }

            // First, check if this is a native subagent (has a .native.json notification file)
            let notif_path =
                Self::notifications_dir().join(format!("{}.native.json", input.session_id));
            if notif_path.exists() {
                if let Ok(content) = fs::read_to_string(&notif_path) {
                    if let Ok(notif) = serde_json::from_str::<serde_json::Value>(&content) {
                        let status = notif
                            .get("status")
                            .and_then(|s| s.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let output_text = notif
                            .get("output")
                            .and_then(|s| s.as_str())
                            .map(|s| s.to_string());
                        let error_text = notif
                            .get("error")
                            .and_then(|s| s.as_str())
                            .map(|s| s.to_string());
                        let duration = notif.get("duration_ms").and_then(|s| s.as_u64());

                        // Don't delete the native notification here — it will be
                        // picked up and deleted by check_and_inject_subagent_notifications
                        // in run_turn_internal.

                        return Ok(CheckSubagentStatusToolOutput {
                            session_id: input.session_id.clone(),
                            status,
                            output: output_text,
                            error: error_text,
                            duration_ms: duration,
                            report: None,
                            all_notifications: None,
                        });
                    }
                }
            }

            // Fall back to checking via claw CLI (claw subagents)
            let terminal = self
                .environment
                .create_terminal(
                    format!(
                        "claw subagent status {} --output-format json",
                        input.session_id
                    ),
                    Some(PathBuf::from(&input.cd)),
                    Some(16 * 1024),
                    cx,
                )
                .await
                .map_err(|e| CheckSubagentStatusToolOutput {
                    session_id: input.session_id.clone(),
                    status: "error".into(),
                    output: None,
                    error: Some(e.to_string()),
                    duration_ms: None,
                    report: None,
                    all_notifications: None,
                })?;

            // Wait for the command to complete (claw subagent status returns immediately)
            let wait_for_exit =
                terminal
                    .wait_for_exit(cx)
                    .map_err(|e| CheckSubagentStatusToolOutput {
                        session_id: input.session_id.clone(),
                        status: "error".into(),
                        output: None,
                        error: Some(e.to_string()),
                        duration_ms: None,
                        report: None,
                        all_notifications: None,
                    })?;
            wait_for_exit.await;

            let output_response =
                terminal
                    .current_output(cx)
                    .map_err(|e| CheckSubagentStatusToolOutput {
                        session_id: input.session_id.clone(),
                        status: "error".into(),
                        output: None,
                        error: Some(e.to_string()),
                        duration_ms: None,
                        report: None,
                        all_notifications: None,
                    })?;

            let combined = output_response.output.trim().to_string();

            // Try to parse the JSON output from claw
            // The new JSON format (with --no-full) includes a "report" field.
            // The old format includes "output" (raw JSONL).
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&combined) {
                let status = value
                    .get("status")
                    .and_then(|s| s.as_str())
                    .unwrap_or("unknown")
                    .to_string();

                // Check if we got the new augmented format with a "report" field
                let report = value.get("report").cloned();

                // In the new format, the full output might be absent;
                // in the old format, "output" contains the raw JSONL.
                let output_text = value
                    .get("output")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());

                let error_text = value
                    .get("error")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());

                let duration = value.get("duration_ms").and_then(|s| s.as_u64());

                return Ok(CheckSubagentStatusToolOutput {
                    session_id: input.session_id.clone(),
                    status,
                    output: output_text,
                    error: error_text,
                    duration_ms: duration,
                    report,
                    all_notifications: None,
                });
            }

            // If we can't parse JSON, return a text-based status indicating we got raw output
            Ok(CheckSubagentStatusToolOutput {
                session_id: input.session_id.clone(),
                status: "unknown".into(),
                output: Some(combined),
                error: None,
                duration_ms: None,
                report: None,
                all_notifications: None,
            })
        })
    }
}
