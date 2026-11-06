//! Finding stacks stays fast on a busy repo.

use std::time::{Duration, Instant};

use stacks::{PullRequest, Rule, Stacks, State};

use crate::support::{With, pr};

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
fn finds_stacks_among_1000_prs_within_50_ms() {
    let prs = busy_repo();
    assert_eq!(prs.len(), 1000);
    let merged = prs.iter().filter(|pr| pr.state != State::Open).count();
    assert_eq!(merged, 10);

    let start = Instant::now();
    let stacks = Stacks::find(&prs);
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
        elapsed < Duration::from_millis(50),
        "took {elapsed:?} to find stacks among 1,000 PRs"
    );
    eprintln!("found stacks among 1,000 PRs in {elapsed:?}");
}
