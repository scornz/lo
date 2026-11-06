//! Finds stacks among a repository's pull requests.
//!
//! A stack is a tree of open pull requests. Each layer sits on the PR below it, and the bottom
//! layer sits on a root branch with no open PR, such as `main` or `feature-x`. [`Stacks::find`]
//! builds the forest from the repo's PRs, and the result drives the stack sidebar and its walk.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::Hash;
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

/// The rule that linked a layer to the PR below it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rule {
    /// Its base branch is the head branch of the PR below. Never links to a fork's branch.
    BaseBranch,
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
    /// The rule that linked it to the PR below, or `None` when no rule did and it sits on its own
    /// base branch.
    pub rule: Option<Rule>,
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

impl Stack {
    /// The layer after `number` in the walk (`]s`), or `None` after the last.
    pub fn next(&self, number: u64) -> Option<u64> {
        let index = self.layers.iter().position(|&layer| layer == number)?;
        self.layers.get(index + 1).copied()
    }

    /// The layer before `number` in the walk (`[s`), or `None` before the first.
    pub fn previous(&self, number: u64) -> Option<u64> {
        let index = self.layers.iter().position(|&layer| layer == number)?;
        index.checked_sub(1).map(|index| self.layers[index])
    }
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
        let graph = Graph::new(prs);
        let links = graph.links();
        let placed = graph
            .prs
            .iter()
            .zip(links)
            .map(|(&pr, link)| match link {
                Some(edge) => Placed {
                    pr,
                    parent: Parent::Pr(graph.prs[edge.below].number),
                    rule: Some(edge.rule),
                },
                None => Placed {
                    pr,
                    parent: Parent::Branch(pr.base_ref.clone()),
                    rule: None,
                },
            })
            .collect();
        Self::build(placed)
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
    fn build(open: Vec<Placed>) -> Self {
        let order: HashMap<u64, (SystemTime, u64)> = open
            .iter()
            .map(|placed| (placed.pr.number, (placed.pr.created_at, placed.pr.number)))
            .collect();
        let by_creation = |numbers: &mut Vec<u64>| numbers.sort_by_key(|number| order[number]);

        let mut layers: HashMap<u64, Layer> = HashMap::with_capacity(open.len());
        let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
        let mut bottoms: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        for Placed { pr, parent, rule } in open {
            match &parent {
                Parent::Pr(below) => children.entry(*below).or_default().push(pr.number),
                Parent::Branch(branch) => {
                    bottoms.entry(branch.clone()).or_default().push(pr.number)
                }
            }
            let layer = Layer {
                number: pr.number,
                parent,
                rule,
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

/// An open PR and where it sits.
struct Placed<'a> {
    pr: &'a PullRequest,
    parent: Parent,
    rule: Option<Rule>,
}

/// The PRs the rules link: the first PR with each number, leaving out the rest.
struct Graph<'a> {
    prs: Vec<&'a PullRequest>,
}

/// A link from a PR to the PR below it, as an index into [`Graph::prs`].
#[derive(Clone, Copy, Debug)]
struct Edge {
    below: usize,
    rule: Rule,
}

impl<'a> Graph<'a> {
    fn new(prs: &'a [PullRequest]) -> Self {
        let mut seen = HashSet::new();
        let prs = prs
            .iter()
            .filter(|pr| seen.insert(pr.number) && pr.state == State::Open)
            .collect();
        Self { prs }
    }

    /// Each PR's link to the PR below it, by the first rule that applies.
    fn links(&self) -> Vec<Option<Edge>> {
        let by_head_ref = self.index(|pr| pr.head_in_base_repo.then_some(pr.head_ref.as_str()));
        self.prs
            .iter()
            .enumerate()
            .map(|(index, pr)| {
                let below = *by_head_ref.get(pr.base_ref.as_str())?;
                (below != index).then_some(Edge {
                    below,
                    rule: Rule::BaseBranch,
                })
            })
            .collect()
    }

    /// Indexes the PRs by `key`. Where several PRs share a key, keeps the one a rule links to: an
    /// open PR before a merged one, then the newest.
    fn index<K: Eq + Hash>(&self, key: impl Fn(&'a PullRequest) -> Option<K>) -> HashMap<K, usize> {
        let mut index = HashMap::with_capacity(self.prs.len());
        for (i, &pr) in self.prs.iter().enumerate() {
            if let Some(key) = key(pr) {
                index
                    .entry(key)
                    .and_modify(|best: &mut usize| {
                        if rank(pr) > rank(self.prs[*best]) {
                            *best = i;
                        }
                    })
                    .or_insert(i);
            }
        }
        index
    }
}

/// Orders the PRs a rule could link to, best last.
fn rank(pr: &PullRequest) -> (bool, SystemTime, u64) {
    (pr.state == State::Open, pr.created_at, pr.number)
}
