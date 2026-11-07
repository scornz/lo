//! Stacks found in recorded answers (`tests/record_fixtures.rs`), and in synthetic ones for what
//! GitHub can't be made to show on demand.
//!
//! - `rozed-dev/playground`: GitHub stack #3, PR #1 (`stack-a` → `main`) under PR #2 (`stack-b`
//!   → `stack-a`).
//! - `rozed-dev/e2e` (`script/e2e/README.md`): E2's tree, #2 a → main, #3 b → a, #4 c → b, #5 e
//!   → b and #6 d → main holding c, with the GitHub stack #9 (#7, #8) beside it; and E3, #11 y →
//!   `e3/x` on #10 x, squash-merged.
//! - `github/gh-stack` and `cli/cli`, read only by an account that can't administer them:
//!   rulesets and branch protection as a reader sees them, and cli/cli's eight-layer GitHub
//!   stack.

use std::sync::Arc;

use futures::executor::block_on;
use github::queries::merged_pull_requests::{BRANCHES_A_REQUEST, MAX_BRANCH_LOOKUPS};
use github::queries::open_pull_requests::RECENTLY_UPDATED_MERGED;
use github::queries::{
    OpenPullRequestList, PAGE_SIZE, PullRequestReviewDecision, StatusState, TrunkRuleList,
    ViewedFiles,
};
use github::replay::ReplayHttpClient;
use github::{GithubClient, RepoRef, Token};
use stacks::{NativeStackEntry, NeedsRebase, Parent, Rule, Stacks};

fn client(cassette: &str) -> (GithubClient, Arc<ReplayHttpClient>) {
    client_for(ReplayHttpClient::cassette(cassette))
}

fn client_for(http: Arc<ReplayHttpClient>) -> (GithubClient, Arc<ReplayHttpClient>) {
    (
        GithubClient::new(http.clone(), Token::new("gho_FAKE")),
        http,
    )
}

/// What a recorded cassette holds for `repo`'s stacks: its open pull requests, with the merged
/// ones looked up, its trunk rules, and its client, for the layers.
fn open(cassette: &str, repo: &RepoRef) -> (OpenPullRequestList, TrunkRuleList, GithubClient) {
    let (client, _) = client(cassette);
    let rules = block_on(client.trunk_rules(repo, None, &[])).unwrap();
    let list = block_on(client.open_pull_requests(repo, &rules)).unwrap();
    (list, rules, client)
}

/// The node IDs of `numbers`, in that order.
fn ids(list: &OpenPullRequestList, numbers: &[i64]) -> Vec<String> {
    (numbers.iter())
        .map(|&number| list.pull_request(number).unwrap().id.clone())
        .collect()
}

/// Each layer of `stacks`'s stack holding `number`, with its depth.
fn walk(stacks: &Stacks, number: u64) -> Vec<(u64, usize)> {
    (stacks.stack_of(number).unwrap().layers.iter())
        .map(|&layer| (layer, stacks.layer(layer).unwrap().depth))
        .collect()
}

fn roots(stacks: &Stacks) -> Vec<&str> {
    (stacks.roots().iter())
        .map(|root| root.branch.as_str())
        .collect()
}

/// What `number` sits on, and by which rule.
fn link(stacks: &Stacks, number: u64) -> (Parent, Option<Rule>) {
    let layer = stacks.layer(number).unwrap();
    (layer.parent.clone(), layer.rule)
}

fn on_branch(branch: &str) -> Parent {
    Parent::Branch(branch.into())
}

/// A repository without rulesets.
fn no_rules() -> TrunkRuleList {
    TrunkRuleList::default()
}

fn now() -> std::time::SystemTime {
    std::time::SystemTime::now()
}

#[test]
fn the_playgrounds_github_stack() {
    let playground = RepoRef::new("rozed-dev", "playground");
    let (list, rules, client) = open("stacks_playground", &playground);
    // Each read costs a point.
    assert_eq!(client.rate_limits().last_graphql_cost, Some(1));
    assert_eq!(rules, TrunkRuleList::default());
    let trunks = list.trunks(&rules);
    assert_eq!(trunks.default_branch.as_deref(), Some("main"));
    assert!(trunks.protected.is_empty());
    assert!(list.merged.is_empty());

    // As finding stacks takes them: the base branch's tip now, which is the lower layer's head,
    // and the commits, the head last.
    let prs = list.stack_pull_requests(&[]);
    let [upper, lower] = prs.as_slice() else {
        panic!("two open pull requests, newest first");
    };
    assert_eq!((upper.number, lower.number), (2, 1));
    assert_eq!(upper.base_tip.as_ref(), Some(&lower.head_oid));
    assert_eq!(upper.commits.last(), Some(&upper.head_oid));
    assert!(upper.head_in_base_repo);
    assert_eq!(
        upper.native_stack,
        Some(NativeStackEntry {
            stack: 3,
            position: 2
        })
    );

    let stacks = list.stacks(&rules, &[]);
    assert_eq!(roots(&stacks), ["main"]);
    assert_eq!(walk(&stacks, 2), [(1, 0), (2, 1)]);
    assert_eq!(link(&stacks, 1), (on_branch("main"), None));
    assert_eq!(link(&stacks, 2), (Parent::Pr(1), Some(Rule::NativeStack)));
    let top = stacks.layer(2).unwrap();
    assert_eq!((&top.needs_rebase, &top.retarget_to), (&None, &None));

    let pr = list.pull_request(2).unwrap();
    assert_eq!(pr.author.as_ref().unwrap().login, "rozed-bot");
    assert_eq!(pr.title, "feat: stack layer B");
    let layers =
        block_on(client.stack_layers(&playground, &ids(&list, &[1, 2]), None, now())).unwrap();
    assert_eq!(client.rate_limits().last_graphql_cost, Some(1));
    let numbers: Vec<i64> = layers.layers.iter().map(|layer| layer.number).collect();
    assert_eq!(numbers, [1, 2]);
    let layer = layers.layer(&pr.id).unwrap();
    assert_eq!(layer.review_decision, None);
    assert_eq!(layer.status_check_rollup, None);
    assert!(!layer.is_in_merge_queue);
    assert_eq!(layer.moved_from(), None);
    assert_eq!(
        layer.viewed_files(),
        ViewedFiles {
            viewed: 0,
            files: 1,
            complete: true
        }
    );
    assert!(layers.merged.is_empty());
}

