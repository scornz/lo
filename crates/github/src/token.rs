//! A GitHub token held in memory.

use std::fmt;

/// A GitHub token. Its `Debug` hides the value and it has no `Display`, so it can't end up in a
/// log line or an error message by accident. [`Token::expose`] is the one way to read it.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// Wraps a token, trimming the whitespace a file or a command's output leaves around it.
    pub fn new(token: impl Into<String>) -> Self {
        let token = token.into();
        let trimmed = token.trim();
        if trimmed.len() == token.len() {
            Self(token)
        } else {
            Self(trimmed.to_string())
        }
    }

    /// The token itself, for an `Authorization` header or the credential store.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// What kind of token this is, from its prefix.
    pub fn kind(&self) -> TokenKind {
        TokenKind::of(&self.0)
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Token({:?}, redacted)", self.kind())
    }
}

/// GitHub's token types, told apart by prefix
/// ([GitHub's token formats](https://github.blog/engineering/platform-security/behind-githubs-new-authentication-token-formats/)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    /// `gho_`: an OAuth app's token, which is what Lo's own sign-in and `gh` produce.
    OAuth,
    /// `ghp_`: a classic personal access token.
    Personal,
    /// `github_pat_`: a fine-grained personal access token.
    FineGrained,
    /// `ghu_`: a GitHub App's user token. GitHub refuses these for attachment uploads.
    GitHubApp,
    Unknown,
}

impl TokenKind {
    fn of(token: &str) -> Self {
        if token.starts_with("gho_") {
            Self::OAuth
        } else if token.starts_with("ghp_") {
            Self::Personal
        } else if token.starts_with("github_pat_") {
            Self::FineGrained
        } else if token.starts_with("ghu_") {
            Self::GitHubApp
        } else {
            Self::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_token() {
        let token = Token::new("gho_secretsecretsecret");
        let debug = format!("{token:?}");
        assert!(!debug.contains("secret"), "{debug}");
        assert_eq!(debug, "Token(OAuth, redacted)");
    }

    #[test]
    fn new_trims_whitespace() {
        assert_eq!(Token::new("  gho_abc\n").expose(), "gho_abc");
        assert_eq!(Token::new("gho_abc").expose(), "gho_abc");
    }

    #[test]
    fn kind_comes_from_the_prefix() {
        assert_eq!(Token::new("gho_x").kind(), TokenKind::OAuth);
        assert_eq!(Token::new("ghp_x").kind(), TokenKind::Personal);
        assert_eq!(Token::new("github_pat_x").kind(), TokenKind::FineGrained);
        assert_eq!(Token::new("ghu_x").kind(), TokenKind::GitHubApp);
        assert_eq!(Token::new("x").kind(), TokenKind::Unknown);
    }
}
