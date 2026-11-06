//! Signing in over real HTTP, against fake servers on this Mac: a proxy that wants a password, a
//! server that redirects to another host, and a server that wants a password. The token must
//! reach only the remote's own host.

use std::ffi::OsString;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use crate::{Git, PullRequestRefs, RepoCache, Token};

/// The token the tests sign in with. Not a real one.
const TOKEN: &str = "SECRET_TOKEN_FOR_TESTS";

/// A server on `127.0.0.1` that answers every request the same way and keeps each request's head.
struct FakeServer {
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
}

impl FakeServer {
    fn start(respond: impl Fn(&str) -> String + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        std::thread::spawn({
            let requests = requests.clone();
            move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut head = String::new();
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                        head.push_str(&line);
                    }
                    let response = respond(&head);
                    requests.lock().unwrap().push(head);
                    let _ = stream.write_all(response.as_bytes());
                }
            }
        });
        Self { port, requests }
    }

    /// A proxy that wants a password for everything.
    fn proxy() -> Self {
        Self::start(|_| {
            "HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic \
             realm=\"proxy\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .into()
        })
    }

    /// A server that wants a password for everything.
    fn asks_for_a_password() -> Self {
        Self::start(|_| {
            "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"server\"\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n"
                .into()
        })
    }

    /// A server that redirects every request to the same path on `localhost:<port>`.
    fn redirects_to(port: u16) -> Self {
        Self::start(move |head| {
            let path = head.split(' ').nth(1).unwrap_or("/");
            format!(
                "HTTP/1.1 302 Found\r\nLocation: http://localhost:{port}{path}\r\n\
                 Content-Length: 0\r\nConnection: close\r\n\r\n"
            )
        })
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    /// Whether any request carried the token, in any header or credential.
    fn saw_the_token(&self) -> bool {
        self.requests().iter().any(|head| {
            head.contains(TOKEN)
                || head.lines().any(|line| {
                    line.split_once(": Basic ")
                        .and_then(|(_, credentials)| base64_decode(credentials.trim()))
                        .is_some_and(|decoded| decoded.contains(TOKEN))
                })
        })
    }
}

/// Decodes standard base64, as in `Basic` credentials.
fn base64_decode(text: &str) -> Option<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bits = 0u32;
    let mut count = 0;
    let mut bytes = Vec::new();
    for c in text.trim_end_matches('=').bytes() {
        let value = ALPHABET.iter().position(|a| *a == c)? as u32;
        bits = bits << 6 | value;
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    String::from_utf8(bytes).ok()
}

/// git, as if rozed's environment held only `PATH` and `vars`.
fn git_with_env(vars: &[(&str, &str)]) -> Git {
    let program: PathBuf = Git::find().unwrap().program().to_path_buf();
    let env = std::iter::once(("PATH", "/usr/bin:/bin"))
        .chain(vars.iter().copied())
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect();
    Git::with_env(program, env).unwrap()
}

fn fetch_pull_one(cache: &RepoCache) -> crate::Error {
    let repo = cache
        .open("owner", "repo", Some(Token::new(TOKEN)))
        .unwrap();
    repo.fetch_pull(&PullRequestRefs {
        number: 1,
        head: None,
        base_branch: "main".into(),
        base: None,
    })
    .unwrap_err()
}

#[test]
fn a_proxy_that_asks_for_a_password_never_gets_the_token() {
    let proxy = FakeServer::proxy();
    // The proxy's URL names a user but no password, so git asks for one.
    let proxy_url = format!("http://alice@127.0.0.1:{}", proxy.port);
    let git = git_with_env(&[("https_proxy", &proxy_url), ("http_proxy", &proxy_url)]);
    let dir = TempDir::new().unwrap();
    // github.com itself, through the proxy, which answers everything.
    let cache = RepoCache::new(dir.path(), git).unwrap();

    let error = fetch_pull_one(&cache);

    assert!(!proxy.saw_the_token(), "{:#?}", proxy.requests());
    assert!(
        matches!(error, crate::Error::Fetch { .. }),
        "{error:?}: {error}"
    );
}

#[test]
fn a_server_that_asks_for_a_password_gets_the_token() {
    let server = FakeServer::asks_for_a_password();
    let dir = TempDir::new().unwrap();
    let cache = RepoCache::new(dir.path(), Git::find().unwrap())
        .unwrap()
        .with_remote_base(server.url());

    let error = fetch_pull_one(&cache);

    // It asked, got the token, and refused it.
    assert!(server.saw_the_token(), "{:#?}", server.requests());
    assert!(
        matches!(
            error,
            crate::Error::Fetch {
                failure: crate::FetchFailure::Auth,
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn a_redirect_to_another_host_is_never_followed() {
    let elsewhere = FakeServer::asks_for_a_password();
    let redirector = FakeServer::redirects_to(elsewhere.port);
    let dir = TempDir::new().unwrap();
    let cache = RepoCache::new(dir.path(), Git::find().unwrap())
        .unwrap()
        .with_remote_base(redirector.url());

    fetch_pull_one(&cache);

    assert!(!redirector.requests().is_empty());
    assert_eq!(elsewhere.requests(), Vec::<String>::new());
}

#[test]
fn a_host_redirected_to_never_gets_the_token() {
    let elsewhere = FakeServer::asks_for_a_password();
    let redirector = FakeServer::redirects_to(elsewhere.port);
    let dir = TempDir::new().unwrap();
    // Even with redirects on, the token is only for the remote's own host.
    let cache = RepoCache::new(dir.path(), Git::find().unwrap())
        .unwrap()
        .with_remote_base(redirector.url())
        .with_config("http.followRedirects=true");

    fetch_pull_one(&cache);

    assert!(
        !elsewhere.requests().is_empty(),
        "with redirects on, the request reaches the other host"
    );
    assert!(!elsewhere.saw_the_token(), "{:#?}", elsewhere.requests());
}

#[test]
fn decodes_basic_credentials() {
    let decode = |text| base64_decode(text).unwrap();
    assert_eq!(decode("eC1hY2Nlc3MtdG9rZW46YWJj"), "x-access-token:abc");
    assert_eq!(decode("YWI="), "ab");
    assert_eq!(decode("YQ=="), "a");
    assert_eq!(base64_decode("not base64!"), None);
}
