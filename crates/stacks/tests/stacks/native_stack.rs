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
fn a_closed_middle_layer_doesnt_split_the_stack() {
    let stacks = assert_forest(
        &[
            pr(1, "main", "a").native(100, 1),
            pr(2, "a", "b").native(100, 2).closed(),
            pr(3, "b", "c").native(100, 3),
        ],
        &["main", "  #1", "    #3 native-stack"],
    );
    assert_eq!(stacks.layer(3).unwrap().retarget_to, Some("a".into()));
}

#[test]
fn a_middle_layer_not_passed_is_skipped_too() {
    assert_forest(
        &[
            pr(1, "main", "a").native(100, 1),
            pr(3, "b", "c").native(100, 3),
        ],
        &["main", "  #1", "    #3 native-stack"],
    );
}

#[test]
fn the_lowest_layer_passed_links_by_the_other_rules() {
    // GitHub merged position 1 and pointed #2 at main; #1 wasn't passed.
    assert_forest(
        &[
            pr(2, "main", "b").native(100, 2),
            pr(3, "b", "c").native(100, 3),
        ],
        &["main", "  #2", "    #3 native-stack"],
    );
}

#[test]
fn a_layer_on_a_merged_middle_layer_sits_on_the_layer_below_it() {
    // #2 was squash-merged into #1's branch. GitHub rebases the layers above.
    let stacks = assert_forest(
        &[
            pr(1, "main", "a").native(100, 1),
            pr(2, "a", "b").native(100, 2).merged(false),
            pr(3, "b", "c").native(100, 3),
        ],
        &["main", "  #1", "    #3 native-stack"],
    );
    assert_eq!(stacks.layer(3).unwrap().needs_rebase, None);
}
