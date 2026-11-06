//! One file's definitions and references, read with a grammar's tags query.

use std::{cmp::Reverse, collections::HashMap, ops::Range, sync::Arc};

use tree_sitter_tags::TagsContext;

use crate::Grammar;

/// The definitions and references in one file's contents.
///
/// It depends only on the contents and the grammar, not on the file's path, so a [`Store`]
/// keeps one per blob and every commit holding that blob shares it.
///
/// [`Store`]: crate::Store
#[derive(Debug, Default)]
pub struct FileSymbols {
    /// In the order of their names in the file.
    definitions: Vec<Definition>,
    /// In the order of their names in the file.
    references: Vec<Reference>,
}

/// A definition of a name: a function, type, method, constant and so on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Definition {
    pub name: Arc<str>,
    /// What the tags query calls it: the part after `definition.` in its capture, such as
    /// `function`, `method`, `class`, `interface`, `module`, `macro`, `constant` or `type`.
    pub kind: Arc<str>,
    /// The bytes of the whole definition, from the start of its syntax node to the end.
    pub range: Range<usize>,
    /// The bytes of its name.
    pub name_range: Range<usize>,
    /// Where its name starts.
    pub position: Position,
    /// The innermost other definition whose range contains this one's, as an index into
    /// [`FileSymbols::definitions`]. A method's is its class, but in Rust an `impl` block isn't a
    /// definition, so a method in one has none.
    pub parent: Option<usize>,
}

/// A use of a name: a call, a type annotation, an `impl` and so on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub name: Arc<str>,
    /// What the tags query calls it: the part after `reference.` in its capture, such as `call`,
    /// `type`, `class` or `implementation`.
    pub kind: Arc<str>,
    /// The bytes of the name.
    pub range: Range<usize>,
    /// Where the name starts.
    pub position: Position,
    /// The innermost definition whose range contains it, as an index into
    /// [`FileSymbols::definitions`].
    pub enclosing: Option<usize>,
}

/// A place in a file: a zero-based row, and the column in bytes and in UTF-16 code units (what
/// language servers count by default).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Position {
    pub row: u32,
    pub column: u32,
    pub utf16_column: u32,
}

impl FileSymbols {
    /// Reads `source`'s definitions and references with `grammar`'s tags query.
    ///
    /// A file with syntax errors keeps whatever the query still finds in it.
    pub fn read(grammar: &Grammar, source: &[u8]) -> Self {
        Self::read_with(&mut TagsContext::new(), grammar, source)
    }

    /// [`Self::read`], reusing a parser.
    pub(crate) fn read_with(context: &mut TagsContext, grammar: &Grammar, source: &[u8]) -> Self {
        // Generating tags fails only if the grammar can't load, which `Grammar::new` checked, or
        // if it's cancelled, which nothing here does.
        let Ok((tags, _has_error)) = context.generate_tags(&grammar.tags, source, None) else {
            return Self::default();
        };

        let mut strings = Strings::default();
        let mut definitions = Vec::new();
        let mut references = Vec::new();
        for tag in tags.flatten() {
            let name = strings.get(&source[tag.name_range.clone()]);
            let kind = strings.get(grammar.tags.syntax_type_name(tag.syntax_type_id).as_bytes());
            let position = Position {
                row: to_u32(tag.span.start.row),
                column: to_u32(tag.span.start.column),
                utf16_column: to_u32(tag.utf16_column_range.start),
            };
            if tag.is_definition {
                definitions.push(Definition {
                    name,
                    kind,
                    range: tag.range,
                    name_range: tag.name_range,
                    position,
                    parent: None,
                });
            } else {
                references.push(Reference {
                    name,
                    kind,
                    range: tag.name_range,
                    position,
                    enclosing: None,
                });
            }
        }
        link(&mut definitions, &mut references);

        Self {
            definitions,
            references,
        }
    }

    /// The file's definitions, in the order their names appear.
    pub fn definitions(&self) -> &[Definition] {
        &self.definitions
    }

    /// The file's references, in the order they appear.
    pub fn references(&self) -> &[Reference] {
        &self.references
    }

    /// The definition that contains `definition`, one of this file's.
    pub fn parent(&self, definition: &Definition) -> Option<&Definition> {
        definition.parent.map(|index| &self.definitions[index])
    }

    /// The definition that contains `reference`, one of this file's.
    pub fn enclosing(&self, reference: &Reference) -> Option<&Definition> {
        reference.enclosing.map(|index| &self.definitions[index])
    }
}

/// Sets each definition's parent and each reference's enclosing definition.
///
/// Definition ranges are syntax nodes, so any two are nested or apart. Walking them by where they
/// start, outermost first, keeps a stack of the ones still open, each inside the one below it.
fn link(definitions: &mut [Definition], references: &mut [Reference]) {
    let mut order: Vec<usize> = (0..definitions.len()).collect();
    order.sort_by_key(|&index| {
        let range = &definitions[index].range;
        (range.start, Reverse(range.end))
    });
    references.sort_by_key(|reference| reference.range.start);

    let mut open = Vec::new();
    let mut next_reference = 0;
    for index in order {
        let range = definitions[index].range.clone();
        // A reference starting where a definition does is inside it, so it waits.
        while let Some(reference) = references
            .get_mut(next_reference)
            .filter(|reference| reference.range.start < range.start)
        {
            reference.enclosing = innermost(&mut open, definitions, &reference.range);
            next_reference += 1;
        }
        definitions[index].parent = innermost(&mut open, definitions, &range);
        open.push(index);
    }
    for reference in &mut references[next_reference..] {
        reference.enclosing = innermost(&mut open, definitions, &reference.range);
    }
}

/// The innermost of the `open` definitions that contains `range`, given that each starts at or
/// before it. Closes the ones that end before it does, since nothing after it is inside them either.
/// A definition with the very same range isn't its parent: Go's `const a, b = 1, 2` defines two
/// names with one node.
fn innermost(
    open: &mut Vec<usize>,
    definitions: &[Definition],
    range: &Range<usize>,
) -> Option<usize> {
    while open
        .last()
        .is_some_and(|&top| definitions[top].range.end < range.end)
    {
        open.pop();
    }
    open.iter()
        .rev()
        .copied()
        .find(|&index| definitions[index].range != *range)
}

/// Shares one allocation between every use of a name, or a kind, in a file.
#[derive(Default)]
struct Strings<'a>(HashMap<&'a [u8], Arc<str>>);

impl<'a> Strings<'a> {
    fn get(&mut self, bytes: &'a [u8]) -> Arc<str> {
        self.0
            .entry(bytes)
            .or_insert_with(|| String::from_utf8_lossy(bytes).into())
            .clone()
    }
}

/// Rows and columns of any file worth indexing fit in a `u32`.
fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}
