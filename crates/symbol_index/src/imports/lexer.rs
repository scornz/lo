//! Just enough of a lexer to find import statements: identifiers, strings and punctuation, with
//! comments and string contents skipped so they can't look like imports.

/// A token. Strings keep their text between the quotes, escapes and all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Token<'a> {
    Ident(&'a str),
    Str(&'a str),
    Punct(char),
}

/// A language's comments and strings.
pub(super) struct Syntax {
    /// What starts a comment to the end of the line.
    pub line_comment: &'static str,
    /// Whether `/* … */` is a comment.
    pub block_comments: bool,
    /// The characters that start and end a string.
    pub quotes: &'static [char],
    /// Those of [`Self::quotes`] whose strings may span lines. Others end with their line if
    /// unclosed, so one stray quote can't hide the rest of the file.
    pub multiline_quotes: &'static [char],
    /// Whether three quotes start a string that only three end (Python).
    pub triple_quotes: bool,
    /// Whether a `'` may start a lifetime rather than a character (Rust).
    pub lifetimes: bool,
}

pub(super) fn tokens<'a>(text: &'a str, syntax: &Syntax) -> Vec<Token<'a>> {
    let mut tokens = Vec::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if c.is_whitespace() {
            rest = &rest[c.len_utf8()..];
        } else if rest.starts_with(syntax.line_comment) {
            rest = rest.find('\n').map_or("", |end| &rest[end..]);
        } else if syntax.block_comments && rest.starts_with("/*") {
            rest = rest[2..].find("*/").map_or("", |end| &rest[end + 4..]);
        } else if c == '\'' && syntax.lifetimes {
            match char_literal_len(rest) {
                Some(len) => rest = &rest[len..],
                None => {
                    tokens.push(Token::Punct(c));
                    rest = &rest[1..];
                }
            }
        } else if syntax.quotes.contains(&c) {
            let (string, after) = string(rest, c, syntax);
            tokens.push(Token::Str(string));
            rest = after;
        } else if is_ident_char(c) {
            let end = rest.find(|c: char| !is_ident_char(c)).unwrap_or(rest.len());
            tokens.push(Token::Ident(&rest[..end]));
            rest = &rest[end..];
        } else {
            tokens.push(Token::Punct(c));
            rest = &rest[c.len_utf8()..];
        }
    }
    tokens
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The length of the Rust character, like `'a'` or `'\n'`, that starts `text`, or `None` if the
/// `'` starts a lifetime.
fn char_literal_len(text: &str) -> Option<usize> {
    let mut chars = text[1..].chars();
    match chars.next()? {
        // The escaped character comes first, so `'\''` doesn't end at its second quote.
        '\\' => text.get(3..)?.find('\'').map(|end| end + 4),
        c => (chars.next()? == '\'').then_some(c.len_utf8() + 2),
    }
}

/// The string starting `text` with `quote`, and the text after it.
fn string<'a>(text: &'a str, quote: char, syntax: &Syntax) -> (&'a str, &'a str) {
    let triple: String = [quote; 3].iter().collect();
    if syntax.triple_quotes && text.starts_with(&triple) {
        let body = &text[3..];
        return match body.find(&triple) {
            Some(end) => (&body[..end], &body[end + 3..]),
            None => (body, ""),
        };
    }
    let body = &text[1..];
    let mut escaped = false;
    for (index, c) in body.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            return (&body[..index], &body[index + 1..]);
        } else if c == '\n' && !syntax.multiline_quotes.contains(&quote) {
            return (&body[..index], &body[index..]);
        }
    }
    (body, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    const C_LIKE: Syntax = Syntax {
        line_comment: "//",
        block_comments: true,
        quotes: &['"', '\''],
        multiline_quotes: &[],
        triple_quotes: false,
        lifetimes: false,
    };

    #[test]
    fn comments_and_strings_hide_what_they_hold() {
        assert_eq!(
            tokens("a /* import b */ \"import c\" // import d\ne", &C_LIKE),
            [Token::Ident("a"), Token::Str("import c"), Token::Ident("e")]
        );
    }

    #[test]
    fn escaped_quotes_stay_in_the_string() {
        assert_eq!(
            tokens(r#"'it\'s' x"#, &C_LIKE),
            [Token::Str(r"it\'s"), Token::Ident("x")]
        );
    }

    #[test]
    fn an_unclosed_string_ends_with_its_line() {
        assert_eq!(
            tokens("\"open\nimport", &C_LIKE),
            [Token::Str("open"), Token::Ident("import")]
        );
    }

    #[test]
    fn python_triple_quoted_strings() {
        let python = Syntax {
            line_comment: "#",
            block_comments: false,
            quotes: &['"', '\''],
            multiline_quotes: &[],
            triple_quotes: true,
            lifetimes: false,
        };
        assert_eq!(
            tokens("\"\"\"a \"quoted\"\nimport b\"\"\" c # d", &python),
            [Token::Str("a \"quoted\"\nimport b"), Token::Ident("c")]
        );
    }

    #[test]
    fn rust_lifetimes_and_characters() {
        let rust = Syntax {
            line_comment: "//",
            block_comments: true,
            quotes: &['"'],
            multiline_quotes: &['"'],
            triple_quotes: false,
            lifetimes: true,
        };
        assert_eq!(
            tokens("<'a> '\"' '\\'' use", &rust),
            [
                Token::Punct('<'),
                Token::Punct('\''),
                Token::Ident("a"),
                Token::Punct('>'),
                Token::Ident("use"),
            ]
        );
    }
}
