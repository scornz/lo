//! Finding stacks stays fast on a busy repo.

use std::time::{Duration, Instant};

use stacks::{NeedsRebase, Parent, PullRequest, Rule, Stacks, State};

use crate::support::{With, pr};

/// The time budget, which holds for an optimized build. Debug builds run several times slower,
/// so they only catch a blow-up.
const BUDGET: Duration = if cfg!(debug_assertions) {
    Duration::from_millis(500)
} else {
    Duration::from_millis(50)
};

const STACKS: u64 = 250;
const LAYERS: u64 = 4;
/// Each layer's own commits. With the head of the layer below, that's what `commits(last: 100)`
/// lists, so shared commits only find the layer below after checking every other commit.
const COMMITS_PER_LAYER: u64 = 99;
const COMMITS_LISTED: usize = 100;

/// 1,000 PRs in 250 four-layer stacks, linked by each rule in turn, each listing up to 100 commits.
/// Every 25th stack's bottom layer was squash-merged.
fn busy_repo() -> Vec<PullRequest> {
    let mut prs = Vec::new();
    for stack in 0..STACKS {
        let mut commits = Vec::new();
        for layer in 0..LAYERS {
            let number = stack * LAYERS + layer + 1;
            let branch = format!("s{stack}-l{layer}");
            // Rule 3 links a layer of every third stack: it targets main, and only its commits
            // say what's below.
            let on_layer_below = layer > 0 && stack % 3 != 1;
            let base = if on_layer_below {
                format!("s{stack}-l{}", layer - 1)
            } else {
                "main".to_string()
            };
            let below_head = commits.last().cloned();
            commits.extend((0..COMMITS_PER_LAYER).map(|commit| oid(number, commit)));
            let listed = &commits[commits.len().saturating_sub(COMMITS_LISTED)..];
            let listed: Vec<&str> = listed.iter().map(String::as_str).collect();
            let mut pr = pr(number, &base, &branch).commits(&listed);
            if let Some(head) = below_head.filter(|_| on_layer_below) {
                pr = pr.base_at(&head);
            }
            if stack % 3 == 2 {
                pr = pr.native(100_000 + stack, layer as u32 + 1);
            }
            if layer == 0 && stack % 25 == 0 {
                pr = pr.merged(false);
            }
            prs.push(pr);
        }
    }
    prs
}

/// A commit OID, 40 hex digits like a real one.
fn oid(number: u64, commit: u64) -> String {
    let seed = (number << 16 | commit).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    format!("{seed:016x}{:024x}", seed.rotate_left(17))
}

#[test]
fn finds_stacks_among_1000_prs_within_budget() {
    let prs = busy_repo();
    assert_eq!(prs.len(), 1000);
    let merged = prs.iter().filter(|pr| pr.state != State::Open).count();
    assert_eq!(merged, 10);

    let start = Instant::now();
    let stacks = Stacks::find(&prs, &[]);
    let elapsed = start.elapsed();

    // Every open PR is placed, and the stacks are the ones built.
    let layers: usize = stacks
        .roots()
        .iter()
        .flat_map(|root| &root.stacks)
        .map(|stack| stack.layers.len())
        .sum();
    assert_eq!(layers, 1000 - merged);
    assert!(stacks.cycles().is_empty());
    assert_eq!(stacks.stack_of(8).unwrap().layers, [5, 6, 7, 8]);
    assert_eq!(stacks.layer(8).unwrap().rule, Some(Rule::SharedCommits));
    assert_eq!(stacks.layer(12).unwrap().rule, Some(Rule::NativeStack));
    assert_eq!(stacks.layer(4).unwrap().rule, Some(Rule::BaseBranch));
    assert_eq!(stacks.stack_of(2).unwrap().layers, [2, 3, 4]);
    assert!(stacks.layer(2).unwrap().needs_rebase.is_some());

    assert!(
        elapsed < BUDGET,
        "took {elapsed:?} to find stacks among 1,000 PRs"
    );
    eprintln!("found stacks among 1,000 PRs in {elapsed:?}");
}

#[test]
fn walks_a_long_merged_chain_within_budget() {
    // 5,000 squash-merged PRs, each stacked on the one before by commits, and 1,000 open PRs on
    // the last one.
    const MERGED: u64 = 5000;
    const OPEN: u64 = 1000;
    let mut prs = Vec::new();
    let mut commits: Vec<String> = Vec::new();
    for number in 1..=MERGED {
        commits.push(format!("m{number}"));
        let listed = &commits[commits.len().saturating_sub(COMMITS_LISTED)..];
        let listed: Vec<&str> = listed.iter().map(String::as_str).collect();
        let head = format!("m{number}");
        prs.push(pr(number, "main", &head).commits(&listed).merged(false));
    }
    let top = format!("m{MERGED}");
    for number in MERGED + 1..=MERGED + OPEN {
        let head = format!("o{number}");
        prs.push(pr(number, "main", &head).commits(&[top.as_str(), head.as_str()]));
    }

    let start = Instant::now();
    let stacks = Stacks::find(&prs, &["main"]);
    let elapsed = start.elapsed();

    let root = &stacks.roots()[0];
    assert_eq!(root.branch, "main");
    assert_eq!(root.stacks.len() as u64, OPEN);
    let layer = stacks.layer(MERGED + 1).unwrap();
    assert_eq!(layer.parent, Parent::Branch("main".into()));
    assert_eq!(
        layer.needs_rebase,
        Some(NeedsRebase {
            merged: MERGED,
            compare_against: top.clone(),
        })
    );
    assert!(
        elapsed < BUDGET,
        "took {elapsed:?} to walk {MERGED} merged PRs under {OPEN} open ones"
    );
    eprintln!("walked {MERGED} merged PRs under {OPEN} open ones in {elapsed:?}");
}
