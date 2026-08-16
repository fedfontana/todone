//! Syntax highlighting via tree-sitter, using the vendored queries under
//! `queries/`. The engine maps highlight captures to a small built-in theme
//! and emits spans of RGB colors; the renderer turns them into ANSI.

use std::collections::HashMap;
use std::ops::Range;

use thiserror::Error;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// A colored region of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSpan {
    /// Byte range within the file.
    pub range: Range<usize>,
    /// Foreground color, when the theme has one.
    pub fg: Option<(u8, u8, u8)>,
    /// Bold text.
    pub bold: bool,
    /// Italic text.
    pub italic: bool,
}

/// One theme entry: a highlight name and its color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeStyle {
    /// Foreground RGB color.
    pub fg: (u8, u8, u8),
    /// Bold text.
    pub bold: bool,
    /// Italic text.
    pub italic: bool,
}

const fn style(r: u8, g: u8, b: u8) -> ThemeStyle {
    ThemeStyle {
        fg: (r, g, b),
        bold: false,
        italic: false,
    }
}

/// The built-in theme. Names match the captures used by the vendored
/// queries; `configure` matches dot-separated names, so `keyword` also
/// covers `keyword.function`.
pub const THEME: &[(&str, ThemeStyle)] = &[
    (
        "comment",
        ThemeStyle {
            fg: (120, 129, 141),
            bold: false,
            italic: true,
        },
    ),
    ("string", style(152, 195, 121)),
    ("string.special", style(200, 160, 120)),
    ("number", style(209, 154, 102)),
    ("function", style(97, 175, 239)),
    ("function.method", style(97, 175, 239)),
    ("function.builtin", style(86, 182, 194)),
    ("function.macro", style(198, 120, 221)),
    ("type", style(229, 192, 123)),
    ("type.builtin", style(229, 192, 123)),
    ("variable", style(220, 223, 228)),
    ("variable.builtin", style(86, 182, 194)),
    (
        "variable.parameter",
        ThemeStyle {
            fg: (220, 223, 228),
            bold: false,
            italic: true,
        },
    ),
    ("constant", style(209, 154, 102)),
    ("constant.builtin", style(86, 182, 194)),
    ("keyword", style(198, 120, 221)),
    ("keyword.return", style(198, 120, 221)),
    ("keyword.operator", style(171, 178, 191)),
    ("operator", style(171, 178, 191)),
    ("punctuation", style(120, 122, 150)),
    ("punctuation.bracket", style(120, 122, 150)),
    ("punctuation.delimiter", style(120, 122, 150)),
    ("property", style(171, 178, 191)),
    ("property.builtin", style(86, 182, 194)),
    ("label", style(229, 192, 123)),
    ("namespace", style(229, 192, 123)),
    ("module", style(220, 223, 228)),
    ("builtin", style(86, 182, 194)),
    ("attribute", style(198, 120, 221)),
    ("constructor", style(229, 192, 123)),
    ("field", style(171, 178, 191)),
    ("parameter", style(220, 223, 228)),
];

/// Resolves a tree-sitter language for a language id.
pub fn language_ts(id: &str) -> Option<tree_sitter::Language> {
    todone_core::language::ALL
        .iter()
        .find(|lang| lang.id == id)
        .map(|lang| lang.ts())
}

/// Errors from the highlighting engine.
#[derive(Debug, Error)]
pub enum HighlightError {
    /// The language has no vendored highlight queries.
    #[error("no highlight queries for language {0}")]
    NoQueries(String),
    /// A vendored query failed to compile.
    #[error("failed to compile {language} highlight query: {detail}")]
    Query {
        /// The language id.
        language: String,
        /// The underlying query error.
        detail: String,
    },
    /// The highlighter failed while processing the file.
    #[error("failed to highlight {language}: {detail}")]
    Run {
        /// The language id.
        language: String,
        /// The underlying error.
        detail: String,
    },
}

/// Vendored query text for one language.
struct QuerySet {
    highlights: &'static str,
    injections: &'static str,
    locals: &'static str,
}

fn query_set(language: &str) -> Option<QuerySet> {
    let set = match language {
        "rust" => QuerySet {
            highlights: include_str!("../queries/rust/highlights.scm"),
            injections: include_str!("../queries/rust/injections.scm"),
            locals: "",
        },
        "python" => QuerySet {
            highlights: include_str!("../queries/python/highlights.scm"),
            injections: "",
            locals: "",
        },
        "c" => QuerySet {
            highlights: include_str!("../queries/c/highlights.scm"),
            injections: "",
            locals: "",
        },
        "cpp" => QuerySet {
            highlights: include_str!("../queries/cpp/highlights.scm"),
            injections: include_str!("../queries/cpp/injections.scm"),
            locals: "",
        },
        "go" => QuerySet {
            highlights: include_str!("../queries/go/highlights.scm"),
            injections: "",
            locals: "",
        },
        "bash" => QuerySet {
            highlights: include_str!("../queries/bash/highlights.scm"),
            injections: "",
            locals: "",
        },
        "json" => QuerySet {
            highlights: include_str!("../queries/json/highlights.scm"),
            injections: "",
            locals: "",
        },
        "typescript" => QuerySet {
            highlights: include_str!("../queries/typescript/highlights.scm"),
            injections: "",
            locals: "",
        },
        "tsx" => QuerySet {
            highlights: include_str!("../queries/tsx/highlights.scm"),
            injections: "",
            locals: "",
        },
        _ => return None,
    };
    Some(set)
}

