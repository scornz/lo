//! Stacks found in recorded answers (`tests/record_fixtures.rs`):
//!
//! - `rozed-dev/playground`: GitHub stack #3, PR #1 (`stack-a` → `main`) under PR #2 (`stack-b`
//!   → `stack-a`).
//! - `rozed-dev/e2e` (`script/e2e/README.md`): E2's tree, #2 a → main, #3 b → a, #4 c → b, #5 e
//!   → b and #6 d → main holding c, with the GitHub stack #9 (#7, #8) beside it; and E3, #11 y →
//!   `e3/x` on #10 x, squash-merged.
//! - `github/gh-stack`, read only: rulesets as a reader sees them, one of them on `~ALL`.

use std::sync::Arc;

use futures::executor::block_on;
use github::queries::{
    OpenPullRequestList, PAGE_SIZE, PullRequestReviewDecision, StackLayerList, StatusState,
    ViewedFiles,
};
use github::replay::ReplayHttpClient;
use github::trunks::Trunks;
use github::{GithubClient, Token};
use stacks::{NativeStackEntry, NeedsRebase, Parent, Rule, Stacks};

fn client(cassette: &str) -> (GithubClient, Arc<ReplayHttpClient>) {
    let http = ReplayHttpClient::cassette(cassette);
    (
        GithubClient::new(http.clone(), Token::new("gho_FAKE")),
        http,
    )
}

/// The open pull requests the cassette starts with, and its client.
fn open(cassette: &str, owner: &str, name: &str) -> (OpenPullRequestList, GithubClient) {
    let (client, _) = client(cassette);
    let list = block_on(client.open_pull_requests(owner, name)).unwrap();
    (list, client)
}

/// The node IDs of `numbers`, in that order.
fn ids(list: &OpenPullRequestList, numbers: &[i64]) -> Vec<String> {
    (numbers.iter())
        .map(|&number| list.pull_request(number).unwrap().id.clone())
        .collect()
}

/// Each layer of `stacks`'s stack holding `number`, with its depth.
fn walk(stacks: &Stacks, number: u64) -> Vec<(u64, usize)> {
    (stacks.stack_of(number).unwrap().layers.iter())
        .map(|&layer| (layer, stacks.layer(layer).unwrap().depth))
        .collect()
}

#[test]
fn the_playgrounds_github_stack() {
    let (list, client) = open("stacks_playground", "rozed-dev", "playground");
    // One request: the open pull requests, the trunks and the merged pull requests, for a point.
    assert_eq!(client.rate_limits().last_graphql_cost, Some(1));
    assert_eq!(
        list.trunks,
        Trunks {
            default_branch: Some("main".into()),
            ..Trunks::default()
        }
    );
    assert!(list.merged.is_empty());

    // As finding stacks takes them: the base branch's tip now, which is the lower layer's head,
    // and the commits, the head last.
    let prs = list.stack_pull_requests();
    let [upper, lower] = prs.as_slice() else {
        panic!("two open pull requests, newest first");
    };
    assert_eq!((upper.number, lower.number), (2, 1));
    assert_eq!(upper.base_tip.as_ref(), Some(&lower.head_oid));
    assert_eq!(upper.commits.last(), Some(&upper.head_oid));
    assert!(upper.head_in_base_repo);
    assert_eq!(
        upper.native_stack,
        Some(NativeStackEntry {
            stack: 3,
            position: 2
        })
    );

    let stacks = list.stacks();
    let roots: Vec<_> = (stacks.roots().iter())
        .map(|root| root.branch.as_str())
        .collect();
    assert_eq!(roots, ["main"]);
    assert_eq!(walk(&stacks, 2), [(1, 0), (2, 1)]);
    let bottom = stacks.layer(1).unwrap();
    assert_eq!(
        (&bottom.parent, bottom.rule),
        (&Parent::Branch("main".into()), None)
    );
    let top = stacks.layer(2).unwrap();
    assert_eq!(
        (&top.parent, top.rule),
        (&Parent::Pr(1), Some(Rule::NativeStack))
    );
    assert_eq!((&top.needs_rebase, &top.retarget_to), (&None, &None));

    // What the tree shows of each layer besides.
    let pr = list.pull_request(2).unwrap();
    assert_eq!(pr.author.as_ref().unwrap().login, "rozed-bot");
    assert_eq!(pr.title, "feat: stack layer B");
    assert_eq!(pr.review_decision, None);
    assert_eq!(pr.status_check_rollup, None);
    assert!(!pr.is_in_merge_queue);
    let layers = block_on(client.stack_layers(&ids(&list, &[1, 2]))).unwrap();
    assert_eq!(client.rate_limits().last_graphql_cost, Some(1));
    let numbers: Vec<i64> = layers.layers.iter().map(|layer| layer.number).collect();
    assert_eq!(numbers, [1, 2]);
    let viewed = layers.layer(&pr.id).unwrap().viewed_files();
    assert_eq!(
        viewed,
        ViewedFiles {
            viewed: 0,
            files: 1,
            complete: true
        }
    );
}

