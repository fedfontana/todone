//! Issue draft template: rendering and parsing.
//!
//! When the user ports a finding, an editor opens with a prefilled Markdown
//! file that carries TOML frontmatter (category, path, commit) plus
//! `# Title:` and `## Description` sections. The file doubles as the issue
//! body once created, and [`parse_draft`] decodes it back into an
//! [`IssueDraft`].

use std::path::PathBuf;

use thiserror::Error;

/// The template version understood by this crate.
pub const DRAFT_VERSION: u32 = 1;

/// The tool name expected in the frontmatter.
pub const TOOL_NAME: &str = "todone";

/// Placeholder text for the title the user must replace.
pub const TITLE_PLACEHOLDER: &str = "<write title here>";

/// Placeholder text for the description the user must replace.
pub const DESCRIPTION_PLACEHOLDER: &str = "<write the issue description here>";

/// The code snippet embedded in the draft, as shown in the review screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSnippet {
    /// Language id used for the code fence, e.g. `rust`.
    pub language: String,
    /// The snippet lines, without the surrounding fence.
    pub text: String,
}

/// A portable issue: enough information to create an issue on any forge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueDraft {
    /// The matched category, e.g. `TODO`.
    pub category: String,
    /// Repository-relative path of the finding.
    pub path: PathBuf,
    /// Commit hash the scan ran against.
    pub commit: String,
    /// Issue title (one line).
    pub title: String,
    /// Issue body.
    pub description: String,
}

/// Renders the editable draft file for `draft`, embedding `snippet` as
/// context. The output is the exact input format [`parse_draft`] expects.
///
/// # Examples
///
/// ```
/// use std::path::PathBuf;
/// use todone_core::draft::{render_draft, parse_draft, ContextSnippet, IssueDraft};
///
/// let draft = IssueDraft {
///     category: "TODO".into(),
///     path: PathBuf::from("src/lib.rs"),
///     commit: "abc123".into(),
///     title: "Fix the bug".into(),
///     description: "It crashes on empty input.".into(),
/// };
/// let snippet = ContextSnippet {
///     language: "rust".into(),
///     text: "fn main() { // TODO: crash\n".into(),
/// };
/// let rendered = render_draft(&draft, &snippet);
/// assert!(rendered.contains("commit = \"abc123\""));
/// assert!(rendered.contains("# Title: Fix the bug"));
/// assert_eq!(parse_draft(&rendered).unwrap(), draft);
/// ```
pub fn render_draft(draft: &IssueDraft, snippet: &ContextSnippet) -> String {
    format!(
        "---\n\
         tool = \"{TOOL_NAME}\"\n\
         version = {DRAFT_VERSION}\n\
         category = \"{category}\"\n\
         path = \"{path}\"\n\
         commit = \"{commit}\"\n\
         ---\n\
         # Title: {title}\n\
         \n\
         ## Context\n\
         ```{language}\n\
         {text}\
         ```\n\
         \n\
         ## Description\n\
         {description}\n",
        category = draft.category,
        path = draft.path.display(),
        commit = draft.commit,
        title = draft.title,
        language = snippet.language,
        text = snippet.text.trim_end_matches('\n'),
        description = draft.description,
    )
}

/// Errors produced while decoding a draft file.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DraftError {
    /// The leading `---` frontmatter block is missing or unclosed.
    #[error("missing frontmatter block; keep the --- section at the top of the file")]
    NoFrontmatter,
    /// The frontmatter is not valid TOML.
    #[error("frontmatter is not valid: {0}")]
    InvalidFrontmatter(String),
    /// The frontmatter names a different tool or an unknown version.
    #[error("frontmatter must declare tool = \"{TOOL_NAME}\" and version = {DRAFT_VERSION}")]
    UnknownToolOrVersion,
    /// A required frontmatter field is missing.
    #[error("frontmatter is missing the `{0}` field")]
    MissingField(&'static str),
    /// The `# Title:` line is missing.
    #[error("missing `# Title:` line")]
    NoTitle,
    /// The `## Description` section is missing.
    #[error("missing `## Description` section")]
    NoDescription,
    /// The title was not replaced; the template was likely not edited.
    #[error("title still contains the placeholder; write a real title (or quit without saving)")]
    PlaceholderTitle,
}

