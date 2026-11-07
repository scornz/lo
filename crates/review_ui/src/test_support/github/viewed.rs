//! GitHub's viewed marks: `markFileAsViewed` and `unmarkFileAsViewed`, answered as each test says.

use std::collections::VecDeque;

use super::{Answer, Asked, FakeGithub, State};

/// How GitHub answers a viewed-mark mutation.
#[derive(Clone, Copy, Debug)]
pub enum WriteAnswer {
    Done,
    Forbidden,
    RateLimited,
    Offline,
}

/// The answers waiting for viewed-mark mutations, and the mutations asked.
#[derive(Default)]
struct ViewedMarks {
    /// Whether the test answers viewed-mark mutations, so each one must have an answer waiting.
    answering: bool,
    answers: VecDeque<WriteAnswer>,
    /// Each mutation's operation and variables.
    mutations: Vec<(String, serde_json::Value)>,
}

impl FakeGithub {
    /// Answers viewed-mark mutations with the answers [`Self::answer_viewed`] queues, and fails the
    /// test on one with none queued. Until then, or this, the fake answers them as requests it
    /// doesn't know.
    pub fn answer_viewed_marks(&self) {
        self.with(|marks: &mut ViewedMarks| marks.answering = true);
    }

    /// Answers the next viewed-mark mutation with `answer` ([`Self::answer_viewed_marks`]).
    pub fn answer_viewed(&self, answer: WriteAnswer) {
        self.with(|marks: &mut ViewedMarks| {
            marks.answering = true;
            marks.answers.push_back(answer);
        });
    }

    /// Each viewed-mark mutation asked, with its operation and variables.
    pub fn viewed_mutations(&self) -> Vec<(String, serde_json::Value)> {
        self.with(|marks: &mut ViewedMarks| marks.mutations.clone())
    }
}

/// `markFileAsViewed` and `unmarkFileAsViewed`, recorded and answered with the next of
/// [`FakeGithub::answer_viewed`]'s answers, once the test answers them. One with no answer queued
/// fails the test, since nothing it expected sends one.
pub(super) fn mark_file_as_viewed(
    state: &mut State,
    asked: &Asked,
) -> Option<anyhow::Result<Answer>> {
    let graphql = asked.graphql.as_ref()?;
    let operation = graphql.operation.as_str();
    if operation != "MarkFileAsViewed" && operation != "UnmarkFileAsViewed" {
        return None;
    }
    let marks = state.get::<ViewedMarks>();
    if !marks.answering {
        return None;
    }
    marks
        .mutations
        .push((operation.to_string(), graphql.variables.clone()));
    let Some(answer) = marks.answers.pop_front() else {
        panic!(
            "{operation} was sent with no answer queued for it: {}",
            graphql.variables
        );
    };
    let field = format!("{}{}", operation[..1].to_lowercase(), &operation[1..]);
    let field = field.as_str();
    let (remaining, json) = match answer {
        WriteAnswer::Offline => return Some(Err(anyhow::anyhow!("the network is down"))),
        WriteAnswer::Done => (
            "4999",
            serde_json::json!({ "data": { field: { "pullRequest": { "id": "PR_x" } } } }),
        ),
        WriteAnswer::Forbidden => (
            "4999",
            serde_json::json!({
                "data": { field: null },
                "errors": [{
                    "type": "FORBIDDEN",
                    "path": [field],
                    "message": "Resource not accessible by integration",
                }],
            }),
        ),
        WriteAnswer::RateLimited => (
            "0",
            serde_json::json!({
                "errors": [{
                    "type": "RATE_LIMITED",
                    "message": "API rate limit exceeded for user ID 1.",
                }],
            }),
        ),
    };
    Some(Ok(Answer::new(200, json.to_string())
        .header("content-type", "application/json")
        .header("x-ratelimit-remaining", remaining)))
}
