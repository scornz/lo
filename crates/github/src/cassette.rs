//! Recorded GitHub responses ("cassettes"), shared by [`crate::replay`] and [`crate::record`].
//!
//! A cassette is a folder with a `cassette.json` describing it and one file per request and
//! response, named `NNN-<METHOD>-<what>.json` and replayed in name order. Requests are matched by
//! a [`key`]: the method and URL with its query sorted, and the ETag a conditional request sent;
//! for GraphQL the operation's name and its variables; for any other request with a body, a hash
//! of the body.

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
    /// The time a conditional request asked about changes since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_modified_since: Option<String>,
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

/// The key a request is matched by. A conditional request only matches the response recorded
/// for the same ETag or time, so a wrong one can't get a recorded `304`.
pub fn key(
    method: &str,
    url: &Url,
    body: &[u8],
    if_none_match: Option<&str>,
    if_modified_since: Option<&str>,
) -> String {
    let graphql = graphql_body(url, body);
    let url = normalize_url(url);
    let key = if let Some(graphql) = graphql {
        graphql_key(method, &url, &graphql.operation_name, &graphql.variables)
    } else if body.is_empty() {
        format!("{method} {url}")
    } else {
        format!("{method} {url} body={}", sha256(body))
    };
    with_conditions(key, if_none_match, if_modified_since)
}

/// The key of a recorded request.
pub fn recorded_key(request: &RecordedRequest) -> Result<String> {
    let url = Url::parse(&request.url).with_context(|| format!("bad URL {}", request.url))?;
    let normalized = normalize_url(&url);
    let key = if let Some(operation) = &request.operation {
        let variables = request.variables.clone().unwrap_or(serde_json::Value::Null);
        graphql_key(&request.method, &normalized, operation, &variables)
    } else if let Some(body) = &request.body {
        format!("{} {normalized} body={}", request.method, sha256(body))
    } else if let Some(hash) = &request.body_sha256 {
        format!("{} {normalized} body={hash}", request.method)
    } else {
        format!("{} {normalized}", request.method)
    };
    Ok(with_conditions(
        key,
        request.if_none_match.as_deref(),
        request.if_modified_since.as_deref(),
    ))
}

fn with_conditions(
    mut key: String,
    if_none_match: Option<&str>,
    if_modified_since: Option<&str>,
) -> String {
    if let Some(etag) = if_none_match {
        key = format!("{key} if-none-match={etag}");
    }
    if let Some(time) = if_modified_since {
        key = format!("{key} if-modified-since={time}");
    }
    key
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
/// tokens, and fine-grained personal access tokens, also with their underscores
/// percent-encoded, as in a URL.
static TOKEN_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"gh[opsur](?:_|%5[Ff])[A-Za-z0-9]{36,}|github(?:_|%5[Ff])pat(?:_|%5[Ff])(?:[A-Za-z0-9]|_|%5[Ff]){50,}",
    )
    .expect("valid regex")
});

/// Why `bytes` can't be written to a fixture, if it holds a token: one of `secrets`, as is,
/// percent-encoded or in base64, or anything shaped like a GitHub token.
pub fn find_secret(bytes: &[u8], secrets: &[Vec<u8>]) -> Option<&'static str> {
    let secrets: Vec<&Vec<u8>> = secrets.iter().filter(|secret| !secret.is_empty()).collect();
    let decoded = percent_decoded(bytes);
    if secrets
        .iter()
        .any(|secret| contains(bytes, secret) || contains(&decoded, secret))
    {
        return Some("the recording token");
    }
    // Base64 bodies, such as the contents API's, are broken into lines, which JSON escapes.
    let unwrapped = unwrap_lines(bytes);
    if secrets
        .iter()
        .flat_map(|secret| base64_forms(secret))
        .any(|form| contains(&unwrapped, &form))
    {
        return Some("the recording token, in base64");
    }
    if TOKEN_PATTERN.is_match(bytes) {
        return Some("something shaped like a GitHub token");
    }
    None
}