/// The full SHA of E3's x (#10) head: what #11 compares against until it's rebased.
const X_HEAD: &str = "e86aa486704016ee033d555bcca81e33898c5608";

/// E3: y (#11) still targets `e3/x`, whose pull request (#10) was squash-merged into `main`.
fn assert_e3_needs_rebase(stacks: &Stacks) {
    let y = stacks.layer(11).unwrap();
    assert_eq!(
        link(stacks, 11),
        (on_branch("main"), Some(Rule::BaseBranch))
    );
    assert_eq!(
        y.needs_rebase,
        Some(NeedsRebase {
            merged: 10,
            compare_against: X_HEAD.into(),
        })
    );
    // Made a GitHub stack, it would target `main`.
    assert_eq!(y.retarget_to.as_deref(), Some("main"));
    assert_eq!(walk(stacks, 11), [(11, 0)]);
}

#[test]
fn e2s_tree_and_github_stack_and_e3s_rebase() {
    let e2e = RepoRef::new("rozed-dev", "e2e");
    let (list, rules, client) = open("stacks_e2e", &e2e);
    // E3's x is among the merged pull requests updated most recently. All three were squashed.
    let merged: Vec<_> = (list.merged.iter())
        .map(|pr| (pr.number, pr.head_in_base))
        .collect();
    assert_eq!(merged, [(15, false), (12, false), (10, false)]);
    let x = list.merged.iter().find(|pr| pr.number == 10).unwrap();
    assert_eq!((x.head_ref.as_str(), x.head_oid.as_str()), ("e3/x", X_HEAD));
    assert_eq!(x.commits.last().map(String::as_str), Some(X_HEAD));
    // The bases of the later fixtures have no pull request of their own: one request looks them
    // all up, and finds nothing.
    let looked_up: Vec<_> = (list.lookups.iter())
        .map(|lookup| (lookup.branch.as_str(), lookup.found.len()))
        .collect();
    assert_eq!(
        looked_up,
        [
            ("e10/base", 0),
            ("e12/base", 0),
            ("e6/base", 0),
            ("e8/base", 0),
            ("e9/base", 0)
        ]
    );
    assert!(list.lookups.iter().all(|lookup| lookup.tip.is_some()));
    assert!(!list.merged_truncated);

    let stacks = list.stacks(&rules, &[]);
    assert!(stacks.cycles().is_empty());
    assert_eq!(
        roots(&stacks),
        [
            "main", "e10/base", "e12/base", "e6/base", "e8/base", "e9/base"
        ]
    );
    // A stack for each bottom layer on `main`, oldest first: E2's tree, its GitHub stack, E3's
    // y (whose x merged), E1's revisions, and the later fixtures'.
    let bottoms: Vec<u64> = (stacks.roots()[0].stacks.iter())
        .map(|stack| stack.layers[0])
        .collect();
    assert_eq!(bottoms, [2, 7, 11, 16, 17]);

    // E2: a, b, then c and e on b in the order they were opened, and d on c.
    assert_eq!(walk(&stacks, 2), [(2, 0), (3, 1), (4, 2), (6, 3), (5, 2)]);
    assert_eq!(stacks.layer(3).unwrap().children, [4, 5]);
    for number in [3, 4, 5] {
        assert_eq!(stacks.layer(number).unwrap().rule, Some(Rule::BaseBranch));
    }
    // d targets `main` but holds c's head: only shared commits link it, to the nearest layer it
    // holds, c rather than b or a.
    let d = stacks.layer(6).unwrap();
    assert_eq!(link(&stacks, 6), (Parent::Pr(4), Some(Rule::SharedCommits)));
    assert_eq!(d.retarget_to.as_deref(), Some("e2/c"));
    assert_eq!(d.needs_rebase, None);

    // The GitHub stack wins over the base branch, which links s2 to s1 too.
    assert_eq!(walk(&stacks, 8), [(7, 0), (8, 1)]);
    assert_eq!(link(&stacks, 8), (Parent::Pr(7), Some(Rule::NativeStack)));

    assert_e3_needs_rebase(&stacks);

    let asked = ids(&list, &[2, 3, 4, 6, 5, 7, 8, 11, 19]);
    let layers = block_on(client.stack_layers(&e2e, &asked, None, now())).unwrap();
    assert_eq!(layers.ids, asked);
    let numbers: Vec<i64> = layers.layers.iter().map(|layer| layer.number).collect();
    assert_eq!(numbers, [2, 3, 4, 6, 5, 7, 8, 11, 19]);
    // d changes the files of a, b and c, and its own.
    let d = layers.layer(&asked[3]).unwrap();
    assert_eq!(
        d.viewed_files(),
        ViewedFiles {
            viewed: 0,
            files: 4,
            complete: true
        }
    );
    assert_eq!(d.head_ref_oid, list.pull_request(6).unwrap().head_ref_oid);
    // The fixtures script changed #19's base from `main` to `e8/base`, which shows as the branch
    // it moved from; no merged pull request came from `main`. No other layer moved: e3/x is
    // still there.
    let moved: Vec<_> = (layers.layers.iter())
        .filter_map(|layer| Some((layer.number, layer.moved_from()?)))
        .collect();
    assert_eq!(moved, [(19, "main")]);
    assert!(layers.merged.is_empty());
    assert_eq!(layers.lookups_failed, None);
}

