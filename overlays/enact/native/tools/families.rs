//! The consolidated tool families.
//!
//! Thirty-six sibling tools used together all the time — seven scheduling
//! tools, six SOP tools, five memory tools — were thirty-six entries in the
//! model's tool list and thirty-six chances to pick the wrong one. Each
//! family below collapses into one canonical tool with a typed `action`
//! enum: `cron` with `cron_add`, `schedule_add`, `once`, `pause` and the
//! rest of the schedule tool's nine operations.
//!
//! The consolidation is at the *operation* level, not the tool level. A
//! member that takes an `action` of its own contributes every one of its
//! operations to the family enum, and the family hands the right one back
//! down when it dispatches. A member whose contract is a single behaviour
//! contributes one operation and is called without an `action`.
//!
//! The action names are the same ones the canonical registry publishes, so a
//! model that learned `cron`/`schedule_add` from one host is not wrong at
//! another. An operation name that is unique inside its family keeps its
//! name; a name shared by several members is prefixed with a discriminator
//! derived from the member it came from (`cron_add` beside `schedule_add`).
//!
//! The legacy names keep working. A call to `cron_add` still schedules a
//! job; it also carries a one-time notice naming the canonical tool and the
//! action that replaces it, so the model learns the new name instead of
//! guessing it next time. The notice travels beside the result, not inside
//! it, so the result body stays exactly what the tool produced.
//!
//! The legacy names are hidden from the *host* surface — the MCP server
//! publishes the canonical families only — but they stay in the product's own
//! tool list. The runtime keys several behaviours on those names (the
//! delivery defaults it injects into a `cron_add` call, the approved
//! arguments it sets, the tools it withholds from a step), and an agent that
//! was told to call `cron_add` must still find it.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use serde_json::{json, Value};

use zeroclaw_api::attribution::{Attributable, Role, ToolKind};
use zeroclaw_api::tool::{Tool, ToolResult};

/// One consolidated family.
pub struct Family {
    /// The canonical tool name.
    pub name: &'static str,
    /// What the family does.
    pub description: &'static str,
    /// The legacy tools it replaces, in the order their actions appear.
    pub members: &'static [&'static str],
}

/// A member tool and the operations it exposes.
pub struct Member {
    /// The legacy tool name.
    pub tool: &'static str,
    /// The operations the tool exposes, in the order the surface lists them.
    pub ops: &'static [&'static str],
    /// Whether the tool takes its operation as its own `action` argument.
    ///
    /// A tool that does takes one argument per operation. A tool whose
    /// contract is a single behaviour takes none, and the family calls it
    /// without one.
    pub action_backed: bool,
}

/// Every consolidated family.
///
/// The order is the order the actions appear in each family's enum.
pub const FAMILIES: &[Family] = &[
    Family {
        name: "cron",
        description: "Schedule and inspect recurring jobs: add, list, remove, update, run, run history, and the one-shot schedule.",
        members: &[
            "cron_add",
            "cron_list",
            "cron_remove",
            "cron_update",
            "cron_run",
            "cron_runs",
            "schedule",
        ],
    },
    Family {
        name: "sop",
        description: "Standard operating procedures: list them, execute one, advance it, approve a gate, read its status, and open the workshop.",
        members: &[
            "sop_list",
            "sop_execute",
            "sop_advance",
            "sop_approve",
            "sop_status",
            "sop_workshop",
        ],
    },
    Family {
        name: "memory",
        description: "Long-term memory: store a fact, recall it, forget it, export it, or purge it.",
        members: &[
            "memory_store",
            "memory_recall",
            "memory_forget",
            "memory_export",
            "memory_purge",
        ],
    },
    Family {
        name: "a2a",
        description: "Agent-to-agent protocol: discover peers, send a message, inspect a task, and cancel it.",
        members: &["a2a_discover", "a2a_send", "a2a_get_task", "a2a_cancel"],
    },
    Family {
        name: "sessions",
        description: "Inspect and steer agent sessions: the current one, the list of them, their history, and sending to them.",
        members: &[
            "sessions_current",
            "sessions_list",
            "sessions_history",
            "sessions_send",
        ],
    },
    Family {
        name: "model",
        description: "Model control plane: switch the model this turn uses, and read or set the routing rules.",
        members: &["model_switch", "model_routing_config"],
    },
    Family {
        name: "mcp",
        description: "List and read the resources and prompts that connected MCP servers expose.",
        members: &["mcp_resources", "mcp_prompts"],
    },
    Family {
        name: "cloud",
        description: "Cloud architecture advisory: review infrastructure-as-code and the patterns to follow.",
        members: &["cloud_ops", "cloud_patterns"],
    },
    Family {
        // The family is `file_uploads`, not `file_upload`: the legacy
        // single-file tool already owns that name, and two tools cannot
        // share one name in the registry.
        name: "file_uploads",
        description: "Upload local files to a configured remote endpoint, one at a time or bundled.",
        members: &["file_upload", "file_upload_bundle"],
    },
    Family {
        name: "email",
        description: "Search and read mail in a configured IMAP mailbox. Read-only.",
        members: &["email_search", "email_read"],
    },
];

