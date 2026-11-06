//! Builders for pull requests and a text rendering of the forest, so tests read as trees.

use std::time::{Duration, SystemTime};

use stacks::{NativeStackEntry, PullRequest, Rule, Stacks, State};

/// An open pull request from `head` (a branch in the base repository) into `base`, with one commit,
/// `<head>.1`, created `number` seconds after the epoch. The base branch points at `<base>.1`, a
/// PR from that branch's default head.
pub fn pr(number: u64, base: &str, head: &str) -> PullRequest {
    let oid = format!("{head}.1");
    PullRequest {
        number,
        state: State::Open,
        base_ref: base.to_string(),
        base_oid: format!("{base}.1"),
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
    /// Sets the commits, oldest first. The last is the head.
    fn commits(self, commits: &[&str]) -> Self;
    /// Sets the commit the base branch points at.
    fn base_at(self, oid: &str) -> Self;
    fn created(self, seconds: u64) -> Self;
    fn closed(self) -> Self;
    fn merged(self, head_in_base: bool) -> Self;
    /// Makes it a PR from a fork, whose head branch isn't in the base repository.
    fn fork(self) -> Self;
    /// Makes it the layer at `position` of GitHub stack `stack`.
    fn native(self, stack: u64, position: u32) -> Self;
}

impl With for PullRequest {
    fn commits(mut self, commits: &[&str]) -> Self {
        self.commits = commits.iter().map(|oid| oid.to_string()).collect();
        self.head_oid = self.commits.last().cloned().unwrap_or_default();
        self
    }

    fn base_at(mut self, oid: &str) -> Self {
        self.base_oid = oid.to_string();
        self
    }

    fn created(mut self, seconds: u64) -> Self {
        self.created_at = at(seconds);
        self
    }

    fn closed(mut self) -> Self {
        self.state = State::Closed;
        self
    }

    fn merged(mut self, head_in_base: bool) -> Self {
        self.state = State::Merged { head_in_base };
        self
    }

    fn fork(mut self) -> Self {
        self.head_in_base_repo = false;
        self
    }

    fn native(mut self, stack: u64, position: u32) -> Self {
        self.native_stack = Some(NativeStackEntry { stack, position });
        self
    }
}

/// Renders the forest one line per root branch and layer, layers indented under what they sit on,
/// in each stack's walk order. A layer's line ends with the rule that linked it and any rebase it
/// needs.
pub fn render(stacks: &Stacks) -> String {
    let mut lines = Vec::new();
    for root in stacks.roots() {
        lines.push(root.branch.clone());
        for stack in &root.stacks {
            assert_eq!(stack.root, root.branch);
            for &number in &stack.layers {
                let layer = stacks.layer(number).expect("every walked layer exists");
                let mut line = format!("{}#{number}", "  ".repeat(layer.depth + 1));
                if let Some(rule) = layer.rule {
                    line.push(' ');
                    line.push_str(match rule {
                        Rule::NativeStack => "native-stack",
                        Rule::BaseBranch => "base-branch",
                        Rule::SharedCommits => "shared-commits",
                    });
                }
                if let Some(rebase) = &layer.needs_rebase {
                    line.push_str(&format!(
                        " needs-rebase(#{} {})",
                        rebase.merged, rebase.compare_against
                    ));
                }
                lines.push(line);
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
