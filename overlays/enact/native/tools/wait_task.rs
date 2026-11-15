//! `wait` — block until background work finishes.
//!
//! An agent that starts a background task and then has nothing to do with it
//! either spins (polling a result tool) or gives up. `wait` is the blocking
//! form: name the task, get the outcome when it lands, with a bounded budget
//! so a stuck task cannot hang the turn forever.
//!
//! The task itself lives in the runtime's durable task registry, which the
//! delegate and subagent producers already write to. This tool only reads it,
//! so it adds no new lifecycle state.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};

use crate::control_plane::{TaskKind, TaskRecord, TaskRegistry, TaskSnapshot, TaskStatus};

/// The longest a single `wait` call blocks, in milliseconds.
///
/// A turn that blocks for minutes is a turn the operator cannot interrupt.
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// The ceiling on a caller-supplied timeout, in milliseconds.
pub const MAX_TIMEOUT_MS: u64 = 600_000;

/// How long the poll sleeps between checks, in milliseconds.
const POLL_INTERVAL_MS: u64 = 100;

/// Wait for a background task to finish.
pub struct WaitTool {
    /// The durable store, when the caller already holds one.
    registry: Option<Arc<dyn TaskRegistry>>,
    /// Where the durable store lives, when it has to be opened on demand.
    data_dir: Option<PathBuf>,
    /// The agent asking. A task is only visible to the chain that started it.
    caller_alias: Option<String>,
}

impl WaitTool {
    /// Build the tool over a task registry.
    pub fn new(registry: Arc<dyn TaskRegistry>, caller_alias: Option<String>) -> Self {
        Self {
            registry: Some(registry),
            data_dir: None,
            caller_alias,
        }
    }

    /// Build the tool over a data directory, opening the durable store when a
    /// call needs it.
    pub fn from_data_dir(data_dir: PathBuf, caller_alias: Option<String>) -> Self {
        Self {
            registry: None,
            data_dir: Some(data_dir),
            caller_alias,
        }
    }

    /// The durable store the background producers write to.
    ///
    /// The daemon's store is preferred; a process that is not the daemon
    /// opens its own view of the same store, exactly as the delegate tool
    /// does when it starts background work.
    async fn registry(&self) -> anyhow::Result<Arc<dyn TaskRegistry>> {
        if let Some(registry) = &self.registry {
            return Ok(Arc::clone(registry));
        }
        let Some(data_dir) = &self.data_dir else {
            anyhow::bail!(
                "waiting for a task needs a durable task store; no data directory is configured"
            );
        };
        if let Some(handle) = crate::control_plane::control_plane() {
            return Ok(Arc::clone(&handle.store));
        }
        let handle = crate::control_plane::non_daemon_control_plane(data_dir).await?;
        Ok(handle.store)
    }

    /// Whether the caller may see this task.
    ///
    /// A task is visible to the chain that started it: the originating agent,
    /// or any ancestor in the delegation chain. A task with no recorded
    /// originator is only visible once it is terminal, which is what lets a
    /// fresh agent collect a result it was handed but did not start.
    fn is_visible(&self, task: &TaskRecord) -> bool {
        if task.kind != TaskKind::Delegate && task.kind != TaskKind::Subagent {
            return false;
        }
        let Some(caller) = &self.caller_alias else {
            // A caller with no identity cannot claim a task.
            return task.status.is_terminal()
                && task.originator_route.is_none()
                && task.originator_chain.is_empty();
        };
        if task.originator_route.as_deref() == Some(caller.as_str())
            || task.originator_chain.iter().any(|alias| alias == caller)
        {
            return true;
        }
        task.originator_route.is_none()
            && task.originator_chain.is_empty()
            && task.status.is_terminal()
    }

    /// Render the outcome of a finished task.
    fn render(&self, snapshot: &TaskSnapshot) -> String {
        let task = &snapshot.task;
        let mut out = format!(
            "Task {} — {}\n",
            task.id,
            status_label(task.status)
        );
        if let Some(progress) = &snapshot.progress {
            out.push_str(&format!(
                "  {} iteration(s), {} tool call(s) completed\n",
                progress.iterations, progress.tools_completed
            ));
            if let Some(last) = &progress.last_tool {
                out.push_str(&format!("  last tool: {}\n", last.name));
            }
        }
        match (&snapshot.output, &snapshot.error) {
            (Some(output), _) if !output.is_empty() => {
                out.push_str("\nOutput:\n");
                out.push_str(output);
                if !output.ends_with('\n') {
                    out.push('\n');
                }
            }
            (None, Some(error)) => {
                out.push_str("\nError:\n");
                out.push_str(error);
                if !error.ends_with('\n') {
                    out.push('\n');
                }
            }
            _ => {}
        }
        out
    }
}

