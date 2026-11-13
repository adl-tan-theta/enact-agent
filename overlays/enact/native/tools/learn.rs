//! `learn` — record a lesson so it survives the turn.
//!
//! An agent that discovers something — the build command that actually works,
//! the file that holds the answer, the flag that is misspelled — loses it when
//! the session ends. `learn` writes the lesson down, and optionally folds it
//! into a named skill so the next run starts from it instead of from zero.
//!
//! The lessons live in `<workspace>/.enact/lessons.md`, one dated entry per
//! lesson. A lesson aimed at a skill is also appended to that skill's
//! `SKILL.md`, under a `## Lessons` heading, without touching its YAML
//! front-matter.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};

use zeroclaw_api::tool::{Tool, ToolResult};

/// Where the workspace keeps its own state.
const STATE_DIR: &str = ".enact";

/// The file the lessons are appended to.
const LESSONS_FILE: &str = "lessons.md";

/// The heading a folded lesson goes under inside a skill.
const LESSONS_HEADING: &str = "## Lessons";

/// Record a lesson.
pub struct LearnTool {
    /// The workspace the lessons and the skills live in.
    workspace_dir: PathBuf,
}

impl LearnTool {
    /// Build the tool over a workspace.
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }

    /// The workspace's own state directory.
    fn state_dir(&self) -> PathBuf {
        self.workspace_dir.join(STATE_DIR)
    }

    /// The lessons file.
    fn lessons_file(&self) -> PathBuf {
        self.state_dir().join(LESSONS_FILE)
    }

    /// The directory of a named skill.
    ///
    /// A slug names one directory under `skills/`. It may not walk out of
    /// that directory, and it may not be empty.
    fn skill_dir(&self, slug: &str) -> Result<PathBuf, String> {
        let slug = slug.trim();
        if slug.is_empty() {
            return Err("a skill name is required".to_string());
        }
        if slug.contains("..") || slug.contains('/') || slug.contains('\\') {
            return Err(format!(
                "{slug:?} is not a skill name; a skill is one directory under skills/"
            ));
        }
        Ok(self.workspace_dir.join("skills").join(slug))
    }

    /// Append a dated lesson to the lessons file.
    async fn record(&self, lesson: &str, tags: &[String]) -> Result<String, String> {
        let path = self.lessons_file();
        let entry = format_lesson(lesson, tags);
        let mut existing = tokio::fs::read_to_string(&path)
            .await
            .unwrap_or_else(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    String::new()
                } else {
                    format!("# Lessons\n\n")
                }
            });
        if existing.trim().is_empty() {
            existing.push_str("# Lessons\n\n");
        }
        existing.push_str(&entry);
        write_atomic(&path, existing.as_bytes())
            .await
            .map_err(|error| format!("could not write the lessons file: {error}"))?;
        Ok(entry.trim_end().to_string())
    }

    /// Fold a lesson into a named skill.
    async fn fold_into_skill(&self, slug: &str, lesson: &str, tags: &[String]) -> Result<(), String> {
        let dir = self.skill_dir(slug)?;
        let skill_file = dir.join("SKILL.md");
        let mut content = tokio::fs::read_to_string(&skill_file)
            .await
            .map_err(|error| format!("could not read {}: {error}", skill_file.display()))?;
        let entry = format_lesson(lesson, tags);
        content = match split_at_heading(&content, LESSONS_HEADING) {
            Some((before, after)) => format!("{before}\n{entry}\n{after}"),
            None => {
                if !content.ends_with('\n') {
                    content.push('\n');
                }
                format!("{content}\n{LESSONS_HEADING}\n\n{entry}")
            }
        };
        write_atomic(&skill_file, content.as_bytes())
            .await
            .map_err(|error| format!("could not write {}: {error}", skill_file.display()))
    }
}

/// A dated lesson entry.
fn format_lesson(lesson: &str, tags: &[String]) -> String {
    let stamp = timestamp();
    let mut entry = format!("- {stamp} — {}\n", lesson.trim());
    if !tags.is_empty() {
        entry.push_str(&format!("  tags: {}\n", tags.join(", ")));
    }
    entry
}

/// The current time, as an ISO-8601 instant.
fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Split a document around a heading, if it has one.
///
/// The heading must be at the start of a line; the returned `after` part
/// keeps the heading itself, so a second lesson lands under the same one.
fn split_at_heading(content: &str, heading: &str) -> Option<(String, String)> {
    let needle = format!("\n{heading}\n");
    let start = content.find(&needle)? + 1;
    let before = content[..start].to_string();
    let after = content[start..].to_string();
    Some((before, after))
}

/// Write a file atomically: a temporary file beside it, then a rename.
async fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let temporary = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|value| value.to_str()).unwrap_or("tmp")
    ));
    tokio::fs::write(&temporary, bytes).await?;
    tokio::fs::rename(&temporary, path).await
}

