//! When "Make this a GitHub stack" isn't offered, and the reasons it gives.

use stacks::convert::{Facts, Reason, plan_conversion};
use stacks::{PullRequest, Stacks};

use crate::support::{With, pr};

fn plan_with(prs: &[PullRequest], layer: u64, facts: Facts) -> Result<Vec<u64>, Vec<Reason>> {
    let plan = plan_conversion(&Stacks::find(prs, &[]), layer, &facts)?;
    Ok(plan.path)
}

const OFFERED: Facts = Facts {
    stacks_api: true,
    queued: &[],
};

#[test]
fn not_offered_without_the_stacks_api() {
    let prs = [pr(1, "main", "a"), pr(2, "a", "b")];
    let facts = Facts {
        stacks_api: false,
        ..OFFERED
    };
    assert_eq!(plan_with(&prs, 2, facts), Err(vec![Reason::NoStacksApi]));
}

#[test]
fn not_offered_with_a_pr_from_a_fork_at_the_bottom() {
    // A fork's branch can't be a base, so #2 sits on #1 by shared commits.
    let prs = [
        pr(1, "main", "feature").fork().commits(&["f.1"]),
        pr(2, "main", "more").commits(&["f.1", "m.1"]),
    ];
    assert_eq!(
        plan_with(&prs, 2, OFFERED),
        Err(vec![Reason::FromFork { pr: 1 }])
    );
}

#[test]
fn not_offered_with_a_pr_from_a_fork_at_the_top() {
    let prs = [pr(1, "main", "a"), pr(2, "a", "b").fork()];
    assert_eq!(
        plan_with(&prs, 1, OFFERED),
        Err(vec![Reason::FromFork { pr: 2 }])
    );
}

#[test]
fn not_offered_with_a_pr_in_a_merge_queue() {
    let prs = [pr(1, "main", "a"), pr(2, "a", "b"), pr(3, "b", "c")];
    let facts = Facts {
        queued: &[2],
        ..OFFERED
    };
    assert_eq!(
        plan_with(&prs, 3, facts),
        Err(vec![Reason::Queued { pr: 2 }])
    );
}

#[test]
fn prs_off_the_path_dont_matter() {
    // #3, a fork PR in the queue, is on another branch of the tree.
    let prs = [pr(1, "main", "a"), pr(2, "a", "b"), pr(3, "a", "c").fork()];
    let facts = Facts {
        queued: &[3],
        ..OFFERED
    };
    assert_eq!(plan_with(&prs, 2, facts), Ok(vec![1, 2]));
}

#[test]
fn every_reason_is_listed_bottom_to_top() {
    let prs = [
        pr(1, "main", "feature").fork().commits(&["f.1"]),
        pr(2, "main", "b").commits(&["f.1", "b.1"]),
        pr(3, "b", "c").fork(),
    ];
    let facts = Facts {
        stacks_api: false,
        queued: &[3, 2],
    };
    assert_eq!(
        plan_with(&prs, 1, facts),
        Err(vec![
            Reason::NoStacksApi,
            Reason::FromFork { pr: 1 },
            Reason::Queued { pr: 2 },
            Reason::FromFork { pr: 3 },
            Reason::Queued { pr: 3 },
        ])
    );
}

#[test]
fn a_short_path_lists_its_other_reasons_too() {
    let facts = Facts {
        stacks_api: false,
        queued: &[1],
    };
    assert_eq!(
        plan_with(&[pr(1, "main", "a")], 1, facts),
        Err(vec![
            Reason::NoStacksApi,
            Reason::TooShort,
            Reason::Queued { pr: 1 }
        ])
    );
}

#[test]
fn a_pr_that_isnt_a_layer_gives_only_that_reason() {
    let facts = Facts {
        stacks_api: false,
        queued: &[9],
    };
    assert_eq!(
        plan_with(&[pr(1, "main", "a")], 9, facts),
        Err(vec![Reason::NotOpen])
    );
}
