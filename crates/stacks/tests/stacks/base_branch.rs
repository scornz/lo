//! Rule 2: a PR sits on the open PR whose head branch is its base branch. Also the trees and walks
//! it builds.

use stacks::{Parent, Rule, Stacks};

use crate::support::{With, assert_forest, pr};

#[test]
fn a_pr_targeting_another_prs_head_branch_sits_on_it() {
    let stacks = assert_forest(
        &[pr(1, "main", "a"), pr(2, "a", "b"), pr(3, "b", "c")],
        &["main", "  #1", "    #2 base-branch", "      #3 base-branch"],
    );
    let layer = stacks.layer(2).unwrap();
    assert_eq!(layer.parent, Parent::Pr(1));
    assert_eq!(layer.rule, Some(Rule::BaseBranch));
    assert_eq!(layer.depth, 1);
    assert_eq!(stacks.layer(1).unwrap().rule, None);
    assert_eq!(stacks.layer(1).unwrap().children, [2]);

    let stack = stacks.stack_of(3).unwrap();
    assert_eq!(stack.root, "main");
    assert_eq!(stack.layers, [1, 2, 3]);
    assert_eq!(stacks.stack_of(1), Some(stack));
}

#[test]
fn input_order_does_not_matter() {
    assert_forest(
        &[pr(3, "b", "c"), pr(2, "a", "b"), pr(1, "main", "a")],
        &["main", "  #1", "    #2 base-branch", "      #3 base-branch"],
    );
}

#[test]
fn a_base_branch_with_no_pr_is_a_root() {
    assert_forest(
        &[pr(1, "feature-x", "a"), pr(2, "a", "b")],
        &["feature-x", "  #1", "    #2 base-branch"],
    );
}

#[test]
fn children_are_oldest_first_and_the_walk_finishes_each_branch_first() {
    let stacks = assert_forest(
        &[
            pr(1, "main", "a"),
            pr(2, "a", "b").created(20),
            pr(3, "a", "c").created(10),
            pr(4, "b", "d"),
            pr(5, "c", "e"),
            pr(6, "c", "f"),
        ],
        &[
            "main",
            "  #1",
            "    #3 base-branch",
            "      #5 base-branch",
            "      #6 base-branch",
            "    #2 base-branch",
            "      #4 base-branch",
        ],
    );
    assert_eq!(stacks.layer(1).unwrap().children, [3, 2]);
    assert_eq!(stacks.stack_of(4).unwrap().layers, [1, 3, 5, 6, 2, 4]);
}

#[test]
fn next_and_previous_step_through_the_walk() {
    let stacks = Stacks::find(
        &[
            pr(1, "main", "a"),
            pr(2, "a", "b"),
            pr(3, "b", "c"),
            pr(4, "a", "d"),
        ],
        &[],
    );
    let stack = stacks.stack_of(1).unwrap();
    assert_eq!(stack.layers, [1, 2, 3, 4]);
    assert_eq!(stack.next(1), Some(2));
    assert_eq!(stack.next(3), Some(4));
    assert_eq!(stack.next(4), None);
    assert_eq!(stack.previous(4), Some(3));
    assert_eq!(stack.previous(2), Some(1));
    assert_eq!(stack.previous(1), None);
    assert_eq!(stack.next(9), None);
    assert_eq!(stack.previous(9), None);
}

#[test]
fn separate_trees_on_one_root_are_separate_stacks() {
    let stacks = assert_forest(
        &[
            pr(1, "main", "a"),
            pr(2, "main", "b"),
            pr(3, "a", "c"),
            pr(4, "b", "d"),
        ],
        &[
            "main",
            "  #1",
            "    #3 base-branch",
            "  #2",
            "    #4 base-branch",
        ],
    );
    assert_eq!(stacks.stack_of(3).unwrap().layers, [1, 3]);
    assert_eq!(stacks.stack_of(4).unwrap().layers, [2, 4]);
}

#[test]
fn never_links_to_a_fork_head() {
    // A fork's `feature` branch isn't the base repo's `feature` branch.
    assert_forest(
        &[pr(1, "main", "feature").fork(), pr(2, "feature", "b")],
        &["feature", "  #2", "main", "  #1"],
    );
}

#[test]
fn a_fork_pr_can_sit_on_a_base_repo_branch() {
    assert_forest(
        &[pr(1, "main", "a"), pr(2, "a", "patch").fork()],
        &["main", "  #1", "    #2 base-branch"],
    );
}

#[test]
fn of_two_open_prs_from_one_branch_the_newest_is_below() {
    assert_forest(
        &[
            pr(1, "main", "feature"),
            pr(2, "release", "feature"),
            pr(3, "feature", "b"),
        ],
        &["main", "  #1", "release", "  #2", "    #3 base-branch"],
    );
}

#[test]
fn a_pr_never_sits_on_itself() {
    assert_forest(&[pr(1, "a", "a")], &["a", "  #1"]);
}
