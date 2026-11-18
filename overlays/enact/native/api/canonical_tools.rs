//! The canonical tool names the consolidated families publish.
//!
//! The consolidation replaced a family of sibling tools with one canonical
//! tool per family: `cron` with `cron_add`, `schedule_add` and the rest, in
//! place of `cron_add`, `schedule` and their siblings. A host that learned
//! the old names must still resolve them, and a site that keys on a tool name
//! must ask this table what the name means rather than what it is spelled.
//!
//! The table lives here, in the API layer, because every crate that keys on
//! a tool name depends on it and none of them may depend on the runtime,
//! where the families themselves are assembled.
//!
//! The table is the single source of truth. The runtime's family module
//! derives its actions from the same rows, so the vocabulary a host reads
//! and the vocabulary a model sees cannot drift apart.

/// One consolidated family: its canonical name and the legacy names it
/// replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalFamily {
    /// The canonical tool name the model should learn.
    pub name: &'static str,
    /// The legacy tool names the family replaces.
    pub members: &'static [&'static str],
}

/// Every consolidated family, in the order their actions appear.
pub const CANONICAL_FAMILIES: &[CanonicalFamily] = &[
    CanonicalFamily {
        name: "cron",
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
    CanonicalFamily {
        name: "sop",
        members: &[
            "sop_list",
            "sop_execute",
            "sop_advance",
            "sop_approve",
            "sop_status",
            "sop_workshop",
        ],
    },
    CanonicalFamily {
        name: "memory",
        members: &[
            "memory_store",
            "memory_recall",
            "memory_forget",
            "memory_export",
            "memory_purge",
        ],
    },
    CanonicalFamily {
        name: "a2a",
        members: &["a2a_discover", "a2a_send", "a2a_get_task", "a2a_cancel"],
    },
    CanonicalFamily {
        name: "sessions",
        members: &["sessions_current", "sessions_list", "sessions_history", "sessions_send"],
    },
    CanonicalFamily {
        name: "model",
        members: &["model_switch", "model_routing_config"],
    },
    CanonicalFamily {
        name: "mcp",
        members: &["mcp_resources", "mcp_prompts"],
    },
    CanonicalFamily {
        name: "cloud",
        members: &["cloud_ops", "cloud_patterns"],
    },
    CanonicalFamily {
        name: "file_uploads",
        members: &["file_upload", "file_upload_bundle"],
    },
    CanonicalFamily {
        name: "email",
        members: &["email_search", "email_read"],
    },
];

/// The family a legacy tool name belongs to.
#[must_use]
pub fn canonical_family_of(name: &str) -> Option<&'static CanonicalFamily> {
    CANONICAL_FAMILIES
        .iter()
        .find(|family| family.members.contains(&name))
}

/// The canonical name for a tool name.
///
/// A legacy member name resolves to the family that replaced it; a family
/// name resolves to itself; a name that is neither is not a tool name and
/// resolves to nothing.
///
/// Every name-keyed site goes through this one function, so a legacy name
/// and its canonical form cannot drift apart: the site asks what the name
/// means, not what it is spelled.
#[must_use]
pub fn canonical_tool_name(name: &str) -> Option<&'static str> {
    if let Some(family) = CANONICAL_FAMILIES.iter().find(|family| family.name == name) {
        return Some(family.name);
    }
    canonical_family_of(name).map(|family| family.name)
}

/// The canonical name for a tool name, or the name itself when it is not a
/// legacy member.
///
/// This is the form a name-keyed site wants when the name is not a tool name
/// at all — a shell command, a channel id, a free-text label. Those keep
/// their spelling.
#[must_use]
pub fn canonical_tool_name_or_self(name: &str) -> &str {
    canonical_tool_name(name).unwrap_or(name)
}

/// Whether a tool name is a legacy member a canonical family covers.
#[must_use]
pub fn is_legacy_tool_member(name: &str) -> bool {
    canonical_family_of(name).is_some()
}

/// Every legacy member name, in family order.
#[must_use]
pub fn legacy_tool_member_names() -> Vec<&'static str> {
    CANONICAL_FAMILIES
        .iter()
        .flat_map(|family| family.members.iter().copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn a_legacy_name_resolves_to_its_family_and_a_family_to_itself() {
        assert_eq!(canonical_tool_name("cron_add"), Some("cron"));
        assert_eq!(canonical_tool_name("schedule"), Some("cron"));
        assert_eq!(canonical_tool_name("memory_recall"), Some("memory"));
        assert_eq!(canonical_tool_name("cron"), Some("cron"));
        assert_eq!(canonical_tool_name("shell"), None);
        assert_eq!(canonical_tool_name("not_a_tool"), None);
    }

    #[test]
    fn a_name_that_is_not_a_tool_keeps_its_spelling() {
        assert_eq!(canonical_tool_name_or_self("cron_add"), "cron");
        assert_eq!(canonical_tool_name_or_self("cron"), "cron");
        assert_eq!(canonical_tool_name_or_self("shell"), "shell");
        assert_eq!(canonical_tool_name_or_self("bash -lc ls"), "bash -lc ls");
    }

    #[test]
    fn no_legacy_member_is_claimed_twice() {
        let mut seen = HashSet::new();
        for member in legacy_tool_member_names() {
            assert!(seen.insert(member), "{member} is claimed once");
        }
    }

    #[test]
    fn a_family_name_is_never_a_member_of_another_family() {
        for family in CANONICAL_FAMILIES {
            assert!(
                !family.members.contains(&family.name),
                "{} does not list itself as a member",
                family.name
            );
            assert!(
                canonical_family_of(family.name).is_none(),
                "{} is not itself a legacy member",
                family.name
            );
        }
    }

    #[test]
    fn the_legacy_members_are_recognised() {
        assert!(is_legacy_tool_member("cron_add"));
        assert!(is_legacy_tool_member("schedule"));
        assert!(!is_legacy_tool_member("shell"));
        assert!(!is_legacy_tool_member("cron"));
    }
}
