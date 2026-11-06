//! An HTTP client that records GitHub's responses into a cassette, keeping tokens out.
//!
//! Request headers are never stored, only the method, URL and what identifies the body. Response
//! headers are kept only from [`cassette::KEPT_HEADERS`], and query values that grant access,
//! such as signed URLs' signatures, are redacted. Before each file is written, and again over the
//! whole cassette at the end, the recorder checks for the token, as is, percent-encoded or in
//! base64, and for anything shaped like a GitHub token; if it finds one, it deletes the cassette
//! and fails.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail};
use futures::AsyncReadExt as _;
use futures::future::BoxFuture;
use http_client::http::HeaderValue;
use http_client::{AsyncBody, HttpClient, Request, Response, Url};
use parking_lot::Mutex;
use regex::Regex;

use crate::cassette::{
    self, CassetteInfo, Interaction, KEPT_HEADERS, RecordedRequest, RecordedResponse,
};

/// Wraps a real HTTP client, writing each request and response to a cassette and returning the
/// response unchanged.
#[derive(Clone)]
pub struct RecordingHttpClient(Arc<Recorder>);

struct Recorder {
    inner: Arc<dyn HttpClient>,
    dir: PathBuf,
    secrets: Vec<Vec<u8>>,
    state: Mutex<RecorderState>,
}

#[derive(Default)]
struct RecorderState {
    count: usize,
    refused: Option<String>,
}

impl RecordingHttpClient {
    /// Records into `dir`, which is emptied first. `secrets` are byte strings that must never be
    /// written, such as the token the requests use.
    pub fn new(inner: Arc<dyn HttpClient>, dir: &Path, secrets: Vec<Vec<u8>>) -> Result<Self> {
        if dir.exists() {
            std::fs::remove_dir_all(dir).with_context(|| format!("emptying {}", dir.display()))?;
        }
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self(Arc::new(Recorder {
            inner,
            dir: dir.to_path_buf(),
            secrets,
            state: Mutex::default(),
        })))
    }

    /// Writes `cassette.json` and checks every file once more. On any failure the cassette is
    /// deleted.
    pub fn finish(&self, info: CassetteInfo) -> Result<()> {
        let result = self.finish_inner(info);
        if result.is_err() {
            std::fs::remove_dir_all(&self.0.dir).ok();
        }
        result
    }

    fn finish_inner(&self, info: CassetteInfo) -> Result<()> {
        if let Some(reason) = self.0.state.lock().refused.clone() {
            bail!("refused to record: {reason}");
        }
        let json = serde_json::to_string_pretty(&info)? + "\n";
        self.write_checked("cassette.json", json.as_bytes())?;
        for entry in std::fs::read_dir(&self.0.dir)? {
            let path = entry?.path();
            let bytes = std::fs::read(&path)?;
            if let Some(what) = cassette::find_secret(&bytes, &self.0.secrets) {
                bail!("{} contains {what}", path.display());
            }
        }
        Ok(())
    }

    fn write_checked(&self, name: &str, bytes: &[u8]) -> Result<()> {
        if let Some(what) = cassette::find_secret(bytes, &self.0.secrets) {
            let reason = format!("{name} would contain {what}");
            self.0.state.lock().refused = Some(reason.clone());
            return Err(anyhow!(reason));
        }
        std::fs::write(self.0.dir.join(name), bytes)?;
        Ok(())
    }

    fn record(&self, request: RecordedRequest, response: RecordedResponse) -> Result<()> {
        let index = {
            let mut state = self.0.state.lock();
            state.count += 1;
            state.count
        };
        let name = file_name(index, &request);
        let json = serde_json::to_string_pretty(&Interaction { request, response })? + "\n";
        self.write_checked(&name, json.as_bytes())
    }
}

