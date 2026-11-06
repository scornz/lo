//! Rule 1: a layer of a GitHub stack sits on the layer one position below it.

use stacks::{Parent, Rule};

use crate::support::{With, assert_forest, pr};

#[test]
fn adjacent_layers_of_a_github_stack_are_linked() {
    let stacks = assert_forest(
        &[
            pr(1, "main", "a").native(100, 1),
            pr(2, "a", "b").native(100, 2),
            pr(3, "b", "c").native(100, 3),
        ],
        &[
            "main",
            "  #1",
            "    #2 native-stack",
            "      #3 native-stack",
        ],
    );
    let layer = stacks.layer(3).unwrap();
    assert_eq!(layer.parent, Parent::Pr(2));
    assert_eq!(layer.rule, Some(Rule::NativeStack));
}

#[test]
fn native_stack_wins_over_base_branch() {
    // #2 targets #3's branch, but GitHub says it's above #1.
    assert_forest(
        &[
            pr(1, "main", "a").native(100, 1),
            pr(2, "x", "b").native(100, 2),
            pr(3, "main", "x"),
        ],
        &["main", "  #1", "    #2 native-stack", "  #3"],
    );
}

#[test]
fn native_stack_wins_over_shared_commits() {
    // #2 contains #3's head, but GitHub says it's above #1.
    assert_forest(
        &[
            pr(1, "main", "a").native(100, 1).commits(&["a.1"]),
            pr(2, "main", "b").native(100, 2).commits(&["x.1", "b.1"]),
            pr(3, "main", "x").commits(&["x.1"]),
        ],
        &["main", "  #1", "    #2 native-stack", "  #3"],
    );
}

#[test]
fn a_bottom_layer_links_by_the_other_rules() {
    // A GitHub stack whose trunk is another PR's head branch.
    assert_forest(
        &[
            pr(1, "main", "a"),
            pr(2, "a", "b").native(100, 1),
            pr(3, "b", "c").native(100, 2),
        ],
        &[
            "main",
            "  #1",
            "    #2 base-branch",
            "      #3 native-stack",
        ],
    );
}

#[test]
fn layers_of_different_stacks_are_not_linked() {
    assert_forest(
        &[
            pr(1, "main", "a").native(100, 1),
            pr(2, "x", "b").native(200, 2),
        ],
        &["main", "  #1", "x", "  #2"],
    );
}

#[test]
fn a_missing_lower_layer_falls_back_to_the_other_rules() {
    // Position 2 wasn't loaded, so #3 links by its base branch.
    assert_forest(
        &[
            pr(1, "main", "a").native(100, 1),
            pr(3, "a", "c").native(100, 3),
        ],
        &["main", "  #1", "    #3 base-branch"],
    );
}

#[test]
fn a_closed_lower_layer_counts_as_missing() {
    assert_forest(
        &[
            pr(1, "main", "a").native(100, 1),
            pr(2, "a", "b").native(100, 2).closed(),
            pr(3, "b", "c").native(100, 3),
        ],
        &["b", "  #3", "main", "  #1"],
    );
}