/// Every family member, with the operations it exposes.
///
/// The table is the family definition: it decides which operations a family
/// advertises and whether a member takes its operation as an `action`
/// argument. It is checked against the operations inventory at build time,
/// so it cannot drift away from the canonical registry the bridge publishes.
pub const MEMBERS: &[Member] = &[
    Member { tool: "cron_add", ops: &["add"], action_backed: false },
    Member { tool: "cron_list", ops: &["list"], action_backed: false },
    Member { tool: "cron_remove", ops: &["remove"], action_backed: false },
    Member { tool: "cron_update", ops: &["update"], action_backed: false },
    Member { tool: "cron_run", ops: &["run"], action_backed: false },
    Member { tool: "cron_runs", ops: &["list_runs"], action_backed: false },
    Member {
        tool: "schedule",
        ops: &[
            "add",
            "cancel",
            "create",
            "get",
            "list",
            "once",
            "pause",
            "remove",
            "resume",
        ],
        action_backed: true,
    },
    Member { tool: "sop_list", ops: &["list"], action_backed: false },
    Member { tool: "sop_execute", ops: &["execute"], action_backed: false },
    Member { tool: "sop_advance", ops: &["advance"], action_backed: false },
    Member { tool: "sop_approve", ops: &["approve"], action_backed: false },
    Member { tool: "sop_status", ops: &["status"], action_backed: false },
    Member {
        tool: "sop_workshop",
        ops: &[
            "propose",
            "capture_run",
            "list",
            "inspect",
            "apply",
            "reject",
            "quarantine",
        ],
        action_backed: true,
    },
    Member { tool: "memory_store", ops: &["store"], action_backed: false },
    Member { tool: "memory_recall", ops: &["recall"], action_backed: false },
    Member { tool: "memory_forget", ops: &["forget"], action_backed: false },
    Member { tool: "memory_export", ops: &["export"], action_backed: false },
    Member { tool: "memory_purge", ops: &["purge"], action_backed: false },
    Member { tool: "a2a_discover", ops: &["discover"], action_backed: false },
    Member { tool: "a2a_send", ops: &["send"], action_backed: false },
    Member { tool: "a2a_get_task", ops: &["get_task"], action_backed: false },
    Member { tool: "a2a_cancel", ops: &["cancel"], action_backed: false },
    Member { tool: "sessions_current", ops: &["current"], action_backed: false },
    Member { tool: "sessions_list", ops: &["list"], action_backed: false },
    Member { tool: "sessions_history", ops: &["history"], action_backed: false },
    Member { tool: "sessions_send", ops: &["send"], action_backed: false },
    Member {
        tool: "model_switch",
        ops: &["get", "list_model_providers", "list_models", "set"],
        action_backed: true,
    },
    Member {
        tool: "model_routing_config",
        ops: &[
            "get",
            "list_hints",
            "remove_agent",
            "remove_scenario",
            "set_default",
            "upsert_agent",
            "upsert_scenario",
        ],
        action_backed: true,
    },
    Member {
        tool: "mcp_resources",
        ops: &["list", "read"],
        action_backed: true,
    },
    Member {
        tool: "mcp_prompts",
        ops: &["list", "get"],
        action_backed: true,
    },
    Member {
        tool: "cloud_ops",
        ops: &[
            "architecture_review",
            "assess_migration",
            "cost_analysis",
            "review_iac",
        ],
        action_backed: true,
    },
    Member {
        tool: "cloud_patterns",
        ops: &["list", "match"],
        action_backed: true,
    },
    Member { tool: "file_upload", ops: &["upload"], action_backed: false },
    Member {
        tool: "file_upload_bundle",
        ops: &["upload_bundle"],
        action_backed: false,
    },
    Member { tool: "email_search", ops: &["search"], action_backed: false },
    Member { tool: "email_read", ops: &["read"], action_backed: false },
];

