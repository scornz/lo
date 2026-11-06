//! Merge status, merge actions and approve and ship for whole pull requests, from snapshots filled
//! the way the GitHub layer fills them: GraphQL's and REST's enum names, check runs and commit
//! statuses, and full commit SHAs.

use review::merge::{
    AllowedMethods, Approval, ApproveAndShip, AutoMerge, Blocker, CheckState, MergeAction,
    MergeMethod, MergeQueue, MergeSnapshot, MergeStatus, MissingApprovals, NotPossible, Plan,
    RequiredCheck, Review, ReviewRequest, ReviewRules, Reviewer, Ship, Stop, UnmetRule, Viewer,
    after_approving, approve_and_ship, blockers, merge_action, status,
};

const HEAD: &str = "4e1d2c9b7f3a5e60d8c1b2a3f4e5d6c7b8a9f0e1";
/// An earlier head.
const OLD_HEAD: &str = "77c0a1e3d5b7f9a1c3e5d7b9f1a3c5e7d9b1f3a5";

/// A pull request as GraphQL reports it: `state`, `mergeable`, `mergeStateStatus` and
/// `reviewDecision`, with the head on screen up to date.
fn pull_request(
    url: &str,
    state: &str,
    mergeable: &str,
    merge_state: &str,
    decision: Option<&str>,
) -> MergeSnapshot {
    MergeSnapshot {
        url: url.to_string(),
        state: state.parse().unwrap(),
        mergeable: mergeable.parse().unwrap(),
        merge_state: merge_state.parse().unwrap(),
        review_decision: decision.map(|decision| decision.parse().unwrap()),
        head: HEAD.to_string(),
        head_on_screen: HEAD.to_string(),
        ..Default::default()
    }
}

/// An entry of `latestOpinionatedReviews`.
fn review(author: &str, state: &str, commit: &str, on_behalf_of: &[&str]) -> Review {
    Review {
        author: author.to_string(),
        state: state.parse().unwrap(),
        commit: Some(commit.to_string()),
        counts: true,
        on_behalf_of: on_behalf_of.iter().map(|team| team.to_string()).collect(),
    }
}

/// A required check run, from its `status` and `conclusion`.
fn check_run(name: &str, status: &str, conclusion: Option<&str>) -> RequiredCheck {
    RequiredCheck {
        name: name.to_string(),
        state: CheckState::of_check_run(
            status.parse().unwrap(),
            conclusion.map(|conclusion| conclusion.parse().unwrap()),
        ),
        url: Some(format!("https://github.com/acme/shop/actions/runs/{name}")),
    }
}

/// A required commit status, from its `state`.
fn commit_status(context: &str, state: &str) -> RequiredCheck {
    let state = CheckState::of_status(state.parse().unwrap());
    RequiredCheck {
        name: context.to_string(),
        state,
        url: (state != CheckState::Missing).then(|| format!("https://ci.acme.dev/{context}")),
    }
}

fn code_owner(team: &str) -> ReviewRequest {
    ReviewRequest {
        reviewer: Reviewer::Team(team.to_string()),
        as_code_owner: true,
    }
}

/// You, with write access, in the payments team.
fn you() -> Viewer {
    Viewer {
        login: "mika".to_string(),
        teams: vec!["acme/payments".to_string()],
        can_approve: true,
        approval_counts: true,
        can_merge: true,
        can_enable_auto_merge: true,
        ..Default::default()
    }
}

fn plan(approval: Approval, likely: Ship) -> ApproveAndShip {
    ApproveAndShip::Plan(Plan {
        head: HEAD.to_string(),
        approval,
        likely,
        stale_approval_warning: false,
    })
}

/// The plan, with the confirmation warning that a later push could merge without a new review.
fn warned(plan: ApproveAndShip) -> ApproveAndShip {
    match plan {
        ApproveAndShip::Plan(plan) => ApproveAndShip::Plan(Plan {
            stale_approval_warning: true,
            ..plan
        }),
        other => other,
    }
}