/// The full SHA of E3's x (#10) head: what #11 compares against until it's rebased.
const X_HEAD: &str = "e86aa486704016ee033d555bcca81e33898c5608";

/// E3: y (#11) still targets `e3/x`, whose pull request (#10) was squash-merged into `main`.
fn assert_e3_needs_rebase(stacks: &Stacks) {
    let y = stacks.layer(11).unwrap();
    assert_eq!(y.parent, Parent::Branch("main".into()));
    assert_eq!(y.rule, Some(Rule::BaseBranch));
    assert_eq!(
        y.needs_rebase,
        Some(NeedsRebase {
            merged: 10,
            compare_against: X_HEAD.into(),
        })
    );
    // Made a GitHub stack, it would target `main`.
    assert_eq!(y.retarget_to.as_deref(), Some("main"));
    assert_eq!(walk(stacks, 11), [(11, 0)]);
}

#[test]
fn e2s_tree_and_github_stack_and_e3s_rebase() {
    let (list, client) = open("stacks_e2e", "rozed-dev", "e2e");
    assert_eq!(client.rate_limits().last_graphql_cost, Some(1));
    // E3's x is among the pull requests merged most recently, so nothing was looked up. All
    // three were squash-merged.
    let merged: Vec<_> = (list.merged.iter())
        .map(|pr| (pr.number, pr.head_in_base))
        .collect();
    assert_eq!(merged, [(15, false), (12, false), (10, false)]);
    let x = list.merged.iter().find(|pr| pr.number == 10).unwrap();
    assert_eq!((x.head_ref.as_str(), x.head_oid.as_str()), ("e3/x", X_HEAD));
    assert_eq!(x.commits.last().map(String::as_str), Some(X_HEAD));
    assert!(!list.merged_truncated);

    let stacks = list.stacks();
    assert!(stacks.cycles().is_empty());
    let roots: Vec<_> = (stacks.roots().iter())
        .map(|root| root.branch.as_str())
        .collect();
    assert_eq!(roots, ["main"]);
    // A stack for each bottom layer on `main`, oldest first: E2's tree, its GitHub stack, E3's
    // y (whose x merged), E1's revisions and E5's PR.
    let bottoms: Vec<u64> = (stacks.roots()[0].stacks.iter())
        .map(|stack| stack.layers[0])
        .collect();
    assert_eq!(bottoms, [2, 7, 11, 16, 17]);

    // E2: a, b, then c and e on b in the order they were opened, and d on c.
    assert_eq!(walk(&stacks, 2), [(2, 0), (3, 1), (4, 2), (6, 3), (5, 2)]);
    assert_eq!(stacks.layer(3).unwrap().children, [4, 5]);
    for number in [3, 4, 5] {
        assert_eq!(stacks.layer(number).unwrap().rule, Some(Rule::BaseBranch));
    }
    // d targets `main` but holds c's head: only shared commits link it, to the nearest layer it
    // holds, c rather than b or a.
    let d = stacks.layer(6).unwrap();
    assert_eq!(
        (&d.parent, d.rule),
        (&Parent::Pr(4), Some(Rule::SharedCommits))
    );
    assert_eq!(d.retarget_to.as_deref(), Some("e2/c"));
    assert_eq!(d.needs_rebase, None);

    // The GitHub stack wins over the base branch, which links s2 to s1 too.
    assert_eq!(walk(&stacks, 8), [(7, 0), (8, 1)]);
    let s2 = stacks.layer(8).unwrap();
    assert_eq!(
        (&s2.parent, s2.rule),
        (&Parent::Pr(7), Some(Rule::NativeStack))
    );

    assert_e3_needs_rebase(&stacks);
}

