//! A language's grammar and tags query.

use std::{
    error, fmt,
    sync::atomic::{AtomicU32, Ordering},
};

use tree_sitter::{Language, Parser};
use tree_sitter_tags::TagsConfiguration;

/// A tree-sitter grammar with the tags query that finds its definitions and references.
///
/// The caller supplies both, so the app can pass Zed's grammars with the queries in
/// [`queries`](crate::queries).
pub struct Grammar {
    /// Tells grammars apart in [`Store`](crate::Store), which keeps a blob's symbols per grammar.
    pub(crate) id: u32,
    pub(crate) tags: TagsConfiguration,
}

impl Grammar {
    /// Pairs `language` with a tags query, such as [`queries::RUST`](crate::queries::RUST) for the
    /// Rust grammar.
    ///
    /// Fails if the query doesn't fit the grammar, or the grammar was built for a tree-sitter
    /// version this one can't load.
    pub fn new(language: Language, tags_query: &str) -> Result<Self, GrammarError> {
        static NEXT_ID: AtomicU32 = AtomicU32::new(0);

        Parser::new()
            .set_language(&language)
            .map_err(|error| GrammarError(error.into()))?;
        let tags = TagsConfiguration::new(language, tags_query, "").map_err(GrammarError)?;
        Ok(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            tags,
        })
    }
}

impl fmt::Debug for Grammar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Grammar")
            .field("id", &self.id)
            .field("language", &self.tags.language.name())
            .finish_non_exhaustive()
    }
}

/// Why a grammar and tags query can't be used together.
#[derive(Debug)]
pub struct GrammarError(tree_sitter_tags::Error);

impl fmt::Display for GrammarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "can't use the tags query with this grammar: {}", self.0)
    }
}

impl error::Error for GrammarError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        Some(&self.0)
    }
}