/// `NNN-<METHOD>-<path or operation>.json`, with anything but letters, digits and dashes in the
/// path or operation replaced, so a name can't leave the cassette's folder.
fn file_name(index: usize, request: &RecordedRequest) -> String {
    let sanitize = |text: &str| -> String {
        text.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    let what = match &request.operation {
        Some(operation) => format!("graphql-{}", sanitize(operation)),
        None => {
            let url = Url::parse(&request.url).ok();
            let path = url
                .as_ref()
                .map(|url| url.path().trim_matches('/').to_string())
                .unwrap_or_default();
            sanitize(&path)
        }
    };
    format!("{index:03}-{}-{what}.json", sanitize(&request.method))
}

/// What identifies a request, without its headers.
fn recorded_request(
    method: &str,
    url: &Url,
    body: &[u8],
    if_none_match: Option<String>,
) -> RecordedRequest {
    let mut request = RecordedRequest {
        method: method.to_string(),
        url: url.to_string(),
        if_none_match,
        ..Default::default()
    };
    if let Some(graphql) = cassette::graphql_body(url, body) {
        request.operation = Some(graphql.operation_name);
        request.variables = Some(graphql.variables);
        request.query_sha256 = Some(cassette::sha256(&graphql.query));
    } else if !body.is_empty() {
        request.body_sha256 = Some(cassette::sha256(body));
    }
    request
}

/// The response with only the kept headers, and signed URLs redacted.
fn recorded_response(
    status: u16,
    headers: &http_client::http::HeaderMap,
    body: &[u8],
) -> RecordedResponse {
    let mut kept = BTreeMap::new();
    for name in KEPT_HEADERS {
        let values: Vec<&str> = headers
            .get_all(*name)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect();
        if !values.is_empty() {
            kept.insert(name.to_string(), redact_signed_urls(&values.join(", ")));
        }
    }
    let text = String::from_utf8_lossy(body);
    let text = redact_signed_urls(&text);
    let (body, body_text) = match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(json) => (Some(json), None),
        Err(_) if text.is_empty() => (None, None),
        Err(_) => (None, Some(text)),
    };
    RecordedResponse {
        status,
        headers: kept,
        body,
        body_text,
    }
}

/// Replaces the values of query parameters that grant access with `REDACTED`: signed S3 and
/// Azure URLs (Actions logs, release assets), tokens on private raw and codeload URLs, signed
/// private images, and single sign-on requests. They're short-lived, but still not for a
/// public repository.
fn redact_signed_urls(text: &str) -> String {
    static SIGNED: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(concat!(
            r"((?:^|[?&;])",
            r"(?:X-Amz-(?:Signature|Credential|Security-Token)|token|access_token|refresh_token",
            r"|jwt|sig|se|sv|authorization_request)",
            r"=)[^&\s\x22'<>]+",
        ))
        .expect("valid regex")
    });
    SIGNED.replace_all(text, "${1}REDACTED").into_owned()
}

impl HttpClient for RecordingHttpClient {
    fn user_agent(&self) -> Option<&HeaderValue> {
        self.0.inner.user_agent()
    }

    fn proxy(&self) -> Option<&Url> {
        self.0.inner.proxy()
    }

