//! Plans "Make this a GitHub stack" (design §4): the base-branch changes and the Stacks API call
//! that turn a chain of layers into a GitHub stack.
//!
//! A GitHub stack is a straight chain, so in a tree the plan converts one path: from the stack's
//! bottom layer up through the layer you're on, then on up while each layer has exactly one layer
//! on it. A fork in the tree ends the path.

use crate::{Parent, Retarget, Stacks};

/// What "Make this a GitHub stack" will do. Show the steps in its confirmation, then run them in
/// order and stop at the first that fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The root branch the stack sits on: the bottom layer's base branch once the plan has run.
    pub root: String,
    /// The pull requests it converts, bottom to top.
    pub path: Vec<u64>,
    /// The steps, in the order to run them: every base-branch change, bottom layer first, then one
    /// stack step, last.
    pub steps: Vec<Step>,
}

/// One API call in a [`Plan`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Change a pull request's base branch to the head branch of the layer below it, or to the root
    /// branch for the bottom layer, as each layer of a GitHub stack must have.
    Retarget(Retarget),
    /// Create a GitHub stack from these pull requests, bottom to top:
    /// `POST /repos/{owner}/{repo}/stacks` with `{"pull_requests": prs}`.
    CreateStack { prs: Vec<u64> },
    /// Add these pull requests, bottom to top, to the top of GitHub stack `stack`:
    /// `POST /repos/{owner}/{repo}/stacks/{stack}/add` with `{"pull_requests": prs}`.
    AddToStack { stack: u64, prs: Vec<u64> },
}

/// What only the caller knows, beyond the stacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facts<'a> {
    /// Whether the repository has the Stacks API: false once `GET /repos/{owner}/{repo}/stacks`
    /// has answered 404, as on a GitHub Enterprise Server without stacks. GitHub has no setting that
    /// turns stacks on.
    pub stacks_api: bool,
    /// The open pull requests in a merge queue: `isInMergeQueue`.
    pub queued: &'a [u64],
}

/// Why "Make this a GitHub stack" isn't offered.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Reason {
    /// The pull request asked about isn't a layer: it isn't open, or wasn't passed to
    /// [`Stacks::find`]. The only reason given when it applies.
    NotOpen,
    /// The repository doesn't have the Stacks API.
    NoStacksApi,
    /// The path holds one pull request, and a stack needs two. On a bottom layer with several
    /// layers on it, the tree forks right above it, so the path is that layer alone.
    TooShort,
    /// A pull request in the path comes from a fork. A GitHub stack holds only pull requests from
    /// the repository itself.
    FromFork { pr: u64 },
    /// A pull request in the path is in a merge queue, which is already merging it.
    Queued { pr: u64 },
}

/// Plans converting the path through `layer` into a GitHub stack. Each layer that isn't on the
/// branch the stack needs moves there first (its [`Layer::retarget_to`](crate::Layer)), and then
/// the stack is created.
///
/// When it isn't offered, the error lists every reason: those about the repository and the path
/// first, then those about each pull request in it, bottom to top.
pub fn plan_conversion(stacks: &Stacks, layer: u64, facts: &Facts) -> Result<Plan, Vec<Reason>> {
    let Some((root, path)) = path_through(stacks, layer) else {
        return Err(vec![Reason::NotOpen]);
    };
    let mut reasons = Vec::new();
    if !facts.stacks_api {
        reasons.push(Reason::NoStacksApi);
    }
    if path.len() < 2 {
        reasons.push(Reason::TooShort);
    }
    for &pr in &path {
        if !stacks.prs[&pr].head_in_base_repo {
            reasons.push(Reason::FromFork { pr });
        }
        if facts.queued.contains(&pr) {
            reasons.push(Reason::Queued { pr });
        }
    }
    if !reasons.is_empty() {
        return Err(reasons);
    }
    let mut steps: Vec<Step> = path
        .iter()
        .filter_map(|&pr| retarget(stacks, pr))
        .map(Step::Retarget)
        .collect();
    steps.push(Step::CreateStack { prs: path.clone() });
    Ok(Plan {
        root: root.to_string(),
        path,
        steps,
    })
}

/// The path through `layer`, bottom to top, and the root branch it sits on. `None` when `layer`
/// isn't a layer.
fn path_through(stacks: &Stacks, layer: u64) -> Option<(&str, Vec<u64>)> {
    let mut path = Vec::new();
    let mut at = stacks.layer(layer)?;
    // `Stacks::find` broke every cycle, so both walks end.
    let root = loop {
        path.push(at.number);
        match &at.parent {
            Parent::Pr(below) => at = stacks.layer(*below)?,
            Parent::Branch(root) => break root,
        }
    };
    path.reverse();
    let mut at = stacks.layer(layer)?;
    while let [above] = at.children[..] {
        path.push(above);
        at = stacks.layer(above)?;
    }
    Some((root, path))
}

/// The base-branch change `pr` needs to be a layer of the stack, if any.
fn retarget(stacks: &Stacks, pr: u64) -> Option<Retarget> {
    let to = stacks.layer(pr)?.retarget_to.clone()?;
    let from = stacks.prs.get(&pr)?.base_ref.clone();
    Some(Retarget { pr, from, to })
}
