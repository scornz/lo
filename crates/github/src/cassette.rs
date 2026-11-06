//! Recorded GitHub responses ("cassettes"), shared by [`crate::replay`] and [`crate::record`].
//!
//! A cassette is a folder with a `cassette.json` describing it and one file per request and
//! response, named `NNN-<METHOD>-<what>.json` and replayed in name order. Requests are matched by
//! a [`key`]: the method and URL with its query sorted; for GraphQL the operation's name and its
//! variables; for any other request with a body, a hash of the body.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context as _, Result};
use http_client::Url;
use regex::bytes::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// The folder holding the github crate's cassettes.
pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// `cassette.json`.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct CassetteInfo {
    /// What the cassette shows.
    pub description: String,
    /// Hand-written rather than recorded.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub synthetic: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The `github/docs` commit the vendored GraphQL schema came from, when it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
}

/// One request and the response GitHub gave it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Interaction {
    pub request: RecordedRequest,
    pub response: RecordedResponse,
}

/// What identifies a request. Its headers are never stored, so a token can't end up here.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RecordedRequest {
    pub method: String,
    pub url: String,
    /// The GraphQL operation's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// The GraphQL operation's variables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables: Option<serde_json::Value>,
    /// A hash of the GraphQL query's text, so a changed query fails with "re-record".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_sha256: Option<String>,
    /// A hash of any other request body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_sha256: Option<String>,
    /// The body itself, for synthetic cassettes, in place of `body_sha256`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// The ETag the request sent, for conditional requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_none_match: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecordedResponse {
    pub status: u16,
    /// Only [`KEPT_HEADERS`], lowercased.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// The body, when it's JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
    /// The body, when it isn't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_text: Option<String>,
}

impl RecordedResponse {
    pub fn body_bytes(&self) -> Vec<u8> {
        if let Some(body) = &self.body {
            serde_json::to_vec(body).expect("a JSON value serializes")
        } else if let Some(text) = &self.body_text {
            text.as_bytes().to_vec()
        } else {
            Vec::new()
        }
    }
}

/// The response headers a cassette keeps. Everything else, such as cookies and request IDs, is
/// dropped when recording.
pub const KEPT_HEADERS: &[&str] = &[
    "content-type",
    "etag",
    "last-modified",
    "link",
    "location",
    "retry-after",
    "x-accepted-oauth-scopes",
    "x-github-api-version-selected",
    "x-github-sso",
    "x-oauth-scopes",
    "x-poll-interval",
    "x-ratelimit-limit",
    "x-ratelimit-remaining",
    "x-ratelimit-reset",
    "x-ratelimit-resource",
    "x-ratelimit-used",
];

/// What a GraphQL request body holds, as `graphql_client` builds it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphQlBody {
    pub query: String,
    pub operation_name: String,
    #[serde(default)]
    pub variables: serde_json::Value,
}

/// Parses a request body as a GraphQL operation, if it is one.
pub fn graphql_body(url: &Url, body: &[u8]) -> Option<GraphQlBody> {
    if url.path() != "/graphql" {
        return None;
    }
    serde_json::from_slice(body).ok()
}

/// The key a request is matched by.
pub fn key(method: &str, url: &Url, body: &[u8]) -> String {
    let graphql = graphql_body(url, body);
    let url = normalize_url(url);
    if let Some(graphql) = graphql {
        return graphql_key(method, &url, &graphql.operation_name, &graphql.variables);
    }
    if body.is_empty() {
        format!("{method} {url}")
    } else {
        format!("{method} {url} body={}", sha256(body))
    }
}

/// The key of a recorded request.
pub fn recorded_key(request: &RecordedRequest) -> Result<String> {
    let url = Url::parse(&request.url).with_context(|| format!("bad URL {}", request.url))?;
    let normalized = normalize_url(&url);
    Ok(if let Some(operation) = &request.operation {
        let variables = request.variables.clone().unwrap_or(serde_json::Value::Null);
        graphql_key(&request.method, &normalized, operation, &variables)
    } else if let Some(body) = &request.body {
        format!("{} {normalized} body={}", request.method, sha256(body))
    } else if let Some(hash) = &request.body_sha256 {
        format!("{} {normalized} body={hash}", request.method)
    } else {
        format!("{} {normalized}", request.method)
    })
}

fn graphql_key(method: &str, url: &str, operation: &str, variables: &serde_json::Value) -> String {
    let variables = canonical_json(variables);
    format!(
        "{method} {url} {operation} variables={}",
        &sha256(variables.as_bytes())[..16]
    )
}

/// The URL with its query pairs sorted, so their order doesn't matter.
fn normalize_url(url: &Url) -> String {
    let mut url = url.clone();
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if pairs.is_empty() {
        url.set_query(None);
    } else {
        pairs.sort();
        url.query_pairs_mut().clear().extend_pairs(pairs);
    }
    url.set_fragment(None);
    url.to_string()
}

