//! Converting a path that's already a GitHub stack, or starts as one.

use stacks::convert::Reason;

use crate::support::{With, assert_plan, plan, pr};

#[test]
fn a_github_stack_has_nothing_to_convert() {
    let prs = [
        pr(1, "main", "a").native(100, 1),
        pr(2, "a", "b").native(100, 2),
        pr(3, "b", "c").native(100, 3),
    ];
    for layer in 1..=3 {
        assert_eq!(
            plan(&prs, layer),
            Err(vec![Reason::AlreadyStacked { stack: 100 }])
        );
    }
}

#[test]
fn a_path_ending_inside_a_github_stack_has_nothing_to_convert() {
    // #2 has two layers on it, so the path through it ends there.
    let prs = [
        pr(1, "main", "a").native(100, 1),
        pr(2, "a", "b").native(100, 2),
        pr(3, "b", "c").native(100, 3),
        pr(4, "b", "d"),
    ];
    assert_eq!(
        plan(&prs, 2),
        Err(vec![Reason::AlreadyStacked { stack: 100 }])
    );
}

#[test]
fn the_last_open_layer_of_a_github_stack_has_nothing_to_convert() {
    // GitHub merged the two layers below #3.
    let prs = [pr(3, "main", "c").native(100, 3)];
    assert_eq!(
        plan(&prs, 3),
        Err(vec![Reason::AlreadyStacked { stack: 100 }])
    );
}

#[test]
fn layers_on_a_github_stack_are_added_to_it() {
    let prs = [
        pr(1, "main", "a").native(100, 1),
        pr(2, "a", "b").native(100, 2),
        pr(3, "b", "c"),
        pr(4, "main", "d").commits(&["c.1", "d.1"]),
    ];
    for layer in 1..=4 {
        assert_plan(
            &prs,
            layer,
            &[
                "main: #1 #2 #3 #4",
                "#4 main -> c",
                "add #3 #4 to stack 100",
            ],
        );
    }
}

#[test]
fn the_first_added_layer_moves_onto_the_stacks_top() {
    let prs = [
        pr(1, "main", "a").native(100, 1),
        pr(2, "a", "b").native(100, 2),
        pr(3, "main", "c").commits(&["b.1", "c.1"]),
    ];
    assert_plan(
        &prs,
        3,
        &["main: #1 #2 #3", "#3 main -> b", "add #3 to stack 100"],
    );
}

#[test]
fn a_github_stack_whose_bottom_merged_still_takes_layers() {
    // GitHub merged #1 and pointed #2 at main.
    let prs = [
        pr(1, "main", "a").native(100, 1).merged(false),
        pr(2, "main", "b").native(100, 2),
        pr(3, "b", "c"),
    ];
    assert_plan(&prs, 3, &["main: #2 #3", "add #3 to stack 100"]);
}

#[test]
fn layers_already_in_the_stack_keep_their_base_branches() {
    // #2 was closed but is still in the stack, and #3 still targets its branch. A layer found by
    // `Stacks::find` would move #3 onto #1's branch; a GitHub stack's own layers are left alone.
    let prs = [
        pr(1, "main", "a").native(100, 1),
        pr(2, "a", "b").native(100, 2).closed(),
        pr(3, "b", "c").native(100, 3),
        pr(4, "c", "d"),
    ];
    assert_plan(&prs, 4, &["main: #1 #3 #4", "add #4 to stack 100"]);
    assert_eq!(
        plan(&prs[..3], 3),
        Err(vec![Reason::AlreadyStacked { stack: 100 }])
    );
}

#[test]
fn not_offered_with_a_github_stack_above_the_bottom_layer() {
    // #2 and #3 are a GitHub stack on #1's branch. A pull request can be in only one stack.
    let prs = [
        pr(1, "main", "a"),
        pr(2, "a", "b").native(200, 1),
        pr(3, "b", "c").native(200, 2),
    ];
    assert_eq!(
        plan(&prs, 1),
        Err(vec![
            Reason::InAnotherStack { pr: 2, stack: 200 },
            Reason::InAnotherStack { pr: 3, stack: 200 },
        ])
    );
}

#[test]
fn not_offered_with_a_second_github_stack_on_the_first() {
    let prs = [
        pr(1, "main", "a").native(100, 1),
        pr(2, "a", "b").native(100, 2),
        pr(3, "b", "c").native(200, 1),
        pr(4, "c", "d"),
    ];
    assert_eq!(
        plan(&prs, 4),
        Err(vec![Reason::InAnotherStack { pr: 3, stack: 200 }])
    );
}

#[test]
fn not_offered_when_the_github_stack_goes_on_off_the_path() {
    // #2 has two layers on it: #3, the stack's next layer, and #4. Layers join a stack at its top.
    let prs = [
        pr(1, "main", "a").native(100, 1),
        pr(2, "a", "b").native(100, 2),
        pr(3, "b", "c").native(100, 3),
        pr(4, "b", "d"),
        pr(5, "d", "e"),
    ];
    assert_eq!(
        plan(&prs, 4),
        Err(vec![Reason::StackGoesOn {
            stack: 100,
            past: 2
        }])
    );
}
