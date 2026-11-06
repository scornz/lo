//! Putting base branches back after a conversion fails partway.

use stacks::convert::{Kept, Plan, rollback};

use crate::support::{With, assert_plan, pr, render_retarget};

/// The base-branch changes that put back the first `done` steps of `plan`.
fn put_back(plan: &Plan, done: usize) -> Vec<String> {
    let steps = rollback(plan, done).steps;
    steps.iter().map(render_retarget).collect()
}

/// A plan that moves two layers, then creates the stack.
fn moves_two_then_creates() -> Plan {
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
    )
}

#[test]
fn after_each_failing_step_the_base_branches_changed_go_back_last_first() {
    let plan = moves_two_then_creates();
    // The first base-branch change failed.
    assert!(put_back(&plan, 0).is_empty());
    // The second failed.
    assert_eq!(put_back(&plan, 1), ["#2 a -> main"]);
    // Creating the stack failed.
    assert_eq!(put_back(&plan, 2), ["#3 b -> main", "#2 a -> main"]);
    for done in 0..=2 {
        assert_eq!(rollback(&plan, done).kept, None);
    }
}

#[test]
fn a_created_stack_is_kept_and_can_be_unstacked() {
    let plan = moves_two_then_creates();
    let rollback = rollback(&plan, 3);
    assert_eq!(rollback.kept, Some(Kept::Created { prs: vec![1, 2, 3] }));
    let steps: Vec<String> = rollback.steps.iter().map(render_retarget).collect();
    assert_eq!(steps, ["#3 b -> main", "#2 a -> main"]);
}

#[test]
fn layers_added_to_a_github_stack_are_kept() {
    let plan = assert_plan(
        &[
            pr(1, "main", "a").native(100, 1),
            pr(2, "a", "b").native(100, 2),
            pr(3, "main", "c").commits(&["b.1", "c.1"]),
        ],
        3,
        &["main: #1 #2 #3", "#3 main -> b", "add #3 to stack 100"],
    );
    // Moving #3 failed.
    assert!(put_back(&plan, 0).is_empty());
    // Adding it failed.
    assert_eq!(put_back(&plan, 1), ["#3 b -> main"]);
    assert_eq!(rollback(&plan, 1).kept, None);
    // Everything ran.
    assert_eq!(put_back(&plan, 2), ["#3 b -> main"]);
    assert_eq!(
        rollback(&plan, 2).kept,
        Some(Kept::Added {
            stack: 100,
            prs: vec![3]
        })
    );
}

#[test]
fn a_plan_that_moves_nothing_has_nothing_to_put_back() {
    let plan = assert_plan(
        &[pr(1, "main", "a"), pr(2, "a", "b")],
        2,
        &["main: #1 #2", "create #1 #2"],
    );
    assert!(put_back(&plan, 0).is_empty());
    assert_eq!(rollback(&plan, 0).kept, None);
    assert!(put_back(&plan, 1).is_empty());
    assert_eq!(
        rollback(&plan, 1).kept,
        Some(Kept::Created { prs: vec![1, 2] })
    );
}

#[test]
fn more_steps_done_than_the_plan_has_counts_as_all_of_them() {
    let plan = moves_two_then_creates();
    assert_eq!(rollback(&plan, 99), rollback(&plan, 3));
}
