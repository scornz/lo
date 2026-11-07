//! What decides a merge, read from recorded answers (`tests/record_fixtures.rs`) and mapped into
//! `review::merge`, where `blockers`, `status`, `merge_action` and `approve_and_ship` decide with
//! it. Every recording is as `rozed-bot`, read only:
//!
//! - `cli/cli`, which rozed-bot can only read: trunk's classic protection, which a reader sees
//!   only part of, requires an approval, a code owner's (a team rozed-bot can't see) and three
//!   build checks. A failed required check (#13788), conflicts (#12942), a draft (#14781),
//!   changes requested (#13899), a missing approval (#14964), and a first-time contributor's
//!   checks that never ran (#13340).
//! - `microsoft/vscode#346714`: a required check running and two yet to report, and two
//!   unresolved conversations under a rule that requires them resolved.
//! - `pnpm/pnpm#17816`: a code owner, a user, who hasn't approved.
//! - `electron/electron#55594`: approved, auto-merge on, a required check running.
//! - `pytorch/pytorch#115316`: over 100 checks, read a page at a time, and a block Lo can't
//!   explain.
//! - `zed-industries/zed`: rulesets alone, which a reader sees all of: a merge queue, squash
//!   merges, code owners and two required checks. Checks that never reported (#66631), and one
//!   ready for the queue (#66650), whose rules are then read again for free.
//! - `flutter/flutter`'s merge queue, and the pull request in it.
//! - `rozed-dev/e2e#16`, rozed-bot's own pull request in a repository it administers.
//!
//! E4's protected bases and E5's pull requests rozed-bot can approve aren't built yet, so the
//! states only they would show are recorded answers edited here: your pending review, a code
//! owner team you're in, protection only an admin sees, with strict status checks, and rules
//! that can't be read.

use std::sync::Arc;

use futures::executor::block_on;
use github::cassette::{Interaction, fixtures_dir, read_cassette};
use github::poll::CachedRead;
use github::queries::{BranchRule, Local, MergeFacts};
use github::replay::ReplayHttpClient;
use github::{GithubClient, GithubError, PullRef, RepoRef, Token};
use review::merge::{
    AllowedMethods, Approval, ApproveAndShip, Blocker, CheckState, MergeAction, MergeMethod,
    MergeSnapshot, MergeStatus, MissingApprovals, NotPossible, Reviewer, Ship, Stop,
    approve_and_ship, blockers, may_not_see_every_rule, merge_action, status,
};
use review::pending::Refusal;

const BOT: &str = "rozed-bot";

fn client(http: Arc<ReplayHttpClient>) -> GithubClient {
    GithubClient::new(http, Token::new("gho_FAKE"))
}

/// The GraphQL operations and REST paths a replaying client was asked, in order.
fn asked(http: &ReplayHttpClient) -> Vec<String> {
    (http.seen().iter())
        .map(
            |seen| match serde_json::from_slice::<serde_json::Value>(&seen.body) {
                Ok(body) => body["operationName"].as_str().unwrap().to_string(),
                Err(_) => seen
                    .url
                    .trim_start_matches("https://api.github.com")
                    .to_string(),
            },
        )
        .collect()
}

/// What decides whether each of `numbers` in `owner/name` can merge, read in order from
/// `cassette`.
fn read(cassette: &str, owner: &str, name: &str, numbers: &[i64]) -> Vec<MergeFacts> {
    let client = client(ReplayHttpClient::cassette(cassette));
    (numbers.iter())
        .map(|&number| {
            let pull = PullRef::new(owner, name, number);
            block_on(client.merge_facts(&pull, BOT, None)).unwrap()
        })
        .collect()
}

/// `cassette`'s interactions, each edited by `edit` with its file name.
fn edited(cassette: &str, edit: impl Fn(&str, &mut Interaction)) -> Arc<ReplayHttpClient> {
    let (_, interactions) = read_cassette(&fixtures_dir().join(cassette)).unwrap();
    let interactions = interactions.into_iter().map(|(file, mut interaction)| {
        edit(&file, &mut interaction);
        interaction
    });
    Arc::new(ReplayHttpClient::from_interactions(cassette, interactions).unwrap())
}

/// The pull request in a recorded `MergeSnapshot` answer.
fn pull_request(interaction: &mut Interaction) -> &mut serde_json::Value {
    let body = interaction.response.body.as_mut().unwrap();
    &mut body["data"]["repository"]["pullRequest"]
}

