//! Revisions is the generalized notion of a commit.
//!
//! This module provides utilities to walk graphs of revisions and specify revisions and ranges of revisions.

pub use git_revision as plumbing;

///
pub mod walk;
pub use walk::iter::Walk;

///
pub mod spec;

/// The specification of a revision as parsed from a revision specification like `HEAD@{1}` or `v1.2.3...main`.
/// It's typically created by [`repo.rev_parse()`][crate::Repository::rev_parse()].
///
/// See the [official git documentation](https://git-scm.com/docs/git-rev-parse#_specifying_revisions) for reference on how
/// to specify revisions and revision ranges.
#[derive(Clone, Debug)]
pub struct Spec<'repo> {
    pub(crate) inner: git_revision::Spec,
    /// The first name of a reference as seen while parsing a `RevSpec`, for completeness.
    pub(crate) first_ref: Option<git_ref::Reference>,
    /// The second name of a reference as seen while parsing a `RevSpec`, for completeness.
    pub(crate) second_ref: Option<git_ref::Reference>,
    pub(crate) repo: &'repo crate::Repository,
}
