//! "Make this a GitHub stack": which layers it converts, and the base-branch changes and stack that
//! convert them.

use stacks::convert::Reason;

use crate::support::{With, assert_plan, plan, pr};

#[test]
fn the_path_runs_from_the_bottom_layer_through_the_layer_to_the_top() {
    let prs = [
        pr(1, "main", "a"),
        pr(2, "a", "b"),
        pr(3, "b", "c"),
        pr(4, "c", "d"),
    ];
    for layer in 1..=4 {
        assert_plan(&prs, layer, &["main: #1 #2 #3 #4", "create #1 #2 #3 #4"]);
    }
}

#[test]
fn a_fork_in_the_tree_ends_the_path_above_the_layer() {
    // #2 has two layers on it.
    let prs = [
        pr(1, "main", "a"),
        pr(2, "a", "b"),
        pr(3, "b", "c"),
        pr(4, "b", "d"),
    ];
    assert_plan(&prs, 1, &["main: #1 #2", "create #1 #2"]);
    assert_plan(&prs, 2, &["main: #1 #2", "create #1 #2"]);
    assert_plan(&prs, 3, &["main: #1 #2 #3", "create #1 #2 #3"]);
    assert_plan(&prs, 4, &["main: #1 #2 #4", "create #1 #2 #4"]);
}

#[test]
fn a_fork_below_the_layer_doesnt_end_the_path() {
    // #1 has two layers on it; the path through #4 passes one of them.
    let prs = [
        pr(1, "main", "a"),
        pr(2, "a", "b"),
        pr(3, "a", "c"),
        pr(4, "b", "d"),
        pr(5, "d", "e"),
    ];
    assert_plan(&prs, 4, &["main: #1 #2 #4 #5", "create #1 #2 #4 #5"]);
}

#[test]
fn layers_found_by_shared_commits_move_onto_the_branch_below() {
    assert_plan(
        &[
            pr(1, "main", "a").commits(&["a.1"]),
            pr(2, "main", "b").commits(&["a.1", "b.1"]),
            pr(3, "main", "c").commits(&["a.1", "b.1", "c.1"]),
        ],
        3,
        &[
            "main: #1 #2 #3",
            "#2 main -> a",
            "#3 main -> b",
            "create #1 #2 #3",
        ],
    );
}

#[test]
fn base_branches_change_bottom_to_top_whatever_the_numbers() {
    assert_plan(
        &[
            pr(9, "main", "a").commits(&["a.1"]),
            pr(4, "main", "b").commits(&["a.1", "b.1"]).created(10),
            pr(7, "main", "c").commits(&["a.1", "b.1", "c.1"]),
        ],
        9,
        &[
            "main: #9 #4 #7",
            "#4 main -> a",
            "#7 main -> b",
            "create #9 #4 #7",
        ],
    );
}

#[test]
fn only_the_layers_off_the_branch_below_move() {
    assert_plan(
        &[
            pr(1, "main", "a"),
            pr(2, "a", "b"),
            pr(3, "main", "c").commits(&["b.1", "c.1"]),
        ],
        1,
        &["main: #1 #2 #3", "#3 main -> b", "create #1 #2 #3"],
    );
}

#[test]
fn a_bottom_layer_on_a_merged_pr_moves_to_the_root() {
    // #1 was squash-merged into main, and its branch kept.
    assert_plan(
        &[
            pr(1, "main", "a").merged(false),
            pr(2, "a", "b"),
            pr(3, "b", "c"),
        ],
        2,
        &["main: #2 #3", "#2 a -> main", "create #2 #3"],
    );
}

#[test]
fn a_chain_on_a_branch_no_pr_comes_from_stacks_on_that_branch() {
    assert_plan(
        &[pr(1, "feature-x", "a"), pr(2, "a", "b")],
        2,
        &["feature-x: #1 #2", "create #1 #2"],
    );
}

#[test]
fn a_pr_that_isnt_a_layer_has_nothing_to_convert() {
    let prs = [
        pr(1, "main", "a").merged(false),
        pr(2, "a", "b"),
        pr(3, "b", "c"),
        pr(4, "main", "d").closed(),
    ];
    for layer in [1, 4, 99] {
        assert_eq!(plan(&prs, layer), Err(vec![Reason::NotOpen]));
    }
}

#[test]
fn a_lone_pr_is_too_short_to_stack() {
    assert_eq!(plan(&[pr(1, "main", "a")], 1), Err(vec![Reason::TooShort]));
}

#[test]
fn a_bottom_layer_where_the_tree_forks_is_too_short_to_stack() {
    let prs = [pr(1, "main", "a"), pr(2, "a", "b"), pr(3, "a", "c")];
    assert_eq!(plan(&prs, 1), Err(vec![Reason::TooShort]));
}