/// The snapshot with the head GitHub has on screen and nothing remembered.
fn on_screen(facts: &MergeFacts) -> MergeSnapshot {
    facts.snapshot(&Local {
        head_on_screen: facts.pull_request.head.clone(),
        ..Local::default()
    })
}

fn check_states(snapshot: &MergeSnapshot) -> Vec<(&str, CheckState)> {
    (snapshot.required_checks.iter())
        .map(|check| (check.name.as_str(), check.state))
        .collect()
}

/// The blockers, each by kind, with a check's name.
fn kinds(snapshot: &MergeSnapshot) -> Vec<String> {
    (blockers(snapshot).iter())
        .map(|blocker| match blocker {
            Blocker::Check(check) => format!("Check({}, {:?})", check.name, check.state),
            Blocker::NotReported(check) => format!("NotReported({})", check.name),
            other => {
                let debug = format!("{other:?}");
                debug.split([' ', '(', '{']).next().unwrap().to_string()
            }
        })
        .collect()
}

const CLI: [i64; 6] = [13788, 12942, 14781, 13899, 14964, 13340];

fn cli() -> Vec<MergeSnapshot> {
    read("merge_cli", "cli", "cli", &CLI)
        .iter()
        .map(on_screen)
        .collect()
}

#[test]
fn a_reader_sees_trunks_protection_only_in_part() {
    let facts = read("merge_cli", "cli", "cli", &CLI[..1]).remove(0);
    let snapshot = on_screen(&facts);

    let protection = facts.protection.as_ref().unwrap();
    assert!(!protection.complete);
    assert_eq!(snapshot.rules.required_approvals, 1);
    assert!(snapshot.rules.code_owner_review);
    assert!(!snapshot.rules.conversation_resolution);
    // Only an admin sees whether trunk wants the head up to date, so Lo can't tell, and may not
    // see every rule.
    assert!(!facts.requires_up_to_date());
    assert!(!snapshot.sees_every_rule);
    // Its only ruleset asks Copilot to review, which never blocks a merge.
    assert_eq!(
        facts.rules.rules,
        [BranchRule::Harmless {
            kind: "copilot_code_review".into()
        }]
    );
    assert_eq!(
        snapshot.methods,
        AllowedMethods {
            merge: true,
            squash: true,
            rebase: true
        }
    );
    assert_eq!(snapshot.default_method, Some(MergeMethod::Merge));

    // rozed-bot can only read cli/cli: it may approve, but its approval doesn't count, and it
    // can't merge.
    let viewer = &snapshot.viewer;
    assert_eq!(viewer.login, BOT);
    assert!(viewer.can_approve && !viewer.did_author);
    assert!(!viewer.approval_counts && !viewer.can_merge && !viewer.can_enable_auto_merge);
}

#[test]
fn cli_13788_is_blocked_by_its_failed_builds_and_a_missing_approval() {
    let snapshot = &cli()[0];

    assert_eq!(
        kinds(snapshot),
        [
            "Check(build (ubuntu-latest), Failed)",
            "Check(build (windows-latest), Failed)",
            "Check(build (macos-latest), Failed)",
            "Approvals",
        ]
    );
    assert!(snapshot.required_checks.iter().all(|check| {
        (check.url.as_deref())
            .is_some_and(|url| url.starts_with("https://github.com/cli/cli/actions/runs/"))
    }));
    assert!(matches!(status(snapshot), MergeStatus::Blocked { .. }));
    assert!(may_not_see_every_rule(snapshot));
    // Approving can't fix failed builds.
    let ApproveAndShip::Plan(plan) = approve_and_ship(snapshot) else {
        panic!("{:?}", approve_and_ship(snapshot));
    };
    assert_eq!(plan.approval, Approval::Approve);
    assert!(matches!(plan.likely, Ship::Stop(Stop::Blocked { .. })));
}

#[test]
fn cli_12942_conflicts_with_trunk() {
    let snapshot = &cli()[1];

    assert_eq!(kinds(snapshot), ["Conflicts", "Approvals"]);
    assert_eq!(snapshot.unresolved_threads, 5);
    // trunk doesn't require resolving them.
    assert!(!snapshot.rules.conversation_resolution);
    assert!(matches!(
        merge_action(snapshot),
        MergeAction::NotPossible(NotPossible::Blocked)
    ));
}

