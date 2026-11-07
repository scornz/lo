//! The one HTTP client every part of Lo and Zed's crates share.
//!
//! Lo never contacts Zed's servers (design §1). Settings turn off the Zed features that would,
//! and this client refuses any request to Zed's hosts that gets through anyway, logging it at
//! `warn`. It follows redirects itself, so every hop gets the same check. Every request and
//! redirect is logged at `debug` by host and path, never by query, which can hold tokens, so a run
//! with `RUST_LOG=lo=debug` lists everything Lo contacted.

use std::sync::Arc;

use anyhow::{Context as _, bail};
use futures::FutureExt as _;
use futures::future::BoxFuture;
use http_client::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header};
use http_client::{AsyncBody, HttpClient, Inner, RedirectPolicy, Request, Response, Url};

/// Zed's own domain. It and every subdomain are refused.
const ZED_DOMAIN: &str = "zed.dev";

/// The placeholder Lo sets as Zed's `server_url`, so every URL Zed's crates build for a Zed
/// service points here instead. `.invalid` never resolves (RFC 6761), and this client refuses it
/// before trying, so a request here shows up in the log.
pub const ZED_SERVICES_PLACEHOLDER: &str = "lo.invalid";

/// How many redirects a request without a [`RedirectPolicy`] follows: reqwest's default, which
/// Zed's client used.
const DEFAULT_REDIRECT_LIMIT: u32 = 10;

/// How many redirects [`RedirectPolicy::FollowAll`] follows, as in Zed's client.
const FOLLOW_ALL_LIMIT: u32 = 100;

/// Headers that carry credentials. A redirect to another scheme, host or port drops them.
const CREDENTIAL_HEADERS: [HeaderName; 3] = [
    header::AUTHORIZATION,
    header::COOKIE,
    header::PROXY_AUTHORIZATION,
];

/// Headers that describe a request body. A redirect that turns a request into a GET drops them.
const BODY_HEADERS: [HeaderName; 4] = [
    header::CONTENT_TYPE,
    header::CONTENT_LENGTH,
    header::CONTENT_ENCODING,
    header::TRANSFER_ENCODING,
];