#[async_trait]
impl Tool for LearnTool {
    fn name(&self) -> &str {
        "learn"
    }

    fn description(&self) -> &str {
        "Record a lesson learned, so the next run starts from it. Give the lesson as plain text. Optionally name a skill to fold it into: the lesson is appended to that skill's SKILL.md under a Lessons heading, without touching its front-matter. Lessons are also appended to the workspace's lessons file."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "lesson": {
                    "type": "string",
                    "description": "What was learned, in one or two sentences."
                },
                "skill": {
                    "type": "string",
                    "description": "The skill to fold the lesson into. Optional."
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Tags that describe the lesson. Optional."
                }
            },
            "required": ["lesson"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let lesson = args
            .get("lesson")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| anyhow::Error::msg("`learn` requires a `lesson`"))?;
        let tags: Vec<String> = args
            .get("tags")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|tag| !tag.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let entry = self
            .record(lesson, &tags)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;

        let mut folded = None;
        if let Some(skill) = args.get("skill").and_then(Value::as_str) {
            self.fold_into_skill(skill, lesson, &tags)
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            folded = Some(skill.trim().to_string());
        }

        let mut report = format!("Recorded the lesson.\n{entry}");
        if let Some(skill) = folded {
            report.push_str(&format!("Folded it into the skill `{skill}`.\n"));
        }
        Ok(ToolResult::ok(report))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        tempfile::tempdir().expect("a workspace")
    }

    #[tokio::test]
    async fn a_lesson_is_recorded() {
        let dir = workspace();
        let tool = LearnTool::new(dir.path().to_path_buf());
        let result = tool
            .execute(json!({"lesson": "the build command is `cargo build -p x`"}))
            .await
            .unwrap();
        assert!(result.success);
        let lessons = std::fs::read_to_string(dir.path().join(".enact/lessons.md")).unwrap();
        assert!(lessons.contains("cargo build -p x"), "{lessons}");
    }

    #[tokio::test]
    async fn a_lesson_keeps_its_tags() {
        let dir = workspace();
        let tool = LearnTool::new(dir.path().to_path_buf());
        tool.execute(json!({"lesson": "x", "tags": ["build", "ci"]}))
            .await
            .unwrap();
        let lessons = std::fs::read_to_string(dir.path().join(".enact/lessons.md")).unwrap();
        assert!(lessons.contains("tags: build, ci"), "{lessons}");
    }

    #[tokio::test]
    async fn a_lesson_folds_into_a_skill() {
        let dir = workspace();
        let skill_dir = dir.path().join("skills/deploying");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: deploying\n---\n\nHow to deploy.\n",
        )
        .unwrap();
        let tool = LearnTool::new(dir.path().to_path_buf());
        tool.execute(json!({"lesson": "deploy on Fridays", "skill": "deploying"}))
            .await
            .unwrap();
        let skill = std::fs::read_to_string(skill_dir.join("SKILL.md")).unwrap();
        assert!(skill.contains("deploy on Fridays"), "{skill}");
        assert!(skill.contains(LESSONS_HEADING), "{skill}");
        // The front-matter is untouched.
        assert!(skill.starts_with("---\nname: deploying\n---"), "{skill}");
    }

    #[tokio::test]
    async fn a_second_lesson_lands_under_the_same_heading() {
        let dir = workspace();
        let skill_dir = dir.path().join("skills/deploying");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "---\nname: deploying\n---\n\nBody.\n").unwrap();
        let tool = LearnTool::new(dir.path().to_path_buf());
        tool.execute(json!({"lesson": "first", "skill": "deploying"}))
            .await
            .unwrap();
        tool.execute(json!({"lesson": "second", "skill": "deploying"}))
            .await
            .unwrap();
        let skill = std::fs::read_to_string(skill_dir.join("SKILL.md")).unwrap();
        assert_eq!(skill.matches(LESSONS_HEADING).count(), 1, "{skill}");
        assert!(skill.contains("first") && skill.contains("second"), "{skill}");
    }

    #[tokio::test]
    async fn a_skill_that_does_not_exist_is_refused() {
        let dir = workspace();
        let tool = LearnTool::new(dir.path().to_path_buf());
        let error = tool
            .execute(json!({"lesson": "x", "skill": "missing"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("could not read"), "{error}");
    }

    #[tokio::test]
    async fn a_skill_name_cannot_walk_out_of_the_workspace() {
        let dir = workspace();
        let tool = LearnTool::new(dir.path().to_path_buf());
        let error = tool
            .execute(json!({"lesson": "x", "skill": "../../etc"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not a skill name"), "{error}");
    }

    #[tokio::test]
    async fn a_lesson_is_required() {
        let dir = workspace();
        let tool = LearnTool::new(dir.path().to_path_buf());
        assert!(tool.execute(json!({})).await.is_err());
    }
}