#[test]
fn cli_14781_is_a_draft() {
    let snapshot = &cli()[2];

    assert!(snapshot.draft);
    assert_eq!(kinds(snapshot), ["Draft", "Approvals"]);
    assert_eq!(approve_and_ship(snapshot), ApproveAndShip::Draft);
    // Its builds passed, and two queued check suites that will never run (apps that made no
    // check runs) aren't waited on.
    assert!(!snapshot.checks_pending);
    assert!((snapshot.required_checks.iter()).all(|check| check.state == CheckState::Passed));
}

#[test]
fn cli_13899_has_changes_requested_by_a_maintainer() {
    let snapshot = &cli()[3];

    assert_eq!(
        blockers(snapshot)[0],
        Blocker::ChangesRequested {
            by: vec!["BagToad".into()]
        }
    );
    // BagToad's request counts, but not toward the approval still missing.
    assert_eq!(
        blockers(snapshot)[1],
        Blocker::Approvals(MissingApprovals {
            more: 1,
            code_owners: Vec::new(),
        })
    );
    // The request for BagToad's review stays after it, and the code owners' team, which
    // rozed-bot can't see, is left out.
    assert_eq!(snapshot.review_requests.len(), 1);
    assert_eq!(
        snapshot.review_requests[0].reviewer,
        Reviewer::User("BagToad".into())
    );
}

#[test]
fn cli_14964_waits_only_on_an_approval() {
    let snapshot = &cli()[4];

    assert_eq!(kinds(snapshot), ["Approvals"]);
    assert!(blockers(snapshot).iter().all(Blocker::waiting));
    assert!(matches!(
        status(snapshot),
        MergeStatus::Waiting {
            auto_merge: false,
            ..
        }
    ));
    // rozed-bot's approval wouldn't count, and it can't turn on auto-merge.
    let ApproveAndShip::Plan(plan) = approve_and_ship(snapshot) else {
        panic!("{:?}", approve_and_ship(snapshot));
    };
    assert_eq!(
        plan.likely,
        Ship::Stop(Stop::NoAutoMerge(blockers(snapshot)))
    );
}

#[test]
fn cli_13340s_required_builds_never_reported() {
    let snapshot = &cli()[5];

    // A first-time contributor's workflows didn't run, and nothing else will report them.
    assert!(!snapshot.checks_pending);
    assert_eq!(
        kinds(snapshot),
        [
            "NotReported(build (macos-latest))",
            "NotReported(build (ubuntu-latest))",
            "NotReported(build (windows-latest))",
            "Approvals",
        ]
    );
}

#[test]
fn vscode_346714_waits_on_its_checks_and_is_blocked_by_its_conversations() {
    let facts = read("merge_vscode_346714", "microsoft", "vscode", &[346714]).remove(0);
    let snapshot = on_screen(&facts);

    // main's classic protection requires resolving every conversation and five checks; a
    // ruleset requires the latest push approved by someone else.
    assert!(snapshot.rules.conversation_resolution);
    assert!(snapshot.rules.last_push_approval);
    assert!(snapshot.rules.push_needs_new_approval());
    assert!(snapshot.checks_pending);
    assert_eq!(
        check_states(&snapshot),
        [
            ("VS Code PR Check", CheckState::Running),
            ("Dependencies Check", CheckState::Passed),
            ("license/cla", CheckState::Passed),
            ("Screenshots & Tests", CheckState::Missing),
            ("PR Gate", CheckState::Missing),
        ]
    );
    // The two that haven't reported are waits while a check suite is still running.
    assert_eq!(
        kinds(&snapshot),
        [
            "Conversations",
            "Approvals",
            "Check(VS Code PR Check, Running)",
            "Check(Screenshots & Tests, Missing)",
            "Check(PR Gate, Missing)",
        ]
    );
    assert_eq!(
        blockers(&snapshot)[0],
        Blocker::Conversations { unresolved: 2 }
    );
    assert!(!snapshot.sees_every_rule);
}

#[test]
fn pnpm_17816_waits_on_its_code_owner() {
    let facts = read("merge_pnpm_17816", "pnpm", "pnpm", &[17816]).remove(0);
    let snapshot = on_screen(&facts);

    // Two review bots with write access approved, which meets the count, but not the code
    // owner's review.
    assert_eq!(snapshot.reviews.len(), 2);
    assert_eq!(
        blockers(&snapshot),
        [Blocker::Approvals(MissingApprovals {
            more: 0,
            code_owners: vec![Reviewer::User("zkochan".into())],
        })]
    );
    // main requires linear history, and a merge queue through its classic protection.
    assert!(!snapshot.methods.merge);
    assert!(snapshot.queue.required);
}

