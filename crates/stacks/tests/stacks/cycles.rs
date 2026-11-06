//! Links that loop back are broken at one link, and reported.

use stacks::{Cycle, Parent, Stacks};

use crate::support::{With, assert_forest, pr};

#[test]
fn a_cycle_is_broken_at_the_oldest_prs_link() {
    // Each targets the other's head branch.
    let stacks = assert_forest(
        &[pr(1, "b", "a"), pr(2, "a", "b")],
        &["b", "  #1", "    #2 base-branch"],
    );
    assert_eq!(stacks.cycles(), [Cycle { prs: vec![1, 2] }]);
    let layer = stacks.layer(1).unwrap();
    assert_eq!(layer.parent, Parent::Branch("b".into()));
    assert_eq!(layer.rule, None);
}

#[test]
fn a_longer_cycle_is_reported_in_link_order() {
    let stacks = assert_forest(
        &[pr(1, "c", "a"), pr(2, "a", "b"), pr(3, "b", "c")],
        &["c", "  #1", "    #2 base-branch", "      #3 base-branch"],
    );
    // #1 links to #3, #3 to #2, and #2 back to #1.
    assert_eq!(stacks.cycles(), [Cycle { prs: vec![1, 3, 2] }]);
}

#[test]
fn the_link_found_by_the_weakest_rule_is_broken() {
    // #1's commits are stale and contain #2's head, though GitHub says #2 is above #1. #2 is
    // older, so only the rule decides.
    let stacks = assert_forest(
        &[
            pr(1, "main", "a")
                .native(100, 1)
                .commits(&["b.1", "a.1"])
                .created(5),
            pr(2, "a", "b")
                .native(100, 2)
                .commits(&["a.1", "b.1"])
                .created(1),
        ],
        &["main", "  #1", "    #2 native-stack"],
    );
    assert_eq!(stacks.cycles(), [Cycle { prs: vec![1, 2] }]);
}

#[test]
fn layers_above_a_cycle_are_kept() {
    let stacks = assert_forest(
        &[pr(1, "b", "a"), pr(2, "a", "b"), pr(3, "a", "c")],
        &["b", "  #1", "    #2 base-branch", "    #3 base-branch"],
    );
    assert_eq!(stacks.cycles().len(), 1);
    assert_eq!(stacks.stack_of(3).unwrap().layers, [1, 2, 3]);
}

#[test]
fn separate_cycles_are_each_reported() {
    let stacks = Stacks::find(
        &[
            pr(1, "b", "a"),
            pr(2, "a", "b"),
            pr(3, "d", "c"),
            pr(4, "c", "d"),
        ],
        &[],
    );
    assert_eq!(
        stacks.cycles(),
        [Cycle { prs: vec![1, 2] }, Cycle { prs: vec![3, 4] }]
    );
}

#[test]
fn a_forest_without_cycles_reports_none() {
    let stacks = Stacks::find(&[pr(1, "main", "a"), pr(2, "a", "b")], &[]);
    assert!(stacks.cycles().is_empty());
}
