//! Category matching for comment text.
//!
//! A [`MatchConfig`] describes which marker categories a comment must
//! contain for the scanner to report it. Matching is compiled into a
//! [`Matcher`]: either the built-in default (the category must be the first
//! content token of a comment line) or a user-supplied pattern in
//! regex-crate syntax with `{comment}`, `{marker}`, and `{content}`
//! placeholders.
//!
//! Matching runs against the *text of a comment node* — including its
//! marker (`//`, `#`, ...).

use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The default categories: `TODO` and `FIXME`.
pub fn default_categories() -> Vec<String> {
    vec!["TODO".to_string(), "FIXME".to_string()]
}

/// The built-in pattern: the marker must be the first content token of a
/// comment line (the leading run of non-word characters — `//`, `///`, `#`,
/// `/*`, ` * ` decorations — may precede it). The suffix keeps `TODOS`
/// from matching `TODO`.
const DEFAULT_PATTERN: &str = r"(?m)^{comment}{marker}(?::|!|\(|\s|$)";

/// Configuration for which comments count as marker comments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MatchConfig {
    /// Category names to look for, e.g. `["TODO", "FIXME"]`. At least one
    /// non-empty entry is required.
    pub categories: Vec<String>,
    /// If `false` (default), categories match case-insensitively. This only
    /// affects the `{marker}` placeholder; the rest of a custom pattern is
    /// used verbatim.
    pub case_sensitive: bool,
    /// A custom comment pattern in regex-crate syntax. The placeholders
    /// `{comment}`, `{marker}`, and `{content}` are substituted (see
    /// [`Matcher`]); everything else is matched verbatim against the comment
    /// text. When unset, the built-in pattern is used.
    pub pattern: Option<String>,
}

impl Default for MatchConfig {
    fn default() -> Self {
        Self {
            categories: default_categories(),
            case_sensitive: false,
            pattern: None,
        }
    }
}

impl MatchConfig {
    /// Compiles this configuration into a [`Matcher`].
    ///
    /// # Errors
    ///
    /// Returns [`MatchConfigError::NoCategories`] when the category list is
    /// empty, [`MatchConfigError::EmptyCategory`] when an entry is blank,
    /// [`MatchConfigError::MissingMarkerPlaceholder`] when a custom pattern
    /// has no `{marker}`, [`MatchConfigError::UnknownPlaceholder`] when it
    /// uses an unknown placeholder, and [`MatchConfigError::Regex`] when the
    /// expanded pattern fails to compile.
    ///
    /// # Examples
    ///
    /// ```
    /// use todone_core::matcher::{MatchConfig, MatchConfigError};
    ///
    /// let matcher = MatchConfig::default().compile().unwrap();
    /// assert_eq!(matcher.match_category("// TODO: fix this"), Some("TODO"));
    ///
    /// let empty = MatchConfig { categories: vec![], ..Default::default() };
    /// assert!(matches!(
    ///     empty.compile(),
    ///     Err(MatchConfigError::NoCategories)
    /// ));
    /// ```
    pub fn compile(&self) -> Result<Matcher, MatchConfigError> {
        if self.categories.is_empty() {
            return Err(MatchConfigError::NoCategories);
        }
        if self.categories.iter().any(|c| c.trim().is_empty()) {
            return Err(MatchConfigError::EmptyCategory);
        }

        let mut alternation = String::new();
        for category in &self.categories {
            if !alternation.is_empty() {
                alternation.push('|');
            }
            alternation.push_str(&regex::escape(category));
        }
        let marker = if self.case_sensitive {
            format!("(?<marker>{alternation})")
        } else {
            format!("(?<marker>(?i:{alternation}))")
        };

        let pattern = self.pattern.as_deref().unwrap_or(DEFAULT_PATTERN);
        let expanded = substitute_placeholders(pattern, &marker)?;
        if !expanded.contains("(?<marker>") {
            return Err(MatchConfigError::MissingMarkerPlaceholder);
        }
        let regex = Regex::new(&expanded)?;
        let compiled = CompiledPattern {
            categories: self.categories.clone(),
            regex,
        };
        Ok(match self.pattern {
            Some(_) => Matcher::Custom(compiled),
            None => Matcher::Default(compiled),
        })
    }
}

