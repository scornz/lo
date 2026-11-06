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

fn client() -> Arc<ReqwestClient> {
    Arc::new(ReqwestClient::user_agent("rozed-test").unwrap())
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
    let proxy: Url = format!("http://127.0.0.1:{}", closed_port())
        .parse()
        .unwrap();
    let client = Arc::new(ReqwestClient::proxy_and_user_agent(Some(proxy), "rozed-test").unwrap());
    let error = get(&client, "https://api.github.com/", Duration::from_secs(10));
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
fn a_proxy_that_wants_a_password_isnt_offline() {
    let port = server(|mut stream| {
        let mut request = [0; 1024];
        let _ = stream.read(&mut request);
        let _ = stream.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n");
    });
    let proxy: Url = format!("http://127.0.0.1:{port}").parse().unwrap();
    let client = Arc::new(ReqwestClient::proxy_and_user_agent(Some(proxy), "rozed-test").unwrap());
    let error = get(&client, "https://api.github.com/", Duration::from_secs(10));
    assert!(!error.is_unreachable(), "{error:#}");
    assert!(!error.is_retryable(), "{error:#}");
}

#[test]
fn a_failed_tls_handshake_isnt_offline() {
    // Speaks plain HTTP where TLS is expected, as a broken intercepting proxy might.
    let port = server(|mut stream| {
        let mut hello = [0; 1024];
        let _ = stream.read(&mut hello);
        let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
    });
    let error = get(
        &client(),
        &format!("https://127.0.0.1:{port}/"),
        Duration::from_secs(10),
    );
    assert!(!error.is_unreachable(), "{error:#}");
}