#[test]
fn e3s_merged_layer_is_looked_up_by_its_branch() {
    let e2e = RepoRef::new("rozed-dev", "e2e");
    let (client, http) = client("stacks_e2e_looked_up");
    let mut list = block_on(client.open_pull_requests_paged(&e2e, PAGE_SIZE, 0)).unwrap();
    block_on(client.look_up_merged(&e2e, &mut list, None, &no_rules(), now())).unwrap();
    // Without the merged pull requests updated most recently, `e3/x`, which y targets, is looked
    // up with the later fixtures' bases, in one request.
    let seen = http.seen();
    assert_eq!(seen.len(), 2);
    let request: serde_json::Value = serde_json::from_slice(&seen[1].body).unwrap();
    assert_eq!(request["operationName"], "MergedPullRequests");
    assert_eq!(request["variables"]["b2"], "e3/x");
    let merged: Vec<u64> = list.merged.iter().map(|pr| pr.number).collect();
    assert_eq!(merged, [10]);
    assert_e3_needs_rebase(&list.stacks(&no_rules(), &[]));
}

#[test]
fn policy_rulesets_for_every_branch_name_no_trunks() {
    let gh_stack = RepoRef::new("github", "gh-stack");
    let (list, rules, client) = open("stacks_gh_stack", &gh_stack);
    // A reader sees the rulesets, the organization's too. Copilot reviews on `~ALL` and license
    // scanning on the default branch only enforce a policy; the default branch's pull requests
    // and the enterprise release branches' gate merging.
    assert_eq!(rules.unavailable, None);
    let includes: Vec<_> = (rules.rulesets.iter())
        .map(|ruleset| (ruleset.include[0].as_str(), ruleset.gates_merging))
        .collect();
    assert_eq!(
        includes,
        [
            ("~ALL", false),
            ("~DEFAULT_BRANCH", false),
            ("refs/heads/enterprise-2.*-release", true),
            ("refs/heads/enterprise-*.*.*-release", true),
            ("refs/heads/enterprise-*.*-release", true),
            ("~DEFAULT_BRANCH", true)
        ]
    );
    let trunks = list.trunks(&rules);
    assert!(trunks.contains("enterprise-3.18-release"));
    assert!(!trunks.contains("enterprise-3.18.1-backport-2"));
    let prs = list.stack_pull_requests(&[]);
    assert_eq!(trunks.among(&prs), ["main"]);

    // Every open pull request sits on `main` by itself, none of them a trunk.
    let stacks = list.stacks(&rules, &[]);
    for pr in &list.pull_requests {
        assert!(!trunks.contains(&pr.head_ref_name), "{}", pr.head_ref_name);
        assert_eq!(link(&stacks, pr.number as u64), (on_branch("main"), None));
    }
    // Merged pull requests keep their place in a GitHub stack.
    let merged = list.merged.iter().find(|pr| pr.number == 579).unwrap();
    let entry = merged.native_stack.unwrap();
    assert_eq!((entry.stack, entry.position), (580, 2));

    let every: Vec<String> = list.pull_requests.iter().map(|pr| pr.id.clone()).collect();
    let layers = block_on(client.stack_layers(&gh_stack, &every, None, now())).unwrap();
    assert_eq!(layers.layers.len(), list.pull_requests.len());
    let layer = layers.layer(&list.pull_requests[0].id).unwrap();
    assert_eq!(
        layer.review_decision,
        Some(PullRequestReviewDecision::REVIEW_REQUIRED)
    );
    assert_eq!(
        layer
            .status_check_rollup
            .as_ref()
            .map(|rollup| &rollup.state),
        Some(&StatusState::SUCCESS)
    );
}