/// The plan for a snapshot that gets one.
fn the_plan(snapshot: &MergeSnapshot) -> Plan {
    match approve_and_ship(snapshot) {
        ApproveAndShip::Plan(plan) => plan,
        other => panic!("expected a plan, got {other:?}"),
    }
}

/// The pull request as GitHub reports it once your approval of the head has landed.
fn approved_by_you(snapshot: &MergeSnapshot, decision: &str, merge_state: &str) -> MergeSnapshot {
    let mut after = snapshot.clone();
    (after.reviews).push(review(&after.viewer.login.clone(), "APPROVED", HEAD, &[]));
    after.review_decision = decision.parse().ok();
    after.merge_state = merge_state.parse().unwrap();
    after
}

fn blocked(blockers: Vec<Blocker>) -> Ship {
    Ship::Stop(Stop::Blocked {
        blockers,
        auto_merge: false,
    })
}

const ALL_METHODS: AllowedMethods = AllowedMethods {
    merge: true,
    squash: true,
    rebase: true,
};

// A dependency bump: CI still running, one approval required, and you're the reviewer.

fn dependency_bump() -> MergeSnapshot {
    MergeSnapshot {
        rules: ReviewRules {
            required_approvals: 1,
            ..Default::default()
        },
        required_checks: vec![
            check_run("build", "COMPLETED", Some("SUCCESS")),
            check_run("test (ubuntu)", "IN_PROGRESS", None),
            check_run("test (macos)", "QUEUED", None),
            check_run("codecov/patch", "COMPLETED", Some("NEUTRAL")),
            commit_status("license/cla", "SUCCESS"),
        ],
        methods: ALL_METHODS,
        preferred_method: Some(MergeMethod::Squash),
        viewer: you(),
        ..pull_request(
            "https://github.com/acme/shop/pull/4410",
            "OPEN",
            "MERGEABLE",
            "BLOCKED",
            Some("REVIEW_REQUIRED"),
        )
    }
}

#[test]
fn a_dependency_bump_waits_on_ci_and_an_approval() {
    let snapshot = dependency_bump();

    assert_eq!(
        blockers(&snapshot),
        [
            Blocker::Approvals(MissingApprovals {
                more: 1,
                code_owners: vec![],
            }),
            Blocker::Check(check_run("test (ubuntu)", "IN_PROGRESS", None)),
            Blocker::Check(check_run("test (macos)", "QUEUED", None)),
        ]
    );
    assert!(matches!(
        status(&snapshot),
        MergeStatus::Waiting {
            auto_merge: false,
            ..
        }
    ));
    assert_eq!(
        merge_action(&snapshot),
        MergeAction::NotPossible(NotPossible::Blocked)
    );
    assert_eq!(
        approve_and_ship(&snapshot),
        warned(plan(
            Approval::Approve,
            Ship::AutoMerge {
                method: MergeMethod::Squash,
                waiting_on: vec![
                    Blocker::Check(check_run("test (ubuntu)", "IN_PROGRESS", None)),
                    Blocker::Check(check_run("test (macos)", "QUEUED", None)),
                ],
            }
        ))
    );
}

#[test]
fn a_dependency_bump_with_ci_done_merges_once_github_agrees_after_you_approve() {
    let mut snapshot = dependency_bump();
    snapshot.required_checks[1] = check_run("test (ubuntu)", "COMPLETED", Some("SUCCESS"));
    snapshot.required_checks[2] = check_run("test (macos)", "COMPLETED", Some("SKIPPED"));
    let plan = the_plan(&snapshot);

    assert_eq!(plan.approval, Approval::Approve);
    assert_eq!(plan.likely, Ship::Merge(MergeMethod::Squash));
    assert_eq!(
        plan.wording(),
        "will likely merge. While auto-merge is on, a push could merge without a new review"
    );
    assert_eq!(
        after_approving(&approved_by_you(&snapshot, "APPROVED", "CLEAN"), &plan),
        Ship::Merge(MergeMethod::Squash)
    );
}