/// Decodes the content of a draft file edited by the user.
///
/// The `## Context` section is ignored; only frontmatter, title, and
/// description are read.
///
/// # Errors
///
/// Returns [`DraftError`] when the template structure was broken; keep the
/// `---` frontmatter, `# Title:` line, and `## Description` section intact.
pub fn parse_draft(content: &str) -> Result<IssueDraft, DraftError> {
    let content = content.replace("\r\n", "\n");

    let rest = content
        .trim_start()
        .strip_prefix("---\n")
        .ok_or(DraftError::NoFrontmatter)?;
    let (frontmatter, body) = rest
        .split_once("\n---\n")
        .ok_or(DraftError::NoFrontmatter)?;

    let parsed: toml::Value =
        toml::from_str(frontmatter).map_err(|e| DraftError::InvalidFrontmatter(e.to_string()))?;
    let tool = string_field(&parsed, "tool")?;
    let version = parsed
        .get("version")
        .and_then(toml::Value::as_integer)
        .ok_or(DraftError::MissingField("version"))?;
    if tool != TOOL_NAME || version != DRAFT_VERSION as i64 {
        return Err(DraftError::UnknownToolOrVersion);
    }
    let category = string_field(&parsed, "category")?;
    let path = string_field(&parsed, "path")?;
    let commit = string_field(&parsed, "commit")?;

    let title = body
        .lines()
        .find_map(|line| line.strip_prefix("# Title:"))
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .ok_or(DraftError::NoTitle)?;

    let description = body
        .split_once("## Description")
        .map(|(_, rest)| rest.trim())
        .filter(|description| !description.is_empty())
        .ok_or(DraftError::NoDescription)?;

    if title == TITLE_PLACEHOLDER {
        return Err(DraftError::PlaceholderTitle);
    }

    Ok(IssueDraft {
        category,
        path: PathBuf::from(path),
        commit,
        title: title.to_string(),
        description: description.to_string(),
    })
}

fn string_field(value: &toml::Value, key: &'static str) -> Result<String, DraftError> {
    value
        .get(key)
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .ok_or(DraftError::MissingField(key))
}

/// Whether the title was left at the template placeholder.
pub fn is_placeholder_title(title: &str) -> bool {
    title.trim() == TITLE_PLACEHOLDER
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft() -> IssueDraft {
        IssueDraft {
            category: "TODO".into(),
            path: PathBuf::from("src/lib.rs"),
            commit: "abc123".into(),
            title: "Fix the bug".into(),
            description: "It crashes on empty input.".into(),
        }
    }

    fn snippet() -> ContextSnippet {
        ContextSnippet {
            language: "rust".into(),
            text: "fn main() { // TODO: crash\n".into(),
        }
    }

    #[test]
    fn render_parse_round_trip() {
        let rendered = render_draft(&draft(), &snippet());
        assert_eq!(parse_draft(&rendered).unwrap(), draft());
    }

    #[test]
    fn crlf_draft_parses() {
        let rendered = render_draft(&draft(), &snippet()).replace('\n', "\r\n");
        assert_eq!(parse_draft(&rendered).unwrap(), draft());
    }

    #[test]
    fn description_may_contain_markdown_and_fences() {
        let mut d = draft();
        d.description = "```rust\nlet x = 1;\n```\n\n- bullet".into();
        let rendered = render_draft(&d, &snippet());
        let parsed = parse_draft(&rendered).unwrap();
        assert_eq!(parsed.description, d.description);
        assert!(parsed.description.contains("```rust"));
    }

    #[test]
    fn context_section_is_ignored_on_parse() {
        let rendered = render_draft(&draft(), &snippet());
        let parsed = parse_draft(&rendered).unwrap();
        assert!(!parsed.description.contains("fn main"));
    }

    #[test]
    fn title_without_space_after_marker_parses() {
        let content = render_draft(&draft(), &snippet()).replace("# Title: Fix", "# Title:Fix");
        assert_eq!(parse_draft(&content).unwrap().title, "Fix the bug");
    }

    #[test]
    fn errors() {
        assert!(matches!(
            parse_draft("no frontmatter").unwrap_err(),
            DraftError::NoFrontmatter
        ));
        let mut content = render_draft(&draft(), &snippet());
        assert!(matches!(
            parse_draft(&content.replace("---\n", "")).unwrap_err(),
            DraftError::NoFrontmatter
        ));

        content = render_draft(&draft(), &snippet()).replace("version = 1", "version = 99");
        assert!(matches!(
            parse_draft(&content).unwrap_err(),
            DraftError::UnknownToolOrVersion
        ));

        content =
            render_draft(&draft(), &snippet()).replace("tool = \"todone\"", "tool = \"other\"");
        assert!(matches!(
            parse_draft(&content).unwrap_err(),
            DraftError::UnknownToolOrVersion
        ));

        content = render_draft(&draft(), &snippet()).replace("# Title: Fix the bug", "");
        assert!(matches!(
            parse_draft(&content).unwrap_err(),
            DraftError::NoTitle
        ));

        content = render_draft(&draft(), &snippet()).replace("## Description", "## Notes");
        assert!(matches!(
            parse_draft(&content).unwrap_err(),
            DraftError::NoDescription
        ));

        let mut d = draft();
        d.title = TITLE_PLACEHOLDER.into();
        content = render_draft(&d, &snippet());
        assert!(matches!(
            parse_draft(&content).unwrap_err(),
            DraftError::PlaceholderTitle
        ));
    }

    #[test]
    fn placeholders_detected() {
        assert!(is_placeholder_title(TITLE_PLACEHOLDER));
        assert!(!is_placeholder_title("Real title"));
    }
}
