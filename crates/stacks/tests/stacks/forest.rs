//! The forest's shape when nothing links pull requests: roots, order and which PRs are layers.

use stacks::{Parent, Stacks};

use crate::support::{With, assert_forest, pr};

#[test]
fn unlinked_prs_sit_on_their_base_branch() {
    let stacks = assert_forest(
        &[
            pr(1, "main", "a"),
            pr(2, "feature-x", "b"),
            pr(3, "main", "c"),
        ],
        &["feature-x", "  #2", "main", "  #1", "  #3"],
    );
    let layer = stacks.layer(2).unwrap();
    assert_eq!(layer.parent, Parent::Branch("feature-x".to_string()));
    assert_eq!(layer.depth, 0);
    assert!(layer.children.is_empty());
}

#[test]
fn each_unlinked_pr_is_its_own_stack() {
    let stacks = Stacks::find(&[pr(1, "main", "a"), pr(2, "main", "b")], &[]);
    let one = stacks.stack_of(1).unwrap();
    assert_eq!(one.root, "main");
    assert_eq!(one.layers, [1]);
    assert_eq!(stacks.stack_of(2).unwrap().layers, [2]);
}

#[test]
fn stacks_on_a_root_are_in_creation_order() {
    assert_forest(
        &[
            pr(1, "main", "a").created(30),
            pr(2, "main", "b").created(10),
            pr(3, "main", "c").created(20),
            // Same time as #2: the lower number first.
            pr(4, "main", "d").created(10),
        ],
        &["main", "  #2", "  #4", "  #3", "  #1"],
    );
}

#[test]
fn closed_and_merged_prs_are_not_layers() {
    let stacks = assert_forest(
        &[
            pr(1, "main", "a"),
            pr(2, "main", "b").merged(true),
            pr(3, "main", "c").closed(),
        ],
        &["main", "  #1"],
    );
    assert!(stacks.layer(2).is_none());
    assert!(stacks.stack_of(3).is_none());
}

#[test]
fn unknown_prs_have_no_stack() {
    let stacks = Stacks::find(&[pr(1, "main", "a")], &[]);
    assert!(stacks.layer(9).is_none());
    assert!(stacks.stack_of(9).is_none());
}

#[test]
fn no_prs_make_an_empty_forest() {
    let stacks = Stacks::find(&[], &[]);
    assert!(stacks.roots().is_empty());
}

#[test]
fn a_repeated_number_keeps_the_first() {
    let stacks = assert_forest(
        &[pr(1, "main", "a"), pr(1, "release", "a")],
        &["main", "  #1"],
    );
    assert_eq!(
        stacks.layer(1).unwrap().parent,
        Parent::Branch("main".into())
    );
}