#[test]
fn a_reader_sees_branch_protection_through_pull_requests() {
    let cli = RepoRef::new("cli", "cli");
    let (list, rules, client) = open("stacks_cli", &cli);
    // rozed-bot can't administer cli/cli, so the repository's list of branch protection rules
    // would be empty; `trunk`'s rule shows on the pull requests that target it. The one ruleset,
    // Copilot reviews on `~ALL`, gates nothing.
    let gates: Vec<_> = (rules.rulesets.iter())
        .map(|ruleset| (ruleset.include.as_slice(), ruleset.gates_merging))
        .collect();
    assert_eq!(gates, [(&["~ALL".to_string()][..], false)]);
    let trunks = list.trunks(&rules);
    assert_eq!(trunks.default_branch.as_deref(), Some("trunk"));
    assert_eq!(trunks.protected, ["trunk"]);

    // William Martin's drafts: a GitHub stack of eight layers on `trunk`.
    let stacks = list.stacks(&rules, &[]);
    let bottom = (list.pull_requests.iter())
        .find(|pr| pr.head_ref_name == "williammartin-multiaccount-draft-01-account")
        .unwrap();
    let walked = walk(&stacks, bottom.number as u64);
    let depths: Vec<usize> = walked.iter().map(|&(_, depth)| depth).collect();
    assert_eq!(depths, (0..8).collect::<Vec<_>>());
    assert_eq!(link(&stacks, walked[0].0), (on_branch("trunk"), None));
    for pair in walked.windows(2) {
        assert_eq!(
            link(&stacks, pair[1].0),
            (Parent::Pr(pair[0].0), Some(Rule::NativeStack))
        );
    }
    let ids: Vec<String> = (walked.iter())
        .map(|&(number, _)| list.pull_request(number as i64).unwrap().id.clone())
        .collect();
    let layers = block_on(client.stack_layers(&cli, &ids, None, now())).unwrap();
    assert_eq!(layers.layers.len(), 8);
}

/// Synthetic answers about `o/r`, whose default branch is `main`.
mod synthetic {
    use std::sync::Arc;

    use github::RepoRef;
    use github::cassette::{Interaction, RecordedRequest, RecordedResponse};
    use github::queries::merged_pull_requests::{BRANCHES_A_REQUEST, MERGED_PER_BRANCH};
    use github::replay::ReplayHttpClient;
    use serde_json::{Value, json};

    pub fn repo() -> RepoRef {
        RepoRef::new("o", "r")
    }

    /// A GraphQL answer to `operation` asked with `variables`.
    pub fn answer(operation: &str, variables: Value, data: Value) -> Interaction {
        Interaction {
            request: RecordedRequest {
                method: "POST".into(),
                url: "https://api.github.com/graphql".into(),
                operation: Some(operation.into()),
                variables: Some(variables),
                ..Default::default()
            },
            response: RecordedResponse {
                status: 200,
                headers: Default::default(),
                body: Some(json!({ "data": data })),
                body_text: None,
            },
        }
    }

    pub fn replay(answers: Vec<Interaction>) -> Arc<ReplayHttpClient> {
        Arc::new(ReplayHttpClient::from_interactions("synthetic", answers).unwrap())
    }

    /// A commit's OID, made from a name.
    pub fn oid(name: &str) -> String {
        format!("{name:0>40}")
    }

    fn commits(names: &[&str]) -> Value {
        let nodes: Vec<Value> = (names.iter())
            .map(|name| json!({"commit": {"oid": oid(name)}}))
            .collect();
        json!({ "nodes": nodes })
    }

    /// An open PR `head` → `base` holding the commits `names`, oldest first. Its base branch's
    /// tip is `base`'s, and neither branch is protected.
    pub fn open_pr(number: i64, base: &str, head: &str, names: &[&str]) -> Value {
        json!({
            "id": format!("PR_{number}"), "number": number, "title": "t", "isDraft": false,
            "createdAt": format!("2026-11-06T08:{number:02}:00Z"),
            "updatedAt": "2026-11-06T09:00:00Z", "author": null,
            "baseRefName": base, "baseRefOid": oid(base),
            "baseRef": {"target": {"__typename": "Commit", "oid": oid(base)}, "refUpdateRule": null},
            "headRefName": head, "headRefOid": oid(names.last().unwrap()),
            "headRef": {"refUpdateRule": null}, "isCrossRepository": false,
            "headRepositoryOwner": null, "commits": commits(names), "stackEntry": null,
        })
    }

    /// Marks `pr`'s base or head branch (`end`, `baseRef` or `headRef`) as protected by the
    /// rule `pattern`.
    pub fn protected(mut pr: Value, end: &str, pattern: &str) -> Value {
        pr[end]["refUpdateRule"] = json!({ "pattern": pattern });
        pr
    }

    /// A merged PR `head` → `base` holding the commits `names`, merged by `merge`: `squash`, a
    /// commit with only the base for a parent, or `merge`, a merge commit of the base and head.
    pub fn merged_pr(number: i64, base: &str, head: &str, names: &[&str], merge: &str) -> Value {
        let head_oid = oid(names.last().unwrap());
        let parents = match merge {
            "squash" => vec![json!({"oid": oid(base)})],
            _ => vec![json!({"oid": oid(base)}), json!({"oid": head_oid})],
        };
        json!({
            "number": number, "createdAt": format!("2026-11-06T07:{number:02}:00Z"),
            "baseRefName": base,
            "baseRef": {"target": {"__typename": "Commit", "oid": oid(base)}, "refUpdateRule": null},
            "headRefName": head, "headRefOid": head_oid, "isCrossRepository": false,
            "commits": commits(names),
            "mergeCommit": {"oid": oid(&format!("{merge}{number}")), "parents": {"nodes": parents}},
            "stackEntry": null,
        })
    }

