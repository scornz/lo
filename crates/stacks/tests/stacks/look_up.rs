//! Which merged PRs the caller still has to fetch: those from the branches open PRs sit on.

use stacks::branches_to_look_up;

use crate::support::{With, pr};

#[test]
fn lists_base_branches_no_pr_comes_from() {
    let prs = [
        pr(1, "b", "c"),
        pr(2, "a", "d"),
        pr(3, "a", "e"),
        pr(4, "main", "f"),
    ];
    assert_eq!(branches_to_look_up(&prs, &["main"]), ["a", "b"]);
}

#[test]
fn skips_branches_an_open_or_merged_pr_comes_from() {
    let prs = [
        pr(1, "main", "a"),
        pr(2, "a", "b"),
        pr(3, "main", "x").merged(false),
        pr(4, "x", "y"),
    ];
    assert!(branches_to_look_up(&prs, &["main"]).is_empty());
}

#[test]
fn still_lists_branches_only_a_closed_or_fork_pr_comes_from() {
    let prs = [
        pr(1, "main", "a").closed(),
        pr(2, "a", "b"),
        pr(3, "main", "x").fork(),
        pr(4, "x", "y"),
    ];
    assert_eq!(branches_to_look_up(&prs, &["main"]), ["a", "x"]);
}

#[test]
fn follows_merged_prs_down_to_a_trunk() {
    // #3 sits on #2, which was merged into `a`.
    let mut prs = vec![pr(2, "a", "b").merged(false), pr(3, "b", "c")];
    assert_eq!(branches_to_look_up(&prs, &["main"]), ["a"]);
    prs.push(pr(1, "main", "a").merged(false));
    assert!(branches_to_look_up(&prs, &["main"]).is_empty());
}

#[test]
fn ignores_merged_prs_no_open_pr_sits_on() {
    let prs = [pr(1, "release-1", "x").merged(true), pr(2, "main", "y")];
    assert!(branches_to_look_up(&prs, &["main"]).is_empty());
}