#[test]
fn a_dependency_bump_whose_tests_failed_stops_after_approving() {
    let mut snapshot = dependency_bump();
    snapshot.required_checks[1] = check_run("test (ubuntu)", "COMPLETED", Some("FAILURE"));
    snapshot.required_checks[4] = commit_status("license/cla", "EXPECTED");

    let plan = the_plan(&snapshot);
    let expected = vec![
        Blocker::Check(check_run("test (ubuntu)", "COMPLETED", Some("FAILURE"))),
        Blocker::Check(check_run("test (macos)", "QUEUED", None)),
        Blocker::Check(commit_status("license/cla", "EXPECTED")),
    ];
    assert_eq!(plan.approval, Approval::Approve);
    assert_eq!(plan.likely, blocked(expected.clone()));
    assert_eq!(
        after_approving(&approved_by_you(&snapshot, "APPROVED", "BLOCKED"), &plan),
        blocked(expected)
    );
}

#[test]
fn a_failing_status_beside_a_passing_check_run_of_the_same_name_blocks_it() {
    let mut snapshot = dependency_bump();
    snapshot.required_checks[1] = check_run("test (ubuntu)", "COMPLETED", Some("SUCCESS"));
    snapshot.required_checks[2] = check_run("test (macos)", "COMPLETED", Some("SUCCESS"));
    snapshot
        .required_checks
        .push(commit_status("build", "FAILURE"));

    assert_eq!(
        blockers(&snapshot),
        [
            Blocker::Check(commit_status("build", "FAILURE")),
            Blocker::Approvals(MissingApprovals {
                more: 1,
                code_owners: vec![],
            }),
        ]
    );
}

#[test]
fn a_dependency_bump_pushed_to_while_you_read_it_stops_before_approving() {
    let snapshot = MergeSnapshot {
        head: "a91f03b6c8e0a2c4e6a8c0e2a4c6e8a0c2e4a6c8".to_string(),
        ..dependency_bump()
    };

    assert_eq!(
        approve_and_ship(&snapshot),
        ApproveAndShip::HeadMoved {
            on_screen: HEAD.to_string(),
            head: "a91f03b6c8e0a2c4e6a8c0e2a4c6e8a0c2e4a6c8".to_string(),
        }
    );
}

// A change across two code-owned areas of a monorepo, in a repository that requires code-owner
// review and resolved conversations.

fn checkout_change() -> MergeSnapshot {
    MergeSnapshot {
        review_requests: vec![
            code_owner("acme/payments"),
            code_owner("acme/platform"),
            ReviewRequest {
                reviewer: Reviewer::User("lee".to_string()),
                as_code_owner: false,
            },
        ],
        rules: ReviewRules {
            required_approvals: 1,
            code_owner_review: true,
            conversation_resolution: true,
            dismiss_stale_reviews: true,
            last_push_approval: false,
        },
        required_checks: vec![check_run("ci", "COMPLETED", Some("SUCCESS"))],
        methods: AllowedMethods {
            merge: false,
            ..ALL_METHODS
        },
        preferred_method: Some(MergeMethod::Merge),
        viewer: you(),
        ..pull_request(
            "https://github.com/acme/shop/pull/4398",
            "OPEN",
            "MERGEABLE",
            "BLOCKED",
            Some("REVIEW_REQUIRED"),
        )
    }
}

#[test]
fn a_change_across_code_owned_areas_names_the_owners_missing() {
    assert_eq!(
        blockers(&checkout_change()),
        [Blocker::Approvals(MissingApprovals {
            more: 1,
            code_owners: vec![
                Reviewer::Team("acme/payments".to_string()),
                Reviewer::Team("acme/platform".to_string()),
            ],
        })]
    );
}

#[test]
fn your_approval_covers_your_team_and_auto_merge_waits_for_the_other() {
    let snapshot = checkout_change();
    let other_team = Blocker::Approvals(MissingApprovals {
        more: 0,
        code_owners: vec![Reviewer::Team("acme/platform".to_string())],
    });
    let plan = the_plan(&snapshot);

    assert_eq!(
        plan.likely,
        Ship::AutoMerge {
            method: MergeMethod::Squash,
            waiting_on: vec![other_team.clone()],
        }
    );
    assert_eq!(
        plan.wording(),
        "will likely wait for approvals, with auto-merge on"
    );
    let mut after = approved_by_you(&snapshot, "REVIEW_REQUIRED", "BLOCKED");
    after.reviews.last_mut().unwrap().on_behalf_of = vec!["acme/payments".to_string()];
    after.review_requests.remove(0);
    assert_eq!(
        after_approving(&after, &plan),
        Ship::AutoMerge {
            method: MergeMethod::Squash,
            waiting_on: vec![other_team],
        }
    );
}

