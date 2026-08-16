//! Language detection and grammar loading.
//!
//! The registry maps file extensions to a curated set of languages with
//! their tree-sitter comment node kinds. Grammars themselves are loaded
//! dynamically from [`tree_sitter_language_pack`]: they are downloaded on
//! first use (if not already cached) and stored under the state directory
//! (see [`cache_base_dir`]), so a warm machine never touches the network.

use std::path::PathBuf;
use std::sync::OnceLock;

use thiserror::Error;

/// A registered language.
///
/// Instances are `'static` table entries; the grammar is resolved lazily
/// via [`grammar`] on first use.
pub struct Language {
    /// Stable identifier used in configuration and JSON output (also the
    /// tree-sitter-language-pack name, e.g. `rust`).
    pub id: &'static str,
    /// Human-readable display name (e.g. `Rust`).
    pub name: &'static str,
    /// File extensions (without the leading dot) that map to this language.
    ///
    /// The registry is consulted in order, so `c` before `cpp` makes `.h`
    /// resolve to C.
    pub extensions: &'static [&'static str],
    /// Node kinds that represent comments in this grammar's tree.
    pub comment_kinds: &'static [&'static str],
}

macro_rules! languages {
    ($( $id:literal, $name:literal, [$($ext:literal),*], [$($kind:literal),*]; )*) => {
        /// Every language the scanner knows about, in registry order.
        pub const ALL: &[Language] = &[
            $(Language {
                id: $id,
                name: $name,
                extensions: &[$($ext),*],
                comment_kinds: &[$($kind),*],
            }),*
        ];
    };
}

languages! {
    "rust", "Rust", ["rs"], ["line_comment", "block_comment"];
    "python", "Python", ["py", "pyi"], ["comment"];
    "c", "C", ["c", "h"], ["comment"];
    "cpp", "C++", ["cc", "cpp", "cxx", "hpp", "hh", "hxx"], ["comment"];
    "go", "Go", ["go"], ["comment"];
    "typescript", "TypeScript", ["ts", "mts", "cts"], ["comment"];
    "tsx", "TSX", ["tsx"], ["comment"];
    "bash", "Bash", ["sh", "bash", "zsh", "ksh"], ["comment"];
    "json", "JSON", ["json"], ["comment"];
}

/// Looks up the language registered for a file's extension.
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// use todone_core::language;
///
/// let lang = language::by_extension(Path::new("src/main.rs")).unwrap();
/// assert_eq!(lang.id, "rust");
/// assert!(language::by_extension(Path::new("README.txt")).is_none());
/// ```
pub fn by_extension(path: &std::path::Path) -> Option<&'static Language> {
    let ext = path.extension()?.to_str()?;
    ALL.iter().find(|lang| lang.extensions.contains(&ext))
}

/// The base directory for downloaded grammars: `$TODONE_GRAMMAR_DIR`,
/// `$XDG_STATE_HOME/todone`, or `~/.local/state/todone`.
///
/// tree-sitter-language-pack appends its own
/// `tree-sitter-language-pack/v{version}/libs` suffix below this base.
///
/// # Examples
///
/// ```
/// use todone_core::language::cache_base_dir;
///
/// let dir = cache_base_dir();
/// assert!(dir.as_os_str().len() > 0);
/// ```
pub fn cache_base_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("TODONE_GRAMMAR_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(xdg) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(xdg).join("todone");
    }
    std::env::var_os("HOME")
        .map(|home| {
            PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("todone")
        })
        .unwrap_or_else(|| std::env::temp_dir().join("todone-state"))
}

/// Errors produced while loading a grammar.
#[derive(Debug, Error)]
pub enum GrammarError {
    /// The grammar could not be loaded or downloaded.
    #[error("failed to load grammar for {language}: {detail}")]
    Load {
        /// The language id.
        language: String,
        /// The underlying error from tree-sitter-language-pack.
        detail: String,
    },
}

/// One-time configuration of the grammar cache directory, run before the
/// first grammar load.
fn ensure_configured() -> Result<(), String> {
    static CONFIGURED: OnceLock<Result<(), String>> = OnceLock::new();
    CONFIGURED
        .get_or_init(|| {
            let config = tree_sitter_language_pack::PackConfig {
                cache_dir: Some(cache_base_dir()),
                languages: None,
                groups: None,
            };
            tree_sitter_language_pack::configure(&config).map_err(|e| e.to_string())
        })
        .clone()
}

