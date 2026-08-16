//! todone-forge abstracts issue-tracking backends (GitHub, GitLab, ...) so
//! that `todone port` can create issues on whatever forge the user configures.

/// The version of the forge API. Bump on breaking changes to the trait.
pub const FORGE_VERSION: u32 = 1;