/// `bytes` with every `%XX` replaced by the byte it stands for.
fn percent_decoded(bytes: &[u8]) -> Vec<u8> {
    let hex = |byte: u8| (byte as char).to_digit(16).map(|digit| digit as u8);
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let (Some(high), Some(low)) = (
                bytes.get(index + 1).copied().and_then(hex),
                bytes.get(index + 2).copied().and_then(hex),
            )
        {
            decoded.push(high << 4 | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    decoded
}

/// `bytes` without line breaks, raw or escaped in JSON.
fn unwrap_lines(bytes: &[u8]) -> Vec<u8> {
    let mut unwrapped = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some((&byte, tail)) = rest.split_first() {
        match (byte, tail.first()) {
            (b'\\', Some(b'n' | b'r')) => rest = &tail[1..],
            (b'\n' | b'\r', _) => rest = tail,
            _ => {
                unwrapped.push(byte);
                rest = tail;
            }
        }
    }
    unwrapped
}

/// What `secret` looks like inside base64, in both alphabets, wherever it starts: the characters
/// that depend on its bytes alone, at each of the three offsets in a 3-byte group.
fn base64_forms(secret: &[u8]) -> Vec<Vec<u8>> {
    let mut forms = Vec::new();
    for offset in 0..3 {
        let mut padded = vec![0; offset];
        padded.extend_from_slice(secret);
        let encoded = base64(&padded);
        let start = (8 * offset).div_ceil(6);
        let end = 8 * (offset + secret.len()) / 6;
        let form = encoded.as_bytes()[start..end].to_vec();
        let url_safe = form
            .iter()
            .map(|byte| match byte {
                b'+' => b'-',
                b'/' => b'_',
                other => *other,
            })
            .collect();
        forms.push(form);
        forms.push(url_safe);
    }
    forms
}

/// Standard base64, with padding.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let group = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let bits = u32::from_be_bytes([0, group[0], group[1], group[2]]);
        for index in 0..4 {
            if index <= chunk.len() {
                let sextet = (bits >> (18 - 6 * index)) & 0x3f;
                encoded.push(ALPHABET[sextet as usize] as char);
            } else {
                encoded.push('=');
            }
        }
    }
    encoded
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
            key(
                "GET",
                &url("https://api.github.com/x?b=2&a=1"),
                b"",
                None,
                None
            ),
            key(
                "GET",
                &url("https://api.github.com/x?a=1&b=2"),
                b"",
                None,
                None
            ),
        );
        assert_eq!(
            key(
                "GET",
                &url("https://api.github.com/x?a=1&b=2"),
                b"",
                None,
                None
            ),
            "GET https://api.github.com/x?a=1&b=2"
        );
    }

    #[test]
    fn graphql_keys_use_the_operation_and_canonical_variables() {
        let a = br#"{"query":"query A { x }","operationName":"A","variables":{"b":1,"a":2}}"#;
        let b = br#"{"operationName":"A","variables":{"a":2,"b":1},"query":"query A {x}"}"#;
        let graphql = url("https://api.github.com/graphql");
        assert_eq!(
            key("POST", &graphql, a, None, None),
            key("POST", &graphql, b, None, None)
        );
        assert!(
            key("POST", &graphql, a, None, None)
                .starts_with("POST https://api.github.com/graphql A ")
        );
        let other = br#"{"query":"query A { x }","operationName":"A","variables":{"a":3}}"#;
        assert_ne!(
            key("POST", &graphql, a, None, None),
            key("POST", &graphql, other, None, None)
        );
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
            key("POST", &graphql, body, None, None)
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
            key("POST", &device, form.as_bytes(), None, None)
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

    #[test]
    fn base64_encodes_like_everyone_else() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xfb, 0xff]), "+/8=");
    }

    #[test]
    fn find_secret_spots_the_token_percent_encoded_or_in_base64() {
        let secret = b"gho_not-a-real-token-but-secret".to_vec();
        let secrets = std::slice::from_ref(&secret);
        // In a URL's query.
        assert!(find_secret(b"?t=gho%5Fnot-a-real-token-but-secret", secrets).is_some());
        assert!(find_secret(b"?t=gho%5fnot-a-real-token-but-secret", secrets).is_some());
        // In a file's base64 body, as the contents API returns it: at any offset, with GitHub's
        // line breaks, escaped in JSON.
        for prefix in ["", "x", "xy", "a longer prefix: "] {
            let file = format!("{prefix}token = gho_not-a-real-token-but-secret\n");
            let encoded = base64(file.as_bytes());
            let wrapped: Vec<String> = encoded
                .as_bytes()
                .chunks(60)
                .map(|line| String::from_utf8(line.to_vec()).unwrap())
                .collect();
            let json = format!(r#"{{"content":"{}"}}"#, wrapped.join("\\n"));
            assert!(find_secret(json.as_bytes(), secrets).is_some(), "{json}");
        }
        // And a token shape, percent-encoded.
        let shaped = format!("gho%5F{}", "a1".repeat(18));
        assert!(find_secret(shaped.as_bytes(), &[]).is_some());
        assert_eq!(find_secret(b"nothing to see", secrets), None);
    }
}
