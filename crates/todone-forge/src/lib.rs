//! todone-forge abstracts issue-tracking backends (GitHub, GitLab, ...) so
//! that `todone port` can create issues on whatever forge the user
//! configures.
//!
//! The [`Forge`] trait is the seam; v1 ships a GitHub backend that drives
//! the `gh` CLI through a [`process::ProcessRunner`] that tests can fake.

pub mod forge;
pub mod process;

/// The version of the forge API. Bump on breaking changes to the trait.
pub const FORGE_VERSION: u32 = 1;