/// The member specification for a legacy tool name.
#[must_use]
pub fn member_spec(tool: &str) -> Option<&'static Member> {
    MEMBERS.iter().find(|member| member.tool == tool)
}

/// The family a legacy tool name belongs to.
#[must_use]
pub fn family_of(tool: &str) -> Option<&'static Family> {
    FAMILIES
        .iter()
        .find(|family| family.members.contains(&tool))
}

/// The family with this canonical name.
#[must_use]
pub fn family_by_name(name: &str) -> Option<&'static Family> {
    FAMILIES.iter().find(|family| family.name == name)
}

/// Whether a tool name is a legacy member that a canonical family covers.
///
/// A legacy member keeps working, but it is no longer advertised: the
/// family is the name the model should learn.
#[must_use]
pub fn is_legacy_member(tool: &str) -> bool {
    family_of(tool).is_some()
}

/// Every legacy member name, in family order.
#[must_use]
pub fn legacy_member_names() -> Vec<&'static str> {
    FAMILIES
        .iter()
        .flat_map(|family| family.members.iter().copied())
        .collect()
}

/// One (family action, member, member operation) triple.
struct FamilyActionSpec {
    /// The family-level action name.
    action: String,
    /// The member tool the action runs.
    member: &'static str,
    /// The member's own operation.
    op: &'static str,
}

/// A short prefix that tells two colliding operations apart.
///
/// `cron_add`'s `add` and `schedule`'s `add` both want to be `add`; the
/// member name minus the operation name is the natural discriminator (`cron`
/// against `schedule`). Where the member name does not end with the
/// operation, the whole member name is used.
fn discriminator(tool: &str, op: &str) -> String {
    match tool.strip_suffix(&format!("_{op}")) {
        Some(base) => base.to_string(),
        None => tool.to_string(),
    }
}

/// The family-level action for every operation the family's members expose.
///
/// The result is a bijection: no operation is dropped and no two operations
/// share a name. The names are computed over the *whole* family, not over
/// the members that happen to be registered, so a member that is missing
/// from the build does not rename the operations around it.
fn family_actions(family: &Family) -> Vec<FamilyActionSpec> {
    let mut pairs: Vec<(&'static str, &'static str)> = Vec::new();
    for member in family.members {
        let Some(spec) = member_spec(member) else {
            continue;
        };
        for op in spec.ops {
            pairs.push((member, op));
        }
    }

    // An operation name shared by several members needs a discriminator.
    let mut seen: HashSet<&str> = HashSet::new();
    let mut shared: HashSet<&str> = HashSet::new();
    for (_, op) in &pairs {
        if !seen.insert(op) {
            shared.insert(op);
        }
    }

    let mut used: HashSet<String> = HashSet::new();
    for (_, op) in &pairs {
        if !shared.contains(op) {
            used.insert((*op).to_string());
        }
    }

    let mut out = Vec::with_capacity(pairs.len());
    for (member, op) in pairs {
        let action = if shared.contains(op) {
            let base = discriminator(member, op);
            let mut candidate = format!("{base}_{op}");
            if used.contains(&candidate) {
                candidate = format!("{member}_{op}");
            }
            candidate
        } else {
            (*op).to_string()
        };
        used.insert(action.clone());
        out.push(FamilyActionSpec {
            action,
            member,
            op,
        });
    }
    out
}

/// The canonical tool and the family actions that replace a legacy tool.
///
/// A legacy tool that owns several operations names several actions.
#[must_use]
pub fn canonical_for(member: &str) -> Option<(&'static str, Vec<String>)> {
    let family = family_of(member)?;
    let actions = family_actions(family)
        .into_iter()
        .filter(|entry| entry.member == member)
        .map(|entry| entry.action)
        .collect();
    Some((family.name, actions))
}

