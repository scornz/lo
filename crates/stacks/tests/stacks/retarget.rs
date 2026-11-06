//! The base branch each layer needs for "Make this a GitHub stack": the head branch of the PR
//! below, or the root.

use stacks::{PullRequest, Stacks};

use crate::support::{With, pr};

fn retarget_to(prs: &[PullRequest], number: u64) -> Option<String> {
    let stacks = Stacks::find(prs, &[]);
    stacks.layer(number).unwrap().retarget_to.clone()
}

#[test]
fn a_layer_already_on_the_branch_below_stays() {
    let prs = [pr(1, "main", "a"), pr(2, "a", "b")];
    assert_eq!(retarget_to(&prs, 1), None);
    assert_eq!(retarget_to(&prs, 2), None);
}

#[test]
fn a_layer_found_by_shared_commits_moves_to_the_branch_below() {
    let prs = [
        pr(1, "main", "a").commits(&["a.1"]),
        pr(2, "main", "b").commits(&["a.1", "b.1"]),
    ];
    assert_eq!(retarget_to(&prs, 2), Some("a".into()));
}

#[test]
fn a_github_stack_layer_on_another_branch_moves() {
    let prs = [
        pr(1, "main", "a").native(100, 1),
        pr(2, "x", "b").native(100, 2),
        pr(3, "main", "x"),
    ];
    assert_eq!(retarget_to(&prs, 2), Some("a".into()));
}

#[test]
fn a_layer_on_a_merged_pr_moves_to_where_it_now_sits() {
    // #1 was merged into main.
    let prs = [pr(1, "main", "a").merged(false), pr(2, "a", "b")];
    assert_eq!(retarget_to(&prs, 2), Some("main".into()));

    // #2 was merged into #1's branch.
    let prs = [
        pr(1, "main", "a"),
        pr(2, "a", "m").merged(false),
        pr(3, "m", "b"),
    ];
    assert_eq!(retarget_to(&prs, 3), Some("a".into()));
}

#[test]
fn a_layer_on_a_fork_pr_has_no_branch_to_move_to() {
    let prs = [
        pr(1, "main", "feature").fork().commits(&["f.1"]),
        pr(2, "main", "more").commits(&["f.1", "m.1"]),
    ];
    assert_eq!(retarget_to(&prs, 2), None);
}