    /// The one page of open PRs `open`, with `merged`, the merged PRs updated most recently,
    /// `recently` of which were asked for.
    pub fn open_page(open: Vec<Value>, merged: Vec<Value>, recently: i64) -> Interaction {
        answer(
            "OpenPullRequests",
            json!({"owner": "o", "name": "r", "first": 100, "after": null, "merged": recently}),
            json!({"rateLimit": null, "repository": {
                "defaultBranchRef": {"name": "main"},
                "pullRequests": {
                    "totalCount": open.len(),
                    "pageInfo": {"hasNextPage": false, "endCursor": null},
                    "nodes": open,
                },
                "mergedPullRequests": {"nodes": merged},
            }}),
        )
    }

    /// The merged PRs found looking up `branches`, at most ten: `found` from each in turn.
    pub fn looked_up(branches: &[&str], found: Vec<Vec<Value>>) -> Interaction {
        let mut variables = json!({"owner": "o", "name": "r", "first": MERGED_PER_BRANCH});
        let mut repository = json!({});
        for index in 0..BRANCHES_A_REQUEST {
            let branch = branches.get(index).unwrap_or(&branches[0]);
            variables[format!("b{index}")] = json!(branch);
            let nodes = found.get(index).cloned().unwrap_or_default();
            repository[format!("b{index}")] = json!({ "nodes": nodes });
        }
        answer(
            "MergedPullRequests",
            variables,
            json!({"rateLimit": null, "repository": repository}),
        )
    }

    /// A layer as `StackLayers` reads it, moved by GitHub from `moved_from` if it was.
    pub fn layer(number: i64, moved_from: Option<&str>) -> Value {
        let events: Vec<Value> = (moved_from.iter())
            .map(|branch| {
                json!({"__typename": "AutomaticBaseChangeSucceededEvent", "oldBase": branch})
            })
            .collect();
        json!({
            "__typename": "PullRequest", "id": format!("PR_{number}"), "number": number,
            "state": "OPEN", "headRefOid": oid("head"), "reviewDecision": null,
            "statusCheckRollup": null, "isInMergeQueue": false, "mergeQueueEntry": null,
            "files": {"totalCount": 0, "nodes": []},
            "movedFrom": {"nodes": events},
        })
    }

    pub fn layers(ids: &[&str], nodes: Vec<Value>) -> Interaction {
        answer(
            "StackLayers",
            json!({ "ids": ids }),
            json!({"rateLimit": null, "nodes": nodes}),
        )
    }

    /// A ruleset as `TrunkRules` reads it.
    pub fn ruleset(enforcement: &str, include: &[&str], exclude: &[&str], rules: &[&str]) -> Value {
        let rules: Vec<Value> = rules.iter().map(|rule| json!({ "type": rule })).collect();
        json!({
            "enforcement": enforcement,
            "conditions": {"refName": {"include": include, "exclude": exclude}},
            "rules": { "nodes": rules },
        })
    }

    pub fn trunk_rules(rulesets: Value) -> Interaction {
        answer(
            "TrunkRules",
            json!({"owner": "o", "name": "r"}),
            json!({"rateLimit": null, "repository": {"rulesets": rulesets}}),
        )
    }
}

use synthetic::*;

/// The open PRs `open` and what the merged PRs `answers` say, read as the poller reads them.
fn read(
    open: Vec<serde_json::Value>,
    answers: Vec<github::cassette::Interaction>,
) -> OpenPullRequestList {
    let mut all = vec![open_page(open, vec![], RECENTLY_UPDATED_MERGED)];
    all.extend(answers);
    let (client, _) = client_for(replay(all));
    block_on(client.open_pull_requests(&repo(), &no_rules())).unwrap()
}

#[test]
fn a_release_pull_request_from_a_protected_branch_holds_no_layers() {
    // Git flow: `develop` is protected, its release PR R targets `main`, and features target
    // `develop`. Someone who can't administer the repository sees `develop`'s rule only on the
    // pull requests from and to it.
    let release = open_pr(1, "main", "develop", &["d1"]);
    let one = open_pr(2, "develop", "feat-1", &["f1"]);
    let two = open_pr(3, "develop", "feat-2", &["f2"]);
    let list = read(
        vec![
            protected(release.clone(), "headRef", "develop"),
            protected(one.clone(), "baseRef", "develop"),
            protected(two.clone(), "baseRef", "develop"),
        ],
        // `develop` is a trunk, so nothing is looked up.
        vec![],
    );
    let stacks = list.stacks(&no_rules(), &[]);
    assert_eq!(roots(&stacks), ["main", "develop"]);
    assert_eq!(link(&stacks, 1), (on_branch("main"), None));
    assert_eq!(link(&stacks, 2), (on_branch("develop"), None));
    assert_eq!(link(&stacks, 3), (on_branch("develop"), None));

    // Without the rule, the release PR swallows every PR into `develop`.
    let list = read(vec![release, one, two], vec![]);
    let stacks = list.stacks(&no_rules(), &[]);
    assert_eq!(link(&stacks, 2), (Parent::Pr(1), Some(Rule::BaseBranch)));
    assert_eq!(walk(&stacks, 1), [(1, 0), (2, 1), (3, 1)]);
}

