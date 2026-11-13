//! The turn's control channel: plan mode, checkpoints, and rewinds.
//!
//! A tool cannot see the conversation it is part of. The transcript belongs to
//! the turn loop, and the tool contract carries only the call's arguments —
//! there is no context handle, no runtime handle, nothing that would let a
//! tool ask what has already been said. Three capabilities need exactly that
//! view:
//!
//! * **plan mode** — the turn reads and investigates, but does not act, until
//!   the operator lets it;
//! * **checkpoint** — the agent marks a point in the transcript it may want to
//!   return to;
//! * **rewind** — the agent returns to that point.
//!
//! So the loop owns the state and the tools borrow a handle to it through a
//! task-local. That is the same shape the model-switch request already uses:
//! the loop owns the value, the tool writes it while it executes, and the loop
//! reads it back at the top of its next iteration.

use std::sync::{Arc, Mutex, MutexGuard};

/// The mode a turn is running in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentMode {
    /// The turn may act: read, write, and execute.
    #[default]
    Execute,
    /// The turn may only read. Write and execute calls are refused with a
    /// message that tells the agent to plan first.
    Plan,
}

impl AgentMode {
    /// The mode's name, as the model and the operator see it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Execute => "execute",
            Self::Plan => "plan",
        }
    }

    /// The mode a name stands for.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "execute" => Some(Self::Execute),
            "plan" => Some(Self::Plan),
            _ => None,
        }
    }
}

/// Where the transcript stood when a checkpoint was taken.
///
/// The pair is what makes a rewind exact. The provider-visible transcript and
/// the persistence delta are two buffers that grow together, and restoring only
/// one of them would let the post-turn replay put the rewound messages back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TranscriptAnchor {
    /// Messages in the provider-visible transcript.
    pub history_len: usize,
    /// Messages in the persistence delta.
    pub canonical_len: usize,
}

/// A marked point in the transcript.
#[derive(Debug, Clone)]
pub struct Checkpoint {
    /// The checkpoint's identifier.
    pub id: String,
    /// The label the agent gave it.
    pub label: String,
    /// Where the transcript stood when the checkpoint was taken.
    pub anchor: TranscriptAnchor,
}

/// A change the turn loop applies at the top of its next iteration.
#[derive(Debug, Clone)]
pub enum TurnRequest {
    /// Switch the turn's mode.
    SetMode(AgentMode),
    /// Return the transcript to a checkpoint.
    Rewind { checkpoint_id: String },
}

/// The state the loop owns and the tools borrow.
#[derive(Debug)]
pub struct TurnControl {
    mode: AgentMode,
    anchor: TranscriptAnchor,
    checkpoints: Vec<Checkpoint>,
    requests: Vec<TurnRequest>,
    next_checkpoint: u64,
}

/// The handle the loop and the tools share.
pub type TurnControlHandle = Arc<Mutex<TurnControl>>;

tokio::task_local! {
    /// The turn's control channel. The loop scopes it around the turn; a tool
    /// reads and writes through it while it executes.
    static TURN_CONTROL: TurnControlHandle;
}

/// Tools that stay available while a turn is in plan mode.
///
/// The plan-mode control tool reports the mode and enters it, the plan writer
/// is how the plan is written down, and the execution request is how the
/// agent asks to act. None of them changes the machine, so none of them is
/// part of what plan mode withholds. The execution request is still judged by
/// the approval policy like any other write — that decision is the operator's.
pub const PLAN_MODE_EXEMPT: &[&str] = &["plan_mode", "plan_execute", "TodoWrite"];

impl TurnControl {
    /// A fresh control channel in the given mode.
    #[must_use]
    pub fn new(mode: AgentMode) -> TurnControlHandle {
        Arc::new(Mutex::new(Self {
            mode,
            anchor: TranscriptAnchor::default(),
            checkpoints: Vec::new(),
            requests: Vec::new(),
            next_checkpoint: 1,
        }))
    }

