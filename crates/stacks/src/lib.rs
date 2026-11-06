//! Finds stacks among a repository's pull requests.
//!
//! A stack is a tree of open pull requests. Each layer sits on the PR below it, and the bottom
//! layer sits on a root branch with no open PR, such as `main` or `feature-x`. [`Stacks::find`]
//! builds the forest from the repo's PRs, and the result drives the stack sidebar and its walk.
//!
//! ```
//! use std::time::SystemTime;
//! use stacks::{Parent, PullRequest, Rule, Stacks, State};
//!
//! let pr = |number: u64, base: &str, head: &str| PullRequest {
//!     number,
//!     state: State::Open,
//!     base_ref: base.into(),
//!     base_tip: Some(format!("{base}-tip")),
//!     head_ref: head.into(),
//!     head_in_base_repo: true,
//!     head_oid: format!("{head}-tip"),
//!     commits: vec![format!("{head}-tip")],
//!     created_at: SystemTime::UNIX_EPOCH,
//!     native_stack: None,
//! };
//! let stacks = Stacks::find(&[pr(1, "main", "a"), pr(2, "a", "b")], &["main"]);
//!
//! let layer = stacks.layer(2).unwrap();
//! assert_eq!(layer.parent, Parent::Pr(1));
//! assert_eq!(layer.rule, Some(Rule::BaseBranch));
//! let stack = stacks.stack_of(2).unwrap();
//! assert_eq!(stack.root, "main");
//! assert_eq!(stack.layers, [1, 2]);
//! ```

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::Hash;
use std::time::SystemTime;

/// A pull request, with the fields stack detection reads. The doc on each field names the GraphQL
/// `PullRequest` field it comes from.
///
/// Pass the repo's open PRs, its recently merged ones, and the merged PRs from the branches open
/// PRs target: [`branches_to_look_up`] lists the branches still to fetch
/// (`pullRequests(headRefName:, states: MERGED)`). Merged PRs link the layers above them, so
/// without them a layer whose lower layer merged sits on that layer's branch as a root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    /// `number`.
    pub number: u64,
    /// `state`, plus what git says about a merged PR.
    pub state: State,
    /// `baseRefName`: the branch it merges into.
    pub base_ref: String,
    /// The base branch's tip right now: `baseRef { target { oid } }`, or `None` once the branch is
    /// deleted. Not `baseRefOid`, which only moves when the PR's head does, so it's stale on most
    /// open PRs.
    pub base_tip: Option<String>,
    /// `headRefName`: the branch it merges from.
    pub head_ref: String,
    /// Whether the head branch is in the base repository: `!isCrossRepository`. False for a PR from
    /// a fork.
    pub head_in_base_repo: bool,
    /// `headRefOid`: the head branch's latest commit.
    pub head_oid: String,
    /// The PR's recent commit OIDs, oldest first: `commits(last: n)`. The last is the head.
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
        /// Whether the PR's head commit is in its base branch: true after a merge commit or a
        /// fast-forward, false after a squash or rebase merge. From GraphQL, it's true when
        /// `headRefOid` is one of `mergeCommit { parents }` or is `mergeCommit { oid }`.
        ///
        /// Known limit: this is about the branch the PR merged into. A PR merge-committed into
        /// `feature-x`, which was then squash-merged into `main`, still counts as in its base,
        /// so the layers above it aren't marked as needing a rebase.
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

/// Lists the branches whose merged PRs [`Stacks::find`] still needs. These are the branches that
/// open PRs target, or that merged PRs leading down from them target, which no open or merged PR
/// in `prs` comes from and which aren't trunks. Sorted by name.
pub fn branches_to_look_up(prs: &[PullRequest], trunks: &[&str]) -> Vec<String> {
    let from_base_repo = prs.iter().filter(|pr| pr.head_in_base_repo);
    let mut open_heads = HashSet::new();
    let mut merged_by_head: HashMap<&str, Vec<&str>> = HashMap::new();
    for pr in from_base_repo {
        match pr.state {
            State::Open => _ = open_heads.insert(pr.head_ref.as_str()),
            State::Merged { .. } => merged_by_head
                .entry(pr.head_ref.as_str())
                .or_default()
                .push(pr.base_ref.as_str()),
            State::Closed => {}
        }
    }
    let mut pending: Vec<&str> = prs
        .iter()
        .filter(|pr| pr.state == State::Open)
        .map(|pr| pr.base_ref.as_str())
        .collect();
    let mut seen = HashSet::new();
    let mut missing = BTreeSet::new();
    while let Some(branch) = pending.pop() {
        if !seen.insert(branch) || trunks.contains(&branch) || open_heads.contains(branch) {
            continue;
        }
        match merged_by_head.get(branch) {
            Some(bases) => pending.extend(bases),
            None => _ = missing.insert(branch),
        }
    }
    missing.into_iter().map(String::from).collect()
}