#[test]
fn merged_layers_are_looked_up_down_to_a_trunk() {
    // #3 targets `b`, squash-merged into `a`, which merged into `main` with a merge commit.
    let b = merged_pr(2, "a", "b", &["b1"], "squash");
    let a = merged_pr(1, "main", "a", &["a1"], "merge");
    let mut c = open_pr(3, "b", "c", &["b1", "c1"]);
    // `b` hasn't moved since it merged.
    c["baseRef"]["target"]["oid"] = b["headRefOid"].clone();
    let list = read(
        vec![c],
        vec![
            looked_up(&["b"], vec![vec![b]]),
            looked_up(&["a"], vec![vec![a]]),
        ],
    );
    let merged: Vec<_> = (list.merged.iter())
        .map(|pr| (pr.number, pr.head_in_base))
        .collect();
    // `b` was squashed, so its head isn't in `a`. `a`'s merge commit has its head as a parent.
    assert_eq!(merged, [(2, false), (1, true)]);
    assert!(!list.merged_truncated);
    let stacks = list.stacks(&no_rules(), &[]);
    let layer = stacks.layer(3).unwrap();
    assert_eq!(layer.parent, on_branch("main"));
    assert_eq!(
        layer.needs_rebase.as_ref().map(|needs| needs.merged),
        Some(2)
    );
}

#[test]
fn lookups_are_carried_over_while_their_branch_stays_put() {
    let open = open_pr(2, "gone", "y", &["y1"]);
    let (client, http) = client_for(replay(vec![
        open_page(vec![open.clone()], vec![], RECENTLY_UPDATED_MERGED),
        looked_up(&["gone"], vec![vec![]]),
    ]));
    let known = block_on(client.open_pull_requests(&repo(), &no_rules())).unwrap();
    assert_eq!(http.seen().len(), 2);
    assert_eq!(known.lookups[0].tip, Some(oid("gone")));

    // Read again with nothing moved: no lookup.
    let o_r = repo();
    let pages = client.open_pull_requests_paged(&o_r, PAGE_SIZE, RECENTLY_UPDATED_MERGED);
    let mut again = block_on(pages).unwrap();
    block_on(client.look_up_merged(&repo(), &mut again, Some(&known), &no_rules(), now())).unwrap();
    assert_eq!(http.seen().len(), 3);
    assert_eq!(again.lookups, known.lookups);

    // `gone` moved: it's looked up again.
    let mut moved = open;
    moved["baseRef"]["target"]["oid"] = serde_json::json!(oid("moved"));
    let (client, http) = client_for(replay(vec![
        open_page(vec![moved], vec![], RECENTLY_UPDATED_MERGED),
        looked_up(&["gone"], vec![vec![]]),
    ]));
    let pages = client.open_pull_requests_paged(&o_r, PAGE_SIZE, RECENTLY_UPDATED_MERGED);
    let mut later = block_on(pages).unwrap();
    block_on(client.look_up_merged(&repo(), &mut later, Some(&known), &no_rules(), now())).unwrap();
    assert_eq!(http.seen().len(), 2);
    assert_eq!(later.lookups[0].tip, Some(oid("moved")));
}

#[test]
fn the_branches_most_pull_requests_target_are_looked_up_first() {
    // More branches than the lookups allow, one PR on each, and three PRs on `busy`.
    let names: Vec<String> = (0..MAX_BRANCH_LOOKUPS + 5)
        .map(|index| format!("gone-{index:02}"))
        .collect();
    let mut open: Vec<serde_json::Value> = (names.iter().enumerate())
        .map(|(index, name)| open_pr(index as i64 + 10, name, &format!("pr-{index}"), &["x"]))
        .collect();
    for number in 1..=3 {
        open.push(open_pr(number, "busy", &format!("busy-{number}"), &["x"]));
    }
    let mut ranked: Vec<&str> = vec!["busy"];
    ranked.extend(names.iter().map(String::as_str));
    let answers = (ranked[..MAX_BRANCH_LOOKUPS].chunks(BRANCHES_A_REQUEST))
        .map(|batch| looked_up(batch, vec![]))
        .collect();
    let list = read(open, answers);
    assert!(list.merged_truncated);
    assert_eq!(list.lookups.len(), MAX_BRANCH_LOOKUPS);
    assert_eq!(list.lookups[0].branch, "busy");
}

#[test]
fn a_failed_lookup_keeps_the_list() {
    let mut failed = looked_up(&["gone"], vec![]);
    failed.response.status = 502;
    failed.response.body = None;
    failed.response.body_text = Some("Bad gateway".into());
    let list = read(vec![open_pr(2, "gone", "y", &["y1"])], vec![failed]);
    assert_eq!(list.pull_requests.len(), 1);
    assert!(list.merged_truncated);
    assert!(list.lookups.is_empty());
}

#[test]
fn the_merged_pull_requests_updated_most_recently_mark_a_moved_layer() {
    // A was squash-merged and its branch deleted, so GitHub moved B onto `main`. Only A's
    // commits, which B still holds, link B to it.
    let a = merged_pr(1, "main", "a", &["a1", "a2"], "squash");
    let b = open_pr(2, "main", "b", &["a1", "a2", "b1"]);
    let stacks = |recently: i64, merged: Vec<serde_json::Value>| {
        let (client, _) = client_for(replay(vec![open_page(vec![b.clone()], merged, recently)]));
        let o_r = repo();
        let pages = client.open_pull_requests_paged(&o_r, PAGE_SIZE, recently);
        let mut list = block_on(pages).unwrap();
        block_on(client.look_up_merged(&repo(), &mut list, None, &no_rules(), now())).unwrap();
        list.stacks(&no_rules(), &[])
    };
    let within = stacks(RECENTLY_UPDATED_MERGED, vec![a]);
    assert_eq!(
        within.layer(2).unwrap().needs_rebase,
        Some(NeedsRebase {
            merged: 1,
            compare_against: oid("a2"),
        })
    );
    // Out of the window, nothing marks it.
    let outside = stacks(0, vec![]);
    assert_eq!(outside.layer(2).unwrap().needs_rebase, None);
}

