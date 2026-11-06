//! The device flow on synthetic cassettes, one per way it can end. GPUI's test executor runs the
//! poll timers on a fake clock, so the tests also check when each poll was sent.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use github::device_flow::{self, DeviceFlowError};
use github::replay::ReplayHttpClient;
use gpui::{BackgroundExecutor, TestAppContext};
use http_client::http::HeaderValue;
use http_client::{AsyncBody, HttpClient, Request, Response, Url};
use parking_lot::Mutex;

/// Replays a cassette, noting when each request was sent on the test clock.
struct Timed {
    inner: Arc<ReplayHttpClient>,
    executor: BackgroundExecutor,
    sent: Mutex<Vec<(String, Instant)>>,
}

impl HttpClient for Timed {
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
        self.sent
            .lock()
            .push((request.uri().path().to_string(), self.executor.now()));
        self.inner.send(request)
    }
}

struct Run {
    http: Arc<Timed>,
    result: Result<github::AuthenticatedUser, DeviceFlowError>,
    user_code: Option<String>,
}

impl Run {
    /// Seconds after the codes were issued that each poll was sent.
    fn poll_times(&self) -> Vec<u64> {
        let sent = self.http.sent.lock();
        let start = sent[0].1;
        sent.iter()
            .filter(|(path, _)| path == "/login/oauth/access_token")
            .map(|(_, at)| (*at - start).as_secs())
            .collect()
    }
}

async fn sign_in(cassette: &str, cx: &mut TestAppContext) -> Run {
    let executor = cx.executor();
    let http = Arc::new(Timed {
        inner: ReplayHttpClient::cassette(cassette),
        executor: executor.clone(),
        sent: Mutex::default(),
    });
    let client: Arc<dyn HttpClient> = http.clone();
    let mut user_code = None;
    let result = async {
        let codes = device_flow::request_codes(&client).await?;
        user_code = Some(codes.user_code.clone());
        let token = device_flow::wait_for_token(&client, &codes, &executor).await?;
        device_flow::check_token(&client, token).await
    }
    .await;
    Run {
        http,
        result,
        user_code,
    }
}

#[gpui::test]
async fn signs_in_once_the_code_is_entered(cx: &mut TestAppContext) {
    let run = sign_in("device_flow_success", cx).await;
    let user = run.result.as_ref().unwrap();
    assert_eq!(user.user.login, "rozed-bot");
    assert_eq!(user.scopes, ["read:org", "repo", "workflow"]);
    assert_eq!(run.user_code.as_deref(), Some("FAKE-1234"));
    // Every 5 s × 1.2.
    assert_eq!(run.poll_times(), [6, 12, 18]);

    let seen = run.http.inner.seen();
    let codes = &seen[0];
    assert_eq!(codes.url, "https://github.com/login/device/code");
    assert_eq!(codes.header("accept"), Some("application/json"));
    assert_eq!(
        String::from_utf8(codes.body.clone()).unwrap(),
        "client_id=Ov23liBORqwRdiPzqsPP&scope=repo+read%3Aorg+workflow"
    );
    // The token goes only to the API, after the flow.
    let user_request = seen.last().unwrap();
    assert_eq!(user_request.url, "https://api.github.com/user");
    assert_eq!(
        user_request.header("authorization"),
        Some("token gho_FAKE_SIGNED_IN")
    );
    assert!(
        seen[..seen.len() - 1]
            .iter()
            .all(|request| request.header("authorization").is_none())
    );
}

#[gpui::test]
async fn slow_down_adopts_githubs_interval(cx: &mut TestAppContext) {
    let run = sign_in("device_flow_slow_down", cx).await;
    assert!(run.result.is_ok(), "{:?}", run.result);
    // 6 s; slow_down to 10 s, so 12 s later; slow_down without an interval adds 5 s, so 18 s
    // later; then 18 s again.
    assert_eq!(run.poll_times(), [6, 18, 36, 54]);
}

#[gpui::test]
async fn an_expired_code_ends_the_flow(cx: &mut TestAppContext) {
    let run = sign_in("device_flow_expired", cx).await;
    assert!(
        matches!(run.result, Err(DeviceFlowError::Expired)),
        "{:?}",
        run.result
    );
    assert_eq!(run.poll_times(), [6, 12]);
}

#[gpui::test]
async fn polling_stops_at_the_deadline(cx: &mut TestAppContext) {
    let run = sign_in("device_flow_deadline", cx).await;
    assert!(
        matches!(run.result, Err(DeviceFlowError::Expired)),
        "{:?}",
        run.result
    );
    // The code lasts 20 s: a fourth poll at 24 s would be too late, so it isn't sent.
    assert_eq!(run.poll_times(), [6, 12, 18]);
}

#[gpui::test]
async fn cancelling_on_github_ends_the_flow(cx: &mut TestAppContext) {
    let run = sign_in("device_flow_denied", cx).await;
    assert!(
        matches!(run.result, Err(DeviceFlowError::Denied)),
        "{:?}",
        run.result
    );
}

#[gpui::test]
async fn a_refused_poll_shows_githubs_message(cx: &mut TestAppContext) {
    let run = sign_in("device_flow_refused", cx).await;
    let Err(DeviceFlowError::Refused(message)) = &run.result else {
        panic!("{:?}", run.result);
    };
    assert_eq!(
        message,
        "The client_id and/or client_secret passed are incorrect."
    );
}

#[gpui::test]
async fn an_unknown_client_id_is_refused(cx: &mut TestAppContext) {
    let run = sign_in("device_flow_unknown_client", cx).await;
    let Err(DeviceFlowError::Refused(message)) = &run.result else {
        panic!("{:?}", run.result);
    };
    assert!(message.contains("404"), "{message}");
    assert_eq!(run.user_code, None);
}

#[gpui::test]
async fn a_token_without_every_scope_is_rejected(cx: &mut TestAppContext) {
    let run = sign_in("device_flow_missing_scope", cx).await;
    let Err(DeviceFlowError::MissingScopes(missing)) = &run.result else {
        panic!("{:?}", run.result);
    };
    assert_eq!(missing, &["workflow"]);
}

#[gpui::test]
async fn dropping_the_wait_cancels_it(cx: &mut TestAppContext) {
    let executor = cx.executor();
    let replay = ReplayHttpClient::cassette("device_flow_deadline");
    let client: Arc<dyn HttpClient> = replay.clone();
    let codes = device_flow::request_codes(&client).await.unwrap();
    let task = executor.spawn({
        let (client, executor) = (client.clone(), executor.clone());
        async move { device_flow::wait_for_token(&client, &codes, &executor).await }
    });
    executor.advance_clock(Duration::from_secs(7));
    executor.run_until_parked();
    drop(task);
    let polls = replay.seen().len();
    executor.advance_clock(Duration::from_secs(60));
    executor.run_until_parked();
    assert_eq!(replay.seen().len(), polls);
}