/// The rule that linked a layer to the PR below it. When several rules apply, the first in this
/// order wins, so they sort strongest first. When a rule matches several PRs equally well, an open
/// PR wins over a merged one, then the newest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rule {
    /// The PR below is the next layer down in the same GitHub stack. Closed layers, and layers
    /// not passed, are skipped, so a closed middle layer doesn't split the stack. The bottom layer
    /// passed links by the other rules.
    NativeStack,
    /// Its base branch is the head branch of the PR below. Never links to a fork's branch, nor an
    /// open PR to a merged PR's branch that has moved on since the merge.
    BaseBranch,
    /// Its commits contain the head commit of the open PR below, or any commit of a squash- or
    /// rebase-merged PR below. If several PRs match, the nearest wins: the one matching its latest
    /// commit. A PR with the same head commit isn't below it, and neither is a merged PR whose
    /// head is in its base branch.
    SharedCommits,
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
    /// What it sits on. When its rule matched a merged PR, it sits where that PR sat: on the PR or
    /// root branch the merged PR's own link leads to.
    pub parent: Parent,
    /// The rule that linked it to the PR below, which may be a merged PR. `None` when no rule did,
    /// or its link was dropped to break a cycle; it then sits on its own base branch.
    pub rule: Option<Rule>,
    /// Set when a merged PR below it was squash- or rebase-merged, so this layer still carries
    /// commits that never landed as they are. Never set on a layer linked by
    /// [`Rule::NativeStack`], because GitHub rebases those itself.
    ///
    /// Known limit: a layer rebased onto the root branch but still targeting the merged PR's
    /// branch is still marked, because its base branch links it to the merged PR.
    pub needs_rebase: Option<NeedsRebase>,
    /// The base branch "Make this a GitHub stack" must give it: the head branch of the PR below,
    /// or the root branch. `None` when its base branch is already that, or when the PR below is
    /// from a fork, whose branch can't be a base.
    pub retarget_to: Option<String>,
    /// The layers sitting on this one, oldest first.
    pub children: Vec<u64>,
    /// How many layers sit below it: 0 for a stack's bottom layer.
    pub depth: usize,
}

/// Why a layer needs a rebase, and what to compare it against until then.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NeedsRebase {
    /// The merged PR the layer's rule matched: the nearest merged PR below it.
    pub merged: u64,
    /// The commit to compare the layer against, so changes that already landed stay hidden: the
    /// newest of the merged PR's commits that the layer lists, or the merged PR's head if it lists
    /// none.
    pub compare_against: String,
}

/// One tree: a bottom layer sitting on a root branch, and every layer above it. Each PR sitting
/// directly on a root branch starts its own stack, whether the root is `main` or `feature-x`.
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

/// Links that formed a cycle. [`Stacks::find`] breaks each cycle by dropping one link: the one
/// found by the weakest rule (shared commits, then base branch, then native stack), and of those
/// the oldest PR's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cycle {
    /// The PRs in the cycle, each linked to the next and the last back to the first, starting with
    /// the PR whose link was dropped. That PR sits on its own base branch instead.
    pub prs: Vec<u64>,
}

/// The stacks among a repository's pull requests: a forest of trees grouped by root branch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stacks {
    roots: Vec<Root>,
    layers: HashMap<u64, Layer>,
    /// Each layer's stack, as indexes into `roots` and that root's `stacks`.
    stack_index: HashMap<u64, (usize, usize)>,
    cycles: Vec<Cycle>,
}

impl Stacks {
    /// Finds the stacks among `prs`. Every open PR becomes a layer; merged PRs only link the
    /// layers above them, and closed PRs are ignored. When a number repeats, the first PR with it
    /// is used.
    ///
    /// `trunks` are the repo's long-lived branches: its default branch plus protected or
    /// ruleset-covered ones. They're always roots: a PR from a trunk, such as a deploy PR
    /// `production <- main` or a back-merge `develop <- main`, is never below another PR. A fork's
    /// branch is never a trunk, whatever its name.
    pub fn find(prs: &[PullRequest], trunks: &[&str]) -> Self {
        let graph = Graph::new(prs, trunks);
        let mut links = graph.links();
        let cycles = graph.break_cycles(&mut links);
        let placed = (0..graph.prs.len())
            .filter(|&index| graph.prs[index].state == State::Open)
            .map(|index| graph.place(index, &links))
            .collect();
        Self {
            cycles,
            ..Self::build(placed, trunks)
        }
    }