#[test]
fn electron_55594_has_auto_merge_on_while_a_required_check_runs() {
    let facts = read("merge_electron_55594", "electron", "electron", &[55594]).remove(0);
    let snapshot = on_screen(&facts);

    let auto_merge = snapshot.auto_merge.as_ref().unwrap();
    assert_eq!(auto_merge.method, MergeMethod::Squash);
    assert_eq!(auto_merge.enabled_by.as_deref(), Some("MarshallOfSound"));
    // A ruleset's check, pinned to its app, is still running.
    assert_eq!(kinds(&snapshot), ["Check(faraday/cage, Running)"]);
    assert!(matches!(
        status(&snapshot),
        MergeStatus::Waiting {
            auto_merge: true,
            ..
        }
    ));
    // Its rulesets allow squash merges only, and require signed commits, reviewers by file
    // pattern and workflows, which Lo doesn't model.
    assert_eq!(snapshot.methods.to_vec(), [MergeMethod::Squash]);
    assert!(snapshot.signed_commits);
    assert!(!snapshot.sees_every_rule);
    assert!(may_not_see_every_rule(&snapshot));
}

#[test]
fn pytorch_115316s_checks_are_read_a_page_at_a_time() {
    let http = ReplayHttpClient::cassette("merge_pytorch_115316");
    let pull = PullRef::new("pytorch", "pytorch", 115316);
    let facts = block_on(client(http.clone()).merge_facts(&pull, BOT, None)).unwrap();

    assert_eq!(
        asked(&http),
        [
            "MergeSnapshot",
            "MergeChecks",
            "/repos/pytorch/pytorch/rules/branches/main?per_page=100",
        ]
    );
    assert!(facts.cut_short.is_empty() && facts.left_out.is_empty());
    assert!(facts.is_complete());
    let snapshot = on_screen(&facts);
    // None of them is required, and main restricts updates to those who may bypass its rules,
    // which Lo doesn't model: GitHub's block is one Lo can't explain.
    assert_eq!(snapshot.required_checks, []);
    assert_eq!(
        blockers(&snapshot),
        [Blocker::Unexplained {
            url: "https://github.com/pytorch/pytorch/pull/115316".into()
        }]
    );
    assert!(facts.rules.rules.contains(&BranchRule::Other {
        kind: "update".into()
    }));
    // main requires linear history.
    assert!(!snapshot.methods.merge);
}

#[test]
fn a_push_while_the_checks_are_paged_fails_the_read() {
    let http = edited("merge_pytorch_115316", |file, interaction| {
        if file.contains("MergeChecks") {
            pull_request(interaction)["headRefOid"] = "0".repeat(40).into();
        }
    });
    let pull = PullRef::new("pytorch", "pytorch", 115316);
    let error = block_on(client(http).merge_facts(&pull, BOT, None)).unwrap_err();

    assert!(matches!(error, GithubError::Inconsistent(_)), "{error:?}");
}

#[test]
fn zed_66631s_required_checks_never_reported() {
    let facts = read("merge_zed", "zed-industries", "zed", &[66631]).remove(0);
    let snapshot = on_screen(&facts);

    assert_eq!(
        kinds(&snapshot),
        ["NotReported(danger)", "NotReported(tests_pass)"]
    );
    // Rulesets alone, which a reader sees all of: Lo explains GitHub's block in full.
    assert!(facts.protection.is_none());
    assert!(snapshot.sees_every_rule);
    assert!(!may_not_see_every_rule(&snapshot));
    // A first-time contributor's; Copilot's extra approval doesn't apply.
    assert!(snapshot.rules.code_owner_review);
    assert_eq!(snapshot.review_decision, None);
}