/// The member and operation a canonical action names.
///
/// A call to `memory` with the action `recall` is a call to `memory_recall`.
/// The approval gate uses this so a consolidated family keeps the policy of
/// the tool it replaced: an operator who allowed `memory_recall` has allowed
/// the read, whichever name the model used.
#[must_use]
pub fn resolve(tool_name: &str, action: Option<&str>) -> Option<(&'static str, &'static str)> {
    let family = family_by_name(tool_name)?;
    let action = action?;
    family_actions(family)
        .into_iter()
        .find(|entry| entry.action == action)
        .map(|entry| (entry.member, entry.op))
}

/// One action of a family.
struct FamilyAction {
    /// The family-level action name.
    action: String,
    /// The member tool the action runs.
    tool: Arc<dyn Tool>,
    /// The member's own operation.
    op: &'static str,
    /// Whether the member takes its operation as an `action` argument.
    action_backed: bool,
}

/// The canonical tool for a family.
pub struct FamilyTool {
    name: &'static str,
    description: String,
    actions: Vec<FamilyAction>,
}

impl FamilyTool {
    /// Build the canonical tool from the member tools that are present.
    ///
    /// A family with no member present is not built: the canonical tool
    /// would advertise actions the agent cannot run.
    pub fn build(family: &Family, members: Vec<Arc<dyn Tool>>) -> Option<Self> {
        let present: HashMap<&str, &Arc<dyn Tool>> = members
            .iter()
            .map(|tool| (tool.name(), tool))
            .collect();
        let mut actions = Vec::new();
        for spec in family_actions(family) {
            let Some(tool) = present.get(spec.member) else {
                continue;
            };
            let Some(member) = member_spec(spec.member) else {
                continue;
            };
            actions.push(FamilyAction {
                action: spec.action,
                tool: Arc::clone(*tool),
                op: spec.op,
                action_backed: member.action_backed,
            });
        }
        if actions.is_empty() {
            return None;
        }
        let description = describe(family, &actions);
        Some(Self {
            name: family.name,
            description,
            actions,
        })
    }

    /// The actions this family exposes.
    #[must_use]
    pub fn action_names(&self) -> Vec<String> {
        self.actions
            .iter()
            .map(|action| action.action.clone())
            .collect()
    }
}

/// The family description, with the actions spelled out.
///
/// The description is the model's only map from an action name to what that
/// action does, so it lists them. Without this the agent sees an enum of
/// bare names and has to guess.
fn describe(family: &Family, actions: &[FamilyAction]) -> String {
    let mut lines = Vec::new();
    for member in family.members {
        let member_actions: Vec<&str> = actions
            .iter()
            .filter(|action| action.tool.name() == *member)
            .map(|action| action.action.as_str())
            .collect();
        if member_actions.is_empty() {
            continue;
        }
        let Some(action) = actions
            .iter()
            .find(|action| action.tool.name() == *member)
        else {
            continue;
        };
        lines.push(format!(
            "- {member}: actions {} — {}",
            member_actions.join(", "),
            action.tool.description()
        ));
    }
    format!("{}\n\nActions:\n{}", family.description, lines.join("\n"))
}

impl Attributable for FamilyTool {
    fn role(&self) -> Role {
        Role::Tool(ToolKind::Plugin)
    }

    fn alias(&self) -> &str {
        self.name
    }
}

#[async_trait]
impl Tool for FamilyTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        // The schema is bounded: the family declares its action enum and
        // nothing else. The members' own parameters differ from one member
        // to the next, and a union of them would be both wrong and large;
        // the action decides which reading applies.
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": self.action_names(),
                    "description": "Which operation to run.",
                },
            },
            "required": ["action"],
            "additionalProperties": true,
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let Some(action) = args.get("action").and_then(Value::as_str) else {
            anyhow::bail!(
                "the {} tool requires an action; expected one of: {}",
                self.name,
                self.action_names().join(", ")
            );
        };
        let Some(entry) = self.actions.iter().find(|entry| entry.action == action) else {
            anyhow::bail!(
                "unknown action {action:?} for the {} tool; expected one of: {}",
                self.name,
                self.action_names().join(", ")
            );
        };
        let mut call_args = args.clone();
        if let Some(object) = call_args.as_object_mut() {
            // The family's action is replaced by the member's own operation:
            // a member that takes one needs it, and a member that does not
            // must not see it.
            object.remove("action");
            if entry.action_backed {
                object.insert("action".to_string(), json!(entry.op));
            }
        }
        entry.tool.execute(call_args).await
    }
}