    /// The root branches: the trunks passed to [`Stacks::find`], in that order, then the rest
    /// sorted by name. Only branches with a layer on them are listed.
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

    /// The cycles found and broken, in the order found.
    pub fn cycles(&self) -> &[Cycle] {
        &self.cycles
    }

    /// Lays out the open PRs, each with what it sits on, as trees.
    fn build(open: Vec<Placed>, trunks: &[&str]) -> Self {
        let order: HashMap<u64, (SystemTime, u64)> = open
            .iter()
            .map(|placed| (placed.pr.number, (placed.pr.created_at, placed.pr.number)))
            .collect();
        let by_creation = |numbers: &mut Vec<u64>| numbers.sort_by_key(|number| order[number]);

        let mut layers: HashMap<u64, Layer> = HashMap::with_capacity(open.len());
        let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
        let mut bottoms: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        for Placed {
            pr,
            parent,
            rule,
            needs_rebase,
            retarget_to,
        } in open
        {
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
                needs_rebase,
                retarget_to,
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
            cycles: Vec::new(),
        };
        let mut others = bottoms;
        let trunks: Vec<_> = trunks
            .iter()
            .filter_map(|&trunk| others.remove_entry(trunk))
            .collect();
        for (branch, mut bottoms) in trunks.into_iter().chain(others) {
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
    needs_rebase: Option<NeedsRebase>,
    retarget_to: Option<String>,
}

/// The PRs the rules link: the first open or merged PR with each number.
struct Graph<'a> {
    prs: Vec<&'a PullRequest>,
    trunks: HashSet<&'a str>,
}

/// Each rule's lookup from what it matches to the PR below, as indexes into [`Graph::prs`].
struct Lookups<'a> {
    /// By GitHub stack and position.
    position: BTreeMap<(u64, u32), usize>,
    head_ref: HashMap<&'a str, usize>,
    commit: HashMap<&'a str, usize>,
}

/// A link from a PR to the PR below it, as an index into [`Graph::prs`].
#[derive(Clone, Copy, Debug)]
struct Edge {
    below: usize,
    rule: Rule,
}