/// Highlights source files with cached per-language configurations.
#[derive(Default)]
pub struct HighlightEngine {
    configs: HashMap<String, HighlightConfiguration>,
    highlighter: Highlighter,
}

impl HighlightEngine {
    /// Creates an empty engine.
    pub fn new() -> Self {
        Self::default()
    }

    /// Highlights `source` in `language`, returning all colored spans.
    ///
    /// # Errors
    ///
    /// Returns an error when the language has no vendored queries or a query
    /// fails to compile or run.
    pub fn highlight(
        &mut self,
        language: &str,
        source: &str,
    ) -> Result<Vec<HighlightSpan>, HighlightError> {
        let queries =
            query_set(language).ok_or_else(|| HighlightError::NoQueries(language.into()))?;
        if !self.configs.contains_key(language) {
            let ts =
                language_ts(language).ok_or_else(|| HighlightError::NoQueries(language.into()))?;
            let mut config = HighlightConfiguration::new(
                ts,
                language,
                queries.highlights,
                queries.injections,
                queries.locals,
            )
            .map_err(|e| HighlightError::Query {
                language: language.to_string(),
                detail: e.to_string(),
            })?;
            let names: Vec<&str> = THEME.iter().map(|(name, _)| *name).collect();
            config.configure(&names);
            self.configs.insert(language.to_string(), config);
        }
        let config = self.configs.get(language).unwrap();

        let events = self
            .highlighter
            .highlight(config, source.as_bytes(), None, |_| None)
            .map_err(|e| HighlightError::Run {
                language: language.to_string(),
                detail: e.to_string(),
            })?;

        let mut spans = Vec::new();
        let mut current: Option<HighlightSpan> = None;
        let mut range_start = 0;
        for event in events {
            let event = event.map_err(|e| HighlightError::Run {
                language: language.to_string(),
                detail: e.to_string(),
            })?;
            match event {
                HighlightEvent::Source { start, end } => {
                    if let Some(span) = current.as_mut() {
                        span.range = range_start..end;
                    }
                    range_start = end;
                    let _ = start;
                }
                HighlightEvent::HighlightStart(highlight) => {
                    if let Some(span) = current.take() {
                        spans.push(span);
                    }
                    let style = THEME
                        .get(highlight.0)
                        .map(|(_, s)| s)
                        .copied()
                        .unwrap_or(style(220, 223, 228));
                    current = Some(HighlightSpan {
                        range: range_start..range_start,
                        fg: Some(style.fg),
                        bold: style.bold,
                        italic: style.italic,
                    });
                }
                HighlightEvent::HighlightEnd => {
                    if let Some(span) = current.take() {
                        spans.push(span);
                    }
                }
            }
        }
        if let Some(span) = current.take() {
            spans.push(span);
        }
        Ok(spans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_rust_comments() {
        let mut engine = HighlightEngine::new();
        let source = "// TODO: fix me\nfn main() {\n    let x = 1;\n}\n";
        let spans = engine.highlight("rust", source).unwrap();
        assert!(!spans.is_empty());
        for span in &spans {
            assert!(span.range.start <= span.range.end);
            assert!(span.range.end <= source.len());
        }
        assert!(spans.iter().any(|s| s.fg.is_some()));
        // The comment is styled italic.
        assert!(spans.iter().any(|s| s.italic));
    }

    #[test]
    fn unknown_language_errors() {
        let mut engine = HighlightEngine::new();
        assert!(matches!(
            engine.highlight("lisp", "x").unwrap_err(),
            HighlightError::NoQueries(_)
        ));
    }

    #[test]
    fn spans_cover_only_source_bounds() {
        let mut engine = HighlightEngine::new();
        let source = "let a = 1;\nlet b = 2;\n";
        let spans = engine.highlight("typescript", source).unwrap();
        assert!(spans.iter().all(|s| s.range.end <= source.len()));
    }

    #[test]
    fn all_languages_have_vendored_queries() {
        for lang in todone_core::language::ALL {
            assert!(
                query_set(lang.id).is_some(),
                "missing queries for {}",
                lang.id
            );
        }
    }
}