    /// The mode the turn is in.
    #[must_use]
    pub fn mode(&self) -> AgentMode {
        self.mode
    }

    /// Switch the turn's mode.
    pub fn set_mode(&mut self, mode: AgentMode) {
        self.mode = mode;
    }

    /// Publish where the transcript stands.
    pub fn publish_anchor(&mut self, anchor: TranscriptAnchor) {
        self.anchor = anchor;
    }

    /// Where the transcript stands.
    #[must_use]
    pub fn anchor(&self) -> TranscriptAnchor {
        self.anchor
    }

    /// Mark the current position of the transcript.
    pub fn record_checkpoint(&mut self, label: &str) -> Checkpoint {
        let id = format!("cp{}", self.next_checkpoint);
        self.next_checkpoint += 1;
        let checkpoint = Checkpoint {
            id: id.clone(),
            label: label.to_string(),
            anchor: self.anchor,
        };
        self.checkpoints.push(checkpoint.clone());
        checkpoint
    }

    /// Every checkpoint taken in this turn, oldest first.
    #[must_use]
    pub fn checkpoints(&self) -> Vec<Checkpoint> {
        self.checkpoints.clone()
    }

    /// The most recent checkpoint, if there is one.
    #[must_use]
    pub fn latest_checkpoint(&self) -> Option<Checkpoint> {
        self.checkpoints.last().cloned()
    }

    /// One checkpoint by identifier.
    #[must_use]
    pub fn checkpoint(&self, id: &str) -> Option<Checkpoint> {
        self.checkpoints.iter().find(|entry| entry.id == id).cloned()
    }

    /// Ask the loop to return the transcript to a checkpoint.
    pub fn request_rewind(&mut self, checkpoint_id: &str) {
        self.requests.push(TurnRequest::Rewind {
            checkpoint_id: checkpoint_id.to_string(),
        });
    }

    /// Ask the loop to switch the turn's mode.
    pub fn request_mode(&mut self, mode: AgentMode) {
        self.requests.push(TurnRequest::SetMode(mode));
    }

    /// Take the requests the loop has not applied yet.
    pub fn take_requests(&mut self) -> Vec<TurnRequest> {
        std::mem::take(&mut self.requests)
    }
}

/// Lock the channel, recovering from a poisoned lock.
///
/// A panic in one tool must not take the turn's control channel with it: the
/// state behind it is plain data, and losing the channel would strand the
/// turn in whatever mode it was in.
fn lock(handle: &TurnControlHandle) -> MutexGuard<'_, TurnControl> {
    match handle.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The turn's control channel, if a turn is running.
pub(crate) fn current() -> anyhow::Result<TurnControlHandle> {
    TURN_CONTROL.try_with(Arc::clone).map_err(|_| {
        anyhow::Error::msg("this tool is only available inside an active agent turn")
    })
}

/// Run a future with the turn's control channel in scope.
pub(crate) async fn scope<F>(handle: TurnControlHandle, future: F) -> F::Output
where
    F: std::future::Future,
{
    TURN_CONTROL.scope(handle, future).await
}

/// The mode of the running turn, or `execute` outside a turn.
#[must_use]
pub fn current_mode() -> AgentMode {
    TURN_CONTROL
        .try_with(|handle| lock(&handle).mode())
        .unwrap_or(AgentMode::Execute)
}

/// Whether a call may run in the given mode.
///
/// In plan mode only the read-tier tools run, plus the tools that let the
/// agent write its plan down and ask to act. The tier is the same one the
/// approval policy uses, so "read" means the same thing in both places.
#[must_use]
pub fn allowed_in_mode(mode: AgentMode, tool_name: &str, action: Option<&str>) -> bool {
    if mode == AgentMode::Execute {
        return true;
    }
    if PLAN_MODE_EXEMPT.iter().any(|name| *name == tool_name) {
        return true;
    }
    crate::approval::tiers::tier_for_call(tool_name, action) == crate::approval::tiers::ApprovalTier::Read
}