impl<'a> Graph<'a> {
    fn new(prs: &'a [PullRequest], trunks: &[&'a str]) -> Self {
        let mut seen = HashSet::new();
        let prs = prs
            .iter()
            .filter(|pr| seen.insert(pr.number) && pr.state != State::Closed)
            .collect();
        let trunks = trunks.iter().copied().collect();
        Self { prs, trunks }
    }

    /// Each PR's link to the PR below it.
    fn links(&self) -> Vec<Option<Edge>> {
        let by = Lookups {
            position: self
                .index(|pr| pr.native_stack.map(|entry| (entry.stack, entry.position)))
                .into_iter()
                .collect(),
            head_ref: self.index(|pr| pr.head_in_base_repo.then_some(pr.head_ref.as_str())),
            commit: self.index(|pr| match pr.state {
                State::Open => vec![pr.head_oid.as_str()],
                // Squashing or rebasing left all of its commits out of its base branch, so a layer
                // cut before its last commit still has some of them.
                State::Merged {
                    head_in_base: false,
                } => {
                    let head = std::iter::once(pr.head_oid.as_str());
                    pr.commits.iter().map(String::as_str).chain(head).collect()
                }
                // A merge-committed PR's head is in its base branch, so it's history like any
                // other commit there, not a layer below.
                State::Merged { head_in_base: true } | State::Closed => Vec::new(),
            }),
        };
        (0..self.prs.len())
            .map(|index| self.link(index, &by))
            .collect()
    }

    /// The link from `prs[index]` to the PR below it, by the first rule that applies.
    fn link(&self, index: usize, by: &Lookups) -> Option<Edge> {
        let pr = self.prs[index];
        // The nearest lower position that's still there, skipping closed layers and any not
        // passed.
        let native_stack = || {
            let entry = pr.native_stack?;
            let below = (entry.stack, 0)..(entry.stack, entry.position);
            by.position.range(below).next_back().map(|(_, below)| below)
        };
        // An open PR's base branch only leads to a merged PR while it still points where that PR
        // merged from. Once it moves on, like a long-lived `develop`, it's a root. A merged PR's
        // base branch has moved on by merging it, so links between merged PRs skip the check.
        let base_branch = || {
            by.head_ref.get(pr.base_ref.as_str()).filter(|&&below| {
                let lower = self.prs[below];
                let unmoved = || pr.base_tip.as_deref() == Some(lower.head_oid.as_str());
                below != index
                    && (lower.state == State::Open || pr.state != State::Open || unmoved())
            })
        };
        // The nearest: the latest of its commits that another PR matches. A PR with the same head
        // commit isn't below it.
        let shared_commits = || {
            pr.commits
                .iter()
                .rev()
                .filter(|&oid| *oid != pr.head_oid)
                .find_map(|oid| by.commit.get(oid.as_str()))
                .filter(|&&below| below != index)
        };
        let edge = |rule| move |&below: &usize| Edge { below, rule };
        native_stack()
            .map(edge(Rule::NativeStack))
            .or_else(|| base_branch().map(edge(Rule::BaseBranch)))
            .or_else(|| shared_commits().map(edge(Rule::SharedCommits)))
    }

    /// Where the open PR `prs[index]` sits. When its link reaches a merged PR, it sits where that
    /// PR would, following links down to an open PR or a root branch.
    fn place(&self, index: usize, links: &[Option<Edge>]) -> Placed<'a> {
        let pr = self.prs[index];
        let Some(edge) = links[index] else {
            return Placed {
                pr,
                parent: Parent::Branch(pr.base_ref.clone()),
                rule: None,
                needs_rebase: None,
                retarget_to: None,
            };
        };
        let mut squashed = false;
        // `break_cycles` has run, so this ends.
        let mut next = edge.below;
        // What it sits on, and the branch that is: none for a fork's.
        let (parent, base) = loop {
            let below = self.prs[next];
            let State::Merged { head_in_base } = below.state else {
                let base = below.head_in_base_repo.then_some(&below.head_ref);
                break (Parent::Pr(below.number), base);
            };
            squashed |= !head_in_base;
            match links[next] {
                Some(link) => next = link.below,
                None => {
                    break (
                        Parent::Branch(below.base_ref.clone()),
                        Some(&below.base_ref),
                    );
                }
            }
        };
        let retarget_to = base.filter(|&base| *base != pr.base_ref).cloned();
        // GitHub rebases a stack's layers itself when a lower one merges.
        let merged = self.prs[edge.below];
        let needs_rebase = (squashed && edge.rule != Rule::NativeStack).then(|| {
            let listed: HashSet<&str> = pr.commits.iter().map(String::as_str).collect();
            let compare_against = merged
                .commits
                .iter()
                .rev()
                .find(|oid| listed.contains(oid.as_str()))
                .unwrap_or(&merged.head_oid);
            NeedsRebase {
                merged: merged.number,
                compare_against: compare_against.clone(),
            }
        });
        Placed {
            pr,
            parent,
            rule: Some(edge.rule),
            needs_rebase,
            retarget_to,
        }
    }

    /// Breaks each cycle in `links` by dropping one link, and returns the cycles.
    fn break_cycles(&self, links: &mut [Option<Edge>]) -> Vec<Cycle> {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Visit {
            Unseen,
            OnPath,
            Done,
        }
        let mut visits = vec![Visit::Unseen; links.len()];
        let mut path = Vec::new();
        let mut cycles = Vec::new();
        for start in 0..links.len() {
            // Each PR has at most one link, so follow it until it reaches a PR seen before.
            let mut next = Some(start);
            while let Some(index) = next {
                match visits[index] {
                    Visit::Unseen => {
                        visits[index] = Visit::OnPath;
                        path.push(index);
                        next = links[index].map(|edge| edge.below);
                    }
                    Visit::OnPath => {
                        let start = path.iter().rposition(|&i| i == index).unwrap_or(0);
                        let cycle = &path[start..];
                        let weakest = (0..cycle.len())
                            .max_by_key(|&at| {
                                let pr = self.prs[cycle[at]];
                                let rule = links[cycle[at]].map(|edge| edge.rule);
                                (rule, Reverse(pr.created_at), Reverse(pr.number))
                            })
                            .unwrap_or(0);
                        links[cycle[weakest]] = None;
                        let prs = cycle[weakest..].iter().chain(&cycle[..weakest]);
                        cycles.push(Cycle {
                            prs: prs.map(|&i| self.prs[i].number).collect(),
                        });
                        break;
                    }
                    Visit::Done => break,
                }
            }
            for index in path.drain(..) {
                visits[index] = Visit::Done;
            }
        }
        cycles
    }

    /// Indexes the PRs that can be below another by `key`. Where several PRs share a key, keeps
    /// the one a rule links to: an open PR before a merged one, then the newest.
    fn index<K: Eq + Hash, Keys: IntoIterator<Item = K>>(
        &self,
        keys: impl Fn(&'a PullRequest) -> Keys,
    ) -> HashMap<K, usize> {
        let mut index = HashMap::with_capacity(self.prs.len());
        for (i, &pr) in self.prs.iter().enumerate() {
            let from_trunk = pr.head_in_base_repo && self.trunks.contains(pr.head_ref.as_str());
            if from_trunk {
                continue;
            }
            for key in keys(pr) {
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
