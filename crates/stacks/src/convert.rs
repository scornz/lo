//! Plans "Make this a GitHub stack" (design §4): the base-branch changes and the Stacks API call
//! that turn a chain of layers into a GitHub stack.
//!
//! A GitHub stack is a straight chain, so in a tree the plan converts one path: from the stack's
//! bottom layer up through the layer you're on, then on up while each layer has exactly one layer
//! on it. A fork in the tree ends the path.

use crate::{Parent, Retarget, Stacks};

/// What "Make this a GitHub stack" will do. Show the steps in its confirmation, then run them in
/// order and stop at the first that fails; [`rollback`] says how to undo the ones that ran.
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

/// Why "Make this a GitHub stack" isn't offered, in the order they're listed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Reason {
    /// The pull request asked about isn't a layer: it isn't open, or wasn't passed to
    /// [`Stacks::find`]. The only reason given when it applies.
    NotOpen,
    /// The path is already a GitHub stack, or part of one, so there's nothing to do. The only
    /// reason given when it applies.
    AlreadyStacked { stack: u64 },
    /// The repository doesn't have the Stacks API.
    NoStacksApi,
    /// The path holds one pull request, and a stack needs two. On a bottom layer with several
    /// layers on it, the tree forks right above it, so the path is that layer alone.
    TooShort,
    /// The bottom of the path is already GitHub stack `stack`, but the stack goes on above `past`
    /// with layers that aren't in the path. A stack takes new layers only at its top.
    ///
    /// Known limit: only open layers are seen. If the stack's top is a closed layer above `past`,
    /// the Stacks API refuses the add, because the first layer added must sit on the stack's top.
    StackGoesOn { stack: u64, past: u64 },
    /// A pull request in the path comes from a fork. A GitHub stack holds only pull requests from
    /// the repository itself.
    FromFork { pr: u64 },
    /// A pull request in the path is in a merge queue, which is already merging it.
    Queued { pr: u64 },
    /// A pull request in the path is already in GitHub stack `stack`, which doesn't hold the
    /// layers below it. A pull request can be in only one stack.
    InAnotherStack { pr: u64, stack: u64 },
}

/// Plans converting the path through `layer` into a GitHub stack. Each layer that isn't on the
/// branch the stack needs moves there first (its [`Layer::retarget_to`](crate::Layer)), and then
/// the stack is created.
///
/// When the bottom of the path is already a GitHub stack, the rest of the path is added to it
/// instead, and only the layers added move. A GitHub stack's own layers keep their base branches,
/// even when a closed layer between them means `retarget_to` would move one: GitHub holds them as
/// they are.
///
/// When it isn't offered, the error lists every reason in [`Reason`]'s order, except that those
/// about one pull request are grouped, bottom to top.
pub fn plan_conversion(stacks: &Stacks, layer: u64, facts: &Facts) -> Result<Plan, Vec<Reason>> {
    let Some((root, path)) = path_through(stacks, layer) else {
        return Err(vec![Reason::NotOpen]);
    };
    let (stack, stacked) = stacked_bottom(stacks, &path);
    if let Some(stack) = stack
        && stacked == path.len()
    {
        return Err(vec![Reason::AlreadyStacked { stack }]);
    }
    let added = &path[stacked..];

    let mut reasons = Vec::new();
    if !facts.stacks_api {
        reasons.push(Reason::NoStacksApi);
    }
    if path.len() < 2 {
        reasons.push(Reason::TooShort);
    }
    if let Some(stack) = stack {
        let past = path[stacked - 1];
        if goes_on(stacks, stack, past, &path) {
            reasons.push(Reason::StackGoesOn { stack, past });
        }
    }
    for (index, &pr) in path.iter().enumerate() {
        let known = &stacks.prs[&pr];
        if !known.head_in_base_repo {
            reasons.push(Reason::FromFork { pr });
        }
        if facts.queued.contains(&pr) {
            reasons.push(Reason::Queued { pr });
        }
        if let Some(entry) = known.native_stack
            && index >= stacked
        {
            let stack = entry.stack;
            reasons.push(Reason::InAnotherStack { pr, stack });
        }
    }
    if !reasons.is_empty() {
        return Err(reasons);
    }

    let mut steps: Vec<Step> = added
        .iter()
        .filter_map(|&pr| retarget(stacks, pr))
        .map(Step::Retarget)
        .collect();
    let prs = added.to_vec();
    steps.push(match stack {
        Some(stack) => Step::AddToStack { stack, prs },
        None => Step::CreateStack { prs },
    });
    Ok(Plan {
        root: root.to_string(),
        path,
        steps,
    })
}