#[test]
fn once_the_other_team_approved_your_approval_merges_it() {
    let mut snapshot = checkout_change();
    snapshot
        .reviews
        .push(review("ana", "APPROVED", HEAD, &["acme/platform"]));

    assert_eq!(
        approve_and_ship(&snapshot),
        plan(Approval::Approve, Ship::Merge(MergeMethod::Squash))
    );
}

#[test]
fn a_code_owner_who_only_commented_is_waited_on_rather_than_merged_past() {
    // The platform team's owner left a comment. GitHub dropped the team's request, and a comment
    // isn't an opinionated review, so nothing in the snapshot names the owner GitHub still needs.
    let mut snapshot = checkout_change();
    snapshot.review_requests.remove(1);
    let plan = the_plan(&snapshot);
    let mut after = approved_by_you(&snapshot, "REVIEW_REQUIRED", "BLOCKED");
    after.reviews.last_mut().unwrap().on_behalf_of = vec!["acme/payments".to_string()];
    after.review_requests.remove(0);

    assert_eq!(plan.likely, Ship::Merge(MergeMethod::Squash));
    assert_eq!(
        after_approving(&after, &plan),
        Ship::AutoMerge {
            method: MergeMethod::Squash,
            waiting_on: vec![Blocker::Approvals(MissingApprovals {
                more: 0,
                code_owners: vec![],
            })],
        }
    );
}

#[test]
fn unresolved_conversations_stop_it_after_approving() {
    let snapshot = MergeSnapshot {
        unresolved_threads: 2,
        ..checkout_change()
    };

    assert_eq!(
        approve_and_ship(&snapshot),
        plan(
            Approval::Approve,
            blocked(vec![
                Blocker::Conversations { unresolved: 2 },
                Blocker::Approvals(MissingApprovals {
                    more: 0,
                    code_owners: vec![Reviewer::Team("acme/platform".to_string())],
                }),
            ])
        )
    );
}

// A busy repository whose main branch requires a merge queue that squashes.

fn queued_repository() -> MergeSnapshot {
    MergeSnapshot {
        reviews: vec![review("ana", "APPROVED", HEAD, &[])],
        rules: ReviewRules {
            required_approvals: 2,
            ..Default::default()
        },
        required_checks: vec![
            check_run("lint", "COMPLETED", Some("SUCCESS")),
            check_run("test", "COMPLETED", Some("SUCCESS")),
        ],
        queue: MergeQueue {
            required: true,
            method: "SQUASH".parse().ok(),
            ..Default::default()
        },
        methods: ALL_METHODS,
        viewer: you(),
        ..pull_request(
            "https://github.com/acme/shop/pull/4402",
            "OPEN",
            "MERGEABLE",
            "BLOCKED",
            Some("REVIEW_REQUIRED"),
        )
    }
}

#[test]
fn a_queue_branch_joins_the_queue_once_you_approve() {
    let snapshot = queued_repository();
    let queue = the_plan(&snapshot);

    assert_eq!(
        approve_and_ship(&snapshot),
        warned(plan(Approval::Approve, Ship::Enqueue))
    );
    assert_eq!(
        queue.wording(),
        "will likely join the merge queue. While auto-merge is on, a push could merge without \
         a new review"
    );
    assert_eq!(
        after_approving(&approved_by_you(&snapshot, "APPROVED", "CLEAN"), &queue),
        Ship::Enqueue
    );
}