/// Build the canonical tool for every family whose members are present.
///
/// A member the build does not ship is simply absent, so the family's action
/// enum lists the actions that can actually run.
#[must_use]
pub fn build_all(registry: &[Arc<dyn Tool>]) -> Vec<(&'static str, FamilyTool)> {
    FAMILIES
        .iter()
        .filter_map(|family| {
            let members: Vec<Arc<dyn Tool>> = registry
                .iter()
                .filter(|tool| family.members.contains(&tool.name()))
                .cloned()
                .collect();
            FamilyTool::build(family, members).map(|tool| (family.name, tool))
        })
        .collect()
}

/// The legacy names that have already been announced.
///
/// The notice is one-time: a model that keeps calling `cron_add` should be
/// reminded once, not on every call.
fn announced() -> &'static Mutex<HashSet<String>> {
    static ANNOUNCED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    ANNOUNCED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// The deprecation notice for a legacy tool name, if it is due.
///
/// Returns `None` once the notice has been given for that name.
#[must_use]
pub fn take_deprecation_notice(member: &str) -> Option<Value> {
    let mut announced = announced().lock().ok()?;
    take_notice_from(&mut announced, member)
}

/// The notice a member earns, given the set of names already announced.
///
/// The announced set is a parameter so the one-time behaviour can be tested
/// without depending on process-wide state: the tests run in parallel, and a
/// global would let one test's call consume another's notice.
fn take_notice_from(announced: &mut HashSet<String>, member: &str) -> Option<Value> {
    let (canonical, actions) = canonical_for(member)?;
    if !announced.insert(member.to_string()) {
        return None;
    }
    let mut payload = json!({
        "canonical_tool": canonical,
    });
    if actions.len() == 1 {
        let action = &actions[0];
        payload["action"] = json!(action);
        payload["notice"] = json!(format!(
            "{member} is now the {canonical} tool with action=\"{action}\". \
             The old name still works, but use {canonical} from now on."
        ));
    } else {
        payload["actions"] = json!(actions);
        payload["notice"] = json!(format!(
            "{member} is now the {canonical} tool; use one of its actions: {}. \
             The old name still works, but use {canonical} from now on.",
            actions.join(", ")
        ));
    }
    Some(payload)
}

/// Attach the deprecation notice to a tool result's structured data.
///
/// The notice travels in the structured data, beside the result body, so the
/// body stays exactly what the tool produced.
pub fn attach_deprecation_notice(member: &str, data: &mut Option<Value>) {
    let notice = {
        let Ok(mut announced) = announced().lock() else {
            return;
        };
        take_notice_from(&mut announced, member)
    };
    attach_notice(notice, data);
}