#[test]
fn e3s_merged_layer_is_looked_up_by_its_branch() {
    let (client, http) = client("stacks_e2e_looked_up");
    let list = block_on(client.open_pull_requests_paged("rozed-dev", "e2e", PAGE_SIZE, 0)).unwrap();
    // Without the pull requests merged most recently, `e3/x`, which y targets, is the one
    // branch that's neither a trunk nor an open pull request's.
    let seen = http.seen();
    assert_eq!(seen.len(), 2);
    let request: serde_json::Value = serde_json::from_slice(&seen[1].body).unwrap();
    assert_eq!(request["operationName"], "MergedPullRequests");
    assert_eq!(request["variables"]["headRefName"], "e3/x");
    let merged: Vec<u64> = list.merged.iter().map(|pr| pr.number).collect();
    assert_eq!(merged, [10]);
    assert_e3_needs_rebase(&list.stacks());
}

#[test]
fn e2s_layers_as_the_tree_shows_them() {
    let (list, client) = open("stacks_e2e", "rozed-dev", "e2e");
    let asked = ids(&list, &[2, 3, 4, 6, 5, 7, 8, 11]);
    let layers: StackLayerList = block_on(client.stack_layers(&asked)).unwrap();
    assert_eq!(layers.ids, asked);
    let numbers: Vec<i64> = layers.layers.iter().map(|layer| layer.number).collect();
    assert_eq!(numbers, [2, 3, 4, 6, 5, 7, 8, 11]);
    // d changes the files of a, b and c, and its own.
    let d = layers.layer(&asked[3]).unwrap();
    assert_eq!(
        d.viewed_files(),
        ViewedFiles {
            viewed: 0,
            files: 4,
            complete: true
        }
    );
    assert_eq!(d.head_ref_oid, list.pull_request(6).unwrap().head_ref_oid);
    // e2e has no checks, required reviews or merge queue.
    for layer in &layers.layers {
        assert_eq!(layer.review_decision, None);
        assert_eq!(layer.status_check_rollup, None);
        assert_eq!(layer.merge_queue_entry, None);
    }
}

#[test]
fn rulesets_for_every_branch_make_no_trunks() {
    let (list, client) = open("stacks_gh_stack", "github", "gh-stack");
    assert_eq!(client.rate_limits().last_graphql_cost, Some(1));
    // A reader sees no branch protection rules, but sees the rulesets, the organization's too:
    // Copilot reviews on `~ALL`, the default branch's, and the enterprise release branches'.
    let trunks = &list.trunks;
    assert_eq!(trunks.default_branch.as_deref(), Some("main"));
    assert!(trunks.protected.is_empty());
    assert!((trunks.rulesets.iter()).any(|ruleset| ruleset.include == ["~ALL"]));
    assert!(trunks.contains("enterprise-3.18-release"));
    assert!(!trunks.contains("enterprise-3.18.1-backport-2"));
    let prs = list.stack_pull_requests();
    assert_eq!(trunks.among(&prs), ["main"]);

    // Every open pull request sits on `main` by itself, none of them a trunk.
    let stacks = list.stacks();
    for pr in &list.pull_requests {
        assert!(!trunks.contains(&pr.head_ref_name), "{}", pr.head_ref_name);
        let layer = stacks.layer(pr.number as u64).unwrap();
        assert_eq!(
            (&layer.parent, layer.rule),
            (&Parent::Branch("main".into()), None)
        );
    }
    // Merged pull requests keep their place in a GitHub stack.
    let merged = list.merged.iter().find(|pr| pr.number == 579).unwrap();
    let entry = merged.native_stack.unwrap();
    assert_eq!((entry.stack, entry.position), (580, 2));

    let first = &list.pull_requests[0];
    assert_eq!(
        first.review_decision,
        Some(PullRequestReviewDecision::REVIEW_REQUIRED)
    );
    assert_eq!(
        first
            .status_check_rollup
            .as_ref()
            .map(|rollup| &rollup.state),
        Some(&StatusState::SUCCESS)
    );
    let every: Vec<String> = list.pull_requests.iter().map(|pr| pr.id.clone()).collect();
    let layers = block_on(client.stack_layers(&every)).unwrap();
    assert_eq!(layers.layers.len(), list.pull_requests.len());
    let layer = layers.layer(&first.id).unwrap();
    assert_eq!(
        layer.review_decision,
        Some(PullRequestReviewDecision::REVIEW_REQUIRED)
    );
    assert_eq!(
        layer
            .status_check_rollup
            .as_ref()
            .map(|rollup| &rollup.state),
        Some(&StatusState::SUCCESS)
    );
}
