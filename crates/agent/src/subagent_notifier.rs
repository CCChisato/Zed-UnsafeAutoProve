use agent_client_protocol::schema as acp;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use util::paths::PathStyle;

/// Maximum number of notifications to inject in a single poll cycle
const MAX_NOTIFICATIONS_PER_CYCLE: usize = 3;

/// Returns the path to the notifications directory.
fn notifications_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".claw")
        .join("sessions")
        .join("notifications")
}

/// A notification from a completed claw subagent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentNotification {
    pub session_id: String,
    pub status: String,
    pub duration_ms: Option<u64>,
    pub completed_at_ms: Option<u128>,
    pub report: Option<serde_json::Value>,
    pub error: Option<String>,
}

impl SubagentNotification {
    /// Format the notification into a human-readable text block suitable
    /// for injecting into the agent's message stream.
    pub fn format_as_agent_message(&self) -> String {
        let mut msg = String::new();

        msg.push_str("## Subagent Task Completed\n\n");
        if let Some(ref report) = self.report {
            if let Some(task) = report.get("task").and_then(|v| v.as_str()) {
                msg.push_str(&format!("**Task**: {}\n\n", task));
            }
            if let Some(summary) = report.get("summary").and_then(|v| v.as_str()) {
                msg.push_str(&format!("**Summary**: {}\n\n", summary));
            }
            if let Some(findings) = report.get("key_findings").and_then(|v| v.as_str()) {
                msg.push_str(&format!("**Key Findings**: {}\n\n", findings));
            }
            if let Some(raw) = report.get("raw_output").and_then(|v| v.as_str()) {
                if !raw.is_empty() {
                    msg.push_str(&format!("**Raw Output**:\n```\n{}\n```\n\n", raw));
                }
            }
        } else {
            msg.push_str(&format!(
                "**Session**: `{}`\n**Status**: `{}`\n",
                self.session_id, self.status
            ));
        }

        if let Some(dur) = self.duration_ms {
            msg.push_str(&format!("**Duration**: {}s\n", dur as f64 / 1000.0));
        }

        if let Some(ref error) = self.error {
            if !error.is_empty() {
                msg.push_str(&format!("**Error**: {}\n", error));
            }
        }

        msg.push_str("\n*(Use `check_subagent_status` with the session_id for full details)*");
        msg
    }
}

/// Check the notification directory for completed subagents and inject
/// them into the thread's message list.
///
/// This is designed to be called from within the gpui thread context
/// (e.g. inside a `this.update(cx, ...)` call).
pub fn check_and_inject_subagent_notifications(
    thread: &mut crate::Thread,
    cx: &mut gpui::Context<crate::Thread>,
) {
    let notif_dir = notifications_dir();
    if !notif_dir.exists() {
        return;
    }

    let entries = match std::fs::read_dir(&notif_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    // Collect notification files sorted by name (nanosecond timestamps = chronological).
    // We accept both .notification.json (from claw subagents) and .native.json (from
    // Zed-native non-blocking subagents).
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) if n.ends_with(".notification.json") || n.ends_with(".native.json") => {
                n.to_string()
            }
            _ => continue,
        };
        files.push((file_name, path));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));

    if files.is_empty() {
        return;
    }

    let batch: Vec<_> = files
        .into_iter()
        .take(MAX_NOTIFICATIONS_PER_CYCLE)
        .collect();

    let mut had_new = false;
    for (_file_name, path) in &batch {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Try to parse as a native subagent notification first (has "output" raw field),
        // then fall back to SubagentNotification (claw format with "report" field).
        let formatted = if let Ok(native) = serde_json::from_str::<serde_json::Value>(&content) {
            // Use a different formatting for native vs claw notifications
            let source = native
                .get("source")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown");
            let status = native
                .get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown");
            let session_id = native
                .get("session_id")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown");
            let duration = native
                .get("duration_ms")
                .and_then(|s| s.as_u64())
                .unwrap_or(0);

            let mut msg = format!(
                "## {} Subagent Completed\n\n**Session**: `{}`\n**Status**: `{}`\n**Duration**: {}s\n\n",
                source,
                session_id,
                status,
                duration as f64 / 1000.0
            );

            if let Some(output) = native.get("output").and_then(|s| s.as_str()) {
                if !output.is_empty() {
                    msg.push_str(&format!("**Output**:\n```\n{}\n```\n\n", output));
                }
            }

            if let Some(error) = native.get("error").and_then(|s| s.as_str()) {
                if !error.is_empty() {
                    msg.push_str(&format!("**Error**: {}\n\n", error));
                }
            }

            msg.push_str("*(Use `check_subagent_status` with the session_id for full details)*");
            msg
        } else if let Ok(notification) = serde_json::from_str::<SubagentNotification>(&content) {
            notification.format_as_agent_message()
        } else {
            continue;
        };

        // Inject as a user message via the public API.
        // The system/LLM will see this on the next completion cycle.
        use acp_thread::UserMessageId;
        let msg_id = UserMessageId::new();
        let content_block = acp::ContentBlock::Text(acp::TextContent::new(formatted));
        thread.push_acp_user_block(msg_id, [content_block], PathStyle::Posix, cx);

        // Delete notification file after processing
        let _ = std::fs::remove_file(path);
        had_new = true;
    }

    if had_new {
        cx.notify();
    }
}
