//! Parse `.gitattribute` and `.gitignore` files and provide utilities to match against them.
//!
//! ## Feature Flags
#![cfg_attr(
    feature = "document-features",
    cfg_attr(doc, doc = ::document_features::document_features!())
)]
#![forbid(unsafe_code)]
#![deny(rust_2018_idioms, missing_docs)]

use std::path::PathBuf;

use bstr::{BStr, BString};
use compact_str::CompactString;
pub use git_glob as glob;

mod assignment;
///
pub mod name;
mod state;

mod match_group;
pub use match_group::{Attributes, Ignore, Match, Pattern};

///
pub mod parse;
/// Parse attribute assignments line by line from `bytes`.
pub fn parse(bytes: &[u8]) -> parse::Lines<'_> {
    parse::Lines::new(bytes)
}

/// The state an attribute can be in, referencing the value.
///
/// Note that this doesn't contain the name.
#[derive(PartialEq, Eq, Debug, Hash, Ord, PartialOrd, Clone, Copy)]
#[cfg_attr(feature = "serde1", derive(serde::Serialize, serde::Deserialize))]
pub enum StateRef<'a> {
    /// The attribute is listed, or has the special value 'true'
    Set,
    /// The attribute has the special value 'false', or was prefixed with a `-` sign.
    Unset,
    /// The attribute is set to the given value, which followed the `=` sign.
    /// Note that values can be empty.
    #[cfg_attr(feature = "serde1", serde(borrow))]
    Value(&'a BStr),
    /// The attribute isn't mentioned with a given path or is explicitly set to `Unspecified` using the `!` sign.
    Unspecified,
}

/// The state an attribute can be in, owning the value.
///
/// Note that this doesn't contain the name.
#[derive(PartialEq, Eq, Debug, Hash, Ord, PartialOrd, Clone)]
#[cfg_attr(feature = "serde1", derive(serde::Serialize, serde::Deserialize))]
pub enum State {
    /// The attribute is listed, or has the special value 'true'
    Set,
    /// The attribute has the special value 'false', or was prefixed with a `-` sign.
    Unset,
    /// The attribute is set to the given value, which followed the `=` sign.
    /// Note that values can be empty.
    Value(CompactString), // TODO: use `kstring`, maybe it gets a binary string soon, needs binary, too, no UTF8 is required for attr values
    /// The attribute isn't mentioned with a given path or is explicitly set to `Unspecified` using the `!` sign.
    Unspecified,
}

/// Represents a validated attribute name
#[derive(PartialEq, Eq, Debug, Hash, Ord, PartialOrd, Clone)]
#[cfg_attr(feature = "serde1", derive(serde::Serialize, serde::Deserialize))]
pub struct Name(pub(crate) CompactString);

/// Holds a validated attribute name as a reference
#[derive(PartialEq, Eq, Debug, Hash, Ord, PartialOrd)]
pub struct NameRef<'a>(&'a str);

/// Name an attribute and describe it's assigned state.
#[derive(PartialEq, Eq, Debug, Hash, Ord, PartialOrd, Clone)]
#[cfg_attr(feature = "serde1", derive(serde::Serialize, serde::Deserialize))]
pub struct Assignment {
    /// The validated name of the attribute.
    pub name: Name,
    /// The state of the attribute.
    pub state: State,
}

/// Holds validated attribute data as a reference
#[derive(PartialEq, Eq, Debug, Hash, Ord, PartialOrd)]
pub struct AssignmentRef<'a> {
    /// The name of the attribute.
    pub name: NameRef<'a>,
    /// The state of the attribute.
    pub state: StateRef<'a>,
}

/// A grouping of lists of patterns while possibly keeping associated to their base path.
///
/// Pattern lists with base path are queryable relative to that base, otherwise they are relative to the repository root.
#[derive(PartialEq, Eq, Debug, Hash, Ord, PartialOrd, Clone, Default)]
pub struct MatchGroup<T: Pattern = Attributes> {
    /// A list of pattern lists, each representing a patterns from a file or specified by hand, in the order they were
    /// specified in.
    ///
    /// During matching, this order is reversed.
    pub patterns: Vec<PatternList<T>>,
}

/// A list of patterns which optionally know where they were loaded from and what their base is.
///
/// Knowing their base which is relative to a source directory, it will ignore all path to match against
/// that don't also start with said base.
#[derive(PartialEq, Eq, Debug, Hash, Ord, PartialOrd, Clone, Default)]
pub struct PatternList<T: Pattern> {
    /// Patterns and their associated data in the order they were loaded in or specified,
    /// the line number in its source file or its sequence number (_`(pattern, value, line_number)`_).
    ///
    /// During matching, this order is reversed.
    pub patterns: Vec<PatternMapping<T::Value>>,

    /// The path from which the patterns were read, or `None` if the patterns
    /// don't originate in a file on disk.
    pub source: Option<PathBuf>,

    /// The parent directory of source, or `None` if the patterns are _global_ to match against the repository root.
    /// It's processed to contain slashes only and to end with a trailing slash, and is relative to the repository root.
    pub base: Option<BString>,
}

/// An association of a pattern with its value, along with a sequence number providing a sort order in relation to its peers.
#[derive(PartialEq, Eq, Debug, Hash, Ord, PartialOrd, Clone)]
pub struct PatternMapping<T> {
    /// The pattern itself, like `/target/*`
    pub pattern: git_glob::Pattern,
    /// The value associated with the pattern.
    pub value: T,
    /// Typically the line number in the file the pattern was parsed from.
    pub sequence_number: usize,
}