/// Errors produced while compiling a [`MatchConfig`].
#[derive(Debug, Error)]
pub enum MatchConfigError {
    /// The category list is empty.
    #[error("at least one category is required")]
    NoCategories,
    /// A category entry is blank.
    #[error("categories must not be empty")]
    EmptyCategory,
    /// A custom pattern uses an unknown placeholder.
    #[error(
        "unknown placeholder {{{0}}} in pattern; supported placeholders are \
         {{comment}}, {{marker}}, and {{content}}"
    )]
    UnknownPlaceholder(String),
    /// A custom pattern has no `{marker}` placeholder, so the category
    /// cannot be determined.
    #[error("custom pattern must contain the {{marker}} placeholder")]
    MissingMarkerPlaceholder,
    /// The expanded pattern failed to compile.
    #[error("failed to compile category regex: {0}")]
    Regex(#[from] regex::Error),
}

/// A compiled [`MatchConfig`], cheap to match against comment text.
///
/// Both variants resolve the matched category from the `marker` capture
/// group and normalize it back to the configured spelling (so `// todo: x`
/// reports `TODO` when matching case-insensitively).
#[derive(Debug)]
pub enum Matcher {
    /// The built-in pattern: the marker must be the first content token of a
    /// comment line.
    Default(CompiledPattern),
    /// A user-supplied pattern.
    Custom(CompiledPattern),
}

impl Matcher {
    /// Returns the configured category matched in `text`, or `None` if the
    /// comment is not a marker comment.
    ///
    /// # Examples
    ///
    /// ```
    /// use todone_core::matcher::MatchConfig;
    ///
    /// let matcher = MatchConfig::default().compile().unwrap();
    /// assert_eq!(matcher.match_category("// TODO: handle"), Some("TODO"));
    /// assert_eq!(matcher.match_category("// todo"), Some("TODO"));
    /// // Doc comments merely mentioning the marker are not matches.
    /// assert_eq!(matcher.match_category("/// Triage TODO comments: scan"), None);
    /// assert_eq!(matcher.match_category("// plain note"), None);
    ///
    /// let custom = MatchConfig {
    ///     pattern: Some("^{comment}{marker}:{content}".into()),
    ///     ..Default::default()
    /// };
    /// let matcher = custom.compile().unwrap();
    /// assert_eq!(matcher.match_category("// TODO: handle"), Some("TODO"));
    /// assert_eq!(matcher.match_category("// TODO handle"), None);
    /// ```
    pub fn match_category(&self, text: &str) -> Option<&str> {
        match self {
            Matcher::Default(compiled) => compiled.match_category(text),
            Matcher::Custom(compiled) => compiled.match_category(text),
        }
    }
}

/// The compiled regex plus the configured categories for normalization.
#[derive(Debug)]
pub struct CompiledPattern {
    categories: Vec<String>,
    regex: Regex,
}

impl CompiledPattern {
    fn match_category(&self, text: &str) -> Option<&str> {
        let matched = self.regex.captures(text)?.name("marker")?.as_str();
        // The capture always comes from the alternation of configured
        // categories, so normalization finds it (exact match when
        // case-sensitive, ignore-case otherwise).
        self.categories
            .iter()
            .find(|category| category.eq_ignore_ascii_case(matched))
            .map(String::as_str)
    }
}

