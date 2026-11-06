//! Merging a chain that can't become a GitHub stack one layer at a time.

use stacks::merge::merge_one_layer_at_a_time;
use stacks::{PullRequest, Stacks};

use crate::support::{With, pr, render_retarget};

/// Plans merging the stack under `layer`, among `prs` with no trunk branches, and renders it as its
/// confirmation lists it, in the order it runs.
fn render(prs: &[PullRequest], layer: u64) -> Vec<String> {
    let stacks = Stacks::find(prs, &[]);
    let merge = merge_one_layer_at_a_time(&stacks, layer).expect("the PR is a layer");
    let mut lines: Vec<String> = merge.retarget_before.iter().map(render_retarget).collect();
    lines.push(format!("merge #{} into {}", merge.pr, merge.into));
    lines.extend(merge.retarget_after.iter().map(render_retarget));
    lines.extend(
        merge
            .needs_rebase
            .iter()
            .map(|pr| format!("#{pr} needs rebase")),
    );
    lines
}

#[test]
fn the_bottom_layer_merges_and_the_next_moves_to_the_root() {
    let prs = [pr(1, "main", "a"), pr(2, "a", "b"), pr(3, "b", "c")];
    for layer in 1..=3 {
        assert_eq!(
            render(&prs, layer),
            ["merge #1 into main", "#2 a -> main", "#2 needs rebase"]
        );
    }
}

#[test]
fn a_layer_already_based_on_the_root_only_needs_a_rebase() {
    let prs = [
        pr(1, "main", "a").commits(&["a.1"]),
        pr(2, "main", "b").commits(&["a.1", "b.1"]),
    ];
    assert_eq!(render(&prs, 2), ["merge #1 into main", "#2 needs rebase"]);
}

#[test]
fn every_layer_on_the_bottom_one_moves_to_the_root() {
    let prs = [
        pr(1, "main", "a"),
        pr(2, "a", "b"),
        pr(3, "a", "c"),
        pr(4, "c", "d"),
    ];
    assert_eq!(
        render(&prs, 4),
        [
            "merge #1 into main",
            "#2 a -> main",
            "#3 a -> main",
            "#2 needs rebase",
            "#3 needs rebase",
        ]
    );
}

#[test]
fn a_lone_pr_just_merges() {
    assert_eq!(render(&[pr(1, "main", "a")], 1), ["merge #1 into main"]);
}

#[test]
fn a_bottom_layer_still_on_a_merged_prs_branch_moves_to_the_root_first() {
    // #1 was squash-merged into main, and its branch kept.
    let prs = [
        pr(1, "main", "a").merged(false),
        pr(2, "a", "b"),
        pr(3, "b", "c"),
    ];
    assert_eq!(
        render(&prs, 3),
        [
            "#2 a -> main",
            "merge #2 into main",
            "#3 b -> main",
            "#3 needs rebase",
        ]
    );
}

#[test]
fn a_chain_on_a_branch_no_pr_comes_from_merges_into_that_branch() {
    let prs = [pr(1, "feature-x", "a"), pr(2, "a", "b")];
    assert_eq!(
        render(&prs, 2),
        [
            "merge #1 into feature-x",
            "#2 a -> feature-x",
            "#2 needs rebase",
        ]
    );
}

#[test]
fn github_moves_a_github_stacks_own_layers() {
    // #1 and #2 are a GitHub stack. #3 sits on #1 too, so the path through it can't join the stack.
    let prs = [
        pr(1, "main", "a").native(100, 1),
        pr(2, "a", "b").native(100, 2),
        pr(3, "a", "c"),
    ];
    assert_eq!(
        render(&prs, 3),
        ["merge #1 into main", "#3 a -> main", "#3 needs rebase"]
    );
}

#[test]
fn a_pr_that_isnt_a_layer_has_nothing_to_merge() {
    let stacks = Stacks::find(&[pr(1, "main", "a").merged(false), pr(2, "a", "b")], &[]);
    assert_eq!(merge_one_layer_at_a_time(&stacks, 1), None);
    assert_eq!(merge_one_layer_at_a_time(&stacks, 9), None);
}