/// Whether `host` is one of Zed's or the placeholder that stands in for Zed's services.
pub fn is_refused_host(host: &str) -> bool {
    // A trailing dot is the fully qualified form of the same name.
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    [ZED_DOMAIN, ZED_SERVICES_PLACEHOLDER]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

/// A URI as Lo logs it: host and path, without the scheme, query or fragment.
fn loggable(uri: &Uri) -> String {
    format!("{}{}", uri.host().unwrap_or_default(), uri.path())
}

/// Wraps an HTTP client. It refuses requests to Zed's hosts, and follows redirects itself, up to
/// each request's [`RedirectPolicy`], checking every hop.
pub struct ZedHostGuard {
    inner: Arc<dyn HttpClient>,
}

impl ZedHostGuard {
    pub fn new(inner: Arc<dyn HttpClient>) -> Self {
        Self { inner }
    }
}

impl HttpClient for ZedHostGuard {
    fn user_agent(&self) -> Option<&HeaderValue> {
        self.inner.user_agent()
    }

    fn proxy(&self) -> Option<&Url> {
        self.inner.proxy()
    }

    fn send(
        &self,
        request: Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        send(self.inner.clone(), request).boxed()
    }
}

/// A copy of a request's body, to send again after a redirect.
enum Replay {
    Empty,
    Bytes(Vec<u8>),
    /// A stream, which the first send consumes.
    Stream,
}

async fn send(
    inner: Arc<dyn HttpClient>,
    request: Request<AsyncBody>,
) -> anyhow::Result<Response<AsyncBody>> {
    let (mut parts, body) = request.into_parts();
    let limit = match parts.extensions.get::<RedirectPolicy>() {
        None => DEFAULT_REDIRECT_LIMIT,
        Some(RedirectPolicy::NoFollow) => 0,
        Some(RedirectPolicy::FollowLimit(limit)) => *limit,
        Some(RedirectPolicy::FollowAll) => FOLLOW_ALL_LIMIT,
    };
    // The inner client hands every redirect back, so it's checked here.
    parts.extensions.insert(RedirectPolicy::NoFollow);

    let mut replay = match &body.0 {
        Inner::Empty => Replay::Empty,
        Inner::Bytes(cursor) => Replay::Bytes(cursor.get_ref().to_vec()),
        Inner::AsyncReader(_) => Replay::Stream,
    };
    let mut body = Some(body);
    let mut redirects = 0;
    loop {
        refuse_zed_hosts(&parts.method, &parts.uri)?;
        log::debug!("http request: {} {}", parts.method, loggable(&parts.uri));
        let body = match (body.take(), &replay) {
            (Some(body), _) => body,
            (None, Replay::Empty) => AsyncBody::empty(),
            (None, Replay::Bytes(bytes)) => AsyncBody::from(bytes.clone()),
            (None, Replay::Stream) => unreachable!("a streamed body is only ever sent once"),
        };
        let mut request = Request::new(body);
        *request.method_mut() = parts.method.clone();
        *request.uri_mut() = parts.uri.clone();
        *request.version_mut() = parts.version;
        *request.headers_mut() = parts.headers.clone();
        *request.extensions_mut() = parts.extensions.clone();
        let response = inner.send(request).await?;

        let status = response.status();
        if !status.is_redirection() || limit == 0 {
            return Ok(response);
        }
        let Some(next) = redirect_target(&parts.uri, response.headers())? else {
            return Ok(response);
        };
        // 307 and 308 send the same request again. 301, 302 and 303 send a GET (or HEAD)
        // without a body, as reqwest does.
        let resends = matches!(
            status,
            StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT
        );
        if resends && matches!(replay, Replay::Stream) {
            log::debug!(
                "not following a {} redirect from {}: its body was a stream",
                status.as_u16(),
                loggable(&parts.uri),
            );
            return Ok(response);
        }
        if redirects == limit {
            bail!(
                "too many redirects (more than {limit}) from {}",
                loggable(&parts.uri)
            );
        }
        redirects += 1;
        log::debug!(
            "http redirect {}: {} -> {}",
            status.as_u16(),
            loggable(&parts.uri),
            loggable(&next)
        );

        if !resends {
            if parts.method != Method::GET && parts.method != Method::HEAD {
                parts.method = Method::GET;
            }
            replay = Replay::Empty;
            for name in &BODY_HEADERS {
                parts.headers.remove(name);
            }
        }
        if origin(&parts.uri) != origin(&next) {
            for name in &CREDENTIAL_HEADERS {
                parts.headers.remove(name);
            }
        }
        parts.uri = next;
    }
}

/// Fails, without sending anything, for a request to one of Zed's hosts.
fn refuse_zed_hosts(method: &Method, uri: &Uri) -> anyhow::Result<()> {
    let host = uri.host().unwrap_or_default();
    if is_refused_host(host) {
        log::warn!(
            "refused a request to a Zed host: {method} {}",
            loggable(uri)
        );
        bail!("Lo refuses requests to Zed's hosts ({host})");
    }
    Ok(())
}

/// Where a redirect points, resolved against the URI it came from. None when the response has no
/// `Location` (a 304, say), so it's returned as it is.
fn redirect_target(from: &Uri, headers: &HeaderMap) -> anyhow::Result<Option<Uri>> {
    let Some(location) = headers.get(header::LOCATION) else {
        return Ok(None);
    };
    let location = location.to_str().with_context(|| {
        format!(
            "a redirect from {} has a Location that isn't text",
            loggable(from)
        )
    })?;
    let next = Url::parse(&from.to_string())
        .and_then(|from| from.join(location))
        .with_context(|| format!("a redirect from {} has a bad Location", loggable(from)))?;
    if !matches!(next.scheme(), "http" | "https") {
        bail!(
            "a redirect from {} leads to a {} URL",
            loggable(from),
            next.scheme()
        );
    }
    let next = next
        .as_str()
        .parse()
        .with_context(|| format!("a redirect from {} has a bad Location", loggable(from)))?;
    Ok(Some(next))
}

/// The scheme, host and port a URI addresses. Credentials go no further than these.
fn origin(uri: &Uri) -> (Option<&str>, Option<String>, Option<u16>) {
    let port = uri.port_u16().or(match uri.scheme_str() {
        Some("http") => Some(80),
        Some("https") => Some(443),
        _ => None,
    });
    (
        uri.scheme_str(),
        uri.host().map(str::to_ascii_lowercase),
        port,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    #[test]
    fn refuses_zed_and_its_subdomains() {
        for host in [
            "zed.dev",
            "api.zed.dev",
            "cloud.zed.dev",
            "collab.zed.dev",
            "a.b.zed.dev",
            "ZED.DEV",
            "Api.Zed.Dev",
            "zed.dev.",
            "api.zed.dev.",
        ] {
            assert!(is_refused_host(host), "{host} should be refused");
        }
    }

    #[test]
    fn refuses_the_zed_services_placeholder() {
        assert!(is_refused_host("lo.invalid"));
        assert!(is_refused_host("api.lo.invalid"));
    }

    #[test]
    fn allows_other_hosts() {
        for host in [
            "github.com",
            "api.github.com",
            "objects.githubusercontent.com",
            "nodejs.org",
            "notzed.dev",
            "zed.dev.example.com",
            "zeddev.com",
            "zed.devs",
            "lo.dev",
            "127.0.0.1",
            "",
        ] {
            assert!(!is_refused_host(host), "{host} should be allowed");
        }
    }

    #[test]
    fn logs_host_and_path_but_not_the_query() {
        let uri: Uri = "https://api.github.com/repos/a/b?access_token=secret#x"
            .parse()
            .unwrap();
        assert_eq!(loggable(&uri), "api.github.com/repos/a/b");
    }

    /// A request as it reached [`Fake`].
    struct Sent {
        method: Method,
        uri: String,
        headers: HeaderMap,
        body: Vec<u8>,
        policy: Option<RedirectPolicy>,
    }

    type Respond = dyn Fn(&str) -> Response<AsyncBody> + Send + Sync;

    /// Stands in for the network: records each request and answers with whatever `respond`
    /// returns for its URI.
    struct Fake {
        sent: Mutex<Vec<Sent>>,
        respond: Box<Respond>,
    }

    impl Fake {
        fn new(respond: impl Fn(&str) -> Response<AsyncBody> + Send + Sync + 'static) -> Arc<Self> {
            Arc::new(Self {
                sent: Mutex::default(),
                respond: Box::new(respond),
            })
        }

        fn uris(&self) -> Vec<String> {
            let sent = self.sent.lock().unwrap();
            sent.iter().map(|sent| sent.uri.clone()).collect()
        }
    }

    impl HttpClient for Fake {
        fn user_agent(&self) -> Option<&HeaderValue> {
            None
        }

        fn proxy(&self) -> Option<&Url> {
            None
        }

        fn send(
            &self,
            request: Request<AsyncBody>,
        ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
            let (parts, body) = request.into_parts();
            let bytes = match body.0 {
                Inner::Empty => Vec::new(),
                Inner::Bytes(cursor) => cursor.into_inner().to_vec(),
                Inner::AsyncReader(_) => b"<stream>".to_vec(),
            };
            let uri = parts.uri.to_string();
            let response = (self.respond)(&uri);
            self.sent.lock().unwrap().push(Sent {
                method: parts.method,
                uri,
                headers: parts.headers,
                body: bytes,
                policy: parts.extensions.get::<RedirectPolicy>().cloned(),
            });
            Box::pin(async move { Ok(response) })
        }
    }

    fn redirect(status: u16, location: &str) -> Response<AsyncBody> {
        Response::builder()
            .status(status)
            .header(header::LOCATION, location)
            .body(AsyncBody::empty())
            .unwrap()
    }

    fn ok() -> Response<AsyncBody> {
        Response::new(AsyncBody::empty())
    }

    fn get(url: &str) -> Request<AsyncBody> {
        Request::get(url).body(AsyncBody::empty()).unwrap()
    }

    fn get_with(url: &str, policy: RedirectPolicy) -> Request<AsyncBody> {
        Request::get(url)
            .extension(policy)
            .body(AsyncBody::empty())
            .unwrap()
    }

    fn get_with_token(url: &str) -> Request<AsyncBody> {
        Request::get(url)
            .header(header::AUTHORIZATION, "Bearer secret")
            .header(header::ACCEPT, "application/octet-stream")
            .body(AsyncBody::empty())
            .unwrap()
    }

    fn post(url: &str, body: &'static str) -> Request<AsyncBody> {
        Request::post(url)
            .header(header::CONTENT_TYPE, "application/json")
            .body(AsyncBody::from(body))
            .unwrap()
    }

    fn send_through(
        inner: &Arc<Fake>,
        request: Request<AsyncBody>,
    ) -> anyhow::Result<Response<AsyncBody>> {
        let guard = ZedHostGuard::new(inner.clone());
        futures::executor::block_on(guard.send(request))
    }

    /// The error a send failed with. A response isn't `Debug`, so the result can't be unwrapped.
    fn failure(result: anyhow::Result<Response<AsyncBody>>) -> anyhow::Error {
        match result {
            Ok(response) => panic!("expected an error, got a {}", response.status()),
            Err(error) => error,
        }
    }

    #[test]
    fn a_refused_request_fails_without_reaching_the_network() {
        let inner = Fake::new(|_| ok());
        let error = failure(send_through(
            &inner,
            get("https://api.zed.dev/telemetry/events"),
        ));
        assert!(error.to_string().contains("api.zed.dev"), "{error}");
        assert!(inner.uris().is_empty());
    }

    #[test]
    fn other_requests_pass_through() {
        let inner = Fake::new(|_| ok());
        let url = "https://api.github.com/repos/rust-lang/rust-analyzer/releases";
        let response = send_through(&inner, get(url)).unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(inner.uris(), [url]);
    }

    #[test]
    fn the_inner_client_never_follows_redirects() {
        let inner = Fake::new(|_| ok());
        send_through(
            &inner,
            get_with("https://github.com/", RedirectPolicy::FollowAll),
        )
        .unwrap();
        let sent = inner.sent.lock().unwrap();
        assert_eq!(sent[0].policy, Some(RedirectPolicy::NoFollow));
    }

    #[test]
    fn a_redirect_to_a_zed_host_is_refused() {
        let inner = Fake::new(|uri| {
            if uri.starts_with("https://example.com/") {
                redirect(302, "https://api.zed.dev/telemetry/events?token=x")
            } else {
                ok()
            }
        });
        let error = failure(send_through(&inner, get("https://example.com/download")));
        assert!(error.to_string().contains("api.zed.dev"), "{error}");
        assert_eq!(inner.uris(), ["https://example.com/download"]);
    }

    #[test]
    fn a_redirect_to_another_host_drops_the_token() {
        let inner = Fake::new(|uri| {
            if uri.starts_with("https://api.github.com/") {
                redirect(302, "https://objects.githubusercontent.com/a.tar.gz?sig=x")
            } else {
                ok()
            }
        });
        let response = send_through(
            &inner,
            get_with_token("https://api.github.com/repos/a/b/releases/assets/1"),
        )
        .unwrap();
        assert_eq!(response.status(), 200);

        let sent = inner.sent.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].headers[header::AUTHORIZATION], "Bearer secret");
        assert_eq!(
            sent[1].uri,
            "https://objects.githubusercontent.com/a.tar.gz?sig=x"
        );
        assert!(!sent[1].headers.contains_key(header::AUTHORIZATION));
        assert_eq!(sent[1].headers[header::ACCEPT], "application/octet-stream");
    }

    #[test]
    fn a_redirect_on_the_same_host_keeps_the_token() {
        let inner = Fake::new(|uri| {
            if uri.ends_with("/old") {
                redirect(301, "/new")
            } else {
                ok()
            }
        });
        send_through(&inner, get_with_token("https://api.github.com/old")).unwrap();
        let sent = inner.sent.lock().unwrap();
        assert_eq!(sent[1].uri, "https://api.github.com/new");
        assert_eq!(sent[1].headers[header::AUTHORIZATION], "Bearer secret");
    }

    #[test]
    fn a_redirect_from_https_to_http_drops_the_token() {
        let inner = Fake::new(|uri| {
            if uri.starts_with("https:") {
                redirect(302, "http://example.com/")
            } else {
                ok()
            }
        });
        send_through(&inner, get_with_token("https://example.com/")).unwrap();
        let sent = inner.sent.lock().unwrap();
        assert!(!sent[1].headers.contains_key(header::AUTHORIZATION));
    }

    /// Redirects `/<n>` to `/<n + 1>`, forever.
    fn endless() -> Arc<Fake> {
        Fake::new(|uri| {
            let n: u32 = uri.rsplit('/').next().unwrap().parse().unwrap();
            redirect(302, &format!("/{}", n + 1))
        })
    }

    #[test]
    fn redirects_stop_at_the_requests_limit() {
        let inner = endless();
        let error = failure(send_through(
            &inner,
            get_with("https://example.com/0", RedirectPolicy::FollowLimit(3)),
        ));
        assert!(error.to_string().contains("too many redirects"), "{error}");
        assert_eq!(inner.uris().len(), 4);
    }

    #[test]
    fn a_request_without_a_policy_follows_ten_redirects() {
        let inner = endless();
        failure(send_through(&inner, get("https://example.com/0")));
        assert_eq!(inner.uris().len(), 11);
    }

    #[test]
    fn follow_all_follows_a_hundred_redirects() {
        let inner = endless();
        failure(send_through(
            &inner,
            get_with("https://example.com/0", RedirectPolicy::FollowAll),
        ));
        assert_eq!(inner.uris().len(), 101);
    }

    #[test]
    fn no_follow_returns_the_redirect() {
        let inner = endless();
        let response = send_through(
            &inner,
            get_with("https://example.com/0", RedirectPolicy::NoFollow),
        )
        .unwrap();
        assert_eq!(response.status(), 302);
        assert_eq!(inner.uris().len(), 1);
    }

    #[test]
    fn a_303_turns_a_post_into_a_get() {
        let inner = Fake::new(|uri| {
            if uri.ends_with("/form") {
                redirect(303, "/done")
            } else {
                ok()
            }
        });
        send_through(&inner, post("https://example.com/form", "{}")).unwrap();
        let sent = inner.sent.lock().unwrap();
        assert_eq!(sent[1].method, Method::GET);
        assert!(sent[1].body.is_empty());
        assert!(!sent[1].headers.contains_key(header::CONTENT_TYPE));
    }

    #[test]
    fn a_302_turns_a_streamed_put_into_a_get() {
        let inner = Fake::new(|uri| {
            if uri.ends_with("/upload") {
                redirect(302, "/uploaded")
            } else {
                ok()
            }
        });
        let request = Request::put("https://example.com/upload")
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(AsyncBody::from_reader(futures::io::Cursor::new(b"data")))
            .unwrap();
        let response = send_through(&inner, request).unwrap();
        assert_eq!(response.status(), 200);
        let sent = inner.sent.lock().unwrap();
        assert_eq!(sent[1].method, Method::GET);
        assert!(sent[1].body.is_empty());
        assert!(!sent[1].headers.contains_key(header::CONTENT_TYPE));
    }

    #[test]
    fn a_307_after_a_streamed_body_is_returned() {
        let inner = Fake::new(|_| redirect(307, "/elsewhere"));
        let request = Request::put("https://example.com/upload")
            .body(AsyncBody::from_reader(futures::io::Cursor::new(b"data")))
            .unwrap();
        let response = send_through(&inner, request).unwrap();
        assert_eq!(response.status(), 307);
        assert_eq!(inner.uris().len(), 1);
    }

    #[test]
    fn a_307_sends_the_post_again() {
        let inner = Fake::new(|uri| {
            if uri.ends_with("/form") {
                redirect(307, "/elsewhere")
            } else {
                ok()
            }
        });
        send_through(&inner, post("https://example.com/form", r#"{"a":1}"#)).unwrap();
        let sent = inner.sent.lock().unwrap();
        assert_eq!(sent[1].uri, "https://example.com/elsewhere");
        assert_eq!(sent[1].method, Method::POST);
        assert_eq!(sent[1].body, br#"{"a":1}"#);
        assert_eq!(sent[1].headers[header::CONTENT_TYPE], "application/json");
    }

    #[test]
    fn a_redirect_to_another_scheme_fails() {
        let inner = Fake::new(|_| redirect(302, "file:///etc/passwd"));
        let error = failure(send_through(&inner, get("https://example.com/")));
        assert!(error.to_string().contains("file URL"), "{error}");
        assert_eq!(inner.uris().len(), 1);
    }
}
