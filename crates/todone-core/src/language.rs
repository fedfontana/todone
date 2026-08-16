//! Language detection and the tree-sitter grammar registry.
//!
//! Each language knows the file extensions that map to it and the
//! tree-sitter node kinds that represent comments. The registry is a
//! curated, fixed set for v1; unknown languages are reported as skipped
//! rather than guessed at.

use std::path::Path;

use tree_sitter_language::LanguageFn;

/// A registered language backed by a tree-sitter grammar.
///
/// Instances are `'static` table entries; all fields are fixed at compile
/// time.
pub struct Language {
    /// Stable identifier used in configuration and JSON output (e.g. `rust`).
    pub id: &'static str,
    /// Human-readable display name (e.g. `Rust`).
    pub name: &'static str,
    /// File extensions (without the leading dot) that map to this language.
    ///
    /// The registry is consulted in order, so `c` before `cpp` makes `.h`
    /// resolve to C.
    pub extensions: &'static [&'static str],
    /// The tree-sitter grammar.
    pub grammar: LanguageFn,
    /// Node kinds that represent comments in this grammar's tree.
    pub comment_kinds: &'static [&'static str],
}

impl Language {
    /// Converts the grammar into a `tree_sitter::Language`.
    pub fn ts(&self) -> tree_sitter::Language {
        self.grammar.into()
    }
}

macro_rules! languages {
    ($( $id:literal, $name:literal, [$($ext:literal),*], $grammar:expr, [$($kind:literal),*]; )*) => {
        /// Every language the scanner knows about, in registry order.
        pub const ALL: &[Language] = &[
            $(Language {
                id: $id,
                name: $name,
                extensions: &[$($ext),*],
                grammar: $grammar,
                comment_kinds: &[$($kind),*],
            }),*
        ];
    };
}

languages! {
    "rust", "Rust", ["rs"], tree_sitter_rust::LANGUAGE, ["line_comment", "block_comment"];
    "python", "Python", ["py", "pyi"], tree_sitter_python::LANGUAGE, ["comment"];
    "c", "C", ["c", "h"], tree_sitter_c::LANGUAGE, ["comment"];
    "cpp", "C++", ["cc", "cpp", "cxx", "hpp", "hh", "hxx"], tree_sitter_cpp::LANGUAGE, ["comment"];
    "go", "Go", ["go"], tree_sitter_go::LANGUAGE, ["comment"];
    "typescript", "TypeScript", ["ts", "mts", "cts"], tree_sitter_typescript::LANGUAGE_TYPESCRIPT, ["comment"];
    "tsx", "TSX", ["tsx"], tree_sitter_typescript::LANGUAGE_TSX, ["comment"];
    "bash", "Bash", ["sh", "bash", "zsh", "ksh"], tree_sitter_bash::LANGUAGE, ["comment"];
    "json", "JSON", ["json"], tree_sitter_json::LANGUAGE, ["comment"];
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
pub fn by_extension(path: &Path) -> Option<&'static Language> {
    let ext = path.extension()?.to_str()?;
    ALL.iter().find(|lang| lang.extensions.contains(&ext))
}

#[cfg(test)]
mod tests {
    use super::*;

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
                by_extension(Path::new(file)).map(|l| l.id),
                Some(id),
                "{file}"
            );
        }
    }

    #[test]
    fn unknown_and_extensionless_files_resolve_to_none() {
        assert!(by_extension(Path::new("Makefile")).is_none());
        assert!(by_extension(Path::new("notes.txt")).is_none());
        assert!(by_extension(Path::new("noext")).is_none());
    }

    #[test]
    fn c_wins_over_cpp_for_dot_h() {
        assert_eq!(by_extension(Path::new("x.h")).unwrap().id, "c");
    }
}