/// The label for a task status.
fn status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Running => "still running",
        TaskStatus::Paused => "paused",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Lost => "lost",
        TaskStatus::TimedOut => "timed out",
    }
}

#[async_trait]
impl Tool for WaitTool {
    fn name(&self) -> &str {
        "wait"
    }

    fn description(&self) -> &str {
        "Block until a background task finishes, then return its output. Use it after starting work you want the answer to — a background delegate, a spawned subagent — instead of polling a result tool in a loop. Give the task id and an optional `timeout_ms`; the call returns as soon as the task reaches a terminal state."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The id of the task to wait for, as returned when the task was started."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": format!("How long to wait, in milliseconds (default {DEFAULT_TIMEOUT_MS}, max {MAX_TIMEOUT_MS}).")
                }
            },
            "required": ["task_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let task_id = args
            .get("task_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow::Error::msg("`wait` requires a `task_id`"))?;
        let timeout_ms = args
            .get("timeout_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .clamp(1, MAX_TIMEOUT_MS);

        let registry = self.registry().await?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        loop {
            let snapshot = registry
                .get_snapshot(task_id)
                .await
                .map_err(|error| anyhow::anyhow!("could not read the task registry: {error}"))?;
            let Some(snapshot) = snapshot else {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("No task named '{task_id}' exists.")),
                });
            };
            if !self.is_visible(&snapshot.task) {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "Task '{task_id}' is not visible to this agent."
                    )),
                });
            }
            if snapshot.task.status.is_terminal() {
                let text = self.render(&snapshot);
                let success = snapshot.task.status == TaskStatus::Completed;
                return Ok(ToolResult {
                    success,
                    output: ToolOutput::text(text),
                    error: if success {
                        None
                    } else {
                        Some(format!(
                            "task {task_id} ended as {}",
                            status_label(snapshot.task.status)
                        ))
                    },
                });
            }
            if tokio::time::Instant::now() >= deadline {
                let mut out = format!(
                    "Task {task_id} is still {} after {timeout_ms} ms.\n",
                    status_label(snapshot.task.status)
                );
                if let Some(progress) = &snapshot.progress {
                    out.push_str(&format!(
                        "  {} iteration(s), {} tool call(s) completed\n",
                        progress.iterations, progress.tools_completed
                    ));
                }
                out.push_str("Call `wait` again to keep waiting, or check the task's result.");
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::text(out),
                    error: Some(format!("timed out waiting for task {task_id}")),
                });
            }
            tokio::time::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// A registry that hands back whatever the test put in it.
    struct FakeRegistry {
        tasks: Mutex<HashMap<String, TaskSnapshot>>,
    }

    impl FakeRegistry {
        fn new() -> Self {
            Self {
                tasks: Mutex::new(HashMap::new()),
            }
        }

        fn put(&self, snapshot: TaskSnapshot) {
            self.tasks.lock().unwrap().insert(snapshot.task.id.clone(), snapshot);
        }
    }

    #[async_trait]
    impl TaskRegistry for FakeRegistry {
        async fn create(&self, _rec: TaskRecord) -> anyhow::Result<()> {
            Ok(())
        }
        async fn heartbeat(&self, _id: &str, _owner_boot_id: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn update_status(
            &self,
            _id: &str,
            _status: TaskStatus,
            _output: Option<String>,
            _error: Option<String>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn claim_owner(
            &self,
            _id: &str,
            _owner_pid: u32,
            _owner_boot_id: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn get(&self, id: &str) -> anyhow::Result<Option<TaskRecord>> {
            Ok(self
                .tasks
                .lock()
                .unwrap()
                .get(id)
                .map(|snapshot| snapshot.task.clone()))
        }
        async fn get_snapshot(&self, id: &str) -> anyhow::Result<Option<TaskSnapshot>> {
            Ok(self.tasks.lock().unwrap().get(id).cloned())
        }
        async fn list_running(&self) -> anyhow::Result<Vec<TaskRecord>> {
            Ok(Vec::new())
        }
        async fn list_by_agent(&self, _agent: &str) -> anyhow::Result<Vec<TaskRecord>> {
            Ok(Vec::new())
        }
        async fn reconcile_lost(&self, _id: &str, _now_boot_id: &str) -> anyhow::Result<bool> {
            Ok(false)
        }
        async fn reconcile_timed_out(
            &self,
            _id: &str,
            _owner_pid: u32,
            _owner_boot_id: &str,
            _heartbeat_at: &str,
        ) -> anyhow::Result<bool> {
            Ok(false)
        }
    }

    fn record(id: &str, status: TaskStatus, caller: Option<&str>) -> TaskRecord {
        TaskRecord {
            id: id.to_string(),
            kind: TaskKind::Delegate,
            agent: "worker".to_string(),
            status,
            owner_pid: 1,
            owner_boot_id: "boot".to_string(),
            heartbeat_at: None,
            depth: 0,
            parent_id: None,
            originator_route: caller.map(str::to_string),
            originator_chain: caller.map(|alias| vec![alias.to_string()]).unwrap_or_default(),
            delivered: false,
            idem_key: None,
            principal_id: None,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            finished_at: None,
        }
    }

    fn snapshot(record: TaskRecord, output: Option<String>, error: Option<String>) -> TaskSnapshot {
        TaskSnapshot {
            task: record,
            output,
            error,
            progress: None,
        }
    }

    #[tokio::test]
    async fn a_finished_task_returns_its_output() {
        let registry = Arc::new(FakeRegistry::new());
        registry.put(snapshot(
            record("t1", TaskStatus::Completed, Some("main")),
            Some("the answer".to_string()),
            None,
        ));
        let tool = WaitTool::new(registry, Some("main".to_string()));
        let result = tool.execute(json!({"task_id": "t1"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.as_str().contains("the answer"));
        assert!(result.output.as_str().contains("completed"));
    }

    #[tokio::test]
    async fn an_unknown_task_is_reported_as_such() {
        let registry = Arc::new(FakeRegistry::new());
        let tool = WaitTool::new(registry, Some("main".to_string()));
        let result = tool.execute(json!({"task_id": "nope"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("No task named"));
    }

    #[tokio::test]
    async fn a_task_another_agent_started_is_not_visible() {
        let registry = Arc::new(FakeRegistry::new());
        registry.put(snapshot(
            record("t1", TaskStatus::Completed, Some("other")),
            Some("secret".to_string()),
            None,
        ));
        let tool = WaitTool::new(registry, Some("main".to_string()));
        let result = tool.execute(json!({"task_id": "t1"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("not visible"));
    }

    #[tokio::test]
    async fn a_running_task_times_out_rather_than_hanging() {
        let registry = Arc::new(FakeRegistry::new());
        registry.put(snapshot(
            record("t1", TaskStatus::Running, Some("main")),
            None,
            None,
        ));
        let tool = WaitTool::new(registry, Some("main".to_string()));
        let result = tool
            .execute(json!({"task_id": "t1", "timeout_ms": 1}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("timed out"));
    }

    #[tokio::test]
    async fn a_failed_task_reports_the_error() {
        let registry = Arc::new(FakeRegistry::new());
        registry.put(snapshot(
            record("t1", TaskStatus::Failed, Some("main")),
            None,
            Some("it broke".to_string()),
        ));
        let tool = WaitTool::new(registry, Some("main".to_string()));
        let result = tool.execute(json!({"task_id": "t1"})).await.unwrap();
        assert!(!result.success);
        assert!(result.output.as_str().contains("it broke"));
    }

    #[tokio::test]
    async fn a_missing_task_id_is_refused() {
        let registry = Arc::new(FakeRegistry::new());
        let tool = WaitTool::new(registry, Some("main".to_string()));
        assert!(tool.execute(json!({})).await.is_err());
    }

    #[tokio::test]
    async fn a_tool_without_a_store_says_so() {
        let tool = WaitTool {
            registry: None,
            data_dir: None,
            caller_alias: Some("main".to_string()),
        };
        let error = tool.execute(json!({"task_id": "t1"})).await.unwrap_err();
        assert!(error.to_string().contains("durable task store"));
    }
}
