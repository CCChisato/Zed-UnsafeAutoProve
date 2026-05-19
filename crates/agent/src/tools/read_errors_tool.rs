use crate::error_collector::ErrorCollector;
use crate::{AgentTool, ToolCallEventStream, ToolInput};
use agent_client_protocol::schema as acp;
use anyhow::Result;
use db::kvp::KeyValueStore;
use gpui::{App, AsyncApp, Entity, Task};
use project::Project;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use ui::SharedString;
use util::markdown::MarkdownInlineCode;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
/// Read errors previously recorded in Zed's error collector database.
///
/// This tool is useful for quickly surfacing recent diagnostics and runtime errors
/// that were captured by language servers or tools, without re-running the failing action.
///
/// Optional filters:
/// - `path`: substring match against the recorded file path
/// - `source`: substring match against the error source (e.g. `rust_analyzer`, `tool_error`)
/// - `limit`: maximum number of records to return (default: 20)
pub struct ReadErrorsToolInput {
    /// Only include errors whose recorded file path contains this substring.
    pub path: Option<String>,
    /// Only include errors whose recorded source contains this substring.
    pub source: Option<String>,
    /// Maximum number of matching errors to return. Defaults to 20.
    pub limit: Option<usize>,
}

/// Tool implementation for `read_errors`.
pub struct ReadErrorsTool;

impl ReadErrorsTool {
    pub fn new(_project: Entity<Project>) -> Self {
        Self
    }
}

impl AgentTool for ReadErrorsTool {
    type Input = ReadErrorsToolInput;
    type Output = String;

    const NAME: &'static str = "read_errors";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Read
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        if let Ok(input) = input {
            if let Some(path) = &input.path {
                return format!("Check recorded errors for {}", MarkdownInlineCode(path)).into();
            }
        }
        "Check recorded errors".into()
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
                .map_err(|e| format!("Failed to receive tool input: {e}"))?;

            let result = query_errors(&input, cx).await;
            Ok(result)
        })
    }
}

async fn query_errors(input: &ReadErrorsToolInput, cx: &mut AsyncApp) -> String {
    let collector = cx.update(|cx| {
        let db = KeyValueStore::global(cx);
        let conn = db.connection().clone();
        ErrorCollector::new(&conn)
    });

    let mut records = collector.query().await;

    if let Some(ref path) = input.path {
        records.retain(|r| {
            r.file
                .as_deref()
                .map_or(false, |f| f.contains(path.as_str()))
        });
    }
    if let Some(ref source) = input.source {
        records.retain(|r| r.source.contains(source.as_str()));
    }

    let limit = input.limit.unwrap_or(20);
    records.truncate(limit);

    if records.is_empty() {
        return "No recorded errors found.".to_string();
    }

    let mut output = String::new();
    output.push_str(&format!("Found {} recorded error(s):\n\n", records.len()));

    for (i, record) in records.iter().enumerate() {
        output.push_str(&format!(
            "{}. **{}** (occurred {} time(s))",
            i + 1,
            record.message,
            record.count
        ));
        if let Some(ref file) = record.file {
            output.push_str(&format!(" in `{}`", file));
            if let Some(line) = record.line {
                output.push_str(&format!(":{}", line));
            }
        }
        output.push_str(&format!(
            "\n   Source: {} | First seen: {} | Last seen: {}\n\n",
            record.source,
            format_timestamp(record.first_seen),
            format_timestamp(record.last_seen),
        ));
    }

    output
}

fn format_timestamp(ts: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let diff = now.saturating_sub(ts);

    if diff < 60 {
        format!("{}s ago", diff)
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error_collector::ErrorCollector;
    use db::sqlez::thread_safe_connection::ThreadSafeConnection;

    async fn setup_collector(name: &str) -> ErrorCollector {
        let conn = ThreadSafeConnection::builder::<db::AppMigrator>(name, false)
            .build()
            .await
            .unwrap();
        ErrorCollector::new(&conn)
    }

    async fn seed_errors(collector: &ErrorCollector) {
        collector
            .record(
                "undefined variable `x`",
                "rust_analyzer",
                Some("src/main.rs"),
                Some(42),
                Some(10),
            )
            .await
            .unwrap();
        collector
            .record(
                "missing semicolon",
                "rust_analyzer",
                Some("src/main.rs"),
                Some(15),
                None,
            )
            .await
            .unwrap();
        collector
            .record(
                "unused import `std::collections`",
                "rust_analyzer",
                Some("src/lib.rs"),
                Some(3),
                None,
            )
            .await
            .unwrap();
        collector
            .record("file not found", "tool_error", None::<&str>, None, None)
            .await
            .unwrap();
    }

    async fn run_query(input: &ReadErrorsToolInput, cx: &mut gpui::TestAppContext) -> String {
        let mut async_cx = cx.to_async();
        query_errors(input, &mut async_cx).await
    }

    #[gpui::test]
    async fn test_query_no_filters(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let collector = setup_collector("ret_no_filter").await;
        seed_errors(&collector).await;
        let conn = collector.connection().clone();
        cx.update(|cx| cx.set_global(db::AppDatabase(conn)));

        let input = ReadErrorsToolInput {
            path: None,
            source: None,
            limit: None,
        };
        let result = run_query(&input, cx).await;
        assert!(result.contains("4 recorded error(s)"));
        assert!(result.contains("undefined variable"));
        assert!(result.contains("missing semicolon"));
        assert!(result.contains("file not found"));
    }

    #[gpui::test]
    async fn test_query_filter_by_path(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let collector = setup_collector("ret_path_filter").await;
        seed_errors(&collector).await;
        let conn = collector.connection().clone();
        cx.update(|cx| cx.set_global(db::AppDatabase(conn)));

        let input = ReadErrorsToolInput {
            path: Some("src/main.rs".into()),
            source: None,
            limit: None,
        };
        let result = run_query(&input, cx).await;
        assert!(result.contains("2 recorded error(s)"));
    }

    #[gpui::test]
    async fn test_query_filter_by_source(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let collector = setup_collector("ret_source_filter").await;
        seed_errors(&collector).await;
        let conn = collector.connection().clone();
        cx.update(|cx| cx.set_global(db::AppDatabase(conn)));

        let input = ReadErrorsToolInput {
            path: None,
            source: Some("tool_error".into()),
            limit: None,
        };
        let result = run_query(&input, cx).await;
        assert!(result.contains("1 recorded error(s)"));
        assert!(result.contains("file not found"));
    }

    #[gpui::test]
    async fn test_query_limit(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let collector = setup_collector("ret_limit").await;
        seed_errors(&collector).await;
        let conn = collector.connection().clone();
        cx.update(|cx| cx.set_global(db::AppDatabase(conn)));

        let input = ReadErrorsToolInput {
            path: None,
            source: None,
            limit: Some(2),
        };
        let result = run_query(&input, cx).await;
        assert!(result.contains("2 recorded error(s)"));
    }

    #[gpui::test]
    async fn test_query_empty(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let _collector = setup_collector("ret_empty").await;

        let input = ReadErrorsToolInput {
            path: None,
            source: None,
            limit: None,
        };
        let result = run_query(&input, cx).await;
        assert_eq!(result, "No recorded errors found.");
    }
}
