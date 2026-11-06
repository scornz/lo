//! Builders for pull requests and text renderings of the forest and of plans, so tests read as
//! trees and as the confirmations rozed shows.

use std::time::{Duration, SystemTime};

use stacks::convert::{Facts, Plan, Reason, Step, plan_conversion};
use stacks::{NativeStackEntry, PullRequest, Retarget, Rule, Stacks, State};

/// An open pull request from `head` (a branch in the base repository) into `base`, with one commit,
/// `<head>.1`, created `number` seconds after the epoch. The base branch's tip is `<base>.1`, the
/// default head of a PR from that branch.
pub fn pr(number: u64, base: &str, head: &str) -> PullRequest {
    let oid = format!("{head}.1");
    PullRequest {
        number,
        state: State::Open,
        base_ref: base.to_string(),
        base_tip: Some(format!("{base}.1")),
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
    /// Lists no commits, keeping the head.
    fn no_commits(self) -> Self;
    /// Sets the base branch's tip.
    fn base_at(self, oid: &str) -> Self;
    /// Makes the base branch deleted, so it has no tip.
    fn base_deleted(self) -> Self;
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

    fn no_commits(mut self) -> Self {
        self.commits.clear();
        self
    }

    fn base_at(mut self, oid: &str) -> Self {
        self.base_tip = Some(oid.to_string());
        self
    }

    fn base_deleted(mut self) -> Self {
        self.base_tip = None;
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

/// Plans converting the path through `layer`, among `prs` with no trunk branches, in a repo with
/// the Stacks API and nothing in its merge queue.
pub fn plan(prs: &[PullRequest], layer: u64) -> Result<Plan, Vec<Reason>> {
    let facts = Facts {
        stacks_api: true,
        queued: &[],
    };
    plan_conversion(&Stacks::find(prs, &[]), layer, &facts)
}

/// Plans converting the path through `layer` and checks the rendered plan against `expected`, one
/// line each.
#[track_caller]
pub fn assert_plan(prs: &[PullRequest], layer: u64, expected: &[&str]) -> Plan {
    let plan = plan(prs, layer).expect("conversion is offered");
    assert_eq!(render_plan(&plan), expected);
    plan
}

/// Renders a conversion plan as its confirmation lists it: the root branch and the path, then one
/// line per step.
pub fn render_plan(plan: &Plan) -> Vec<String> {
    let mut lines = vec![format!("{}: {}", plan.root, numbers(&plan.path))];
    lines.extend(plan.steps.iter().map(|step| match step {
        Step::Retarget(retarget) => render_retarget(retarget),
        Step::CreateStack { prs } => format!("create {}", numbers(prs)),
        Step::AddToStack { stack, prs } => format!("add {} to stack {stack}", numbers(prs)),
    }));
    lines
}

/// `#2 main -> a`.
pub fn render_retarget(retarget: &Retarget) -> String {
    let Retarget { pr, from, to } = retarget;
    format!("#{pr} {from} -> {to}")
}

/// `#1 #2 #3`.
pub fn numbers(prs: &[u64]) -> String {
    let numbers: Vec<String> = prs.iter().map(|pr| format!("#{pr}")).collect();
    numbers.join(" ")
}

/// Finds the stacks among `prs`, with no trunk branches, and checks the rendered forest against
/// `expected`, one line each.
#[track_caller]
pub fn assert_forest(prs: &[PullRequest], expected: &[&str]) -> Stacks {
    assert_trunk_forest(prs, &[], expected)
}

/// Like [`assert_forest`], with trunk branches.
#[track_caller]
pub fn assert_trunk_forest(prs: &[PullRequest], trunks: &[&str], expected: &[&str]) -> Stacks {
    let stacks = Stacks::find(prs, trunks);
    assert_eq!(render(&stacks), expected.join("\n"));
    stacks
}
