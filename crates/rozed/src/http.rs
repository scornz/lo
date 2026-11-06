//! The one HTTP client every part of rozed and Zed's crates share.
//!
//! rozed never contacts Zed's servers (design §1). Settings turn off the Zed features that would,
//! and this client refuses any request to Zed's hosts that gets through anyway, logging it at
//! `warn`. Every request's host is logged at `debug`, so a run with `RUST_LOG=rozed=debug` lists
//! every host rozed contacted.

use std::sync::Arc;

use futures::future::BoxFuture;
use http_client::http::HeaderValue;
use http_client::{AsyncBody, HttpClient, Request, Response, Url};

/// Zed's own domain. It and every subdomain are refused.
const ZED_DOMAIN: &str = "zed.dev";

/// The placeholder rozed sets as Zed's `server_url`, so every URL Zed's crates build for a Zed
/// service points here instead. `.invalid` never resolves (RFC 6761), and this client refuses it
/// before trying, so a request here shows up in the log.
pub const ZED_SERVICES_PLACEHOLDER: &str = "rozed.invalid";

/// Whether `host` is one of Zed's or the placeholder that stands in for Zed's services.
pub fn is_refused_host(host: &str) -> bool {
    // A trailing dot is the fully qualified form of the same name.
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    [ZED_DOMAIN, ZED_SERVICES_PLACEHOLDER]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

/// Wraps an HTTP client and refuses requests to Zed's hosts.
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
        let host = request.uri().host().unwrap_or_default().to_string();
        if is_refused_host(&host) {
            let uri = request.uri().to_string();
            log::warn!(
                "refused a request to a Zed host: {} {uri}",
                request.method()
            );
            return Box::pin(async move {
                Err(anyhow::anyhow!(
                    "rozed refuses requests to Zed's hosts ({host})"
                ))
            });
        }
        log::debug!("http request: {} {host}", request.method());
        self.inner.send(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

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
        assert!(is_refused_host("rozed.invalid"));
        assert!(is_refused_host("api.rozed.invalid"));
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
            "rozed.dev",
            "127.0.0.1",
            "",
        ] {
            assert!(!is_refused_host(host), "{host} should be allowed");
        }
    }

    /// Counts the requests that reach it and answers each with an empty 200.
    #[derive(Default)]
    struct Counting {
        sent: AtomicUsize,
    }

    impl HttpClient for Counting {
        fn user_agent(&self) -> Option<&HeaderValue> {
            None
        }

        fn proxy(&self) -> Option<&Url> {
            None
        }

        fn send(
            &self,
            _: Request<AsyncBody>,
        ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
            self.sent.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(Response::new(AsyncBody::empty())) })
        }
    }

    fn get(url: &str) -> Request<AsyncBody> {
        Request::get(url).body(AsyncBody::empty()).unwrap()
    }

    #[test]
    fn a_refused_request_fails_without_reaching_the_network() {
        let inner = Arc::new(Counting::default());
        let guard = ZedHostGuard::new(inner.clone());
        let result =
            futures::executor::block_on(guard.send(get("https://api.zed.dev/telemetry/events")));
        let Err(error) = result else {
            panic!("a request to api.zed.dev went through");
        };
        assert!(error.to_string().contains("api.zed.dev"), "{error}");
        assert_eq!(inner.sent.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn other_requests_pass_through() {
        let inner = Arc::new(Counting::default());
        let guard = ZedHostGuard::new(inner.clone());
        let response = futures::executor::block_on(guard.send(get(
            "https://api.github.com/repos/rust-lang/rust-analyzer/releases",
        )))
        .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(inner.sent.load(Ordering::SeqCst), 1);
    }
}
