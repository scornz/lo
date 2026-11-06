//! An HTTP client that serves recorded GitHub responses, so tests run offline.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use futures::AsyncReadExt as _;
use futures::future::BoxFuture;
use http_client::http::{HeaderMap, HeaderValue};
use http_client::{AsyncBody, HttpClient, Request, Response, Url};
use parking_lot::Mutex;

use crate::cassette::{self, Interaction, RecordedResponse};

/// Serves a cassette's responses: each request gets the next recorded response for its key, and
/// once only one is left it's repeated, the steady state of a poll. A request with no recorded
/// response panics, naming its key and the keys the cassette has.
#[derive(Clone)]
pub struct ReplayHttpClient(Arc<Mutex<Replay>>);

struct Replay {
    name: String,
    by_key: HashMap<String, VecDeque<Interaction>>,
    seen: Vec<SeenRequest>,
}

/// A request the replaying client received.
#[derive(Debug, Clone)]
pub struct SeenRequest {
    pub method: String,
    pub url: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl SeenRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }
}

impl ReplayHttpClient {
    /// Replays the cassette `name` from the github crate's fixtures.
    pub fn cassette(name: &str) -> Arc<Self> {
        let dir = cassette::fixtures_dir().join(name);
        Arc::new(
            Self::from_dir(&dir)
                .unwrap_or_else(|error| panic!("loading cassette {name}: {error:#}")),
        )
    }

    pub fn from_dir(dir: &Path) -> Result<Self> {
        let (_, interactions) = cassette::read_cassette(dir)?;
        let name = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self::from_interactions(name, interactions.into_iter().map(|(_, i)| i))
    }

    pub fn from_interactions(
        name: impl Into<String>,
        interactions: impl IntoIterator<Item = Interaction>,
    ) -> Result<Self> {
        let mut by_key: HashMap<String, VecDeque<Interaction>> = HashMap::new();
        for interaction in interactions {
            let key = cassette::recorded_key(&interaction.request)?;
            by_key.entry(key).or_default().push_back(interaction);
        }
        Ok(Self(Arc::new(Mutex::new(Replay {
            name: name.into(),
            by_key,
            seen: Vec::new(),
        }))))
    }

    /// Every request received so far, in order.
    pub fn seen(&self) -> Vec<SeenRequest> {
        self.0.lock().seen.clone()
    }

    fn respond(&self, request: SeenRequest) -> Result<Response<AsyncBody>> {
        let url = Url::parse(&request.url)?;
        let key = cassette::key(&request.method, &url, &request.body);
        let graphql = cassette::graphql_body(&url, &request.body);
        let mut replay = self.0.lock();
        let name = replay.name.clone();
        replay.seen.push(request);
        let Some(queue) = replay.by_key.get_mut(&key) else {
            let mut known: Vec<_> = replay.by_key.keys().cloned().collect();
            known.sort();
            panic!(
                "cassette {name} has no response for\n  {key}\nIt has:\n  {}",
                known.join("\n  ")
            );
        };
        let interaction = if queue.len() > 1 {
            queue.pop_front().expect("not empty")
        } else {
            queue.front().expect("never empty").clone()
        };
        if let (Some(graphql), Some(recorded)) = (graphql, &interaction.request.query_sha256)
            && cassette::sha256(&graphql.query) != *recorded
        {
            panic!(
                "re-record cassette {name}: the {} query changed since it was recorded",
                graphql.operation_name
            );
        }
        Ok(to_response(&interaction.response))
    }
}

fn to_response(recorded: &RecordedResponse) -> Response<AsyncBody> {
    let mut builder = Response::builder().status(recorded.status);
    for (name, value) in &recorded.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    builder
        .body(AsyncBody::from(recorded.body_bytes()))
        .expect("a recorded response is valid")
}

impl HttpClient for ReplayHttpClient {
    fn user_agent(&self) -> Option<&HeaderValue> {
        None
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn send(&self, request: Request<AsyncBody>) -> BoxFuture<'static, Result<Response<AsyncBody>>> {
        let this = self.clone();
        Box::pin(async move {
            let (parts, mut body) = request.into_parts();
            let mut bytes = Vec::new();
            body.read_to_end(&mut bytes).await?;
            this.respond(SeenRequest {
                method: parts.method.to_string(),
                url: parts.uri.to_string(),
                headers: parts.headers,
                body: bytes,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::cassette::RecordedRequest;

    fn interaction(method: &str, url: &str, status: u16, body: &str) -> Interaction {
        Interaction {
            request: RecordedRequest {
                method: method.into(),
                url: url.into(),
                ..Default::default()
            },
            response: RecordedResponse {
                status,
                headers: [("etag".to_string(), format!("\"{status}\""))].into(),
                body: None,
                body_text: Some(body.into()),
            },
        }
    }

    fn get(client: &ReplayHttpClient, url: &str) -> (u16, String, Option<String>) {
        let request = Request::get(url).body(AsyncBody::empty()).unwrap();
        let mut response = futures::executor::block_on(client.send(request)).unwrap();
        let mut body = String::new();
        futures::executor::block_on(response.body_mut().read_to_string(&mut body)).unwrap();
        let etag = response
            .headers()
            .get("etag")
            .map(|v| v.to_str().unwrap().to_string());
        (response.status().as_u16(), body, etag)
    }

    #[test]
    fn serves_responses_in_order_then_repeats_the_last() {
        let url = "https://api.github.com/repos/o/r/pulls/1";
        let client = ReplayHttpClient::from_interactions(
            "t",
            [
                interaction("GET", url, 200, "first"),
                interaction("GET", url, 304, ""),
            ],
        )
        .unwrap();
        assert_eq!(
            get(&client, url),
            (200, "first".into(), Some("\"200\"".into()))
        );
        assert_eq!(get(&client, url).0, 304);
        assert_eq!(get(&client, url).0, 304);
        assert_eq!(client.seen().len(), 3);
    }

    #[test]
    fn keeps_what_each_request_sent() {
        let url = "https://api.github.com/user";
        let client =
            ReplayHttpClient::from_interactions("t", [interaction("GET", url, 200, "{}")]).unwrap();
        let request = Request::get(url)
            .header("Authorization", "token fake")
            .body(AsyncBody::empty())
            .unwrap();
        futures::executor::block_on(client.send(request)).unwrap();
        let seen = client.seen();
        assert_eq!(seen[0].method, "GET");
        assert_eq!(seen[0].url, url);
        assert_eq!(seen[0].header("authorization"), Some("token fake"));
    }

    #[test]
    #[should_panic(expected = "has no response for\n  GET https://api.github.com/other")]
    fn an_unexpected_request_fails_the_test() {
        let client = ReplayHttpClient::from_interactions(
            "t",
            [interaction("GET", "https://api.github.com/user", 200, "")],
        )
        .unwrap();
        get(&client, "https://api.github.com/other");
    }

    #[test]
    #[should_panic(expected = "re-record cassette t: the A query changed")]
    fn a_changed_query_asks_for_a_new_recording() {
        let mut recorded = interaction("POST", "https://api.github.com/graphql", 200, "{}");
        recorded.request.operation = Some("A".into());
        recorded.request.variables = Some(serde_json::json!({}));
        recorded.request.query_sha256 = Some(cassette::sha256("query A { old }"));
        let client = ReplayHttpClient::from_interactions("t", [recorded]).unwrap();
        let body = r#"{"query":"query A { new }","operationName":"A","variables":{}}"#;
        let request = Request::post("https://api.github.com/graphql")
            .body(AsyncBody::from(body))
            .unwrap();
        let _ = futures::executor::block_on(client.send(request));
    }
}