/// How to undo the steps of a [`Plan`] that ran before one failed (design §11, "Conversion fails
/// partway").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rollback {
    /// The base-branch changes that put back the ones made, the last made first.
    pub steps: Vec<Retarget>,
    /// The stack step, if it ran. Putting base branches back doesn't undo it.
    pub kept: Option<Kept>,
}

/// A stack step that ran, which rolling back leaves in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kept {
    /// The stack was created from these pull requests. It can be unstacked:
    /// `POST /repos/{owner}/{repo}/stacks/{number}/unstack`, with the number GitHub gave it when
    /// it was created. Unstacking it, then putting the base branches back, undoes the whole plan.
    Created { prs: Vec<u64> },
    /// These pull requests were added to GitHub stack `stack`, and can't be taken out alone:
    /// unstacking removes every open layer, those that were there before too.
    Added { stack: u64, prs: Vec<u64> },
}

/// Undoes the first `done` steps of `plan`: the ones that succeeded before a step failed. If it's
/// unclear whether a step went through, count it: putting back a base branch that never changed
/// changes nothing.
pub fn rollback(plan: &Plan, done: usize) -> Rollback {
    let done = &plan.steps[..done.min(plan.steps.len())];
    let mut rollback = Rollback {
        steps: Vec::new(),
        kept: None,
    };
    for step in done.iter().rev() {
        match step {
            Step::Retarget(Retarget { pr, from, to }) => rollback.steps.push(Retarget {
                pr: *pr,
                from: to.clone(),
                to: from.clone(),
            }),
            Step::CreateStack { prs } => {
                rollback.kept = Some(Kept::Created { prs: prs.clone() });
            }
            Step::AddToStack { stack, prs } => {
                let (stack, prs) = (*stack, prs.clone());
                rollback.kept = Some(Kept::Added { stack, prs });
            }
        }
    }
    rollback
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

/// The GitHub stack the bottom layer of `path` is in, if any, and how many layers from the bottom
/// up are in it, at rising positions.
fn stacked_bottom(stacks: &Stacks, path: &[u64]) -> (Option<u64>, usize) {
    let entry = |pr: &u64| stacks.prs[pr].native_stack;
    let Some(bottom) = path.first().and_then(entry) else {
        return (None, 0);
    };
    let mut position = bottom.position;
    let above = path[1..]
        .iter()
        .take_while(|&pr| match entry(pr) {
            Some(next) if next.stack == bottom.stack && next.position > position => {
                position = next.position;
                true
            }
            _ => false,
        })
        .count();
    (Some(bottom.stack), 1 + above)
}

/// Whether GitHub stack `stack` has an open layer above `past` that isn't in `path`.
fn goes_on(stacks: &Stacks, stack: u64, past: u64, path: &[u64]) -> bool {
    let Some(top) = stacks.prs[&past].native_stack else {
        return false;
    };
    stacks.prs.iter().any(|(number, pr)| {
        let above = pr
            .native_stack
            .is_some_and(|entry| entry.stack == stack && entry.position > top.position);
        above && !path.contains(number)
    })
}

/// The base-branch change `pr` needs to be a layer of the stack, if any.
fn retarget(stacks: &Stacks, pr: u64) -> Option<Retarget> {
    let to = stacks.layer(pr)?.retarget_to.clone()?;
    let from = stacks.prs.get(&pr)?.base_ref.clone();
    Some(Retarget { pr, from, to })
}
