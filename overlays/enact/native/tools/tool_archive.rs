//! The tool-output archive.
//!
//! A tool result that is large enough to crowd out the conversation is worth
//! keeping: the model asked for it, and "it was long" is not an answer. The
//! archive is where the full text goes. The model-visible result keeps a
//! readable head and a handle; `expand` retrieves the rest, so any
//! compression the runtime does stays lossless.
//!
//! The archive is scoped to the agent's workspace, at
//! `<workspace>/.enact/tool-archive/`, and holds one file per entry.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// The directory name the archive lives under, inside the workspace.
pub const ARCHIVE_DIR: &str = ".enact/tool-archive";

/// The result of a tool call is archived when it is longer than this.
pub const ARCHIVE_THRESHOLD_CHARS: usize = 8_000;

/// How much of an archived result stays in the model-visible text.
pub const ARCHIVE_HEAD_CHARS: usize = 2_000;

/// The root the runtime writes archives into.
///
/// The registry build knows the agent's workspace; the tool-result path does
/// not carry it, so the last registry build publishes it here.
static ROOT: RwLock<Option<PathBuf>> = RwLock::new(None);

/// The archive directory for a workspace.
#[must_use]
pub fn archive_dir_for(workspace: &Path) -> PathBuf {
    workspace.join(ARCHIVE_DIR)
}

/// Publish the archive root for this process.
pub fn use_workspace(workspace: &Path) {
    if let Ok(mut root) = ROOT.write() {
        *root = Some(archive_dir_for(workspace));
    }
}

/// The archive root this process writes into, if one is published.
#[must_use]
pub fn root() -> Option<PathBuf> {
    ROOT.read().ok().and_then(|root| root.clone())
}

/// One archived result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntry {
    /// The handle the model uses to retrieve the entry.
    pub id: String,
    /// The tool that produced it.
    pub tool: String,
    /// The size of the archived text, in characters.
    pub chars: usize,
}

/// Whether a handle is shaped like a handle.
///
/// The handle reaches the model as text, so it must not be able to name a
/// path. Only alphanumerics, dashes and underscores are accepted: a dot or a
/// slash is what turns an identifier into a path, and neither is allowed.
/// Underscores are allowed because tool names carry them (`file_read`).
#[must_use]
pub fn is_valid_handle(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_')
}

/// The file an entry is stored in.
fn entry_path(root: &Path, id: &str) -> PathBuf {
    root.join(format!("{id}.txt"))
}

/// Archive a tool result and return its handle.
///
/// Returns `None` when the archive has no root, when the directory cannot be
/// created, or when the handle cannot be written. The caller keeps the
/// original text in that case: an archive that fails must never lose output.
pub fn archive(tool: &str, output: &str) -> Option<String> {
    let root = root()?;
    if std::fs::create_dir_all(&root).is_err() {
        return None;
    }
    let id = handle_for(tool, output);
    if !is_valid_handle(&id) {
        return None;
    }
    let path = entry_path(&root, &id);
    std::fs::write(&path, output).ok()?;
    Some(id)
}

/// A deterministic handle for a result.
///
/// The same tool and the same text always produce the same handle, so a
/// repeated result does not fill the archive with copies.
fn handle_for(tool: &str, output: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in tool.as_bytes().iter().chain(output.as_bytes()) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{tool}-{:016x}", hash)
}

/// Retrieve an archived result.
#[must_use]
pub fn retrieve(root: &Path, id: &str) -> Option<String> {
    if !is_valid_handle(id) {
        return None;
    }
    std::fs::read_to_string(entry_path(root, id)).ok()
}

/// The entries in an archive, newest first.
#[must_use]
pub fn list(root: &Path) -> Vec<ArchiveEntry> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<ArchiveEntry> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("txt") {
                return None;
            }
            let id = path.file_stem()?.to_string_lossy().to_string();
            if !is_valid_handle(&id) {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            // The handle is `{tool}-{hash}`; the hash is the trailing hex run.
            let tool = id
                .rsplit_once('-')
                .map(|(tool, _hash)| tool)
                .unwrap_or("tool")
                .to_string();
            Some(ArchiveEntry {
                id,
                tool,
                chars: metadata.len() as usize,
            })
        })
        .collect();
    found.sort_by(|a, b| b.id.cmp(&a.id));
    found
}

/// The model-visible text for an archived result.
///
/// The head stays so the model can see what it got without spending the
/// whole budget, and the handle says how to get the rest.
#[must_use]
pub fn archived_text(id: &str, output: &str) -> String {
    let head: String = output.chars().take(ARCHIVE_HEAD_CHARS).collect();
    let omitted = output.chars().count().saturating_sub(ARCHIVE_HEAD_CHARS);
    format!(
        "{head}\n…[{omitted} more characters archived as {id}; call expand with id=\"{id}\" for the full output]"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_is_hex_and_the_tool_name() {
        let id = handle_for("file_read", "hello");
        assert!(is_valid_handle(&id));
        assert!(id.starts_with("file_read-"));
    }

    #[test]
    fn a_handle_cannot_name_a_path() {
        assert!(!is_valid_handle("../../etc/passwd"));
        assert!(!is_valid_handle(""));
        assert!(!is_valid_handle("a/b"));
        assert!(!is_valid_handle(&"x".repeat(65)));
    }

    #[test]
    fn the_same_result_always_gets_the_same_handle() {
        assert_eq!(
            handle_for("shell", "out"),
            handle_for("shell", "out")
        );
        assert_ne!(
            handle_for("shell", "out"),
            handle_for("shell", "other")
        );
    }

    #[test]
    fn a_result_is_archived_and_retrieved() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        let id = handle_for("shell", "the full output");
        std::fs::write(entry_path(&dir, &id), "the full output").unwrap();
        assert_eq!(
            retrieve(&dir, &id).as_deref(),
            Some("the full output")
        );
    }

    #[test]
    fn an_unknown_handle_retrieves_nothing() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(retrieve(root.path(), "shell-0000000000000000"), None);
    }

    #[test]
    fn the_archived_text_keeps_a_head_and_names_the_handle() {
        let output = "x".repeat(5000);
        let text = archived_text("shell-abc", &output);
        assert!(text.contains("shell-abc"));
        assert!(text.contains("expand"));
        assert!(text.len() < output.len());
    }

    #[test]
    fn the_archive_lists_what_it_holds() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().to_path_buf();
        let id = handle_for("shell", "out");
        std::fs::write(entry_path(&dir, &id), "out").unwrap();
        let entries = list(&dir);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, id);
        assert_eq!(entries[0].tool, "shell");
    }
}
