//! Category matching for comment text.
//!
//! A [`MatchConfig`] describes which marker categories a comment must
//! contain for the scanner to report it, plus the strictness of the match
//! (case sensitivity, whether a space must precede the category, and
//! whether the category must be followed by a colon).
//!
//! Matching runs against the *text of a comment node* — including its
//! marker (`//`, `#`, ...). A space requirement therefore also governs
//! `//TODO` versus `// TODO`.

use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The default categories: `TODO` and `FIXME`.
pub fn default_categories() -> Vec<String> {
    vec!["TODO".to_string(), "FIXME".to_string()]
}

const fn default_true() -> bool {
    true
}

/// Configuration for which comments count as marker comments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MatchConfig {
    /// Category names to look for, e.g. `["TODO", "FIXME"]`. At least one
    /// non-empty entry is required; categories are tested in order.
    pub categories: Vec<String>,
    /// If `false` (default), categories match case-insensitively.
    pub case_sensitive: bool,
    /// If `true` (default), the category must be preceded by whitespace (or
    /// the start of the comment), so `//TODO` does not match but
    /// `// TODO` does.
    pub require_space_before: bool,
    /// If `true`, the category must be followed by a colon
    /// (`// TODO: x`), not just whitespace.
    pub require_colon: bool,
}

impl Default for MatchConfig {
    fn default() -> Self {
        Self {
            categories: default_categories(),
            case_sensitive: false,
            require_space_before: default_true(),
            require_colon: false,
        }
    }
}

impl MatchConfig {
    /// Compiles this configuration into a [`Matcher`].
    ///
    /// # Errors
    ///
    /// Returns [`MatchConfigError::NoCategories`] when the category list is
    /// empty, [`MatchConfigError::EmptyCategory`] when an entry is blank, and
    /// [`MatchConfigError::Regex`] if a generated expression fails to
    /// compile (cannot happen with escaped input, but kept for completeness).
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

        let flags = if self.case_sensitive { "" } else { "(?i)" };
        let prefix = if self.require_space_before {
            r"(?:\s|^)"
        } else {
            r"(?:\b|^)"
        };
        let suffix = if self.require_colon {
            r"\s*:"
        } else {
            r"(?::|!|\(|\s|$)"
        };

        let mut alternation = String::new();
        let mut per_category = Vec::with_capacity(self.categories.len());
        for category in &self.categories {
            let escaped = regex::escape(category);
            if !alternation.is_empty() {
                alternation.push('|');
            }
            alternation.push_str(&escaped);
            per_category.push(Regex::new(&format!(
                "(?m){flags}{prefix}{escaped}{suffix}"
            ))?);
        }

        let combined = Regex::new(&format!("(?m){flags}{prefix}(?:{alternation}){suffix}"))?;

        Ok(Matcher {
            categories: self.categories.clone(),
            combined,
            per_category,
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
    /// A generated regular expression failed to compile.
    #[error("failed to compile category regex: {0}")]
    Regex(#[from] regex::Error),
}

/// A compiled [`MatchConfig`], cheap to match against comment text.
#[derive(Debug)]
pub struct Matcher {
    categories: Vec<String>,
    combined: Regex,
    per_category: Vec<Regex>,
}

impl Matcher {
    /// Returns the name of the first configured category matched anywhere in
    /// `text`, or `None` if the comment is not a marker comment.
    ///
    /// # Examples
    ///
    /// ```
    /// use todone_core::matcher::MatchConfig;
    ///
    /// let matcher = MatchConfig::default().compile().unwrap();
    /// assert_eq!(matcher.match_category("// TODO: handle"), Some("TODO"));
    /// assert_eq!(matcher.match_category("// TODO handle"), Some("TODO"));
    /// assert_eq!(matcher.match_category("//todo"), None);
    /// assert_eq!(matcher.match_category("// plain note"), None);
    /// ```
    pub fn match_category(&self, text: &str) -> Option<&str> {
        if !self.combined.is_match(text) {
            return None;
        }
        self.categories
            .iter()
            .zip(&self.per_category)
            .find(|(_, re)| re.is_match(text))
            .map(|(category, _)| category.as_str())
    }
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
    }

    #[test]
    fn default_config_rejects_non_markers() {
        let config = MatchConfig::default();
        assert_eq!(match_of(&config, "//TODO: no space"), None);
        assert_eq!(match_of(&config, "// TODOS: plural"), None);
        assert_eq!(match_of(&config, "// NOTTODO"), None);
        assert_eq!(match_of(&config, "// plain note"), None);
        assert_eq!(match_of(&config, "// see http://TODO.example"), None);
        assert_eq!(match_of(&config, "// XTODO"), None);
        assert_eq!(match_of(&config, "// TODOX"), None);
    }

    #[test]
    fn space_before_is_configurable() {
        let config = MatchConfig {
            require_space_before: false,
            ..Default::default()
        };
        assert_eq!(match_of(&config, "//TODO: no space"), Some("TODO".into()));
        assert_eq!(match_of(&config, "// XTODO"), None);
        assert_eq!(match_of(&config, "// TODOX"), None);
        assert_eq!(match_of(&config, "#TODO"), Some("TODO".into()));
    }

    #[test]
    fn colon_requirement() {
        let config = MatchConfig {
            require_colon: true,
            ..Default::default()
        };
        assert_eq!(match_of(&config, "// TODO: fix"), Some("TODO".into()));
        assert_eq!(match_of(&config, "// TODO fix"), None);
        assert_eq!(match_of(&config, "// TODO"), None);
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
    fn first_category_in_config_order_wins() {
        let config = MatchConfig {
            categories: vec!["PERF".into(), "TODO".into(), "FIXME".into()],
            ..Default::default()
        };
        assert_eq!(match_of(&config, "// FIXME: x"), Some("FIXME".into()));
        assert_eq!(match_of(&config, "// PERF: x"), Some("PERF".into()));
        assert_eq!(match_of(&config, "// TODO: x"), Some("TODO".into()));
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

    #[test]
    fn categories_with_regex_metacharacters_are_literal() {
        let config = MatchConfig {
            categories: vec!["C(TODO".into()],
            require_space_before: false,
            ..Default::default()
        };
        assert_eq!(match_of(&config, "// C(TODO"), Some("C(TODO".into()));
        assert_eq!(match_of(&config, "// CTODO"), None);
    }
}