#[test]
fn a_layer_github_moved_finds_the_merged_layer_it_sat_on() {
    // A was squash-merged and its branch `a` deleted. GitHub moved B onto `main`, and A is
    // outside the window of merged pull requests updated most recently.
    let a = merged_pr(1, "main", "a", &["a1", "a2"], "squash");
    let b = open_pr(2, "main", "b", &["a1", "a2", "b1"]);
    let (client, http) = client_for(replay(vec![
        open_page(vec![b], vec![], RECENTLY_UPDATED_MERGED),
        layers(&["PR_2"], vec![layer(2, Some("a"))]),
        looked_up(&["a"], vec![vec![a]]),
    ]));
    let list = block_on(client.open_pull_requests(&repo(), &no_rules())).unwrap();
    assert_eq!(
        list.stacks(&no_rules(), &[]).layer(2).unwrap().needs_rebase,
        None
    );

    let ids = vec!["PR_2".to_string()];
    let layers = block_on(client.stack_layers(&repo(), &ids, None, now())).unwrap();
    assert_eq!(layers.layers[0].moved_from(), Some("a"));
    assert_eq!(layers.lookups_failed, None);
    let stacks = list.stacks(&no_rules(), &layers.merged);
    let b = stacks.layer(2).unwrap();
    assert_eq!(b.parent, on_branch("main"));
    assert_eq!(b.needs_rebase.as_ref().map(|needs| needs.merged), Some(1));
    assert_eq!(http.seen().len(), 3);

    // Read again, the deleted branch's lookup is carried over.
    let again = block_on(client.stack_layers(&repo(), &ids, Some(&layers), now())).unwrap();
    assert_eq!(again.merged, layers.merged);
    assert_eq!(http.seen().len(), 4);
}

#[test]
fn rulesets_name_trunks_only_when_active_and_gating_merges() {
    let rulesets = serde_json::json!({"nodes": [
        ruleset("ACTIVE", &["refs/heads/release/*"], &[], &["PULL_REQUEST"]),
        // Signed commits and Copilot reviews on work branches.
        ruleset(
            "ACTIVE",
            &["refs/heads/feature/**/*"],
            &[],
            &["REQUIRED_SIGNATURES", "COPILOT_CODE_REVIEW"],
        ),
        // Being tried out.
        ruleset("EVALUATE", &["refs/heads/staging"], &[], &["REQUIRED_STATUS_CHECKS"]),
        // Every branch but work branches.
        ruleset("ACTIVE", &["~ALL"], &["refs/heads/feature/**/*"], &["UPDATE"]),
        // Every branch, with no exception.
        ruleset("ACTIVE", &["~ALL"], &[], &["MERGE_QUEUE"]),
        // GitHub's defaults for a new ruleset, on one branch and on a pattern.
        ruleset(
            "ACTIVE",
            &["refs/heads/feature/kept", "refs/heads/feature/next-*"],
            &[],
            &["DELETION", "NON_FAST_FORWARD"],
        ),
    ]});
    let (client, _) = client_for(replay(vec![
        trunk_rules(rulesets),
        open_page(vec![], vec![], RECENTLY_UPDATED_MERGED),
    ]));
    let rules = block_on(client.trunk_rules(&repo(), None, &[])).unwrap();
    // All but the one being tried out.
    let gates: Vec<bool> = rules
        .rulesets
        .iter()
        .map(|ruleset| ruleset.gates_merging)
        .collect();
    assert_eq!(gates, [true, false, true, true, false]);
    let list = block_on(client.open_pull_requests(&repo(), &no_rules())).unwrap();
    let trunks = list.trunks(&rules);
    assert!(trunks.contains("release/2.0"));
    assert!(trunks.contains("develop"));
    assert!(trunks.contains("staging"), "every branch but work branches");
    assert!(!trunks.contains("feature/a"));
    assert!(!trunks.contains("feature/a/b"));
    assert!(
        trunks.contains("feature/kept"),
        "named exactly, whatever it enforces"
    );
    assert!(
        !trunks.contains("feature/next-1"),
        "a pattern that gates nothing"
    );
}

#[test]
fn rulesets_that_cant_be_read_leave_the_trunks_to_the_rest() {
    let mut refused = trunk_rules(serde_json::Value::Null);
    refused.response.body = Some(serde_json::json!({
        "data": {"rateLimit": null, "repository": {"rulesets": null}},
        "errors": [{
            "type": "FORBIDDEN",
            "path": ["repository", "rulesets"],
            "message": "Resource not accessible by personal access token"
        }],
    }));
    let (client, _) = client_for(replay(vec![refused]));
    let rules = block_on(client.trunk_rules(&repo(), None, &[])).unwrap();
    assert!(rules.rulesets.is_empty());
    let reason = rules.unavailable.as_deref().unwrap();
    assert!(reason.contains("Resource not accessible"), "{reason}");
    // What was read before is kept.
    let known = TrunkRuleList {
        rulesets: vec![github::trunks::RefNames {
            include: vec!["refs/heads/develop".into()],
            exclude: Vec::new(),
            gates_merging: true,
        }],
        unavailable: None,
    };
    let rules = block_on(client.trunk_rules(&repo(), Some(&known), &[])).unwrap();
    assert_eq!(rules.rulesets, known.rulesets);
    assert!(rules.unavailable.is_some());
}

