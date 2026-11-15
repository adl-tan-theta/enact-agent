//! Shared workspace scanning helpers for the context and code-intelligence
//! tools.
//!
//! Every tool that reads the workspace has to answer the same three questions
//! the same way: which files are worth looking at, which of them may this
//! agent read, and how big is a token. Getting those answers in one place is
//! what keeps `compose`, `ast_grep`, `callgraph`, and `graph` from drifting
//! apart on what they consider "the workspace".

use std::path::{Path, PathBuf};

use zeroclaw_config::policy::SecurityPolicy;

/// Directories that are never worth scanning.
///
/// The list is deliberately about *cost*, not about trust: the path guard
/// still decides what may be read.
const SKIPPED_DIRECTORIES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "target",
    "node_modules",
    "vendor",
    "dist",
    "build",
    ".next",
    ".turbo",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".gradle",
    ".idea",
    ".vscode",
    "coverage",
    "Pods",
    "DerivedData",
];

/// Files larger than this are not worth inlining.
pub const MAX_SCANNED_FILE_BYTES: u64 = 512 * 1024;

/// Extensions treated as source code for scoring and language detection.
pub const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "py", "js", "jsx", "ts", "tsx", "go", "java", "kt", "kts", "c", "h", "cc", "cpp",
    "hpp", "cs", "rb", "php", "swift", "scala", "sh", "bash", "zsh", "lua", "pl", "ex", "exs",
    "hs", "ml", "dart", "vue", "svelte", "sql", "proto", "graphql",
];

/// Extensions treated as documentation or configuration, which still matter
/// for context but rank below source.
pub const DOC_EXTENSIONS: &[&str] = &[
    "md", "mdx", "rst", "txt", "adoc", "toml", "yaml", "yml", "json", "ini", "cfg", "lock",
];

/// Whether a directory name is one the scanner never descends into.
#[must_use]
pub fn is_skipped_directory(name: &str) -> bool {
    SKIPPED_DIRECTORIES.contains(&name)
}

/// Whether a path looks like source code.
#[must_use]
pub fn is_source_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| SOURCE_EXTENSIONS.contains(&extension))
        .unwrap_or(false)
}

/// Whether a path looks like documentation or configuration.
#[must_use]
pub fn is_doc_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| DOC_EXTENSIONS.contains(&extension))
        .unwrap_or(false)
}

/// The language name for a path, in the spelling ast-grep uses.
#[must_use]
pub fn language_for(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?;
    let language = match extension {
        "rs" => "rust",
        "py" => "python",
        "js" | "jsx" => "javascript",
        "ts" | "tsx" => "typescript",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "c" | "h" => "c",
        "cc" | "cpp" | "hpp" => "cpp",
        "cs" => "csharp",
        "rb" => "ruby",
        "php" => "php",
        "swift" => "swift",
        "scala" => "scala",
        "sh" | "bash" | "zsh" => "bash",
        "lua" => "lua",
        "hs" => "haskell",
        "dart" => "dart",
        "vue" => "vue",
        "svelte" => "svelte",
        "sql" => "sql",
        _ => return None,
    };
    Some(language)
}

/// An approximate token count.
///
/// The runtime's own provider tokenizers are model-specific, and a context
/// budget that claims precision it does not have is worse than one that says
/// "about". Four characters per token is the usual English average and is
/// what the MCP search surface already uses for its own budget.
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

/// Collect the workspace's readable text files, up to `limit` paths.
///
/// Files are returned in a stable, sorted order so two runs over the same
/// workspace produce the same list. Files the agent may not read are skipped
/// exactly as the path guard would skip them elsewhere.
#[must_use]
pub fn collect_workspace_files(
    workspace: &Path,
    security: &SecurityPolicy,
    limit: usize,
) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![workspace.to_path_buf()];
    while let Some(directory) = stack.pop() {
        if found.len() >= limit {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(std::fs::DirEntry::path);
        for entry in entries {
            if found.len() >= limit {
                break;
            }
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if is_skipped_directory(&name) {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if !is_readable_text_file(&path, security) {
                continue;
            }
            found.push(path);
        }
    }
    found.sort();
    found
}

/// Whether a path is a text file this agent may read.
#[must_use]
pub fn is_readable_text_file(path: &Path, security: &SecurityPolicy) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() || metadata.len() > MAX_SCANNED_FILE_BYTES {
        return false;
    }
    let Ok(resolved) = std::fs::canonicalize(path) else {
        return false;
    };
    if !security.is_resolved_path_readable(&resolved) {
        return false;
    }
    is_probably_text(&resolved)
}

/// Whether a file's leading bytes look like text rather than binary.
fn is_probably_text(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let sample = &bytes[..bytes.len().min(4096)];
    !sample.contains(&0)
}

/// The lowercase alphanumeric words of a text, used for relevance scoring.
#[must_use]
pub fn terms(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() {
            current.extend(character.to_lowercase());
        } else if !current.is_empty() {
            terms.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        terms.push(current);
    }
    terms
}

/// The distinct lowercase words of a text, longer than one character.
///
/// A single letter carries no meaning as a keyword; two do ("id", "io", "db"
/// are ordinary words in code). Filtering at two characters threw away real
/// terms, so the floor is one.
#[must_use]
pub fn keywords(text: &str) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for term in terms(text) {
        if term.len() < 2 {
            continue;
        }
        if seen.insert(term.clone()) {
            out.push(term);
        }
    }
    out
}

/// The path of a file relative to the workspace, for display.
#[must_use]
pub fn display_path(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_estimate_is_about_a_quarter_of_the_characters() {
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("a".repeat(400).as_str()), 100);
    }

    #[test]
    fn the_scanner_skips_the_expensive_directories() {
        assert!(is_skipped_directory("target"));
        assert!(is_skipped_directory("node_modules"));
        assert!(is_skipped_directory(".git"));
        assert!(!is_skipped_directory("src"));
    }

    #[test]
    fn source_and_doc_extensions_are_told_apart() {
        assert!(is_source_path(Path::new("src/main.rs")));
        assert!(!is_source_path(Path::new("README.md")));
        assert!(is_doc_path(Path::new("README.md")));
        assert!(!is_doc_path(Path::new("src/main.rs")));
    }

    #[test]
    fn the_language_spelling_matches_ast_grep() {
        assert_eq!(language_for(Path::new("a.rs")), Some("rust"));
        assert_eq!(language_for(Path::new("a.py")), Some("python"));
        assert_eq!(language_for(Path::new("a.unknown")), None);
    }

    #[test]
    fn terms_are_lowercased_and_split_on_punctuation() {
        assert_eq!(terms("Hello, World!"), vec!["hello", "world"]);
        assert_eq!(keywords("a bb CCC"), vec!["bb", "ccc"]);
    }

    #[test]
    fn a_display_path_is_relative_to_the_workspace() {
        assert_eq!(
            display_path(Path::new("/w"), Path::new("/w/src/a.rs")),
            "src/a.rs"
        );
        assert_eq!(
            display_path(Path::new("/w"), Path::new("/other/a.rs")),
            "/other/a.rs"
        );
    }
}
