//! What rozed's real HTTP client reports when GitHub can't be reached, and which of those count as
//! offline: run against local sockets, so a change in reqwest's or hyper's errors shows here.

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use futures::executor::block_on;
use github::GithubError;
use http_client::{AsyncBody, HttpClient, HttpRequestExt as _, Request, Url};
use reqwest_client::ReqwestClient;

/// What `client` gets for `GET url`, as rozed's GitHub client reports it.
fn get(client: &Arc<ReqwestClient>, url: &str, timeout: Duration) -> GithubError {
    let request = Request::get(url)
        .timeout(timeout)
        .body(AsyncBody::empty())
        .unwrap();
    match block_on(client.send(request)) {
        Ok(response) => panic!("{url} answered {}", response.status()),
        Err(error) => GithubError::Network(error),
    }
}

/// Clears the proxy variables, which reqwest reads, so the tests' requests go where they say,
/// whatever the environment they run in.
fn without_proxies() {
    static CLEARED: std::sync::Once = std::sync::Once::new();
    CLEARED.call_once(|| {
        for name in [
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
            "ALL_PROXY",
            "all_proxy",
            "NO_PROXY",
            "no_proxy",
        ] {
            // SAFETY: every test clears them through this `Once` before making its first
            // client, and nothing else in these tests reads or writes the environment.
            unsafe { std::env::remove_var(name) };
        }
    });
}

/// A client that goes straight to the host.
fn client() -> Arc<ReqwestClient> {
    without_proxies();
    Arc::new(ReqwestClient::user_agent("rozed-test").unwrap())
}

/// A client that sends everything through the proxy at `port`.
fn client_through(port: u16) -> Arc<ReqwestClient> {
    without_proxies();
    let proxy: Url = format!("http://127.0.0.1:{port}").parse().unwrap();
    Arc::new(ReqwestClient::proxy_and_user_agent(Some(proxy), "rozed-test").unwrap())
}

/// A port nothing listens on.
fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// A local server that accepts connections and does `answer` with each.
fn server(answer: impl Fn(std::net::TcpStream) + Send + Sync + 'static) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            answer(stream);
        }
    });
    port
}

/// A server that reads each request and answers it with `answer`.
fn answering(answer: &'static [u8]) -> u16 {
    server(move |mut stream| {
        let mut request = [0; 1024];
        let _ = stream.read(&mut request);
        let _ = stream.write_all(answer);
    })
}

#[test]
fn a_port_nothing_listens_on_is_unreachable() {
    let port = closed_port();
    let error = get(
        &client(),
        &format!("http://127.0.0.1:{port}/"),
        Duration::from_secs(10),
    );
    assert!(error.is_unreachable(), "{error:#}");
    assert!(error.is_retryable(), "{error:#}");
}

#[test]
fn a_host_with_no_dns_answer_is_unreachable() {
    let error = get(
        &client(),
        "http://api.rozed-test.invalid/",
        Duration::from_secs(10),
    );
    assert!(error.is_unreachable(), "{error:#}");
}

#[test]
fn a_dead_proxy_is_unreachable() {
    let error = get(
        &client_through(closed_port()),
        "https://api.github.com/",
        Duration::from_secs(10),
    );
    assert!(error.is_unreachable(), "{error:#}");
}

#[test]
fn a_server_that_never_answers_isnt_offline_but_is_worth_trying_again() {
    let port = server(|stream| {
        // Holds the connection open without a word.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(30));
            drop(stream);
        });
    });
    let error = get(
        &client(),
        &format!("http://127.0.0.1:{port}/"),
        Duration::from_millis(500),
    );
    assert!(!error.is_unreachable(), "{error:#}");
    assert!(error.is_retryable(), "{error:#}");
}

#[test]
fn a_connection_closed_before_the_answer_isnt_offline_but_is_worth_trying_again() {
    // Reads the request and hangs up, as a connection that died under it does.
    let port = server(|mut stream| {
        let mut request = [0; 1024];
        let _ = stream.read(&mut request);
        drop(stream);
    });
    let error = get(
        &client(),
        &format!("http://127.0.0.1:{port}/"),
        Duration::from_secs(10),
    );
    assert!(!error.is_unreachable(), "{error:#}");
    assert!(error.is_retryable(), "{error:#}");
}

#[test]
fn a_proxy_that_cant_reach_github_is_worth_trying_again() {
    let port = answering(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n");
    let error = get(
        &client_through(port),
        "https://api.github.com/",
        Duration::from_secs(10),
    );
    assert!(!error.is_unreachable(), "{error:#}");
    assert!(error.is_retryable(), "{error:#}");
}

#[test]
fn a_proxy_that_wants_a_password_isnt_offline() {
    let port = answering(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n");
    let error = get(
        &client_through(port),
        "https://api.github.com/",
        Duration::from_secs(10),
    );
    assert!(!error.is_unreachable(), "{error:#}");
    assert!(!error.is_retryable(), "{error:#}");
}

#[test]
fn a_failed_tls_handshake_isnt_offline() {
    // Speaks plain HTTP where TLS is expected, as a broken intercepting proxy might.
    let port = answering(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
    let error = get(
        &client(),
        &format!("https://127.0.0.1:{port}/"),
        Duration::from_secs(10),
    );
    assert!(!error.is_unreachable(), "{error:#}");
}