#[test]
fn an_approved_pull_request_on_a_queue_branch_offers_the_queue() {
    let mut snapshot = queued_repository();
    snapshot.reviews.push(review("mika", "APPROVED", HEAD, &[]));
    snapshot.review_decision = "APPROVED".parse().ok();
    snapshot.merge_state = "CLEAN".parse().unwrap();

    assert_eq!(
        merge_action(&snapshot),
        MergeAction::Enqueue {
            method: Some(MergeMethod::Squash)
        }
    );
    assert_eq!(status(&snapshot), MergeStatus::Ready);
    assert_eq!(
        approve_and_ship(&snapshot),
        plan(Approval::AlreadyApproved, Ship::Enqueue)
    );
}

#[test]
fn a_queued_pull_request_shows_its_position_and_ships_nothing_more() {
    let mut snapshot = queued_repository();
    snapshot.reviews.push(review("mika", "APPROVED", HEAD, &[]));
    snapshot.review_decision = "APPROVED".parse().ok();
    snapshot.merge_state = "CLEAN".parse().unwrap();
    snapshot.queue.in_queue = true;
    snapshot.queue.position = Some(3);

    assert_eq!(status(&snapshot), MergeStatus::Queued { position: Some(3) });
    assert_eq!(
        merge_action(&snapshot),
        MergeAction::NotPossible(NotPossible::Queued { position: Some(3) })
    );
    assert_eq!(
        approve_and_ship(&snapshot),
        plan(Approval::AlreadyApproved, Ship::AlreadyQueued)
    );
}

#[test]
fn a_queue_branch_with_another_approval_missing_joins_when_ready() {
    let mut snapshot = queued_repository();
    snapshot.rules.required_approvals = 3;

    assert_eq!(
        approve_and_ship(&snapshot),
        warned(plan(
            Approval::Approve,
            Ship::EnqueueWhenReady {
                waiting_on: vec![Blocker::Approvals(MissingApprovals {
                    more: 1,
                    code_owners: vec![],
                })],
            }
        ))
    );
}

// Your own pull request, as REST reports it.

fn your_pull_request() -> MergeSnapshot {
    MergeSnapshot {
        reviews: vec![review("ana", "APPROVED", OLD_HEAD, &[])],
        rules: ReviewRules {
            required_approvals: 1,
            ..Default::default()
        },
        required_checks: vec![commit_status("ci/build", "pending")],
        auto_merge: Some(AutoMerge {
            method: "rebase".parse().unwrap(),
            enabled_by: Some("mika".to_string()),
        }),
        methods: ALL_METHODS,
        viewer: Viewer {
            did_author: true,
            can_approve: false,
            ..you()
        },
        ..pull_request(
            "https://github.com/acme/shop/pull/4415",
            "open",
            "MERGEABLE",
            "blocked",
            Some("APPROVED"),
        )
    }
}

#[test]
fn your_pull_request_with_auto_merge_on_waits_for_ci() {
    let snapshot = your_pull_request();

    assert_eq!(
        status(&snapshot),
        MergeStatus::Waiting {
            blockers: vec![Blocker::Check(commit_status("ci/build", "pending"))],
            auto_merge: true,
            partial: true,
        }
    );
    assert_eq!(
        approve_and_ship(&snapshot),
        warned(plan(
            Approval::OwnPullRequest,
            Ship::AutoMergeAlreadyOn {
                waiting_on: vec![Blocker::Check(commit_status("ci/build", "pending"))],
            }
        ))
    );
}

#[test]
fn your_pull_request_with_ci_done_merges_without_approving() {
    let snapshot = MergeSnapshot {
        required_checks: vec![commit_status("ci/build", "success")],
        auto_merge: None,
        merge_state: "clean".parse().unwrap(),
        preferred_method: Some(MergeMethod::Rebase),
        ..your_pull_request()
    };

    assert_eq!(
        approve_and_ship(&snapshot),
        plan(Approval::OwnPullRequest, Ship::Merge(MergeMethod::Rebase))
    );
}

// Pull requests that can't merge for reasons waiting won't fix.

