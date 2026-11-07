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
    answers: VecDeque<WriteAnswer>,
    /// Each mutation's operation and variables.
    mutations: Vec<(String, serde_json::Value)>,
}

impl FakeGithub {
    /// Answers the next viewed-mark mutation with `answer`.
    pub fn answer_viewed(&self, answer: WriteAnswer) {
        self.with(|marks: &mut ViewedMarks| marks.answers.push_back(answer));
    }

    /// Each viewed-mark mutation asked, with its operation and variables.
    pub fn viewed_mutations(&self) -> Vec<(String, serde_json::Value)> {
        self.with(|marks: &mut ViewedMarks| marks.mutations.clone())
    }
}

/// `markFileAsViewed` and `unmarkFileAsViewed`, each answered with the next of
/// [`FakeGithub::answer_viewed`]'s answers. With none waiting, the fake GitHub answers as for a
/// request it doesn't know.
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
    let answer = marks.answers.pop_front()?;
    marks
        .mutations
        .push((operation.to_string(), graphql.variables.clone()));
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