#[test]
fn ruleset_trunks_are_never_looked_up_as_layers() {
    // `develop` is protected by a ruleset alone, which pull requests' own branches don't show.
    let release = open_pr(1, "main", "develop", &["d1"]);
    let feature = open_pr(2, "develop", "feat", &["f1"]);
    let (client, http) = client_for(replay(vec![
        trunk_rules(serde_json::json!({"nodes": [
            ruleset("ACTIVE", &["refs/heads/develop"], &[], &["DELETION"]),
        ]})),
        open_page(vec![release, feature], vec![], RECENTLY_UPDATED_MERGED),
    ]));
    let rules = block_on(client.trunk_rules(&repo(), None, &[])).unwrap();
    let list = block_on(client.open_pull_requests(&repo(), &rules)).unwrap();
    // No lookup of `develop`: the replay would have no answer for it.
    assert_eq!(http.seen().len(), 2);
    assert!(list.lookups.is_empty());
    let stacks = list.stacks(&rules, &[]);
    assert_eq!(link(&stacks, 2), (on_branch("develop"), None));
}

#[test]
fn a_forks_protected_head_isnt_the_repositorys() {
    let mut fork = protected(open_pr(1, "main", "release", &["r1"]), "headRef", "release");
    fork["isCrossRepository"] = true.into();
    let ours = protected(open_pr(2, "main", "x", &["x1"]), "baseRef", "main");
    let list = read(vec![fork, ours], vec![]);
    assert_eq!(list.trunks(&no_rules()).protected, ["main"]);
}

#[test]
fn branches_are_checked_one_by_one_when_the_rulesets_cant_be_read() {
    let mut refused = trunk_rules(serde_json::Value::Null);
    refused.response.body = Some(serde_json::json!({
        "data": {"rateLimit": null, "repository": {"rulesets": null}},
        "errors": [{"type": "FORBIDDEN", "path": ["repository", "rulesets"], "message": "No"}],
    }));
    let rules_of = |branch: &str, types: &[&str]| {
        let rules: Vec<serde_json::Value> = (types.iter())
            .map(|rule| serde_json::json!({"type": rule, "ruleset_id": 1}))
            .collect();
        github::cassette::Interaction {
            request: github::cassette::RecordedRequest {
                method: "GET".into(),
                url: format!("https://api.github.com/repos/o/r/rules/branches/{branch}"),
                ..Default::default()
            },
            response: github::cassette::RecordedResponse {
                status: 200,
                headers: Default::default(),
                body: Some(serde_json::Value::Array(rules)),
                body_text: None,
            },
        }
    };
    // A release PR from `develop`, which features target, and a stack `a` ← `b`.
    let open = vec![
        open_pr(1, "main", "develop", &["d1"]),
        open_pr(2, "develop", "feat", &["f1"]),
        open_pr(3, "main", "a", &["a1"]),
        open_pr(4, "a", "b", &["b1"]),
    ];
    let (client, _) = client_for(replay(vec![
        open_page(open, vec![], RECENTLY_UPDATED_MERGED),
        refused,
        rules_of("a", &["copilot_code_review"]),
        rules_of("develop", &["deletion", "pull_request"]),
    ]));
    let list =
        block_on(client.open_pull_requests_paged(&repo(), PAGE_SIZE, RECENTLY_UPDATED_MERGED))
            .unwrap();
    assert_eq!(list.branches_to_check(), ["a", "develop"]);
    let rules = block_on(client.trunk_rules(&repo(), None, &list.branches_to_check())).unwrap();
    assert!(rules.unavailable.is_some());
    let stacks = list.stacks(&rules, &[]);
    assert_eq!(link(&stacks, 2), (on_branch("develop"), None));
    assert_eq!(link(&stacks, 4), (Parent::Pr(3), Some(Rule::BaseBranch)));
}

#[test]
fn a_lookup_that_found_nothing_is_read_again_after_a_day() {
    let open = open_pr(2, "gone", "y", &["y1"]);
    let (client, http) = client_for(replay(vec![
        open_page(vec![open], vec![], RECENTLY_UPDATED_MERGED),
        looked_up(&["gone"], vec![vec![]]),
    ]));
    let known = block_on(client.open_pull_requests(&repo(), &no_rules())).unwrap();
    assert_eq!(http.seen().len(), 2);
    let o_r = repo();
    let again = |at: std::time::SystemTime| {
        let pages = client.open_pull_requests_paged(&o_r, PAGE_SIZE, RECENTLY_UPDATED_MERGED);
        let mut list = block_on(pages).unwrap();
        block_on(client.look_up_merged(&o_r, &mut list, Some(&known), &no_rules(), at)).unwrap();
    };
    let hour = std::time::Duration::from_secs(3600);
    again(now() + 23 * hour);
    assert_eq!(http.seen().len(), 3, "carried over within a day");
    again(now() + 25 * hour);
    assert_eq!(http.seen().len(), 5, "looked up again after one");
}