/// Every reason a conflicting draft can have at once. Being merged or closed, a block nothing
/// explains, GitHub working out mergeability and a check nothing will report can't happen beside
/// these, so other tests cover them.
#[test]
fn a_conflicting_draft_lists_every_other_reason_it_cannot_merge() {
    let signatures = UnmetRule {
        description: "Commits must have verified signatures".to_string(),
        url: None,
        waiting: false,
    };
    let deployment = UnmetRule {
        description: "Deployment to staging must succeed".to_string(),
        url: Some("https://github.com/acme/shop/deployments/staging".to_string()),
        waiting: true,
    };
    let snapshot = MergeSnapshot {
        draft: true,
        out_of_date: true,
        reviews: vec![
            review("ana", "CHANGES_REQUESTED", OLD_HEAD, &[]),
            review("lee", "APPROVED", HEAD, &[]),
        ],
        review_requests: vec![code_owner("acme/platform")],
        rules: ReviewRules {
            required_approvals: 2,
            code_owner_review: true,
            conversation_resolution: true,
            ..Default::default()
        },
        required_checks: vec![
            check_run("build", "COMPLETED", Some("TIMED_OUT")),
            check_run("e2e", "WAITING", None),
            check_run("test", "IN_PROGRESS", None),
            commit_status("license/cla", "EXPECTED"),
        ],
        unresolved_threads: 5,
        unmet_rules: vec![deployment.clone(), signatures.clone()],
        viewer: you(),
        methods: ALL_METHODS,
        ..pull_request(
            "https://github.com/acme/shop/pull/4377",
            "OPEN",
            "CONFLICTING",
            "DIRTY",
            Some("CHANGES_REQUESTED"),
        )
    };

    let expected = vec![
        Blocker::Draft,
        Blocker::Conflicts,
        Blocker::OutOfDate,
        Blocker::ChangesRequested {
            by: vec!["ana".to_string()],
        },
        Blocker::Check(check_run("build", "COMPLETED", Some("TIMED_OUT"))),
        Blocker::Conversations { unresolved: 5 },
        Blocker::Rule(signatures),
        Blocker::Approvals(MissingApprovals {
            more: 1,
            code_owners: vec![Reviewer::Team("acme/platform".to_string())],
        }),
        Blocker::Check(check_run("e2e", "WAITING", None)),
        Blocker::Check(check_run("test", "IN_PROGRESS", None)),
        Blocker::Check(commit_status("license/cla", "EXPECTED")),
        Blocker::Rule(deployment),
    ];
    assert_eq!(blockers(&snapshot), expected);
    assert_eq!(
        status(&snapshot),
        MergeStatus::Blocked {
            blockers: expected,
            auto_merge: false,
            partial: false,
        }
    );
    assert_eq!(approve_and_ship(&snapshot), ApproveAndShip::Draft);
}

#[test]
fn a_block_github_reports_without_a_reason_links_to_the_pull_request() {
    let snapshot = MergeSnapshot {
        reviews: vec![review("ana", "APPROVED", HEAD, &[])],
        required_checks: vec![check_run("ci", "COMPLETED", Some("SUCCESS"))],
        viewer: you(),
        methods: ALL_METHODS,
        ..pull_request(
            "https://github.com/acme/shop/pull/4390",
            "OPEN",
            "MERGEABLE",
            "BLOCKED",
            Some("APPROVED"),
        )
    };
    let unexplained = Blocker::Unexplained {
        url: "https://github.com/acme/shop/pull/4390".to_string(),
    };

    assert_eq!(blockers(&snapshot), std::slice::from_ref(&unexplained));
    assert_eq!(
        approve_and_ship(&snapshot),
        plan(Approval::Approve, blocked(vec![unexplained]))
    );
}

#[test]
fn a_merged_pull_request_has_nothing_to_do() {
    let snapshot = MergeSnapshot {
        viewer: you(),
        ..pull_request(
            "https://github.com/acme/shop/pull/4300",
            "MERGED",
            "UNKNOWN",
            "UNKNOWN",
            Some("APPROVED"),
        )
    };

    assert_eq!(blockers(&snapshot), [Blocker::Merged]);
    assert_eq!(status(&snapshot), MergeStatus::Merged);
    assert_eq!(
        merge_action(&snapshot),
        MergeAction::NotPossible(NotPossible::Merged)
    );
    assert_eq!(approve_and_ship(&snapshot), ApproveAndShip::Merged);
}