#[test]
fn zed_66650_is_ready_for_the_queue_and_its_rules_are_read_again_for_free() {
    let http = ReplayHttpClient::cassette("merge_zed");
    let client = client(http.clone());
    let blocked = PullRef::new("zed-industries", "zed", 66631);
    block_on(client.merge_facts(&blocked, BOT, None)).unwrap();
    let ready = PullRef::new("zed-industries", "zed", 66650);
    let known = block_on(client.merge_facts(&ready, BOT, None)).unwrap();
    let again = block_on(client.merge_facts(&ready, BOT, Some(&known))).unwrap();

    // The second read sent the rules' ETag, and GitHub answered 304.
    let seen = http.seen();
    let rules = (seen.iter()).filter(|seen| seen.url.contains("/rules/branches/main"));
    let etags: Vec<Option<&str>> = rules.map(|seen| seen.header("if-none-match")).collect();
    assert_eq!(etags[..2], [None, None]);
    assert_eq!(etags[2], known.rules.etag.as_deref());
    assert_eq!(again, known);

    let snapshot = on_screen(&known);
    assert_eq!(blockers(&snapshot), []);
    // The queue's method comes from main's merge_queue rule: GitHub shows its settings only
    // to writers. rozed-bot can't merge.
    assert!(snapshot.queue.required);
    assert_eq!(snapshot.queue.method, Some(MergeMethod::Squash));
    assert_eq!(snapshot.methods.to_vec(), [MergeMethod::Squash]);
    assert_eq!(
        merge_action(&snapshot),
        MergeAction::NotPossible(NotPossible::NoPermission)
    );
}

#[test]
fn flutters_merge_queue_lists_its_entries_and_the_pull_request_in_it() {
    let http = ReplayHttpClient::cassette("merge_queue_flutter");
    let client = client(http);
    let queue =
        block_on(client.merge_queue(&RepoRef::new("flutter", "flutter"), "master")).unwrap();

    let entries = queue.entries();
    assert_eq!(entries.len(), 1);
    assert!(!queue.cut_short());
    let entry = entries[0];
    assert_eq!(entry.position, 1);
    let number = entry.pull_request.as_ref().unwrap().number;
    // Only those who can write see its settings.
    assert!(queue.queue.as_ref().unwrap().configuration.is_none());

    let pull = PullRef::new("flutter", "flutter", number);
    let facts = block_on(client.merge_facts(&pull, BOT, None)).unwrap();
    let snapshot = on_screen(&facts);
    assert!(snapshot.queue.in_queue);
    assert_eq!(snapshot.queue.position, Some(1));
    assert_eq!(
        merge_action(&snapshot),
        MergeAction::NotPossible(NotPossible::Queued { position: Some(1) })
    );
    // GitHub requires every check run under a required name: three are named Check Code Freeze.
    let freezes = (snapshot.required_checks.iter())
        .filter(|check| check.name == "Check Code Freeze")
        .count();
    assert_eq!(freezes, 3);
}

#[test]
fn e2e_16_is_rozed_bots_own_and_merges_as_it_likes() {
    let facts = read("merge_e2e_16", "rozed-dev", "e2e", &[16]).remove(0);
    let snapshot = facts.snapshot(&Local {
        head_on_screen: facts.pull_request.head.clone(),
        preferred_method: Some(MergeMethod::Rebase),
        head_contains_base: None,
    });

    let viewer = &snapshot.viewer;
    assert!(viewer.did_author && !viewer.can_approve);
    assert!(viewer.approval_counts && viewer.can_merge);
    // No protection and no rulesets: nothing Lo can't see.
    assert!(snapshot.sees_every_rule);
    assert_eq!(blockers(&snapshot), []);
    assert_eq!(
        merge_action(&snapshot),
        MergeAction::Merge {
            methods: vec![MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase],
            default: MergeMethod::Rebase,
        }
    );
    let ApproveAndShip::Plan(plan) = approve_and_ship(&snapshot) else {
        panic!("{:?}", approve_and_ship(&snapshot));
    };
    assert_eq!(plan.approval, Approval::OwnPullRequest);
    assert_eq!(plan.likely, Ship::Merge(MergeMethod::Rebase));
}

// What no recording shows yet, from recorded answers edited.

/// zed#66650, ready for the queue, with rozed-bot's pending review on `commit`.
fn zed_with_pending_review(commit: Option<String>) -> MergeSnapshot {
    let http = edited("merge_zed", move |file, interaction| {
        if file.starts_with("003-") {
            let pull = pull_request(interaction);
            pull["viewerPendingReview"]["nodes"] = serde_json::json!([{
                "id": "PRR_kwDOAbc",
                "commit": commit.as_ref().map(|oid| serde_json::json!({"oid": oid})),
                "comments": {"totalCount": 2},
            }]);
        }
    });
    let client = client(http);
    let blocked = PullRef::new("zed-industries", "zed", 66631);
    block_on(client.merge_facts(&blocked, BOT, None)).unwrap();
    let ready = PullRef::new("zed-industries", "zed", 66650);
    on_screen(&block_on(client.merge_facts(&ready, BOT, None)).unwrap())
}