/// JSON with every object's keys sorted. Zed's workspace turns on serde_json's `preserve_order`,
/// so serializing a `Value` keeps insertion order and isn't canonical by itself.
pub fn canonical_json(value: &serde_json::Value) -> String {
    fn sorted(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let mut entries: Vec<_> = map.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                serde_json::Value::Object(
                    entries
                        .into_iter()
                        .map(|(k, v)| (k.clone(), sorted(v)))
                        .collect(),
                )
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(sorted).collect())
            }
            other => other.clone(),
        }
    }
    sorted(value).to_string()
}

pub fn sha256(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}

/// Reads a cassette's interactions, in file-name order.
pub fn read_cassette(dir: &Path) -> Result<(CassetteInfo, Vec<(String, Interaction)>)> {
    let info_path = dir.join("cassette.json");
    let info: CassetteInfo = serde_json::from_slice(
        &std::fs::read(&info_path).with_context(|| format!("reading {}", info_path.display()))?,
    )
    .with_context(|| format!("parsing {}", info_path.display()))?;
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .with_context(|| format!("listing {}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".json") && name != "cassette.json")
        .collect();
    names.sort();
    let mut interactions = Vec::new();
    for name in names {
        let path = dir.join(&name);
        let interaction: Interaction = serde_json::from_slice(&std::fs::read(&path)?)
            .with_context(|| format!("parsing {}", path.display()))?;
        interactions.push((name, interaction));
    }
    Ok((info, interactions))
}

/// Patterns of GitHub tokens: OAuth, personal, user-to-server, server-to-server and refresh
/// tokens, and fine-grained personal access tokens.
static TOKEN_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"gh[opsur]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{50,}").expect("valid regex")
});

/// Why `bytes` can't be written to a fixture, if it holds a token: one of `secrets`, or anything
/// shaped like a GitHub token.
pub fn find_secret(bytes: &[u8], secrets: &[Vec<u8>]) -> Option<&'static str> {
    if secrets
        .iter()
        .any(|secret| !secret.is_empty() && contains(bytes, secret))
    {
        return Some("the recording token");
    }
    if TOKEN_PATTERN.is_match(bytes) {
        return Some("something shaped like a GitHub token");
    }
    None
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn query_order_does_not_change_the_key() {
        assert_eq!(
            key("GET", &url("https://api.github.com/x?b=2&a=1"), b""),
            key("GET", &url("https://api.github.com/x?a=1&b=2"), b""),
        );
        assert_eq!(
            key("GET", &url("https://api.github.com/x?a=1&b=2"), b""),
            "GET https://api.github.com/x?a=1&b=2"
        );
    }

    #[test]
    fn graphql_keys_use_the_operation_and_canonical_variables() {
        let a = br#"{"query":"query A { x }","operationName":"A","variables":{"b":1,"a":2}}"#;
        let b = br#"{"operationName":"A","variables":{"a":2,"b":1},"query":"query A {x}"}"#;
        let graphql = url("https://api.github.com/graphql");
        assert_eq!(key("POST", &graphql, a), key("POST", &graphql, b));
        assert!(key("POST", &graphql, a).starts_with("POST https://api.github.com/graphql A "));
        let other = br#"{"query":"query A { x }","operationName":"A","variables":{"a":3}}"#;
        assert_ne!(key("POST", &graphql, a), key("POST", &graphql, other));
    }

    #[test]
    fn a_recorded_request_has_the_same_key_as_the_live_one() {
        let body = br#"{"query":"query A { x }","operationName":"A","variables":{"n":1}}"#;
        let graphql = url("https://api.github.com/graphql");
        let recorded = RecordedRequest {
            method: "POST".into(),
            url: graphql.to_string(),
            operation: Some("A".into()),
            variables: Some(serde_json::json!({"n": 1})),
            ..Default::default()
        };
        assert_eq!(
            recorded_key(&recorded).unwrap(),
            key("POST", &graphql, body)
        );

        let form = "client_id=abc&scope=repo";
        let device = url("https://github.com/login/device/code");
        let recorded = RecordedRequest {
            method: "POST".into(),
            url: device.to_string(),
            body: Some(form.into()),
            ..Default::default()
        };
        assert_eq!(
            recorded_key(&recorded).unwrap(),
            key("POST", &device, form.as_bytes())
        );
    }

    #[test]
    fn canonical_json_sorts_nested_keys() {
        let value = serde_json::json!({"b": {"y": 1, "x": [ {"d": 1, "c": 2} ]}, "a": null});
        assert_eq!(
            canonical_json(&value),
            r#"{"a":null,"b":{"x":[{"c":2,"d":1}],"y":1}}"#
        );
    }

    #[test]
    fn find_secret_spots_the_token_and_token_shapes() {
        let secret = b"not-a-real-token-but-secret".to_vec();
        assert_eq!(
            find_secret(
                b"xx not-a-real-token-but-secret yy",
                std::slice::from_ref(&secret)
            ),
            Some("the recording token")
        );
        let shaped = format!("\"gho_{}\"", "a1".repeat(18));
        assert_eq!(
            find_secret(shaped.as_bytes(), &[]),
            Some("something shaped like a GitHub token")
        );
        let pat = format!("github_pat_{}", "A_".repeat(30));
        assert!(find_secret(pat.as_bytes(), &[]).is_some());
        assert_eq!(find_secret(b"gho_FAKE short", &[secret]), None);
    }
}