/// The message the model sees when plan mode refuses a call.
#[must_use]
pub fn plan_mode_refusal(tool_name: &str) -> String {
    format!(
        "Plan mode is active: '{tool_name}' is not available, because it changes the machine \
         and plan mode may only read. Investigate and write the plan down with TodoWrite, then \
         ask to act with plan_mode(action=\"execute\"); the operator decides."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mode_has_a_name_and_a_parse() {
        assert_eq!(AgentMode::default(), AgentMode::Execute);
        assert_eq!(AgentMode::Plan.as_str(), "plan");
        assert_eq!(AgentMode::parse("PLAN"), Some(AgentMode::Plan));
        assert_eq!(AgentMode::parse("execute"), Some(AgentMode::Execute));
        assert_eq!(AgentMode::parse("nope"), None);
    }

    #[test]
    fn a_checkpoint_records_the_published_anchor() {
        let handle = TurnControl::new(AgentMode::Execute);
        {
            let mut control = lock(&handle);
            control.publish_anchor(TranscriptAnchor {
                history_len: 7,
                canonical_len: 3,
            });
        }
        let checkpoint = {
            let mut control = lock(&handle);
            control.record_checkpoint("before the refactor")
        };
        assert_eq!(checkpoint.anchor.history_len, 7);
        assert_eq!(checkpoint.anchor.canonical_len, 3);
        assert_eq!(checkpoint.label, "before the refactor");
        assert_eq!(checkpoint.id, "cp1");
    }

    #[test]
    fn checkpoints_get_distinct_identifiers() {
        let handle = TurnControl::new(AgentMode::Execute);
        let mut control = lock(&handle);
        let first = control.record_checkpoint("one");
        let second = control.record_checkpoint("two");
        assert_ne!(first.id, second.id);
        assert_eq!(control.checkpoints().len(), 2);
        assert_eq!(control.latest_checkpoint().unwrap().id, second.id);
        assert_eq!(control.checkpoint(&first.id).unwrap().label, "one");
        assert!(control.checkpoint("nope").is_none());
    }

    #[test]
    fn requests_are_taken_once() {
        let handle = TurnControl::new(AgentMode::Execute);
        {
            let mut control = lock(&handle);
            control.request_rewind("cp1");
            control.request_mode(AgentMode::Plan);
        }
        let requests = lock(&handle).take_requests();
        assert_eq!(requests.len(), 2);
        assert!(matches!(requests[0], TurnRequest::Rewind { .. }));
        assert!(matches!(requests[1], TurnRequest::SetMode(AgentMode::Plan)));
        assert!(lock(&handle).take_requests().is_empty());
    }

    #[test]
    fn plan_mode_allows_reads_and_the_plan_tools() {
        assert!(allowed_in_mode(AgentMode::Plan, "file_read", None));
        assert!(allowed_in_mode(AgentMode::Plan, "TodoWrite", None));
        assert!(allowed_in_mode(AgentMode::Plan, "plan_mode", Some("plan")));
        assert!(allowed_in_mode(
            AgentMode::Plan,
            "plan_execute",
            None
        ));
        assert!(!allowed_in_mode(AgentMode::Plan, "file_write", None));
        assert!(!allowed_in_mode(AgentMode::Plan, "shell", None));
    }

    #[test]
    fn execute_mode_allows_everything() {
        assert!(allowed_in_mode(AgentMode::Execute, "file_write", None));
        assert!(allowed_in_mode(AgentMode::Execute, "shell", None));
    }

    #[tokio::test]
    async fn the_channel_is_reachable_inside_a_scope() {
        let handle = TurnControl::new(AgentMode::Plan);
        let seen = scope(Arc::clone(&handle), async {
            current_mode()
        })
        .await;
        assert_eq!(seen, AgentMode::Plan);
    }

    #[tokio::test]
    async fn outside_a_turn_the_mode_is_execute_and_the_channel_is_absent() {
        assert_eq!(current_mode(), AgentMode::Execute);
        assert!(current().is_err());
    }
}
