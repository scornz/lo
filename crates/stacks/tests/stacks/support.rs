//! Builders for pull requests and a text rendering of the forest, so tests read as trees.

use std::time::{Duration, SystemTime};

use stacks::{PullRequest, Stacks, State};

/// An open pull request from `head` (a branch in the base repository) into `base`, with one commit,
/// `<head>.1`, created `number` seconds after the epoch.
pub fn pr(number: u64, base: &str, head: &str) -> PullRequest {
    let oid = format!("{head}.1");
    PullRequest {
        number,
        state: State::Open,
        base_ref: base.to_string(),
        head_ref: head.to_string(),
        head_in_base_repo: true,
        head_oid: oid.clone(),
        commits: vec![oid],
        created_at: at(number),
        native_stack: None,
    }
}

pub fn at(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
}

pub trait With: Sized {
    fn created(self, seconds: u64) -> Self;
    fn closed(self) -> Self;
}

impl With for PullRequest {
    fn created(mut self, seconds: u64) -> Self {
        self.created_at = at(seconds);
        self
    }

    fn closed(mut self) -> Self {
        self.state = State::Closed;
        self
    }
}

/// Renders the forest one line per root branch and layer, layers indented under what they sit on,
/// in each stack's walk order.
pub fn render(stacks: &Stacks) -> String {
    let mut lines = Vec::new();
    for root in stacks.roots() {
        lines.push(root.branch.clone());
        for stack in &root.stacks {
            assert_eq!(stack.root, root.branch);
            for &number in &stack.layers {
                let layer = stacks.layer(number).expect("every walked layer exists");
                lines.push(format!("{}#{number}", "  ".repeat(layer.depth + 1)));
            }
        }
    }
    lines.join("\n")
}

/// Finds the stacks among `prs` and checks the rendered forest against `expected`, one line each.
#[track_caller]
pub fn assert_forest(prs: &[PullRequest], expected: &[&str]) -> Stacks {
    let stacks = Stacks::find(prs);
    assert_eq!(render(&stacks), expected.join("\n"));
    stacks
}
