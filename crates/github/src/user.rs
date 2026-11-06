//! The signed-in user, and the scopes rozed needs.

use serde::Deserialize;

/// The scopes rozed asks for (design §3): `repo` to read, review and merge private repos,
/// `read:org` for team review requests and code owners, and `workflow` because GitHub refuses to
/// merge a PR that changes `.github/workflows` without it.
pub const SCOPES: &[&str] = &["repo", "read:org", "workflow"];

/// A GitHub user, from `GET /user`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct User {
    pub login: String,
    pub id: u64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

/// The user a token belongs to, and the scopes GitHub says it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedUser {
    pub user: User,
    pub scopes: Vec<String>,
}

impl AuthenticatedUser {
    /// The scopes rozed needs that the token lacks.
    pub fn missing_scopes(&self) -> Vec<&'static str> {
        missing_scopes(&self.scopes)
    }
}

/// Parses `x-oauth-scopes`, e.g. `read:org, repo, workflow`.
pub fn parse_scopes(header: Option<&str>) -> Vec<String> {
    header
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .map(str::to_string)
        .collect()
}

/// The scopes in [`SCOPES`] that `scopes` doesn't grant. A broader scope grants the narrower
/// one: `admin:org` and `write:org` include `read:org`.
pub fn missing_scopes(scopes: &[String]) -> Vec<&'static str> {
    let has = |scope: &str| scopes.iter().any(|s| s == scope);
    SCOPES
        .iter()
        .copied()
        .filter(|needed| match *needed {
            "read:org" => !(has("read:org") || has("write:org") || has("admin:org")),
            scope => !has(scope),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_scopes_header() {
        assert_eq!(
            parse_scopes(Some("read:org, repo, workflow")),
            ["read:org", "repo", "workflow"]
        );
        assert!(parse_scopes(Some("")).is_empty());
        assert!(parse_scopes(None).is_empty());
    }

    #[test]
    fn finds_missing_scopes() {
        let scopes = |s: &str| parse_scopes(Some(s));
        assert!(missing_scopes(&scopes("read:org, repo, workflow")).is_empty());
        assert_eq!(
            missing_scopes(&scopes("gist, read:org, repo")),
            ["workflow"]
        );
        assert!(missing_scopes(&scopes("admin:org, repo, workflow")).is_empty());
        assert_eq!(
            missing_scopes(&scopes("")),
            ["repo", "read:org", "workflow"]
        );
    }
}