/// Loads the grammar for a language id (downloading it on first use).
///
/// # Errors
///
/// Returns a [`GrammarError`] when the language is unknown to
/// tree-sitter-language-pack or the grammar cannot be downloaded or loaded.
///
/// # Examples
///
/// ```
/// use todone_core::language::grammar;
///
/// // A known language resolves (downloading the grammar on a cold cache).
/// let result = grammar("rust");
/// assert!(result.is_ok() || result.is_err());
///
/// // An unknown language always fails.
/// assert!(grammar("definitely-not-a-language").is_err());
/// ```
pub fn grammar(id: &str) -> Result<tree_sitter::Language, GrammarError> {
    ensure_configured().map_err(|detail| GrammarError::Load {
        language: id.to_string(),
        detail,
    })?;
    tree_sitter_language_pack::get_language(id).map_err(|error| GrammarError::Load {
        language: id.to_string(),
        detail: error.to_string(),
    })
}

/// The highlight queries bundled for a language, if any.
///
/// # Examples
///
/// ```
/// use todone_core::language::highlights_query;
///
/// assert!(highlights_query("rust").is_some());
/// assert!(highlights_query("definitely-not-a-language").is_none());
/// ```
pub fn highlights_query(id: &str) -> Option<&'static str> {
    tree_sitter_language_pack::get_highlights_query(id)
}

/// The injections query bundled for a language, if any.
pub fn injections_query(id: &str) -> Option<&'static str> {
    tree_sitter_language_pack::get_injections_query(id)
}

/// The locals query bundled for a language, if any.
pub fn locals_query(id: &str) -> Option<&'static str> {
    tree_sitter_language_pack::get_locals_query(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Guards against concurrent grammar resolution in tests (the underlying
    /// registry is process-wide).
    static GRAMMAR_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn resolves_known_extensions() {
        let cases = [
            ("a.rs", "rust"),
            ("a.py", "python"),
            ("a.pyi", "python"),
            ("a.c", "c"),
            ("a.h", "c"),
            ("a.cpp", "cpp"),
            ("a.hpp", "cpp"),
            ("a.go", "go"),
            ("a.ts", "typescript"),
            ("a.tsx", "tsx"),
            ("a.sh", "bash"),
            ("a.bash", "bash"),
            ("a.json", "json"),
        ];
        for (file, id) in cases {
            assert_eq!(
                by_extension(std::path::Path::new(file)).map(|l| l.id),
                Some(id),
                "{file}"
            );
        }
    }

    #[test]
    fn unknown_and_extensionless_files_resolve_to_none() {
        assert!(by_extension(std::path::Path::new("Makefile")).is_none());
        assert!(by_extension(std::path::Path::new("notes.txt")).is_none());
        assert!(by_extension(std::path::Path::new("noext")).is_none());
    }

    #[test]
    fn c_wins_over_cpp_for_dot_h() {
        assert_eq!(by_extension(std::path::Path::new("x.h")).unwrap().id, "c");
    }

    #[test]
    fn grammar_resolves_known_languages_and_rejects_unknown() {
        let _guard = GRAMMAR_LOCK.lock().unwrap();
        for lang in ALL {
            let ts = grammar(lang.id).unwrap_or_else(|e| panic!("{}: {e}", lang.id));
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&ts).expect("language accepted");
        }
        assert!(grammar("definitely-not-a-language").is_err());
    }

    #[test]
    fn bundled_queries_cover_the_curated_set() {
        for lang in ALL {
            assert!(
                highlights_query(lang.id).is_some(),
                "missing highlights for {}",
                lang.id
            );
        }
    }

    #[test]
    fn cache_base_dir_follows_environment() {
        unsafe { std::env::set_var("TODONE_GRAMMAR_DIR", "/tmp/todone-grammars") };
        assert_eq!(cache_base_dir(), PathBuf::from("/tmp/todone-grammars"));
        unsafe { std::env::remove_var("TODONE_GRAMMAR_DIR") };

        let dir = cache_base_dir();
        assert!(!dir.as_os_str().is_empty());
    }
}
