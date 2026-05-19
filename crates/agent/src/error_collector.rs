use db::kvp::KeyValueStore;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

const ERROR_NAMESPACE: &str = "ide_errors";

/// A single error observation, deduplicated by its content fingerprint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorRecord {
    pub message: String,
    /// The source/category of the error (e.g. `tool_error`, `rust_analyzer`).
    pub source: String,
    /// Optional file path associated with the error.
    pub file: Option<String>,
    /// Optional line number.
    pub line: Option<u32>,
    /// Optional column number.
    pub column: Option<u32>,
    pub first_seen: u64,
    pub last_seen: u64,
    pub count: u64,
    /// The tool that was executing when the error occurred (if applicable).
    pub tool: Option<String>,
    /// The raw input arguments passed to the tool.
    pub tool_input: Option<serde_json::Value>,
    /// The raw output returned by the tool (including error details).
    pub tool_output: Option<serde_json::Value>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn message_key(message: &str, source: &str, file: Option<&str>, line: Option<u32>) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    file.hash(&mut hasher);
    line.hash(&mut hasher);
    message.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Persistent error store backed by the app-wide SQLite database.
///
/// All async operations use the `KeyValueStore`'s internal write queue,
/// making them safe to await from any async context.
#[derive(Clone)]
pub struct ErrorCollector {
    store: KeyValueStore,
}

impl ErrorCollector {
    /// Create a collector backed by the given connection.
    pub fn new(connection: &db::sqlez::thread_safe_connection::ThreadSafeConnection) -> Self {
        Self {
            store: KeyValueStore::from_connection(connection),
        }
    }

    /// Record an error. Automatically deduplicates by fingerprint
    /// (source + file + line + message hash) — increments count on match.
    pub async fn record(
        &self,
        message: impl Into<String>,
        source: impl Into<String>,
        file: Option<impl Into<String>>,
        line: Option<u32>,
        column: Option<u32>,
    ) -> anyhow::Result<()> {
        let message: String = message.into();
        let source: String = source.into();
        let file: Option<String> = file.map(|f| f.into());
        let key = message_key(&message, &source, file.as_deref(), line);
        let now = now_secs();

        let new_record = ErrorRecord {
            message: message.clone(),
            source: source.clone(),
            file: file.clone(),
            line,
            column,
            first_seen: now,
            last_seen: now,
            count: 1,
            tool: None,
            tool_input: None,
            tool_output: None,
        };

        let scope = self.store.scoped(ERROR_NAMESPACE);

        let merged = match scope.read(&key).ok().and_then(|v| v) {
            Some(json) => match serde_json::from_str::<ErrorRecord>(&json) {
                Ok(mut r) => {
                    r.count += 1;
                    r.last_seen = now;
                    r
                }
                Err(_) => new_record,
            },
            None => new_record,
        };

        let json = serde_json::to_string(&merged)?;
        scope.write(key, json).await?;
        Ok(())
    }

    /// Record a tool execution error with full context
    /// (message, tool name, tool input, tool output).
    /// This is the primary ingestion point for tool-level errors.
    pub async fn record_tool_error(
        &self,
        message: impl Into<String>,
        tool_name: impl Into<String>,
        tool_input: Option<serde_json::Value>,
        tool_output: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        let message: String = message.into();
        let tool: String = tool_name.into();
        let now = now_secs();

        // Fingerprint on tool_name + first 200 chars of message
        let key = format!("tool:{:016x}", {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            tool.hash(&mut hasher);
            message[..message.len().min(200)].hash(&mut hasher);
            hasher.finish()
        });

        let mut new_record = ErrorRecord {
            message: message.clone(),
            source: "tool_error".to_string(),
            file: None,
            line: None,
            column: None,
            first_seen: now,
            last_seen: now,
            count: 1,
            tool: Some(tool.clone()),
            tool_input,
            tool_output,
        };

        let scope = self.store.scoped(ERROR_NAMESPACE);

        let merged = match scope.read(&key).ok().and_then(|v| v) {
            Some(json) => match serde_json::from_str::<ErrorRecord>(&json) {
                Ok(mut r) => {
                    r.count += 1;
                    r.last_seen = now;
                    // Update input/output with latest occurrence
                    if new_record.tool_input.is_some() {
                        r.tool_input = new_record.tool_input.take();
                    }
                    if new_record.tool_output.is_some() {
                        r.tool_output = new_record.tool_output.take();
                    }
                    r
                }
                Err(_) => new_record,
            },
            None => new_record,
        };

        let json = serde_json::to_string(&merged)?;
        scope.write(key, json).await?;
        Ok(())
    }

    /// Query all records, sorted by last_seen descending.
    pub async fn query(&self) -> Vec<ErrorRecord> {
        let conn = self.store.connection();
        let result: Vec<String> = conn
            .write(|connection| {
                let mut binder = match connection.select_bound::<&str, String>(
                    "SELECT value FROM scoped_kv_store WHERE namespace = ? ORDER BY rowid DESC",
                ) {
                    Ok(b) => b,
                    Err(_) => return vec![],
                };
                binder(ERROR_NAMESPACE).unwrap_or_default()
            })
            .await;
        result
            .into_iter()
            .filter_map(|json| serde_json::from_str::<ErrorRecord>(&json).ok())
            .collect()
    }

    /// Borrow the inner connection.
    pub fn connection(&self) -> &db::sqlez::thread_safe_connection::ThreadSafeConnection {
        self.store.connection()
    }

    /// Delete all error records.
    pub async fn clear(&self) -> anyhow::Result<()> {
        let scope = self.store.scoped(ERROR_NAMESPACE);
        scope.delete_all().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use db::sqlez::thread_safe_connection::ThreadSafeConnection;

    async fn test_collector(name: &str) -> ErrorCollector {
        let conn = ThreadSafeConnection::builder::<db::AppMigrator>(name, false)
            .build()
            .await
            .unwrap();
        ErrorCollector::new(&conn)
    }

    #[gpui::test]
    async fn test_record_and_query(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let collector = test_collector("ec_test_record").await;
        collector
            .record(
                "undefined variable x",
                "rust_analyzer",
                Some("src/main.rs"),
                Some(42),
                Some(10),
            )
            .await
            .unwrap();

        let errors = collector.query().await;
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].message, "undefined variable x");
        assert_eq!(errors[0].count, 1);
    }

    #[gpui::test]
    async fn test_dedup_same_error(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let collector = test_collector("ec_test_dedup").await;
        collector
            .record("x", "src", Some("a.rs"), Some(1), None)
            .await
            .unwrap();
        collector
            .record("x", "src", Some("a.rs"), Some(1), None)
            .await
            .unwrap();

        let errors = collector.query().await;
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].count, 2);
    }

    #[gpui::test]
    async fn test_different_errors(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let collector = test_collector("ec_test_diff").await;
        collector
            .record("a", "t", Some("a.rs"), Some(1), None)
            .await
            .unwrap();
        collector
            .record("b", "t", Some("b.rs"), Some(2), None)
            .await
            .unwrap();

        assert_eq!(collector.query().await.len(), 2);
    }

    #[gpui::test]
    async fn test_clear(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        let collector = test_collector("ec_test_clear").await;
        collector
            .record("t", "t", None::<&str>, None, None)
            .await
            .unwrap();
        collector.clear().await.unwrap();
        assert!(collector.query().await.is_empty());
    }
}
