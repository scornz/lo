//! Finds stacks among a repository's pull requests.
//!
//! A stack is a tree of open pull requests. Each layer sits on the PR below it, and the bottom
//! layer sits on a root branch with no open PR, such as `main` or `feature-x`. [`Stacks::find`]
//! builds the forest from the repo's PRs, and the result drives the stack sidebar and its walk.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::SystemTime;

/// A pull request, with the fields stack detection reads. The doc on each field names the GraphQL
/// `PullRequest` field it comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    /// `number`.
    pub number: u64,
    /// `state`, plus what git says about a merged PR.
    pub state: State,
    /// `baseRefName`: the branch it merges into.
    pub base_ref: String,
    /// `headRefName`: the branch it merges from.
    pub head_ref: String,
    /// Whether the head branch is in the base repository: `!isCrossRepository`. False for a PR from
    /// a fork.
    pub head_in_base_repo: bool,
    /// `headRefOid`: the head branch's latest commit.
    pub head_oid: String,
    /// The PR's recent commit OIDs, oldest first: `commits(last: n)`. The last is usually the head.
    pub commits: Vec<String>,
    /// `createdAt`.
    pub created_at: SystemTime,
    /// `stackEntry`, when the PR is a layer of a GitHub stack.
    pub native_stack: Option<NativeStackEntry>,
}

/// A pull request's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum State {
    Open,
    /// Closed without merging. Closed PRs are ignored, as if they weren't passed.
    Closed,
    Merged {
        /// Whether the PR's head commit is in its base branch, which the caller works out with git.
        /// True after a merge commit or a fast-forward; false after a squash or rebase merge.
        head_in_base: bool,
    },
}

/// A pull request's place in a GitHub stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NativeStackEntry {
    /// The stack's number: `stack.number`. Stack numbers share the repo's PR numbers but are never
    /// a PR's.
    pub stack: u64,
    /// `position`: 1 is the bottom layer.
    pub position: u32,
}

/// What a layer sits on.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Parent {
    /// The open pull request below it.
    Pr(u64),
    /// A root branch: one with no open PR, such as `main` or `feature-x`.
    Branch(String),
}

/// An open pull request's place in the forest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layer {
    pub number: u64,
    pub parent: Parent,
    /// The layers sitting on this one, oldest first.
    pub children: Vec<u64>,
    /// How many layers sit below it: 0 for a stack's bottom layer.
    pub depth: usize,
}

/// One tree: a bottom layer sitting on a root branch, and every layer above it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stack {
    /// The root branch the bottom layer sits on.
    pub root: String,
    /// Every layer, in the order `]s` walks them: a pre-order walk that starts at the bottom layer
    /// and finishes each child's branch before starting the next child's.
    pub layers: Vec<u64>,
}

/// A root branch and the stacks sitting on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    pub branch: String,
    /// One stack per bottom layer, oldest bottom layer first.
    pub stacks: Vec<Stack>,
}

/// The stacks among a repository's pull requests: a forest of trees grouped by root branch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stacks {
    roots: Vec<Root>,
    layers: HashMap<u64, Layer>,
    /// Each layer's stack, as indexes into `roots` and that root's `stacks`.
    stack_index: HashMap<u64, (usize, usize)>,
}

impl Stacks {
    /// Finds the stacks among `prs`. Every open PR becomes a layer; merged PRs only link the
    /// layers above them, and closed PRs are ignored. When a number repeats, the first PR with it
    /// is used.
    pub fn find(prs: &[PullRequest]) -> Self {
        let mut seen = HashSet::new();
        let open: Vec<(&PullRequest, Parent)> = prs
            .iter()
            .filter(|pr| seen.insert(pr.number) && pr.state == State::Open)
            .map(|pr| (pr, Parent::Branch(pr.base_ref.clone())))
            .collect();
        Self::build(open)
    }

    /// The root branches, sorted by name.
    pub fn roots(&self) -> &[Root] {
        &self.roots
    }

    /// The layer for an open PR, or `None` for any other number.
    pub fn layer(&self, number: u64) -> Option<&Layer> {
        self.layers.get(&number)
    }

    /// The stack an open PR is a layer of, or `None` for any other number.
    pub fn stack_of(&self, number: u64) -> Option<&Stack> {
        let &(root, stack) = self.stack_index.get(&number)?;
        Some(&self.roots[root].stacks[stack])
    }

    /// Lays out the open PRs, each with what it sits on, as trees.
    fn build(open: Vec<(&PullRequest, Parent)>) -> Self {
        let order: HashMap<u64, (SystemTime, u64)> = open
            .iter()
            .map(|(pr, _)| (pr.number, (pr.created_at, pr.number)))
            .collect();
        let by_creation = |numbers: &mut Vec<u64>| numbers.sort_by_key(|number| order[number]);

        let mut layers: HashMap<u64, Layer> = HashMap::with_capacity(open.len());
        let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
        let mut bottoms: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        for (pr, parent) in open {
            match &parent {
                Parent::Pr(below) => children.entry(*below).or_default().push(pr.number),
                Parent::Branch(branch) => {
                    bottoms.entry(branch.clone()).or_default().push(pr.number)
                }
            }
            let layer = Layer {
                number: pr.number,
                parent,
                children: Vec::new(),
                depth: 0,
            };
            layers.insert(pr.number, layer);
        }
        for (number, mut above) in children {
            by_creation(&mut above);
            if let Some(layer) = layers.get_mut(&number) {
                layer.children = above;
            }
        }

        let mut stacks = Self {
            roots: Vec::with_capacity(bottoms.len()),
            layers,
            stack_index: HashMap::new(),
        };
        for (branch, mut bottoms) in bottoms {
            by_creation(&mut bottoms);
            let root = stacks.roots.len();
            let mut root_stacks = Vec::with_capacity(bottoms.len());
            for bottom in bottoms {
                let walk = stacks.walk_from(bottom);
                for &number in &walk {
                    stacks.stack_index.insert(number, (root, root_stacks.len()));
                }
                root_stacks.push(Stack {
                    root: branch.clone(),
                    layers: walk,
                });
            }
            stacks.roots.push(Root {
                branch,
                stacks: root_stacks,
            });
        }
        stacks
    }

    /// Walks the tree above `bottom` in pre-order, setting each layer's depth on the way.
    fn walk_from(&mut self, bottom: u64) -> Vec<u64> {
        let mut walk = Vec::new();
        let mut pending = vec![(bottom, 0)];
        while let Some((number, depth)) = pending.pop() {
            let Some(layer) = self.layers.get_mut(&number) else {
                continue;
            };
            layer.depth = depth;
            walk.push(number);
            pending.extend(layer.children.iter().rev().map(|&child| (child, depth + 1)));
        }
        walk
    }
}