#[test]
fn your_pending_review_on_the_head_is_submitted_as_the_approval() {
    let head = zed_with_pending_review(None).head;
    let snapshot = zed_with_pending_review(Some(head.clone()));

    let pending = snapshot.pending_review.as_ref().unwrap();
    assert_eq!(pending.commit.as_deref(), Some(head.as_str()));
    assert_eq!(pending.comments, 2);
    let ApproveAndShip::Plan(plan) = approve_and_ship(&snapshot) else {
        panic!("{:?}", approve_and_ship(&snapshot));
    };
    assert_eq!(plan.approval, Approval::SubmitPending { comments: 2 });
}

#[test]
fn your_pending_review_on_another_revision_stops_approve_and_ship() {
    let older = "0d1d4ead0d1d4ead0d1d4ead0d1d4ead0d1d4ead".to_string();
    let snapshot = zed_with_pending_review(Some(older.clone()));

    assert_eq!(
        approve_and_ship(&snapshot),
        ApproveAndShip::PendingReviewElsewhere(Refusal::OtherRevision {
            review_on: Some(older),
        })
    );
}

#[test]
fn a_code_owner_team_you_are_in_counts_your_approval_for_it() {
    // cli#14964, with the code owners' team visible, rozed-bot in it, and write access.
    let http = edited("merge_cli", |file, interaction| {
        if !file.starts_with("009-") {
            return;
        }
        let body = interaction.response.body.as_mut().unwrap();
        body["data"]["repository"]["viewerPermission"] = "WRITE".into();
        let requests = &mut body["data"]["repository"]["pullRequest"]["reviewRequests"];
        requests["nodes"][0]["requestedReviewer"] = serde_json::json!({
            "__typename": "Team",
            "combinedSlug": "cli/code-reviewers",
            // The search also matches others whose login or name holds rozed-bot's.
            "members": {"nodes": [{"login": "rozed-bot-2"}, {"login": "Rozed-Bot"}]},
        });
    });
    let client = client(http);
    let mut facts = None;
    for number in &CLI[..5] {
        let pull = PullRef::new("cli", "cli", *number);
        facts = Some(block_on(client.merge_facts(&pull, BOT, None)).unwrap());
    }
    let snapshot = on_screen(&facts.unwrap());

    assert_eq!(snapshot.viewer.teams, ["cli/code-reviewers"]);
    assert!(snapshot.viewer.approval_counts);
    assert_eq!(
        blockers(&snapshot),
        [Blocker::Approvals(MissingApprovals {
            more: 1,
            code_owners: vec![Reviewer::Team("cli/code-reviewers".into())],
        })]
    );
    // Its approval meets the count and the team's review, so it's expected to merge, but
    // rozed-bot isn't allowed through trunk's push restrictions.
    let ApproveAndShip::Plan(plan) = approve_and_ship(&snapshot) else {
        panic!("{:?}", approve_and_ship(&snapshot));
    };
    assert_eq!(plan.approval, Approval::Approve);
    assert_eq!(plan.likely, Ship::Stop(Stop::NoPermission));
}

/// e2e#16 with classic protection only an admin sees in full: strict status checks requiring
/// `ci`, which passed.
fn e2e_protected() -> MergeFacts {
    let http = edited("merge_e2e_16", |file, interaction| {
        if !file.contains("MergeSnapshot") {
            return;
        }
        let pull = pull_request(interaction);
        pull["mergeStateStatus"] = "BLOCKED".into();
        pull["baseRef"]["refUpdateRule"] = serde_json::json!({
            "requiredApprovingReviewCount": 1,
            "requiredStatusCheckContexts": ["ci"],
            "requiresCodeOwnerReviews": false,
            "requiresConversationResolution": true,
            "requiresLinearHistory": false,
            "requiresSignatures": false,
            "viewerCanPush": true,
        });
        pull["baseRef"]["branchProtectionRule"] = serde_json::json!({
            "requiresApprovingReviews": true,
            "requiredApprovingReviewCount": 1,
            "requiresCodeOwnerReviews": false,
            "dismissesStaleReviews": true,
            "requireLastPushApproval": false,
            "requiresConversationResolution": true,
            "requiresStatusChecks": true,
            "requiresStrictStatusChecks": true,
            "requiredStatusChecks": [{"context": "ci", "app": {"databaseId": 15368}}],
            "requiresLinearHistory": false,
            "requiresCommitSignatures": false,
            "requiresDeployments": false,
            "lockBranch": false,
        });
        pull["statusCheckRollup"] = serde_json::json!({"contexts": {
            "totalCount": 1,
            "checkRunCountsByState": [{"state": "SUCCESS", "count": 1}],
            "statusContextCountsByState": [],
            "pageInfo": {"hasNextPage": false, "endCursor": null},
            "nodes": [{
                "__typename": "CheckRun",
                "name": "ci",
                "status": "COMPLETED",
                "conclusion": "SUCCESS",
                "detailsUrl": "https://github.com/rozed-dev/e2e/actions/runs/1/job/2",
                "url": "https://github.com/rozed-dev/e2e/runs/2",
                "isRequired": true,
            }],
        }});
    });
    let pull = PullRef::new("rozed-dev", "e2e", 16);
    block_on(client(http).merge_facts(&pull, BOT, None)).unwrap()
}