/// Attach a notice to the structured data, if there is one.
fn attach_notice(notice: Option<Value>, data: &mut Option<Value>) {
    let Some(notice) = notice else {
        return;
    };
    let payload = data.get_or_insert_with(|| json!({}));
    if !payload.is_object() {
        *payload = json!({ "result": payload.clone() });
    }
    payload["deprecation"] = notice;
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::attribution::ToolProvenance;

    /// A tool that records the arguments it was called with.
    struct Fake {
        name: &'static str,
        seen: Arc<Mutex<Vec<Value>>>,
    }

    impl Fake {
        fn new(name: &'static str) -> (Arc<dyn Tool>, Arc<Mutex<Vec<Value>>>) {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let tool: Arc<dyn Tool> = Arc::new(Self {
                name,
                seen: Arc::clone(&seen),
            });
            (tool, seen)
        }
    }

    #[async_trait]
    impl Tool for Fake {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "a fake tool"
        }

        fn parameters_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": { "value": {"type": "string"} },
            })
        }

        async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
            self.seen.lock().expect("the fake records its calls").push(args.clone());
            Ok(ToolResult::ok(format!("{}:{}", self.name, args)))
        }
    }

    impl Attributable for Fake {
        fn role(&self) -> Role {
            Role::Agent
        }
        fn alias(&self) -> &str {
            self.name
        }
        fn tool_provenance(&self) -> ToolProvenance {
            ToolProvenance::Extension
        }
    }

    fn family(name: &str) -> &'static Family {
        FAMILIES
            .iter()
            .find(|family| family.name == name)
            .unwrap_or_else(|| panic!("the {name} family exists"))
    }

    fn actions_of(name: &str) -> Vec<String> {
        family_actions(family(name))
            .into_iter()
            .map(|entry| entry.action)
            .collect()
    }

    #[test]
    fn every_family_member_is_declared_with_its_operations() {
        for family in FAMILIES {
            for member in family.members {
                let spec = member_spec(member).unwrap_or_else(|| panic!("{member} is declared"));
                assert!(!spec.ops.is_empty(), "{member} exposes at least one operation");
            }
        }
        for member in MEMBERS {
            assert!(
                family_of(member.tool).is_some(),
                "{} belongs to a family",
                member.tool
            );
        }
    }

    #[test]
    fn no_legacy_member_appears_in_two_families() {
        let mut seen = HashSet::new();
        for member in legacy_member_names() {
            assert!(seen.insert(member), "{member} is claimed once");
        }
    }

    #[test]
    fn the_action_names_inside_a_family_are_unique() {
        for family in FAMILIES {
            let mut seen = HashSet::new();
            for action in actions_of(family.name) {
                assert!(seen.insert(action.clone()), "{action} is unique in {family}", family = family.name);
            }
        }
    }

    #[test]
    fn a_shared_operation_gets_a_discriminator() {
        // `cron_add` and `schedule` both have an `add`.
        let actions = actions_of("cron");
        assert!(actions.contains(&"cron_add".to_string()));
        assert!(actions.contains(&"schedule_add".to_string()));
    }

    #[test]
    fn the_cron_family_exposes_every_schedule_operation() {
        let actions = actions_of("cron");
        for op in member_spec("schedule").unwrap().ops {
            assert!(
                actions.iter().any(|action| action.contains(op)),
                "the schedule operation {op} is reachable"
            );
        }
    }

    #[test]
    fn the_action_names_match_the_canonical_registry() {
        // These are the names the canonical registry publishes. A model that
        // learned them from one host must not be wrong at another.
        assert_eq!(
            actions_of("cron"),
            vec![
                "cron_add",
                "cron_list",
                "cron_remove",
                "update",
                "run",
                "list_runs",
                "schedule_add",
                "cancel",
                "create",
                "get",
                "schedule_list",
                "once",
                "pause",
                "schedule_remove",
                "resume",
            ]
        );
        assert_eq!(
            actions_of("sop"),
            vec![
                "sop_list",
                "execute",
                "advance",
                "approve",
                "status",
                "propose",
                "capture_run",
                "sop_workshop_list",
                "inspect",
                "apply",
                "reject",
                "quarantine",
            ]
        );
        assert_eq!(
            actions_of("model"),
            vec![
                "model_switch_get",
                "list_model_providers",
                "list_models",
                "set",
                "model_routing_config_get",
                "list_hints",
                "remove_agent",
                "remove_scenario",
                "set_default",
                "upsert_agent",
                "upsert_scenario",
            ]
        );
        assert_eq!(
            actions_of("mcp"),
            vec!["mcp_resources_list", "read", "mcp_prompts_list", "get"]
        );
        assert_eq!(actions_of("memory"), vec!["store", "recall", "forget", "export", "purge"]);
        assert_eq!(actions_of("file_uploads"), vec!["upload", "upload_bundle"]);
    }

    #[tokio::test]
    async fn a_family_dispatches_to_its_member() {
        let (cron_list, seen) = Fake::new("cron_list");
        let tool = FamilyTool::build(
            family("cron"),
            vec![Arc::clone(&cron_list)],
        )
        .unwrap();
        // `list` is shared with the schedule tool, so the family names it
        // `cron_list` — the same name the canonical registry publishes.
        assert!(tool.action_names().contains(&"cron_list".to_string()));
        let result = tool.execute(json!({"action": "cron_list"})).await.unwrap();
        assert!(result.output.as_str().contains("cron_list"));
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0], json!({}));
    }

    #[tokio::test]
    async fn a_tool_contract_member_is_called_without_an_action() {
        let (cron_add, seen) = Fake::new("cron_add");
        let tool = FamilyTool::build(
            family("cron"),
            vec![Arc::clone(&cron_add)],
        )
        .unwrap();
        tool.execute(json!({"action": "cron_add", "value": "x"}))
            .await
            .unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0], json!({"value": "x"}));
    }

    #[tokio::test]
    async fn an_action_backed_member_receives_its_own_operation() {
        let (schedule, seen) = Fake::new("schedule");
        let tool = FamilyTool::build(
            family("cron"),
            vec![Arc::clone(&schedule)],
        )
        .unwrap();
        tool.execute(json!({"action": "schedule_add", "expression": "daily"}))
            .await
            .unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0]["action"], json!("add"));
        assert_eq!(seen[0]["expression"], json!("daily"));
    }

    #[tokio::test]
    async fn a_family_without_an_action_says_what_it_expects() {
        let (cron_add, _) = Fake::new("cron_add");
        let tool = FamilyTool::build(
            family("cron"),
            vec![Arc::clone(&cron_add)],
        )
        .unwrap();
        let error = tool.execute(json!({})).await.unwrap_err();
        assert!(error.to_string().contains("requires an action"));
    }

    #[tokio::test]
    async fn a_family_rejects_an_unknown_action() {
        let (cron_add, _) = Fake::new("cron_add");
        let tool = FamilyTool::build(
            family("cron"),
            vec![Arc::clone(&cron_add)],
        )
        .unwrap();
        let error = tool.execute(json!({"action": "nope"})).await.unwrap_err();
        assert!(error.to_string().contains("unknown action"));
    }

    #[test]
    fn a_family_with_no_member_is_not_built() {
        assert!(FamilyTool::build(family("cron"), Vec::new()).is_none());
    }

    #[test]
    fn a_family_schema_declares_its_action_enum() {
        let (cron_add, _) = Fake::new("cron_add");
        let tool = FamilyTool::build(
            family("cron"),
            vec![Arc::clone(&cron_add)],
        )
        .unwrap();
        let schema = tool.parameters_schema();
        assert_eq!(schema["properties"]["action"]["enum"], json!(["cron_add"]));
        assert_eq!(schema["required"], json!(["action"]));
        // The schema is bounded: the member's own parameters are not merged
        // into it, because they differ from one action to the next.
        assert_eq!(
            schema["properties"].as_object().unwrap().len(),
            1,
            "only the action is declared"
        );
        assert_eq!(schema["additionalProperties"], json!(true));
    }

    #[test]
    fn a_family_description_lists_its_actions() {
        let (schedule, _) = Fake::new("schedule");
        let tool = FamilyTool::build(
            family("cron"),
            vec![Arc::clone(&schedule)],
        )
        .unwrap();
        let description = tool.description().to_string();
        assert!(description.contains("schedule_add"), "{description}");
        assert!(description.contains("a fake tool"), "{description}");
    }

    #[test]
    fn the_deprecation_notice_is_given_once_per_name() {
        // A local announced set: the process-wide one is shared, and the
        // tests run in parallel, so a global would let one test consume
        // another's notice.
        let mut announced = HashSet::new();
        let first =
            take_notice_from(&mut announced, "cron_add").expect("the first call gets a notice");
        assert_eq!(first["canonical_tool"], json!("cron"));
        assert_eq!(first["action"], json!("cron_add"));
        assert!(take_notice_from(&mut announced, "cron_add").is_none());
    }

    #[test]
    fn the_notice_names_every_action_of_a_multi_operation_member() {
        let mut announced = HashSet::new();
        let notice = take_notice_from(&mut announced, "schedule").expect("the notice is due");
        assert_eq!(notice["canonical_tool"], json!("cron"));
        let actions = notice["actions"].as_array().expect("the actions are listed");
        assert_eq!(actions.len(), 9);
        assert!(actions.contains(&json!("schedule_add")));
    }

    #[test]
    fn the_notice_attaches_to_the_structured_data() {
        let mut announced = HashSet::new();
        let mut data: Option<Value> = None;
        attach_notice(
            take_notice_from(&mut announced, "cron_list"),
            &mut data,
        );
        let data = data.expect("the notice attaches");
        assert_eq!(data["deprecation"]["canonical_tool"], json!("cron"));
    }

    #[test]
    fn a_tool_that_is_not_a_member_gets_no_notice() {
        let mut data: Option<Value> = None;
        attach_deprecation_notice("shell", &mut data);
        assert!(data.is_none());
    }

    #[test]
    fn the_legacy_members_are_the_ones_the_families_replace() {
        assert!(is_legacy_member("cron_add"));
        assert!(is_legacy_member("schedule"));
        assert!(!is_legacy_member("shell"));
        assert!(!is_legacy_member("cron"));
        assert_eq!(legacy_member_names().len(), 36);
    }
}
