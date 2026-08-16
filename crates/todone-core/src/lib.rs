//! todone-core provides the domain model and logic behind the `todone` CLI:
//! scanning repositories for marker comments with tree-sitter, matching
//! user-defined categories, removing comments from source, layered
//! configuration, and the interactive session state machine.

/// The version of the domain model. Bump on breaking changes to the public API.
pub const CORE_VERSION: u32 = 1;