#[test]
fn a_head_behind_the_base_is_out_of_date_under_strict_status_checks() {
    let facts = e2e_protected();
    let local = |head_contains_base| Local {
        head_on_screen: facts.pull_request.head.clone(),
        preferred_method: None,
        head_contains_base,
    };

    assert!(facts.protection.as_ref().unwrap().complete);
    assert!(facts.requires_up_to_date());
    assert_eq!(
        facts.base_tip(),
        Some("a45754b66cccf1879cd0a252c781639330e6d1fa")
    );
    let behind = facts.snapshot(&local(Some(false)));
    assert!(behind.out_of_date);
    assert_eq!(blockers(&behind), [Blocker::OutOfDate]);
    // The admin's view is complete: Lo sees every rule.
    assert!(behind.sees_every_rule);
    assert!(behind.rules.dismiss_stale_reviews && behind.rules.conversation_resolution);
    assert_eq!(check_states(&behind), [("ci", CheckState::Passed)]);
    // Up to date, or before the local copy can tell, GitHub's own block stands unexplained.
    for head_contains_base in [Some(true), None] {
        let snapshot = facts.snapshot(&local(head_contains_base));
        assert!(!snapshot.out_of_date);
        assert!(matches!(
            blockers(&snapshot)[..],
            [Blocker::Unexplained { .. }]
        ));
    }
}

#[test]
fn rules_that_cant_be_read_leave_lo_unsure_it_sees_every_rule() {
    let http = edited("merge_zed", |file, interaction| {
        if file.starts_with("002-") {
            interaction.response.status = 403;
            interaction.response.headers.remove("etag");
            interaction.response.body = Some(serde_json::json!({
                "message": "Resource not accessible by integration",
            }));
        }
    });
    let pull = PullRef::new("zed-industries", "zed", 66631);
    let facts = block_on(client(http).merge_facts(&pull, BOT, None)).unwrap();

    assert!(facts.rules.unavailable.is_some());
    assert_eq!(facts.rules.rules, []);
    // It's kept: only a part GitHub left out with an error makes it read again early.
    assert!(facts.is_complete());
    let snapshot = on_screen(&facts);
    assert!(!snapshot.sees_every_rule);
    // Without the rules, the required checks' names are unknown, and the queue is GitHub's word.
    assert_eq!(snapshot.required_checks, []);
    assert!(snapshot.queue.required);
    assert!(matches!(
        blockers(&snapshot)[..],
        [Blocker::Unexplained { .. }]
    ));
}

#[test]
fn a_list_github_leaves_out_is_read_again() {
    let http = edited("merge_zed", |file, interaction| {
        if file.starts_with("001-") {
            let body = interaction.response.body.as_mut().unwrap();
            body["data"]["repository"]["pullRequest"]["latestOpinionatedReviews"] =
                serde_json::Value::Null;
            body["errors"] = serde_json::json!([{
                "type": "FORBIDDEN",
                "path": ["repository", "pullRequest", "latestOpinionatedReviews"],
                "message": "Resource not accessible",
            }]);
        }
    });
    let pull = PullRef::new("zed-industries", "zed", 66631);
    let facts = block_on(client(http).merge_facts(&pull, BOT, None)).unwrap();

    assert_eq!(facts.left_out, ["reviews"]);
    assert!(!facts.is_complete());
    assert!(!on_screen(&facts).sees_every_rule);
}