    fn send(&self, request: Request<AsyncBody>) -> BoxFuture<'static, Result<Response<AsyncBody>>> {
        let this = self.clone();
        Box::pin(async move {
            let (parts, mut body) = request.into_parts();
            let mut request_body = Vec::new();
            body.read_to_end(&mut request_body).await?;
            let url = Url::parse(&parts.uri.to_string())?;
            let if_none_match = parts
                .headers
                .get("if-none-match")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let recorded =
                recorded_request(parts.method.as_str(), &url, &request_body, if_none_match);

            let request = Request::from_parts(parts, AsyncBody::from(request_body));
            let response = this.0.inner.send(request).await?;
            let (parts, mut body) = response.into_parts();
            let mut response_body = Vec::new();
            body.read_to_end(&mut response_body).await?;
            this.record(
                recorded,
                recorded_response(parts.status.as_u16(), &parts.headers, &response_body),
            )?;
            Ok(Response::from_parts(parts, AsyncBody::from(response_body)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::replay::ReplayHttpClient;

    /// Answers every request with a fixed response.
    struct Fixed {
        status: u16,
        headers: Vec<(&'static str, String)>,
        body: String,
    }

    impl HttpClient for Fixed {
        fn user_agent(&self) -> Option<&HeaderValue> {
            None
        }

        fn proxy(&self) -> Option<&Url> {
            None
        }

        fn send(&self, _: Request<AsyncBody>) -> BoxFuture<'static, Result<Response<AsyncBody>>> {
            let mut builder = Response::builder().status(self.status);
            for (name, value) in &self.headers {
                builder = builder.header(*name, value.as_str());
            }
            let response = builder.body(AsyncBody::from(self.body.clone())).unwrap();
            Box::pin(async move { Ok(response) })
        }
    }

    fn fixed(body: &str) -> Arc<Fixed> {
        Arc::new(Fixed {
            status: 200,
            headers: vec![
                ("content-type", "application/json; charset=utf-8".into()),
                ("etag", "W/\"abc\"".into()),
                ("x-ratelimit-remaining", "4999".into()),
                ("set-cookie", "_gh_sess=secret".into()),
                ("x-github-request-id", "ABCD:1234".into()),
            ],
            body: body.into(),
        })
    }

    fn send(client: &RecordingHttpClient, request: Request<AsyncBody>) -> Result<String> {
        let mut response = futures::executor::block_on(client.send(request))?;
        let mut body = String::new();
        futures::executor::block_on(response.body_mut().read_to_string(&mut body))?;
        Ok(body)
    }

    const FAKE_TOKEN: &str = "fake-token-for-the-recorder-test";

    #[test]
    fn records_a_replayable_cassette_without_request_headers() {
        let dir = tempfile::tempdir().unwrap();
        let cassette = dir.path().join("c");
        let client = RecordingHttpClient::new(
            fixed(r#"{"login":"rozed-bot"}"#),
            &cassette,
            vec![FAKE_TOKEN.into()],
        )
        .unwrap();
        let request = Request::get("https://api.github.com/user")
            .header("Authorization", format!("token {FAKE_TOKEN}"))
            .header("If-None-Match", "W/\"old\"")
            .body(AsyncBody::empty())
            .unwrap();
        assert_eq!(send(&client, request).unwrap(), r#"{"login":"rozed-bot"}"#);
        client
            .finish(CassetteInfo {
                description: "test".into(),
                ..Default::default()
            })
            .unwrap();

        let (_, interactions) = cassette::read_cassette(&cassette).unwrap();
        assert_eq!(interactions.len(), 1);
        let (name, interaction) = &interactions[0];
        assert_eq!(name, "001-GET-user.json");
        assert_eq!(
            interaction.request.if_none_match.as_deref(),
            Some("W/\"old\"")
        );
        let headers: Vec<_> = interaction.response.headers.keys().cloned().collect();
        assert_eq!(headers, ["content-type", "etag", "x-ratelimit-remaining"]);
        let file = std::fs::read_to_string(cassette.join(name)).unwrap();
        assert!(!file.contains("token"), "{file}");
        assert!(!file.contains("secret"), "{file}");

        let replay = ReplayHttpClient::from_dir(&cassette).unwrap();
        let request = Request::get("https://api.github.com/user")
            .header("If-None-Match", "W/\"old\"")
            .body(AsyncBody::empty())
            .unwrap();
        let response = futures::executor::block_on(replay.send(request)).unwrap();
        assert_eq!(response.headers()["etag"], "W/\"abc\"");
    }

    #[test]
    fn refuses_a_response_holding_the_token_and_deletes_the_cassette() {
        let dir = tempfile::tempdir().unwrap();
        let cassette = dir.path().join("leak");
        let client = RecordingHttpClient::new(
            fixed(&format!(r#"{{"echo":"{FAKE_TOKEN}"}}"#)),
            &cassette,
            vec![FAKE_TOKEN.into()],
        )
        .unwrap();
        let request = Request::get("https://api.github.com/echo")
            .body(AsyncBody::empty())
            .unwrap();
        let error = send(&client, request).unwrap_err();
        assert!(error.to_string().contains("the recording token"), "{error}");
        let error = client.finish(CassetteInfo::default()).unwrap_err();
        assert!(error.to_string().contains("refused"), "{error}");
        assert!(!cassette.exists());
    }

    #[test]
    fn refuses_anything_shaped_like_a_token() {
        let dir = tempfile::tempdir().unwrap();
        let cassette = dir.path().join("shaped");
        let other_token = format!("ghp_{}", "x".repeat(36));
        let client = RecordingHttpClient::new(
            fixed(&format!(r#"{{"token":"{other_token}"}}"#)),
            &cassette,
            vec![FAKE_TOKEN.into()],
        )
        .unwrap();
        let request = Request::get("https://api.github.com/x")
            .body(AsyncBody::empty())
            .unwrap();
        assert!(send(&client, request).is_err());
        assert!(client.finish(CassetteInfo::default()).is_err());
        assert!(!cassette.exists());
    }

    #[test]
    fn redacts_signed_urls() {
        let response = recorded_response(
            302,
            &[(
                http_client::http::header::LOCATION,
                HeaderValue::from_static(
                    "https://b.s3.amazonaws.com/x?X-Amz-Credential=AKIA%2F1&X-Amz-Signature=abc123&X-Amz-Expires=300",
                ),
            )]
            .into_iter()
            .collect(),
            b"",
        );
        assert_eq!(
            response.headers["location"],
            "https://b.s3.amazonaws.com/x?X-Amz-Credential=REDACTED&X-Amz-Signature=REDACTED&X-Amz-Expires=300"
        );
    }

    #[test]
    fn redacts_signed_query_values() {
        let redacted = redact_signed_urls(concat!(
            "https://raw.githubusercontent.com/o/r/main/a.txt?token=GHSAT0AAAAA ",
            "https://private-user-images.githubusercontent.com/1/x.png?jwt=eyJhbGciOi.x.y ",
            "https://productionresultssa1.blob.core.windows.net/logs/1.txt?sv=2021-08-06&se=2026-11-06T10%3A00Z&sr=b&sp=r&sig=abc%2Bdef ",
            "required; url=https://github.com/orgs/acme/sso?authorization_request=ABCDEF ",
            "<a href=\"https://codeload.github.com/o/r/zip/main?x=1&amp;token=AAAA\">",
        ));
        assert_eq!(
            redacted,
            concat!(
                "https://raw.githubusercontent.com/o/r/main/a.txt?token=REDACTED ",
                "https://private-user-images.githubusercontent.com/1/x.png?jwt=REDACTED ",
                "https://productionresultssa1.blob.core.windows.net/logs/1.txt?sv=REDACTED&se=REDACTED&sr=b&sp=r&sig=REDACTED ",
                "required; url=https://github.com/orgs/acme/sso?authorization_request=REDACTED ",
                "<a href=\"https://codeload.github.com/o/r/zip/main?x=1&amp;token=REDACTED\">",
            )
        );
    }

    #[test]
    fn a_kept_sso_header_is_redacted() {
        let response = recorded_response(
            403,
            &[(
                http_client::http::HeaderName::from_static("x-github-sso"),
                HeaderValue::from_static(
                    "required; url=https://github.com/orgs/acme/sso?authorization_request=SECRET",
                ),
            )]
            .into_iter()
            .collect(),
            b"{}",
        );
        assert_eq!(
            response.headers["x-github-sso"],
            "required; url=https://github.com/orgs/acme/sso?authorization_request=REDACTED"
        );
    }

    #[test]
    fn file_names_never_leave_the_cassette() {
        let request = RecordedRequest {
            method: "POST".into(),
            url: "https://api.github.com/graphql".into(),
            operation: Some("../../../evil/Name".into()),
            ..Default::default()
        };
        assert_eq!(
            file_name(1, &request),
            "001-POST-graphql-_________evil_Name.json"
        );
    }

    #[test]
    fn graphql_requests_record_their_operation() {
        let body = br#"{"query":"query Viewer { viewer { login } }","operationName":"Viewer","variables":{}}"#;
        let url = Url::parse("https://api.github.com/graphql").unwrap();
        let request = recorded_request("POST", &url, body, None);
        assert_eq!(request.operation.as_deref(), Some("Viewer"));
        assert_eq!(request.variables, Some(serde_json::json!({})));
        assert_eq!(
            request.query_sha256,
            Some(cassette::sha256("query Viewer { viewer { login } }"))
        );
        assert_eq!(request.body_sha256, None);
        assert_eq!(file_name(3, &request), "003-POST-graphql-Viewer.json");
    }
}