/// Replaces the `{comment}`, `{marker}`, and `{content}` placeholders in a
/// user pattern. Repetition quantifiers (`{2}`, `{1,3}`) and escaped braces
/// (`\{`) are left untouched; any other `{name}` is an error.
fn substitute_placeholders(pattern: &str, marker: &str) -> Result<String, MatchConfigError> {
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut rest = pattern;
    while let Some(rel) = rest.find('{') {
        let backslashes = rest[..rel].chars().rev().take_while(|&c| c == '\\').count();
        if backslashes % 2 == 1 {
            // Escaped brace: `\{` matches a literal `{`.
            out.push_str(&rest[..rel + 1]);
            rest = &rest[rel + 1..];
            continue;
        }
        out.push_str(&rest[..rel]);
        let tail = &rest[rel + 1..];
        match tail.find('}') {
            None => {
                // Unterminated brace: keep the rest verbatim; the regex
                // compiler reports it if it is invalid.
                out.push_str(&rest[rel..]);
                return Ok(out);
            }
            Some(end) => {
                let name = &tail[..end];
                if is_quantifier(name) {
                    out.push('{');
                    out.push_str(name);
                    out.push('}');
                } else {
                    match name {
                        "comment" => out.push_str(r"\W*"),
                        "marker" => out.push_str(marker),
                        "content" => out.push_str(r"(?<content>.*)"),
                        "" => out.push_str("{}"),
                        other => {
                            return Err(MatchConfigError::UnknownPlaceholder(other.to_string()));
                        }
                    }
                }
                rest = &tail[end + 1..];
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// Whether `{...}` is a repetition quantifier like `{2}`, `{2,}`, `{1,3}`.
fn is_quantifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ',' | ' '))
        && name.chars().any(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn match_of(config: &MatchConfig, text: &str) -> Option<String> {
        config
            .compile()
            .unwrap()
            .match_category(text)
            .map(str::to_string)
    }

    fn custom(pattern: &str) -> MatchConfig {
        MatchConfig {
            pattern: Some(pattern.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn default_config_matches_common_forms() {
        let config = MatchConfig::default();
        assert_eq!(match_of(&config, "// TODO: fix this"), Some("TODO".into()));
        assert_eq!(match_of(&config, "// TODO fix this"), Some("TODO".into()));
        assert_eq!(match_of(&config, "// FIXME: broken"), Some("FIXME".into()));
        assert_eq!(match_of(&config, "// todo"), Some("TODO".into()));
        assert_eq!(match_of(&config, "// TODO!"), Some("TODO".into()));
        assert_eq!(match_of(&config, "// TODO(fonta): x"), Some("TODO".into()));
        assert_eq!(match_of(&config, "// TODO:"), Some("TODO".into()));
        assert_eq!(
            match_of(&config, "/*\n * TODO: later\n */"),
            Some("TODO".into())
        );
        assert_eq!(match_of(&config, "/// TODO: rustdoc"), Some("TODO".into()));
        assert_eq!(match_of(&config, "# TODO: shell"), Some("TODO".into()));
        assert_eq!(match_of(&config, "//TODO: no space"), Some("TODO".into()));
    }

    #[test]
    fn default_config_rejects_doc_mentions_and_non_markers() {
        let config = MatchConfig::default();
        assert_eq!(match_of(&config, "// TODOS: plural"), None);
        assert_eq!(match_of(&config, "// NOTTODO"), None);
        assert_eq!(match_of(&config, "// plain note"), None);
        assert_eq!(match_of(&config, "// see http://TODO.example"), None);
        assert_eq!(match_of(&config, "// XTODO"), None);
        assert_eq!(match_of(&config, "// TODOX"), None);
        // The point of anchoring: doc comments merely mentioning the marker.
        assert_eq!(
            match_of(&config, "/// Triage TODO comments: scan, review"),
            None
        );
        assert_eq!(
            match_of(&config, "/// Some documentation `// TODO: example`"),
            None
        );
    }

    #[test]
    fn case_sensitivity() {
        let config = MatchConfig {
            case_sensitive: true,
            ..Default::default()
        };
        assert_eq!(match_of(&config, "// TODO: x"), Some("TODO".into()));
        assert_eq!(match_of(&config, "// todo: x"), None);
    }

    #[test]
    fn marker_capture_normalizes_case() {
        let config = MatchConfig::default();
        assert_eq!(match_of(&config, "// todo: x"), Some("TODO".into()));
        assert_eq!(match_of(&config, "// FixMe: x"), Some("FIXME".into()));
    }

    #[test]
    fn custom_pattern_user_sketch_line_anchored() {
        // The canonical custom pattern: line-anchored, so `{comment}` cannot
        // match a stray space before a mid-text marker.
        let config = custom(r"^\w*{comment}\w*{marker}\w*:\w*{content}");
        assert_eq!(match_of(&config, "// TODO: fix"), Some("TODO".into()));
        assert_eq!(match_of(&config, "// TODO: fix"), Some("TODO".into()));
        assert_eq!(match_of(&config, "# TODO: shell"), Some("TODO".into()));
        assert_eq!(match_of(&config, "// TODO fix"), None);
        assert_eq!(match_of(&config, "// TODO"), None);
        assert_eq!(match_of(&config, "/// Triage TODO comments: scan"), None);
        assert_eq!(
            match_of(&config, "/// Some documentation `// TODO: example`"),
            None
        );
    }

    #[test]
    fn custom_pattern_is_unanchored_by_default() {
        // Without a `^`, `{comment}` (`\W*`) may match the space before a
        // mid-text marker — the documented reason to anchor patterns.
        let config = custom("{comment}{marker}{content}");
        assert_eq!(
            match_of(&config, "/// Triage TODO comments: scan"),
            Some("TODO".into())
        );
        assert_eq!(match_of(&config, "// TODO: fix"), Some("TODO".into()));
    }

    #[test]
    fn custom_pattern_block_comments() {
        let config = custom(r"^{comment}{marker}:{content}");
        assert_eq!(
            match_of(&config, "/*\n * TODO: later\n */"),
            Some("TODO".into())
        );
        assert_eq!(match_of(&config, "/* TODO: inline */"), Some("TODO".into()));
    }

    #[test]
    fn custom_pattern_case_sensitivity() {
        let config = MatchConfig {
            case_sensitive: true,
            pattern: Some(r"^{comment}{marker}:{content}".into()),
            ..Default::default()
        };
        assert_eq!(match_of(&config, "// TODO: x"), Some("TODO".into()));
        assert_eq!(match_of(&config, "// todo: x"), None);
    }

    #[test]
    fn custom_pattern_metacharacter_categories() {
        let config = MatchConfig {
            categories: vec!["C(TODO".into()],
            pattern: Some(r"^{comment}{marker}:{content}".into()),
            ..Default::default()
        };
        assert_eq!(match_of(&config, "// C(TODO: x"), Some("C(TODO".into()));
        assert_eq!(match_of(&config, "// CTODO: x"), None);
    }

    #[test]
    fn custom_pattern_leaves_quantifiers_and_escapes_alone() {
        let config = custom(r"^{comment}{marker}{content}");
        assert_eq!(match_of(&config, "// TODO something"), Some("TODO".into()));
        let config = custom(r"^{comment}{marker}:{content} {2}");
        assert!(config.compile().is_ok());
        let config = custom(r"^{comment}{marker}:{content}\{escaped\}");
        assert!(config.compile().is_ok());
    }

    #[test]
    fn custom_pattern_errors() {
        let err = custom(r"{comment}{marker}:{foo}").compile().unwrap_err();
        assert!(matches!(
            err,
            MatchConfigError::UnknownPlaceholder(ref name) if name == "foo"
        ));
        assert!(err.to_string().contains("{comment}"));

        let err = custom(r"{comment}{content}").compile().unwrap_err();
        assert!(matches!(err, MatchConfigError::MissingMarkerPlaceholder));

        let err = custom(r"{comment}{marker}(").compile().unwrap_err();
        assert!(matches!(err, MatchConfigError::Regex(_)));
    }

    #[test]
    fn invalid_configs_error() {
        assert!(matches!(
            MatchConfig {
                categories: vec![],
                ..Default::default()
            }
            .compile()
            .unwrap_err(),
            MatchConfigError::NoCategories
        ));
        assert!(matches!(
            MatchConfig {
                categories: vec!["  ".into()],
                ..Default::default()
            }
            .compile()
            .unwrap_err(),
            MatchConfigError::EmptyCategory
        ));
    }
}
