# Revisions research for Lo (milestone 6)

Researched 2026-11-06 for the `review` crate: push boundaries, revision bases, the two markers,
viewed marks, rebase-aware comparison and missing revisions. Design: §6, §11 "Missing revisions".

- **GraphQL schema:** `https://docs.github.com/public/fpt/schema.docs.graphql`, downloaded
  2026-11-06 (1,562,841 bytes).
- **GitHub docs:** read from the `github/docs` source (`main`, 2026-11-06) and the docs article API;
  URLs below are the published pages.
- **git:** 2.54.0 (Apple Git-157). Its man page, and `git/git`'s release notes for version floors.
- **Live checks** (2026-11-06, 02:20–03:15 PST): read-only `gh api` as `scornz` (gh 2.103.3), and
  read-only `git ls-remote` and `git fetch --depth=1 --filter=blob:none` from github.com into
  scratch repos. PRs: `cli/cli` #14745, #14356, #9208, #4005, #12942; `rust-lang/rust` #165376,
  #165460, #30000; `zed-industries/zed` #66482, #65981; `scornz/*` PRs for viewer fields. No GitHub
  writes.
- **Experiment:** `rebase-aware.sh` (Appendix A) builds throwaway repos and a bare blobless clone
  served over `file://` with `uploadpack.allowFilter`. Its full output is in Appendix A.
- **Marks:** **[observed]** means I ran it. **[inferred]** means I reasoned it out and didn't verify.
  Everything else was read in the schema, docs or man page.

## Recommendations

1. **Build each PR's push log from the repository activity API**, not the timeline:
   `GET /repos/{head_owner}/{head_repo}/activity?ref=refs/heads/{head_ref}&direction=asc&per_page=100`.
   - It lists every push to the branch, normal and forced, with exact `before`, `after`,
     `timestamp`, `activity_type` and `actor`. It still works after the branch is deleted.
   - On all 6 PRs I compared, it held every head the other sources found, plus 8 normal pushes they
     missed on `cli/cli#9208`. Its before→after chains were unbroken **[observed]**.
   - Data starts around March 2023: the oldest `cli/cli` entry is 2023-03-07, and a 2021 fork
     branch returned `[]` **[observed]**.
2. **Fill gaps from GraphQL and Lo's own observations.**
   - Force-push events (`HeadRefForcePushedEvent`: `beforeCommit`, `afterCommit`, `createdAt`) are
     exact.
   - Review commits, check-suite pushes (`CheckSuite.push { previousSha nextSha }`) and every head
     SHA Lo's poller sees each pin a head that existed.
   - The timeline alone misses runs of consecutive normal pushes: 38 of 46 revisions found on
     `cli/cli#9208` **[observed]**. Two normal pushes in a row, with no CI, review, activity entry
     or Lo observation between them, can't be told apart.
3. **Key revisions by head SHA in storage** (markers, viewed records, pins) and number them r1, r2…
   only at display time. A late source can insert a revision and shift the numbers.
4. **Where push boundaries are unknown inside a fast-forward run, offer the commits.** Any commit in
   such a run is a valid comparison point. Show "r3 (2 or more pushes)" with its commits pickable.
5. **A revision's base is `git merge-base <head_i> <base_tip_i>`, computed locally.**
   - `base_tip_i` is the base branch tip when that revision was pushed, or any later tip of the same
     base branch as long as the branch only moved forward and doesn't contain the PR's commits.
   - Use today's base tip for an ordinary base branch.
   - For a PR merged with a merge commit, use the merge commit's first parent.
   - For stacked or retargeted PRs, use the base branch as it was. Sources:
     `BaseRefForcePushedEvent.afterCommit`, the base branch's activity log, and Lo's pins of the
     lower layer.
   - On `cli/cli#14745`, an early revision compared against today's `trunk` shows 65 commits. Against
     the branch it was stacked on at the time, it shows 3 **[observed]**.
6. **Rebase-aware comparison:**
   1. `TREE=$(git merge-tree --write-tree --no-messages --merge-base=<old_base> <new_base> <old_head>)`
   2. `git diff -M <TREE> <new_head>`.
   - Exit 0 is clean and 1 is conflicted (fall back to the plain diff); anything else is an error.
   - Skip the replay when `old_base == new_base`.
   - **Raise Lo's git floor from 2.38 to 2.40**, which added `--merge-base` (`decisions.md`
     says 2.38).
7. **It works in the bare blobless clone.**
   - merge-tree fetches only the blobs it needs, in batches: 0 fetches when each path changed on one
     side, 1 batched fetch for 3 content merges, 2 with a rename **[observed]**.
   - Every git call needs the same `GIT_ASKPASS` environment (plan Task 4.1).
   - With git ≥ 2.45, `git --no-lazy-fetch` turns a missing blob into exit 128 rather than a
     network fetch, which helps offline.
8. **"Your last review":**
   `reviews(last: 1, author: $login, states: [APPROVED, CHANGES_REQUESTED, COMMENTED, DISMISSED]) { nodes { commit { oid } submittedAt } }`,
   mapped to the revision that contains that commit.
   - **"Last seen here"** stays local.
   - `PullRequestRevisionMarker { lastSeenCommit }` exists in the timeline but was empty on every PR
     I checked, and no mutation sets it **[observed]**. Don't use it.
9. **Viewed marks** are per viewer, per PR, per path.
   - `markFileAsViewed(input: { pullRequestId, path })` takes no commit, so it marks against whatever
     GitHub's head is at that moment **[inferred]**. Check the head before sending, and record the
     revision locally.
   - GitHub's docs say "If the file changes after you view the file, it will be unmarked as
     viewed", which is `DISMISSED`. Whether a rebase that doesn't change the file's diff dismisses it
     is unverified; a test plan is below.
   - Reading is 1 point per 100 files **[observed]**. Each mark is a separate mutation:
     5 secondary-limit points, ≥1 s apart.
10. **Missing revisions:** GitHub serves force-pushed-away commits by SHA for years, but not forever.
    - Observed: a 2021 commit and a fork's 2024 commit fetched from the base repo; a 2015 commit
      gone.
    - Gone shows up three ways: the force-push event's commit is `null`, REST `/commits/{sha}`
      returns `422`, and `git fetch` fails with `upload-pack: not our ref`.
    - `refs/pull/<n>/head` holds only the latest head, so pin every head Lo sees
      (`decisions.md` already does).

### Corrections for the design, plan and decisions

- **`decisions.md` "git 2.38 or newer"** should be **2.40**: `--merge-base` arrived in 2.40
  ("merge-tree learns a new `--merge-base` option", RelNotes 2.40.0). Optional extras:
  `--no-lazy-fetch` is 2.45 and `merge-tree --quiet` is 2.50. macOS ships 2.54.
- **Design §6 "taken from the PR's timeline of pushed commits and force-pushes"**: the timeline has
  no normal-push boundaries. Its commit items are the current commits only, ordered by committer
  date, with no push time (`Commit.pushedDate` is deprecated and returns `null` **[observed]**).
  Suggest "taken from the branch's push history".
- **Design §6 "A revision's base is where its head branched off the base branch"** needs "as the
  base branch was then" for stacked and retargeted PRs, and a merged-PR rule (see §2).
- **The 250-commit cap:** GraphQL `PullRequest.commits`, `PULL_REQUEST_COMMIT` timeline items and
  REST `/pulls/{n}/commits` all stop at 250. On a 293-commit PR, `totalCount` said 293 but paging
  returned 250 **[observed]**. Revision commit lists must come from git.
- **Free 304s:** with `gh`'s token, `x-ratelimit-used` rose by 1 on each `304` from both
  `/activity` and `/pulls/{n}`. Other sessions were using the account at the same time (the counter
  jumped by 8 between my runs), so this is **inconclusive**. Re-check with Lo's own token when
  recording fixtures, since `github-layer.md` §7 observed 304s as free.

---

## 1. Push boundaries

### What each source gives

| Source | What it gives about pushes | Exact? | Limits |
|---|---|---|---|
| **REST activity API** `GET /repos/{o}/{r}/activity?ref=…` | Every push to the ref: `before`, `after`, `timestamp`, `activity_type` (`push`, `force_push`, `branch_creation`, `branch_deletion`, `pr_merge`, `merge_queue_merge`), `actor`. Filters: `ref`, `actor`, `time_period`, `activity_type`, `direction`; cursor pagination. | **Exact.** | Needs read access to the head repo (`X-Accepted-Github-Permissions: contents=read` **[observed]**), so not for deleted forks or private forks the user can't see **[inferred]**. History starts around March 2023 **[observed]**. Per ref name, so a renamed branch splits its history **[inferred]**. Sends an ETag **[observed]**. |
| **GraphQL `HeadRefForcePushedEvent`** | `beforeCommit`, `afterCommit`, `createdAt`, `actor`, `ref` | **Exact** for force-pushes. `beforeCommit` also exposes the last normal push before each force-push. | `ref` is `null` once the branch is deleted **[observed]**. Both commits are `null` when GitHub no longer has them (2015 PR) **[observed]**. |
| **GraphQL `PullRequestCommit`** (timeline) / `PullRequest.commits` | The PR's **current** commits | No push information | Force-pushed-away commits drop out: `filteredCount` equals `commits.totalCount` on every PR checked **[observed]**. Ordered by committer date, not push time **[observed]**. 250 cap **[observed]**. |
| **`Commit.pushedDate`** | Nothing | — | `@deprecated(reason: "pushedDate is no longer supported. Removal on 2023-07-01 UTC.")`, returns `null` **[observed]**. |
| **GraphQL `CheckSuite.push`** (`Commit.checkSuites`) | `previousSha`, `nextSha`, `pusher` of the push that triggered the suite; `createdAt` 1–45 s after the push **[observed]** | **Exact** when present | Only for suites created by a push in the repo that holds the branch. Fork PRs' suites in the base repo have `push: null` **[observed]**. A 2024 fork PR had no suites at all **[observed]**. `CheckSuite.branch` is `null` after branch deletion **[observed]**. |
| **GraphQL `PullRequestReview.commit`**, review comments' `originalCommit` | A commit that was the head (usually) when reviewed | Exact head, time unknown | `null` if the commit is gone **[inferred]**. The REST API lets a review target an older commit (`commit_id`). |
| **REST issue timeline** `/issues/{n}/timeline` | `committed` (git data only: author/committer dates, no push time), `head_ref_force_pushed` with `commit_id` = **after** SHA and `created_at`, but **no before** **[observed]** | Partial | Needs `--paginate`: the first 100 events missed the latest force-push on `rust-lang/rust#165460` **[observed]**. GraphQL's event is strictly richer. |
| **REST `/pulls/{n}/commits`** | Current commits | No push info | 250 max (docs, **[observed]**). |
| **REST events API** (`PushEvent`) | Pushes | — | "Up to 300 events… within the past 30 days." Useless for history. |
| **Webhooks** (`pull_request.synchronize` `before`/`after`) | Every push | Exact | Need a server; not for a desktop app. |
| **`refs/pull/<n>/head`** | The current head only | — | No history refs: `ls-remote` shows only `head`, plus `merge` while open **[observed]**. |
| **Lo's own poller** | Each new `head.sha` from `GET /pulls/{n}`, with first-seen time | Exact head, time ≈ ±poll interval | Only while Lo runs. |

### Live comparison

`recon.py` (Appendix B) rebuilds revisions three ways and checks them against the activity log.

- **T:** force-push events, review commits, check suites on current commits and `headRefOid`.
- **T+:** T, plus following `push.previousSha` through force-pushed-away SHAs with
  `repository.object(oid:)`.

| PR | Head | Revisions (activity) | T finds | T+ finds | Notes |
|---|---|---|---|---|---|
| `cli/cli#14745` | same repo | 15 (creation, 1 push, 13 force) | 15/15 | 15/15 | The normal push r3 was found only via the next force-push's `beforeCommit` and a review. Stacked on another branch, then retargeted to `trunk` by GitHub. |
| `cli/cli#14356` | same repo, stacked | 20 (creation, 2 push, 17 force) | 20/20 | 20/20 | 17 `BaseRefForcePushedEvent`s, each followed by a head force-push about a minute later. |
| `cli/cli#9208` | fork, 2024–25 | 46 (18 push, 28 force) | **38/46** | **38/46** | No check suites on any commit. Misses are consecutive normal pushes (r26–27, r34, r36, r38–39, r41–42). |
| `rust-lang/rust#165460` | fork | 6 (creation, 1 push, 4 force) | 6/6 | 6/6 | The normal push r5 was found only via the next force-push's `beforeCommit`. |
| `rust-lang/rust#165376` | fork | 8 (creation, 7 force) | 8/8 | 8/8 | Base-repo suites have `push: null`. |
| `zed-industries/zed#66482` | same repo | 6 (6 push, 0 force) | 6/6 | 6/6 | 47 commits; the 7 with check suites are exactly the 6 PR heads plus one pre-PR push. |

The `cli/cli#14745` reconstruction, from the activity API:

```
r1  2026-10-19T09:30:40Z branch_creation 00000000->ca3cb242   (PR opened 09:35:48)
r2  2026-10-19T12:37:13Z force_push      ca3cb242->a842bc7a   GraphQL force-push event at 12:37:15
r3  2026-10-19T12:52:34Z push            a842bc7a->aae5f981   invisible in the timeline except as
                                                              r4's beforeCommit and an APPROVED review;
                                                              check suite push.previousSha=a842bc7a
r4  2026-10-21T09:34:20Z force_push      aae5f981->17e9abeb
…
r15 2026-10-21T11:45:24Z force_push      e3117afd->21660dd7   GitHub's rebase after retargeting to
                                                              trunk (committer "GitHub"; actor: the author)
```

### Other facts

- **`totalCount` ignores `itemTypes`.** `timelineItems(itemTypes: [HEAD_REF_FORCE_PUSHED_EVENT]) { totalCount }`
  returned the count of all items. `filteredCount` respects the filter **[observed]**. Use
  `filteredCount` or count nodes.
- **Clock skew between sources** **[observed]**:
  - The activity `timestamp` is 1–4 s before the matching force-push event's `createdAt`.
  - Check suites' `createdAt` is 1–45 s after the push.
  - Join on SHAs, never on times.
- **r1** is the head when the PR was opened: the last activity entry at or before
  `PullRequest.createdAt`. On `cli/cli#9208` the push was at 05:39:53 and the PR opened at 05:39:54
  **[observed]**.
- **Cost** **[observed]**:
  - A 100-item filtered timeline page costs 1 point; with nested `checkSuites(first: 10)` it costs 2.
  - One activity page holds 100 pushes.
  - `repository.object(oid:)` batches cost about 1 point per 40 SHAs **[inferred]**.

### Exact, approximate, impossible

- **Exact:**
  - Every push from the activity API.
  - Force-pushes (before, after, time) from GraphQL.
  - Pushes that triggered a check suite in the branch's repo (`push.previousSha/nextSha`).
  - Every head Lo observed itself.
- **Exact head, approximate time:**
  - Review commits.
  - The `beforeCommit` of a force-push: the head just before it, pushed at some time after the
    previous known push.
  - Suites without `push` (fork PRs): the commit was a head at the suite's `createdAt`.
- **Impossible from the API without the activity log:** splitting a run of normal pushes that
  left no suite, review or observation. Example: commits X, Y, Z pushed as X+Y then Z, with no CI on
  the first push. Only git ancestry is left: every commit in the run is a candidate boundary. Push
  times of normal pushes are also unknown; commit dates are not push dates.

### Recommended algorithm

```
inputs: PR (number, createdAt, closedAt, mergedAt, mergeCommit, headRefName, headRepository,
        headRefOid, baseRefName), and Lo's stored observations
1. events  = GraphQL timelineItems(itemTypes: [HEAD_REF_FORCE_PUSHED_EVENT, BASE_REF_FORCE_PUSHED_EVENT,
             BASE_REF_CHANGED_EVENT, AUTOMATIC_BASE_CHANGE_SUCCEEDED_EVENT, PULL_REQUEST_REVIEW,
             PULL_REQUEST_COMMIT { commit { oid checkSuites(first: 10) { nodes { createdAt push { previousSha nextSha } } } } }])
             paged with `after`, later polled with `since:` / the stored cursor
2. log     = activity(headRepository, headRefName), direction=asc, all pages, ETag-cached
             if available: keep entries with createdAt-window start <= timestamp <= (closedAt or now),
             where the window starts at the last entry <= createdAt (that entry is r1);
             drop branch_deletion; check chain continuity (entry.before == previous.after) and
             cut the log at any break (branch name reused for another PR)
3. if log is complete (chain unbroken, reaches headRefOid): revisions = log. Done.
4. otherwise build from the pieces, keyed by head SHA:
     - each force-push: before (time unknown, just before) and after (time = createdAt)
     - suites with push: nextSha at createdAt, previousSha as an earlier head (chase it with
       repository.object(oid:) for force-pushed-away SHAs, a few rounds)
     - suites without push, review commits, Lo's observations, headRefOid
   order: by known time; place unknown-time heads by ancestry (a normal push descends from the
   previous head) and before the force-push whose beforeCommit they are
5. any adjacent pair (A, B) where A is an ancestor of B and B is more than one commit ahead is a
   fast-forward run whose inner boundaries may be unknown: expose its commits (from local git) as
   pickable points, labelled as such, unless the activity log or suites cover it
6. store: (pr, head_sha, seq, pushed_at?, time_exact, kind, before_sha?, actor?, base_ref_name,
   base_tip_sha?, source); number at display time; pin every head in refs/lo/pull/<n>/revs/<sha>
```

### Failure modes

- **No activity log:**
  - The fork was deleted (`headRepository: null`).
  - The fork is private and invisible to the user **[inferred]**.
  - The PR predates about March 2023.
  - The branch was renamed **[inferred]**.
  - In these cases, consecutive normal pushes merge into one revision (step 5 mitigates).
- **Branch name reused.** A branch deleted and re-created for another PR shares the log. The
  time window and the chain check handle it.
- **Same SHA pushed twice** (A→B→A) gives two revisions with one head. Markers stored by SHA
  resolve to the latest.
- **Check suites for the same SHA pushed to another branch** would add false heads. Use only
  suites whose `push.nextSha` is the commit and whose time falls in the PR's window.
- **GitHub lost the commit** (very old PRs): force-push events have `null` commits. Use REST's
  `commit_id` for the after SHA and mark the revision "no longer available".
- **More than 250 commits:** API commit lists are truncated; use git.

---

## 2. Revision bases

### What GitHub gives

- **`PullRequest.baseRefOid` / REST `base.sha`** is one value, not a history. It's roughly the base
  tip at the PR's last head push, not the fork point **[observed]**:
  - On `zed#65981` and `cli#12942` it equalled the merge-base (probably because `main` hadn't moved
    between the authors' last rebase and push **[inferred]**).
  - On `rust#165460`, pushed this morning without rebasing, it equalled `main`'s current tip,
    22 commits past the merge-base.
  - Never use it as a revision base.
- **`BaseRefForcePushedEvent`** has `beforeCommit`, `afterCommit`, `createdAt` and `ref`. It records
  every force-push of the base branch while the PR targets it: 17 on stacked `cli/cli#14356`
  **[observed]**. Normal pushes to the base branch aren't recorded.
- **`BaseRefChangedEvent`** (`previousRefName`, `currentRefName`) and
  **`AutomaticBaseChangeSucceededEvent`** (`oldBase`, `newBase`) record retargets. On
  `cli/cli#14745`, r1–r14 targeted `babakks/refresh-token-review-fixes-round-2`; GitHub retargeted
  to `trunk` at 11:45:21 and rebased the head at 11:45:24 **[observed]**.
- **The activity API on the base branch** gives its tip at any time: the `after` of the last entry
  at or before t. Demonstrated on `cli/cli#14745` r2 (`a842bc7a`, pushed 12:37:13) **[observed]**:

  | Base used | Merge-base | Commits ahead |
  |---|---|---|
  | `trunk` today | `2761962e` | 65 |
  | The stacked branch's tip at 12:37 (`9e953600`, pushed 12:20:02) | `9e953600` | **3** |

- **REST `GET /compare/{a}...{b}`** works on force-pushed-away SHAs. It returns
  `merge_base_commit`, `ahead_by`, `behind_by` and `files` **[observed]**. It's the API-only fallback
  while the local copy is still cloning (design §11 "First open"), without rebase-awareness.

### Rule

```
base_branch_i = the base ref name in force when revision i was pushed (from BaseRefChanged /
                AutomaticBaseChangeSucceeded events)
base_tip_i    = if base_branch_i only moved forward since then and the PR isn't merged by merge commit:
                    current tip of base_branch_i (refs/lo/branches/<name>)      -- exact
                elif the PR was merged with a merge commit (mergeCommit has 2 parents):
                    mergeCommit^1                                                    -- exact
                elif base_branch_i was force-pushed (stacks):
                    latest of {BaseRefForcePushedEvent.afterCommit, activity(base_branch_i).after,
                    Lo's pinned tips of the lower PR} at or before pushed_at_i     -- exact if found
                else: current tip, flagged "base uncertain"
base_i        = git merge-base head_i base_tip_i       (git merge-base --all; see §5 for >1)
```

- **Why today's tip is fine for an ordinary base:**
  - If the tip at push time (B_t) is an ancestor of today's tip (B_now), then
    `merge-base(H, B_now) == merge-base(H, B_t)`. The exception is when B_now contains commits from
    H's history that B_t didn't.
  - That exception happens when the PR itself landed by a merge commit or fast-forward. Then
    `merge-base(H_final, main) == H_final` and the revision's diff becomes empty, hence the
    merge-commit rule **[inferred]**.
  - Squash and rebase merges create new SHAs, so they don't trigger it.
- **Is the base branch's history recoverable?** Yes:
  - The activity API from about March 2023, plus `BaseRefForcePushedEvent` for force-pushes while
    targeted, plus Lo's pins.
  - Base commits fetch by SHA like any other (§6).

---

## 3. Markers

### "Your last review"

- `PullRequestReview.commit: Commit` is "the commit associated with this pull request review".
- `PullRequest.viewerLatestReview` is "the latest review given from the viewer". It returned `null`
  for `scornz` on PRs without a review **[observed]**.
- Whether `viewerLatestReview` can return the viewer's PENDING review isn't documented. The explicit
  query avoids the question:

  ```graphql
  reviews(last: 1, author: $login, states: [APPROVED, CHANGES_REQUESTED, COMMENTED, DISMISSED]) {
    nodes { commit { oid } submittedAt state }
  }
  ```

  (`latestReviews` is documented as "not also pending", but it's per-user across all reviewers.)
- **Mapping to a revision:**
  - Use the revision whose head is `commit.oid`.
  - Otherwise use the first revision whose head contains it. An API review can target a non-head
    commit.
  - If the commit is `null` or force-pushed away and unknown, use the revision current at
    `submittedAt`.
- **Observed on `cli/cli#14745`:** reviews on `ca3cb242` (r1), `a842bc7a` (r2) and `aae5f981` (r3).
  r3's only other trace in the timeline is r4's `beforeCommit`.
- **Reviews Lo submits** carry `commitOID` (design §11), so they're exact.
- **Open question:** a pending review started on head X and submitted after a push to Y. Its
  `commit` is probably X, since comments anchor there **[inferred]**. Verify with `rozed-bot`.

### "Last seen here"

- Local, as designed: store the head SHA of the newest revision on screen when the user leaves
  the PR.
- **`PullRequestRevisionMarker`** is a timeline item: "Represents the latest point in the pull
  request timeline for which the viewer has seen the pull request's commits", with `createdAt` and
  `lastSeenCommit: Commit!`.
  - `filteredCount` was 0 on all 25+ PRs checked, all read by `scornz` **[observed]**. I couldn't
    tell whether commits arrived after `scornz` last viewed any of them.
  - No mutation sets it (schema: none of the mutations mention seen or read state, apart from
    files' `markFileAsViewed`).
  - It reflects GitHub's web visits, if anything **[inferred]**. Don't use it.
- **`PullRequest.isReadByViewer`** is a Boolean: "Is this pull request read by the viewer".
  Notification-style, with no commit **[observed]**: `true` on every PR checked.

---

## 4. Viewed marks

- **Schema:**
  - `PullRequestChangedFile { path additions deletions changeType viewerViewedState: FileViewedState! }`.
  - `FileViewedState`:
    - `VIEWED`: "The file has been marked as viewed."
    - `UNVIEWED`: "The file has not been marked as viewed."
    - `DISMISSED`: "The file has new changes since last viewed."
  - There's no timestamp, commit or blob for a mark.
- **Mutations:**
  - `markFileAsViewed(input: { pullRequestId: ID!, path: String! })` and
    `unmarkFileAsViewed(input: { pullRequestId, path })`.
  - One path per call, **no commit argument**.
  - REST has no viewed-state endpoint: the pulls REST page never mentions it.
- **Scope:** per viewer ("the state of the file for the viewer"), per PR (`pullRequestId`), per path.
  Each PR in a stack has its own marks, as design §6 says.
- **When it resets:**
  - Docs: "After you finish reviewing a file, you can mark the file as viewed. The file will
    collapse. If the file changes after you view the file, it will be unmarked as viewed."
    (Reviewing proposed changes in a pull request).
  - The schema calls that state `DISMISSED`.
  - What counts as "changes" isn't documented. Neither is whether a rebase that leaves the file's
    PR diff unchanged (or changes only its base content) dismisses the mark.
- **Race:** a mark lands on GitHub's head at the time of the mutation **[inferred]**. If a push
  arrives between rendering and pressing `e`, GitHub marks the new version viewed.
  - Lo should compare the head it rendered with the latest polled head before sending.
  - If they differ, mark locally only and show the new revision first.
  - It should record locally `(pr, path) → head SHA rendered`, as design §6 says.
- **Reads:** `files(first: 100) { nodes { path viewerViewedState } }` costs 1 point per page: 1 on a
  214-file PR's first page **[observed]**, so 3 points for 214 files. REST documents a 3,000-file
  cap on `/pulls/{n}/files`; GraphQL's cap is untested.
- **Writes:**
  - Each mutation is 5 secondary-limit points.
  - The docs say "pause at least 1 second between mutative requests".
  - The primary cost of a mutation isn't documented; probably 1 point **[inferred]**.
  - Whether marks count toward the content-creation limit (80/min, 500/h) isn't documented.
  - Queue them per account: update the UI at once and coalesce mark-then-unmark into nothing.
  - Several marks could go in one GraphQL document as aliased mutation fields (one request, 5
    secondary points) **[inferred, test it]**.
- **Test plan** (`rozed-bot` on `rozed-dev/playground`, milestone 6 fixtures):
  1. Mark `a` viewed at H1, then push a commit touching only `b`. Is `a` still `VIEWED`?
  2. Push a change to `a`. Is it `DISMISSED`, and right after the push or later?
  3. Rebase onto a moved `main` where `main` changed nothing in `a`. Does `a` stay `VIEWED`?
  4. Rebase where `main` changed other lines of `a`, so the PR's diff of `a` is unchanged but its
     blob differs.
  5. Remove `a` from the diff (revert it), then re-add it.
  6. `unmarkFileAsViewed` on a `DISMISSED` file. Is it `UNVIEWED`?
  7. Send 5 aliased `markFileAsViewed` fields in one request.
  8. Read `rateLimit { cost }` on a mutation.

---

## 5. Rebase-aware comparison with git

### Invocation

```sh
# Lo's isolated environment, as in plan Task 4.1, including GIT_ASKPASS for lazy fetches
OLD_BASE=$(git merge-base "$OLD_HEAD" "$OLD_BASE_TIP")   # §2
NEW_BASE=$(git merge-base "$NEW_HEAD" "$NEW_BASE_TIP")
if [ "$OLD_BASE" = "$NEW_BASE" ]; then
  git diff -M --name-status "$OLD_HEAD" "$NEW_HEAD"       # nothing to replay
else
  TREE=$(git merge-tree --write-tree --no-messages --merge-base="$OLD_BASE" "$NEW_BASE" "$OLD_HEAD")
  case $? in
    0) git diff -M --name-status "$TREE" "$NEW_HEAD" ;;   # only the author's changes
    1) : conflicted: plain diff of OLD_HEAD..NEW_HEAD, with "couldn't separate the rebase" ;;
    *) : error ;;
  esac
fi
```

- **Argument order:**
  - `<branch1>` is `NEW_BASE` ("ours").
  - `<branch2>` is `OLD_HEAD` ("theirs").
  - `--merge-base` is `OLD_BASE`.
  - The result is NEW_BASE plus (OLD_BASE → OLD_HEAD): "r1 as if it had been written on r2's base".
- **Diffing:**
  - `git diff <tree> <commit>` works directly.
  - For the diff view, read each side with `git cat-file --batch`: `TREE:path` and
    `NEW_HEAD:path`.
- **Exit status** (man page): "0 for a successful, non-conflicted merge", "1 when the merge has
  conflicts", "something other than 0 or 1" on error.
- **Output:**
  - Clean: one line, the tree OID.
  - Conflicted: the tree OID, then "Conflicted file info" lines `<mode> <object> <stage> <path>`
    (with `--name-only`, just paths, one per conflicted file), then a blank line and messages.
  - `-z`:
    - Paths aren't quoted and end in NUL.
    - The messages section starts with NUL.
    - Messages are records `<n>NUL<path>…NUL<type>NUL<message>NUL`.
    - `<type>` is a stable string such as `Auto-merging` or `CONFLICT (contents)`; the message text
      isn't stable.
  - Observed `-z --name-only` output, with NULs shown as `|`:
    `f74d…|src/a.txt||1|src/a.txt|Auto-merging|Auto-merging src/a.txt\n|1|src/a.txt|CONFLICT (contents)|CONFLICT (content): Merge conflict in src/a.txt\n|`
  - Use `--no-messages` unless showing why the replay failed. Then use `-z` and show the `<type>`
    per path.
- **Rules from the man page's "Mistakes to avoid":** don't treat an empty conflicted-file list as
  clean (check the exit status), and don't scan the tree for conflict markers.
- **Version floors:** `--merge-base` and the `--stdin` line form `<base> -- <b1> <b2>` both came
  with `kz/merge-tree-merge-base`, merged 2022-12-14, in **2.40**. `--write-tree` is 2.38,
  `--no-lazy-fetch` 2.45 and `--quiet` 2.50.
- **Batching:** `git merge-tree --stdin` takes lines `<base> -- <branch1> <branch2>` and returns
  `<1 clean|0 conflicted>NUL<tree>NUL…` per line, in one process **[observed]**. Useful for
  changed-since-viewed across several revision pairs. `--merge-base` itself is incompatible with
  `--stdin`.

### What the experiment showed

All from Appendix A, run with git 2.54.0 **[observed]**:

1. **Rebased with one extra change.**
   - Plain `r1..r2` showed `src/b.txt`, `src/c.txt` and `src/e.txt`.
   - The replayed comparison showed only `src/c.txt`, the extra change.
2. **Conflict.** `main` changed the line the PR changed, and the author resolved it in r3.
   - Exit 1, conflicted path `src/a.txt` with stages 1, 2 and 3.
   - `--quiet` also exits 1. The fallback plain diff shows `src/a.txt`.
3. **Stacked layer whose base was force-pushed.**
   - `merge-base(U1, lower now)` is a `main` commit, because L1 is gone.
   - Replaying with it **conflicted** (exit 1), and the result diff showed bogus deletions in
     `src/lower.txt`.
   - Replaying with the old lower tip (`--merge-base=L1 L1' U1`) was clean and the diff was empty,
     which is right: the author changed nothing.
4. **Rename on one side, edit on the other.** PR renames `b.txt` to `bee.txt` and edits it; `main`
   edits `b.txt` elsewhere.
   - The replay was clean.
   - Plain `s1..s2` showed `M src/bee.txt` and `A src/extra.txt`; the replay showed only
     `A src/extra.txt`.
5. **`--stdin`** returned two clean results in one process.

### In a bare blobless partial clone

- **Yes, it fetches on demand** through the promisor remote. It's batched, and only what's needed
  **[observed]**:
  - One-sided changes only (every path changed on one side): **0** fetches. merge-ort resolves
    those by OID. `git diff --name-status` of the result: 0 fetches. The content diff: 1 batched
    fetch.
  - 3 files changed on both sides (3 three-way content merges): **1** fetch process, 12 blobs.
  - Rename plus edit: **2** fetch processes (rename detection, then the content merge).
- **With `git --no-lazy-fetch`** (2.45+), a replay that needs blobs fails with exit 128:
  `fatal: could not fetch <oid> from promisor remote`. A replay that needs none still succeeds.
  - Lo can use this offline to say "needs network" instead of hanging on a fetch.
  - It must treat 128 as "fetch needed", not as a conflict.
- **Fetches need credentials:** every git call against the repo must carry the `GIT_ASKPASS`
  environment, or lazy fetches of private repos fail (plan Task 4.1 already says this).
- **merge-tree writes objects:** the result tree and merged blobs go into the cache repo,
  unreachable. They're harmless and pruned by `gc` **[inferred]**. Cache the result tree OID per
  `(old_base, new_base, old_head)` in SQLite to skip recomputation.

### Edge cases

- **More than one merge-base** (criss-cross from merging `main` in more than once). `--merge-base`
  takes one: "using this option may cause merge results to differ… potentially losing some changes".
  If `git merge-base --all` returns more than one, show the plain diff with a note **[inferred]**.
- **`OLD_BASE` not an ancestor of `NEW_BASE`** (base force-pushed, retargeted). This is fine:
  `--merge-base` makes merge-tree ignore history. It's exactly the stacked case above.
- **Binary files changed on both sides:** a conflict ("CONFLICT (binary)"); fall back.
- **Submodules:** "CONFLICT (submodule lacks merge base)" is possible; fall back.
- **Moving the base only, with no author change:** the diff is empty, and design §6 says to show
  "Nothing new since rN" over the full diff. Different from that: the replay conflicted because the
  author had to resolve something. Then the plain diff plus note is right.

---

## 6. Missing revisions

| Commit | Fetched from | Result **[observed]** |
|---|---|---|
| `cli/cli#14745` r1 `ca3cb242` (force-pushed away 2026-10-19; branch deleted) | `github.com/cli/cli` | OK |
| `cli/cli#14745` r3 `aae5f981` (normal push, then force-pushed away) | `cli/cli` | OK |
| `cli/cli#9208` r1 `3a19f88c` (fork `Frederick888/cli`, 2024-06, force-pushed away) | `cli/cli` (base repo) | OK |
| same | `Frederick888/cli` | OK |
| `Frederick888/cli` pre-PR branch creation `3b13f50a` (2024-06) | `cli/cli` | OK |
| `cli/cli#4005` force-push `before` `72665d38` (2021-08) | `cli/cli` | OK |
| `rust-lang/rust#30000` force-push `after` `c69299a9` (2015-11) | `rust-lang/rust` | **`fatal: remote error: upload-pack: not our ref c69299a9…`** |
| `rust-lang/rust#30000` later `2eca64de` (2015-11) | `rust-lang/rust` | OK |
| A random SHA | `rust-lang/rust` | `upload-pack: not our ref` (same message) |

- **The 2015 loss is visible in the API too:**
  - GraphQL gives `beforeCommit: null, afterCommit: null` on all of that PR's force-push events.
  - REST timeline still has `commit_id: c69299a9…`.
  - REST `GET /commits/c69299a9…` returns `422` "No commit found for SHA" **[observed]**.
- **Why they usually survive:** GitHub keeps "internal references used for displaying the diff view
  in pull requests" (`refs/__gh__/pull/…` alongside `refs/pull/…`), and only support can remove them
  (docs, Removing sensitive data). So force-pushed PR commits stay fetchable while the PR exists
  **[inferred]**. No retention period is documented. The 2015 case shows it isn't forever,
  probably from before those references or a one-off cleanup **[inferred]**.
- **Fork commits fetch from the base repo URL** (same fork network) **[observed]**. Lo doesn't
  need access to the fork to fetch a fork PR's old revisions. For private repos that should hold
  with the user's token **[inferred]**.
- **`refs/pull/<n>/head` has no history.** `ls-remote` on 6 PRs showed only `refs/pull/<n>/head`,
  plus `refs/pull/<n>/merge` on the open, mergeable one **[observed]**. Docs: the `refs/pull/`
  namespace is read-only ("deny updating a hidden ref").
- **Recommendation:**
  1. On opening a PR, fetch every known revision head by SHA in one `git fetch` and pin them under
     `refs/lo/pull/<n>/revs/<sha>`.
  2. Mark a revision "no longer available" when the API reports its commit as `null`/`422`, or
     `git fetch` fails with `not our ref`.
  3. Keep showing it in the picker with its time and author, disabled.

---

## Open questions for implementation

- What exactly triggers `DISMISSED`: a rebase that leaves the file's diff unchanged? (Test plan in §4.)
- Can `viewerLatestReview` return the viewer's PENDING review? Which `commit` does a review started
  before a push carry?
- Does the activity API follow branch renames? What does it return for a private fork the user can't
  read (presumably 404)?
- Are `304`s on `/activity` free with Lo's OAuth token? (Inconclusive here; see Corrections.)
- Is the activity API's history really bounded at about March 2023 for all repos, or per-repo
  (feature launch, or retention)? No retention is documented.
- Can several `markFileAsViewed` calls be aliased in one request, and what do they cost?

## Sources

- GraphQL schema: <https://docs.github.com/public/fpt/schema.docs.graphql>. Types:
  - `HeadRefForcePushedEvent`, `BaseRefForcePushedEvent`
  - `PullRequestCommit`, `PullRequestRevisionMarker`, `PullRequestTimelineItemsItemType`
  - `PullRequestTimelineItemsConnection` (`totalCount` vs `filteredCount`)
  - `Commit.pushedDate`, `CheckSuite.push`, `Push`
  - `PullRequestReview.commit`, `PullRequest.viewerLatestReview`, `PullRequest.isReadByViewer`
  - `PullRequestChangedFile`, `FileViewedState`, `MarkFileAsViewedInput`, `UnmarkFileAsViewedInput`
- [List repository activities](https://docs.github.com/en/rest/repos/repos#list-repository-activities)
  and [Using the activity view](https://docs.github.com/en/repositories/viewing-activity-and-data-for-your-repository/using-the-activity-view-to-see-changes-to-a-repository)
- [List commits on a pull request](https://docs.github.com/en/rest/pulls/pulls#list-commits-on-a-pull-request)
  (250 maximum) and [List pull requests files](https://docs.github.com/en/rest/pulls/pulls#list-pull-requests-files)
  (3,000 maximum)
- [Timeline events](https://docs.github.com/en/rest/issues/timeline) and
  [Events API](https://docs.github.com/en/rest/activity/events) (300 events, 30 days)
- [Reviewing proposed changes in a pull request](https://docs.github.com/en/pull-requests/how-tos/review-pull-requests/reviewing-proposed-changes-in-a-pull-request#marking-a-file-as-viewed)
- [Checking out pull requests locally](https://docs.github.com/en/pull-requests/how-tos/review-pull-requests/checking-out-pull-requests-locally)
  (`refs/pull/` is read-only)
- [Removing sensitive data from a repository](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/removing-sensitive-data-from-a-repository)
  (internal PR references, `refs/__gh__/pull`)
- [Rate limits for the GraphQL API](https://docs.github.com/en/graphql/overview/rate-limits-and-query-limits-for-the-graphql-api)
  (secondary points: 5 per mutation, ≥1 s between mutations)
- `git merge-tree` man page (git 2.54.0); RelNotes [2.38.0](https://github.com/git/git/blob/master/Documentation/RelNotes/2.38.0.adoc),
  [2.40.0](https://github.com/git/git/blob/master/Documentation/RelNotes/2.40.0.adoc),
  [2.45.0](https://github.com/git/git/blob/master/Documentation/RelNotes/2.45.0.adoc),
  [2.50.0](https://github.com/git/git/blob/master/Documentation/RelNotes/2.50.0.adoc); git commit
  `501e3bab99` "merge-tree.c: allow specifying the merge-base when --stdin is passed" (merged as
  `7576e512ce`, 2022-12-14).
- Live PRs: `cli/cli` [#14745](https://github.com/cli/cli/pull/14745),
  [#14356](https://github.com/cli/cli/pull/14356), [#9208](https://github.com/cli/cli/pull/9208),
  [#4005](https://github.com/cli/cli/pull/4005), [#12942](https://github.com/cli/cli/pull/12942);
  `rust-lang/rust` [#165460](https://github.com/rust-lang/rust/pull/165460),
  [#165376](https://github.com/rust-lang/rust/pull/165376), [#30000](https://github.com/rust-lang/rust/pull/30000);
  `zed-industries/zed` [#66482](https://github.com/zed-industries/zed/pull/66482),
  [#65981](https://github.com/zed-industries/zed/pull/65981).

---

## Appendix A: `rebase-aware.sh`

Run with `bash rebase-aware.sh`. It needs git 2.40 or newer, writes only under a new
`mktemp -d` directory, and uses no network. SHAs are deterministic: the dates and identities are
fixed.

```bash
#!/usr/bin/env bash
# Rebase-aware revision comparison with `git merge-tree`, proved on throwaway repos.
#
# Builds an "origin" repo, a PR branch, rebases it onto a moved main with one extra
# change, then works from a bare blobless partial clone (Lo's local copy) to show:
#   1. a plain old_head..new_head diff includes main's changes;
#   2. replaying r1 onto r2's base with merge-tree, then diffing against r2, shows only
#      the author's extra change;
#   3. a conflicting replay exits 1 and lists the conflicted paths;
#   4. a stacked layer whose base was force-pushed needs the old base tip, not a
#      merge-base with today's base branch;
#   5. merge-tree and diff fetch missing blobs on demand in the partial clone, in batches;
#   6. several content merges cost one batched fetch;
#   7. a rename on one side and an edit on the other replay cleanly;
#   8. --stdin replays several revision pairs in one process.
# Needs git >= 2.40 (--merge-base). Writes only under a new temp dir.
set -euo pipefail

# Lo's isolated git environment (decisions.md "Local copy").
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_TERMINAL_PROMPT=0 LC_ALL=C
export GIT_AUTHOR_NAME=a GIT_AUTHOR_EMAIL=a@example.com GIT_COMMITTER_NAME=a GIT_COMMITTER_EMAIL=a@example.com
T=0
tick() { T=$((T + 60)); export GIT_AUTHOR_DATE="@$((1700000000 + T)) +0000" GIT_COMMITTER_DATE="@$((1700000000 + T)) +0000"; }

ROOT=$(mktemp -d "${TMPDIR:-/tmp}/lo-mergetree.XXXXXX")
echo "work dir: $ROOT"; git --version
O="$ROOT/origin"
g() { git -C "$O" "$@"; }
lines() { for i in $(seq 1 "$2"); do echo "$1 line $i"; done; }
section() { printf '\n==== %s\n' "$*"; }

# ---------------------------------------------------------------- origin and r1
git init -q -b main "$O"
g config uploadpack.allowFilter true          # serve --filter=blob:none like GitHub
g config uploadpack.allowAnySHA1InWant true   # fetch by SHA like GitHub
mkdir -p "$O/src"
lines a 20 > "$O/src/a.txt"; lines b 20 > "$O/src/b.txt"; lines c 20 > "$O/src/c.txt"
tick; g add -A; g commit -qm "M1: initial"

g switch -qc feature
sed -i.bak 's/^a line 3$/a line 3 (changed by the PR)/' "$O/src/a.txt"; rm "$O/src/a.txt.bak"
tick; g commit -qam "P1: change a.txt"
lines d 5 > "$O/src/d.txt"
tick; g add -A; g commit -qm "P2: add d.txt"
g update-ref refs/pull/1/head feature
R1=$(g rev-parse feature)

# ---------------------------------------------------------------- main moves on
g switch -q main
sed -i.bak 's/^b line 10$/b line 10 (changed on main)/' "$O/src/b.txt"; rm "$O/src/b.txt.bak"
tick; g commit -qam "M2: change b.txt"
lines e 5 > "$O/src/e.txt"
tick; g add -A; g commit -qm "M3: add e.txt"

# ---------------------------------------------------------------- r2: rebase + one extra change
g switch -q feature
tick; g rebase -q main
sed -i.bak 's/^c line 15$/c line 15 (the extra change in r2)/' "$O/src/c.txt"; rm "$O/src/c.txt.bak"
tick; g commit -qam "P3: the extra change"
g update-ref refs/pull/1/head feature        # a force-push: refs/pull/1/head moves r1 -> r2
R2=$(g rev-parse feature)

# ---------------------------------------------------------------- Lo's bare partial clone
C="$ROOT/cache.git"
git clone -q --bare --filter=blob:none "file://$O" "$C"
c() { git -C "$C" "$@"; }
missing() { c rev-list --objects --all --missing=print | grep -c '^?' || true; }
# fetch both revisions by SHA into Lo's own refs (r1 is no longer on any branch)
c fetch -q origin "$R1:refs/lo/pull/1/revs/$R1" "$R2:refs/lo/pull/1/revs/$R2" \
  "+refs/heads/main:refs/lo/branches/main"
echo "r1=$R1"; echo "r2=$R2"; echo "missing blobs after clone: $(missing)"

OLD_HEAD=$R1; NEW_HEAD=$R2
OLD_BASE=$(c merge-base "$OLD_HEAD" refs/lo/branches/main)
NEW_BASE=$(c merge-base "$NEW_HEAD" refs/lo/branches/main)
echo "old_base=$(c log -1 --format='%h %s' "$OLD_BASE")  new_base=$(c log -1 --format='%h %s' "$NEW_BASE")"

section "1. plain comparison r1..r2 (what a naive revision diff shows)"
c diff --stat "$OLD_HEAD" "$NEW_HEAD"

section "2. rebase-aware: replay r1's changes onto r2's base, then diff against r2"
echo "\$ git merge-tree --write-tree --merge-base=\$OLD_BASE \$NEW_BASE \$OLD_HEAD"
set +e; REPLAY=$(c merge-tree --write-tree --merge-base="$OLD_BASE" "$NEW_BASE" "$OLD_HEAD"); rc=$?; set -e
echo "exit=$rc tree=$REPLAY"
echo "\$ git diff --stat \$REPLAY \$NEW_HEAD"; c diff --stat "$REPLAY" "$NEW_HEAD"
echo "\$ git diff \$REPLAY \$NEW_HEAD"; c diff "$REPLAY" "$NEW_HEAD"
echo "file list (what Lo shows): $(c diff --name-status "$REPLAY" "$NEW_HEAD" | tr '\n\t' '; ')"
echo "missing blobs now: $(missing)"

section "3. conflict: main changes the line the PR changed, and the author resolves it in r3"
g switch -q main
sed -i.bak 's/^a line 3$/a line 3 (changed on main too)/' "$O/src/a.txt"; rm "$O/src/a.txt.bak"
tick; g commit -qam "M4: change a.txt line 3"
g switch -q feature
tick; git -C "$O" rebase -q main >/dev/null 2>&1 || {
  printf '%s\n' "a line 3 (changed by the PR, merged with main)" > "$ROOT/l3"
  awk -v r="$(cat "$ROOT/l3")" 'BEGIN{skip=0} /^<<<<<<< /{print r; skip=1; next} /^>>>>>>> /{skip=0; next} !skip' \
    "$O/src/a.txt" > "$ROOT/a.res" && mv "$ROOT/a.res" "$O/src/a.txt"
  g add src/a.txt; GIT_EDITOR=true g rebase --continue >/dev/null; }
g update-ref refs/pull/1/head feature
R3=$(g rev-parse feature)
c fetch -q origin "$R3:refs/lo/pull/1/revs/$R3" "+refs/heads/main:refs/lo/branches/main"
OLD_HEAD=$R2; NEW_HEAD=$R3
OLD_BASE=$(c merge-base "$OLD_HEAD" refs/lo/branches/main)
NEW_BASE=$(c merge-base "$NEW_HEAD" refs/lo/branches/main)
echo "--- default output"
set +e; c merge-tree --write-tree --merge-base="$OLD_BASE" "$NEW_BASE" "$OLD_HEAD"; echo "exit=$?"
echo "--- --name-only --no-messages (one path per line)"
c merge-tree --write-tree --name-only --no-messages --merge-base="$OLD_BASE" "$NEW_BASE" "$OLD_HEAD"; echo "exit=$?"
echo "--- -z --name-only (NULs shown as |)"
c merge-tree --write-tree -z --name-only --merge-base="$OLD_BASE" "$NEW_BASE" "$OLD_HEAD" | tr '\0' '|'; rc=${PIPESTATUS[0]}; echo; echo "exit=$rc"
echo "--- --quiet"
c merge-tree --write-tree --quiet --merge-base="$OLD_BASE" "$NEW_BASE" "$OLD_HEAD"; echo "exit=$?"
set -e
echo "--- fallback (plain r2..r3), which is what Lo shows with a note"
c diff --stat "$OLD_HEAD" "$NEW_HEAD"

section "4. stacked layer: the layer below is force-pushed, the upper layer only rebases"
g switch -q main; g switch -qc lower
lines l 5 > "$O/src/lower.txt"; tick; g add -A; g commit -qm "L1: lower layer"
LOWER1=$(g rev-parse lower)
g switch -qc upper
lines u 5 > "$O/src/upper.txt"; tick; g add -A; g commit -qm "U1: upper layer"
U1=$(g rev-parse upper)
g switch -q lower
echo "l amended" >> "$O/src/lower.txt"; tick; g commit -q --amend -am "L1': lower layer, amended"
LOWER2=$(g rev-parse lower)                     # base_ref_force_pushed: LOWER1 -> LOWER2
g switch -q upper; tick; g rebase -q --onto lower "$LOWER1" upper
U2=$(g rev-parse upper)                         # head_ref_force_pushed: U1 -> U2
c fetch -q origin "$U1:refs/lo/pull/2/revs/$U1" "$U2:refs/lo/pull/2/revs/$U2" \
  "$LOWER1:refs/lo/pull/1/revs/$LOWER1" "+refs/heads/lower:refs/lo/branches/lower"
NAIVE_OLD_BASE=$(c merge-base "$U1" refs/lo/branches/lower)
echo "merge-base(U1, lower now) = $(c log -1 --format='%h %s' "$NAIVE_OLD_BASE")   <- wrong: L1 is gone from lower"
echo "--- plain U1..U2:"; c diff --stat "$U1" "$U2"
echo "--- replay with the naive base:"
set +e; T_BAD=$(c merge-tree --write-tree --merge-base="$NAIVE_OLD_BASE" "$LOWER2" "$U1"); rc=$?; set -e
echo "exit=$rc"; c diff --stat "$(echo "$T_BAD" | head -1)" "$U2"
echo "--- replay with the old base tip (from base_ref_force_pushed.beforeCommit or Lo's pinned ref):"
T_OK=$(c merge-tree --write-tree --merge-base="$LOWER1" "$LOWER2" "$U1")
echo "exit=0 tree=$T_OK"; echo "diff --stat: [$(c diff --stat "$T_OK" "$U2")] (empty: the author changed nothing)"

section "5. lazy blob fetching in the bare partial clone"
git clone -q --bare --filter=blob:none "file://$O" "$ROOT/cold.git"
k() { git -C "$ROOT/cold.git" "$@"; }
k fetch -q origin "$R1:refs/lo/r1" "$R2:refs/lo/r2"
OB=$(k merge-base refs/lo/r1 refs/heads/main); NB=$(k merge-base refs/lo/r2 refs/heads/main)
echo "missing blobs before: $(k rev-list --objects --all --missing=print | grep -c '^?')"
export GIT_TRACE2_EVENT="$ROOT/trace.json"
TREE=$(k merge-tree --write-tree --merge-base="$OB" "$NB" refs/lo/r1)
echo "merge-tree lazy fetches: $(grep -c '"event":"child_start".*"fetch"' "$ROOT/trace.json" || true)"
: > "$ROOT/trace.json"
k diff --name-status "$TREE" refs/lo/r2 >/dev/null
echo "diff --name-status lazy fetches: $(grep -c '"event":"child_start".*"fetch"' "$ROOT/trace.json" || true)"
: > "$ROOT/trace.json"
k diff "$TREE" refs/lo/r2 >/dev/null
echo "diff (content) lazy fetches: $(grep -c '"event":"child_start".*"fetch"' "$ROOT/trace.json" || true)"
unset GIT_TRACE2_EVENT
echo "missing blobs after: $(k rev-list --objects --all --missing=print | grep -c '^?')"
echo "--- with lazy fetching off (git --no-lazy-fetch, git >= 2.45) in a fresh clone:"
git clone -q --bare --filter=blob:none "file://$O" "$ROOT/off.git"
git -C "$ROOT/off.git" fetch -q origin "$R1:refs/lo/r1" "$R2:refs/lo/r2"
set +e
git -C "$ROOT/off.git" --no-lazy-fetch merge-tree --write-tree --merge-base="$OB" "$NB" refs/lo/r1 2>&1 | tail -2
echo "exit=${PIPESTATUS[0]}"
set -e

section "6. a replay that needs real content merges: are blob fetches batched?"
g switch -q main; g switch -qc pr3
for f in a b c; do sed -i.bak "s/^$f line 6\$/$f line 6 (PR 3)/" "$O/src/$f.txt"; rm "$O/src/$f.txt.bak"; done
tick; g commit -qam "Q1: PR 3 changes line 6 of a, b and c"
Q1=$(g rev-parse pr3)
g switch -q main
for f in a b c; do sed -i.bak "s/^$f line 19\$/$f line 19 (main again)/" "$O/src/$f.txt"; rm "$O/src/$f.txt.bak"; done
tick; g commit -qam "M5: main changes line 19 of a, b and c"
g switch -q pr3; tick; g rebase -q main >/dev/null
Q2=$(g rev-parse pr3)
git clone -q --bare --filter=blob:none "file://$O" "$ROOT/cold2.git"
q() { git -C "$ROOT/cold2.git" "$@"; }
q fetch -q origin "$Q1:refs/lo/q1" "$Q2:refs/lo/q2"
QOB=$(q merge-base refs/lo/q1 refs/heads/main); QNB=$(q merge-base refs/lo/q2 refs/heads/main)
echo "missing blobs before: $(q rev-list --objects --all --missing=print | grep -c '^?')"
export GIT_TRACE2_EVENT="$ROOT/trace2.json"
QT=$(q merge-tree --write-tree --merge-base="$QOB" "$QNB" refs/lo/q1)
unset GIT_TRACE2_EVENT
echo "merge-tree: 3 three-way content merges, lazy fetch processes: $(grep -c '"event":"child_start".*"fetch"' "$ROOT/trace2.json" || true)"
echo "missing blobs after: $(q rev-list --objects --all --missing=print | grep -c '^?')"
echo "diff vs r2 (empty = only rebase noise): [$(q diff --stat "$QT" refs/lo/q2)]"
git clone -q --bare --filter=blob:none "file://$O" "$ROOT/off2.git"
git -C "$ROOT/off2.git" fetch -q origin "$Q1:refs/lo/q1" "$Q2:refs/lo/q2"
set +e
git -C "$ROOT/off2.git" --no-lazy-fetch merge-tree --write-tree --merge-base="$QOB" "$QNB" refs/lo/q1 2>&1 | tail -3
echo "exit with --no-lazy-fetch: ${PIPESTATUS[0]}"
set -e


section "7. renames: PR 4 renames b.txt and edits it; main edits b.txt elsewhere"
g switch -q main; g switch -qc pr4
g mv src/b.txt src/bee.txt
sed -i.bak 's/^b line 4$/b line 4 (PR 4)/' "$O/src/bee.txt"; rm "$O/src/bee.txt.bak"
tick; g commit -qam "S1: rename b.txt to bee.txt and edit it"
S1=$(g rev-parse pr4)
g switch -q main
sed -i.bak 's/^b line 16$/b line 16 (main, after S1)/' "$O/src/b.txt"; rm "$O/src/b.txt.bak"
tick; g commit -qam "M6: main edits b.txt line 16"
g switch -q pr4; tick; g rebase -q main >/dev/null 2>&1
echo "x" > "$O/src/extra.txt"; g add -A; tick; g commit -qm "S2: extra file"
S2=$(g rev-parse pr4)
git clone -q --bare --filter=blob:none "file://$O" "$ROOT/cold3.git"
w() { git -C "$ROOT/cold3.git" "$@"; }
w fetch -q origin "$S1:refs/lo/s1" "$S2:refs/lo/s2"
SOB=$(w merge-base refs/lo/s1 refs/heads/main); SNB=$(w merge-base refs/lo/s2 refs/heads/main)
export GIT_TRACE2_EVENT="$ROOT/trace3.json"
set +e; ST=$(w merge-tree --write-tree --merge-base="$SOB" "$SNB" refs/lo/s1); rc=$?; set -e
unset GIT_TRACE2_EVENT
echo "exit=$rc, lazy fetch processes: $(grep -c '"event":"child_start".*"fetch"' "$ROOT/trace3.json" || true)"
echo "plain s1..s2: $(w diff --name-status -M refs/lo/s1 refs/lo/s2 | tr '\n\t' '; ')"
echo "rebase-aware: $(w diff --name-status -M "$ST" refs/lo/s2 | tr '\n\t' '; ')"

section "8. --stdin: several replays in one process ('<base> -- <branch1> <branch2>' per line)"
printf '%s -- %s %s\n' "$SOB" "$SNB" refs/lo/s1 "$QOB" "$QNB" refs/lo/q1 > "$ROOT/pairs"
git -C "$ROOT/cold2.git" fetch -q origin "$S1:refs/lo/s1" "$S2:refs/lo/s2"
git -C "$ROOT/cold2.git" merge-tree --stdin < "$ROOT/pairs" | tr '\0' '|'; echo "exit=${PIPESTATUS[0]}  (each record: <1 clean|0 conflicted>|<tree>|...)"

echo; echo "done; remove $ROOT when finished"
```

Output (git 2.54.0, macOS; the temp path shortened to `$TMPDIR`):

```
work dir: $TMPDIR/lo-mergetree.eUgy28
git version 2.54.0 (Apple Git-157)
r1=522bacb6ad367d74bfd7ea9730b6b044b8a3ac6a
r2=be3bfe207aa3b0013936541099836bbcab2b08e1
missing blobs after clone: 8
old_base=58ad127 M1: initial  new_base=459228d M3: add e.txt

==== 1. plain comparison r1..r2 (what a naive revision diff shows)
 src/b.txt | 2 +-
 src/c.txt | 2 +-
 src/e.txt | 5 +++++
 3 files changed, 7 insertions(+), 2 deletions(-)

==== 2. rebase-aware: replay r1's changes onto r2's base, then diff against r2
$ git merge-tree --write-tree --merge-base=$OLD_BASE $NEW_BASE $OLD_HEAD
exit=0 tree=6faca596f3600d6c76a28651cc638ec0834e0248
$ git diff --stat $REPLAY $NEW_HEAD
 src/c.txt | 2 +-
 1 file changed, 1 insertion(+), 1 deletion(-)
$ git diff $REPLAY $NEW_HEAD
diff --git a/src/c.txt b/src/c.txt
index 587dca1..109e90b 100644
--- a/src/c.txt
+++ b/src/c.txt
@@ -12,7 +12,7 @@ c line 11
 c line 12
 c line 13
 c line 14
-c line 15
+c line 15 (the extra change in r2)
 c line 16
 c line 17
 c line 18
file list (what Lo shows): M src/c.txt;
missing blobs now: 3

==== 3. conflict: main changes the line the PR changed, and the author resolves it in r3
--- default output
f74d6ab317132cd8ec8c2165016f89a7d9e03caa
100644 ea16d4491cdde1c742d4d3d76f4b243c2f9b7b49 1	src/a.txt
100644 cc32dca581838e287fdd5b8f7c1546ab21b247cf 2	src/a.txt
100644 1b91ef8b05302d8d0635edbb467bc6d31a8abe5e 3	src/a.txt

Auto-merging src/a.txt
CONFLICT (content): Merge conflict in src/a.txt
exit=1
--- --name-only --no-messages (one path per line)
f74d6ab317132cd8ec8c2165016f89a7d9e03caa
src/a.txt
exit=1
--- -z --name-only (NULs shown as |)
f74d6ab317132cd8ec8c2165016f89a7d9e03caa|src/a.txt||1|src/a.txt|Auto-merging|Auto-merging src/a.txt
|1|src/a.txt|CONFLICT (contents)|CONFLICT (content): Merge conflict in src/a.txt
|
exit=1
--- --quiet
exit=1
--- fallback (plain r2..r3), which is what Lo shows with a note
 src/a.txt | 2 +-
 1 file changed, 1 insertion(+), 1 deletion(-)

==== 4. stacked layer: the layer below is force-pushed, the upper layer only rebases
merge-base(U1, lower now) = e6ca360 M4: change a.txt line 3   <- wrong: L1 is gone from lower
--- plain U1..U2:
 src/lower.txt | 1 +
 1 file changed, 1 insertion(+)
--- replay with the naive base:
exit=1
 src/lower.txt | 3 ---
 1 file changed, 3 deletions(-)
--- replay with the old base tip (from base_ref_force_pushed.beforeCommit or Lo's pinned ref):
exit=0 tree=dc6e8bfa9614aab1e7f1b99159f4925421753a99
diff --stat: [] (empty: the author changed nothing)

==== 5. lazy blob fetching in the bare partial clone
missing blobs before: 12
merge-tree lazy fetches: 0
diff --name-status lazy fetches: 0
diff (content) lazy fetches: 1
missing blobs after: 10
--- with lazy fetching off (git --no-lazy-fetch, git >= 2.45) in a fresh clone:
6faca596f3600d6c76a28651cc638ec0834e0248
exit=0

==== 6. a replay that needs real content merges: are blob fetches batched?
missing blobs before: 20
merge-tree: 3 three-way content merges, lazy fetch processes: 1
missing blobs after: 8
diff vs r2 (empty = only rebase noise): []
warning: lazy fetching disabled; some objects may not be available
fatal: could not fetch 587dca11e829709a8c14895cbb1bf79d364847a1 from promisor remote
exit with --no-lazy-fetch: 128

==== 7. renames: PR 4 renames b.txt and edits it; main edits b.txt elsewhere
exit=0, lazy fetch processes: 2
plain s1..s2: M src/bee.txt;A src/extra.txt;
rebase-aware: A src/extra.txt;

==== 8. --stdin: several replays in one process ('<base> -- <branch1> <branch2>' per line)
1|06f925a11fc680083fa7f340e6db642a30922b4c||1|d377351d166606fee7f8acd40d926fc88eedce9b||exit=0  (each record: <1 clean|0 conflicted>|<tree>|...)

done; remove $TMPDIR/lo-mergetree.eUgy28 when finished
```

## Appendix B: `recon.py`

Read-only. Run as `python3 -I recon.py <owner/repo> <number>`; it calls `gh api`. It rebuilds a PR's
revisions from the timeline (T), adds check-suite push chains (T+), and checks both against the
activity API.

```python
#!/usr/bin/env python3
"""Reconstruct a PR's push boundaries (revisions) from GitHub's API, read-only.

Compares three sources:
  T  = GraphQL timeline only (force-push events, current commits, reviews)
  T+ = T plus check-suite `push` chains for every known head SHA
  A  = REST repository activity API on the head branch (ground truth when available)

usage: recon.py owner/repo number
"""
import json
import subprocess
import sys

def gh(args, stdin=None):
    out = subprocess.run(["gh", "api", *args], input=stdin, capture_output=True, text=True)
    if out.returncode != 0:
        raise RuntimeError(out.stderr.strip())
    return out.stdout

def gql(query, **vars):
    body = json.dumps({"query": query, "variables": vars})
    r = json.loads(gh(["graphql", "--input", "-"], stdin=body))
    if r.get("errors"):
        raise RuntimeError(r["errors"])
    return r["data"]

TIMELINE = """
query($o:String!,$r:String!,$n:Int!,$after:String) { repository(owner:$o,name:$r){ pullRequest(number:$n){
  createdAt closedAt mergedAt headRefName headRefOid baseRefName isCrossRepository
  headRepository { nameWithOwner }
  timelineItems(first:100, after:$after, itemTypes:[PULL_REQUEST_COMMIT, HEAD_REF_FORCE_PUSHED_EVENT,
      BASE_REF_FORCE_PUSHED_EVENT, PULL_REQUEST_REVIEW, BASE_REF_CHANGED_EVENT, AUTOMATIC_BASE_CHANGE_SUCCEEDED_EVENT]) {
    pageInfo { hasNextPage endCursor }
    nodes { __typename
      ... on PullRequestCommit { commit { oid committedDate
            checkSuites(first:20){ nodes { createdAt push { previousSha nextSha } } } } }
      ... on HeadRefForcePushedEvent { createdAt beforeCommit{oid} afterCommit{oid} }
      ... on BaseRefForcePushedEvent { createdAt beforeCommit{oid} afterCommit{oid} }
      ... on PullRequestReview { submittedAt commit{oid} }
      ... on BaseRefChangedEvent { createdAt previousRefName currentRefName }
      ... on AutomaticBaseChangeSucceededEvent { createdAt oldBase newBase }
    } } } } rateLimit { cost remaining } }
"""

def timeline(owner, repo, n):
    items, after, pr, cost = [], None, None, 0
    while True:
        d = gql(TIMELINE, o=owner, r=repo, n=n, after=after)
        cost += d["rateLimit"]["cost"]
        pr = d["repository"]["pullRequest"]
        items += pr["timelineItems"]["nodes"]
        pi = pr["timelineItems"]["pageInfo"]
        if not pi["hasNextPage"]:
            return pr, items, cost
        after = pi["endCursor"]

def suites_for(owner, repo, oids):
    """check-suite pushes for arbitrary SHAs (force-pushed-away ones included), batched."""
    res = {}
    oids = list(oids)
    for i in range(0, len(oids), 40):
        chunk = oids[i:i + 40]
        fields = " ".join(
            f'c{j}: object(oid:"{o}") {{ ... on Commit {{ oid checkSuites(first:20) {{ nodes {{ createdAt push {{ previousSha nextSha }} }} }} }} }}'
            for j, o in enumerate(chunk))
        d = gql(f'query($o:String!,$r:String!) {{ repository(owner:$o,name:$r) {{ {fields} }} }}', o=owner, r=repo)
        for j, o in enumerate(chunk):
            obj = d["repository"].get(f"c{j}")
            res[o] = (obj or {}).get("checkSuites", {}).get("nodes", []) if obj else None
    return res

def activity(full, ref):
    try:
        raw = gh(["--paginate", f"repos/{full}/activity?ref=refs/heads/{ref}&per_page=100&direction=asc"])
    except RuntimeError as e:
        return None, str(e)
    entries = []
    dec = json.JSONDecoder()
    raw = raw.strip()
    i = 0
    while i < len(raw):
        page, i = dec.raw_decode(raw, i)
        entries += page
        while i < len(raw) and raw[i].isspace():
            i += 1
    return entries, None

def main():
    full, n = sys.argv[1], int(sys.argv[2])
    owner, repo = full.split("/")
    pr, items, cost = timeline(owner, repo, n)
    head_repo = (pr["headRepository"] or {}).get("nameWithOwner")
    print(f"# {full}#{n}  head={head_repo}:{pr['headRefName']}  base={pr['baseRefName']}  "
          f"created={pr['createdAt']} closed={pr['closedAt']}  timeline cost={cost}")

    # --- T: timeline only ---------------------------------------------------------------
    heads = {}  # sha -> (time or None, source)
    def add(sha, t, src):
        if not sha:
            return
        if sha not in heads or (heads[sha][0] is None and t):
            heads[sha] = (t, src)
    fps, bfps, commits = [], [], []
    for it in items:
        k = it["__typename"]
        if k == "HeadRefForcePushedEvent":
            fps.append(it)
            add((it["beforeCommit"] or {}).get("oid"), None, "fp.before")
            add((it["afterCommit"] or {}).get("oid"), it["createdAt"], "fp.after")
        elif k == "BaseRefForcePushedEvent":
            bfps.append(it)
        elif k == "PullRequestReview" and it["commit"]:
            add(it["commit"]["oid"], None, "review")
        elif k == "PullRequestCommit":
            c = it["commit"]
            commits.append(c)
            cs = c["checkSuites"]["nodes"]
            if cs:
                add(c["oid"], min(s["createdAt"] for s in cs), "suite")
    add(pr["headRefOid"], None, "head")
    T = set(heads)

    # --- T+: chase check-suite push.previousSha from every known head ---------------------
    tplus = dict(heads)
    frontier, seen = set(heads), set()
    rounds = 0
    while frontier and rounds < 6:
        rounds += 1
        got = suites_for(owner, repo, frontier - seen)
        seen |= frontier
        frontier = set()
        for sha, suites in got.items():
            for s in suites or []:
                p = s.get("push")
                if p and p["nextSha"] == sha:
                    if sha not in tplus or tplus[sha][0] is None:
                        tplus[sha] = (s["createdAt"], "suite.push")
                    prev = p["previousSha"]
                    if prev and set(prev) != {"0"} and prev not in tplus:
                        tplus[prev] = (None, "suite.prev")
                        frontier.add(prev)
    TP = set(tplus)

    # --- A: activity API -------------------------------------------------------------------
    acts, err = (activity(head_repo, pr["headRefName"]) if head_repo else (None, "head repo gone"))
    A = None
    if acts is not None:
        created = pr["createdAt"]
        end = pr["closedAt"] or "9999"
        pushes = [a for a in acts if a["activity_type"] != "branch_deletion"]
        before = [a for a in pushes if a["timestamp"] <= created]
        during = [a for a in pushes if created < a["timestamp"] <= end]
        A_list = ([before[-1]] if before else []) + during
        A = [a["after"] for a in A_list]
        print(f"activity: {len(acts)} entries on the branch, {len(A_list)} revisions while the PR was open "
              f"({sum(a['activity_type']=='push' for a in A_list)} push, "
              f"{sum(a['activity_type']=='force_push' for a in A_list)} force_push)")
        for i, a in enumerate(A_list, 1):
            marks = ("T" if a["after"] in T else "-") + ("+" if a["after"] in TP else "-")
            print(f"  r{i:<3} {a['timestamp']} {a['activity_type']:<15} {a['before'][:8]}->{a['after'][:8]}  [{marks}]")
        miss_T = [s for s in A if s not in T]
        miss_TP = [s for s in A if s not in TP]
        extra = [s for s in T if s not in set(A)]
        print(f"timeline-only finds {len(A) - len(miss_T)}/{len(A)}; with suite chains {len(A) - len(miss_TP)}/{len(A)}; "
              f"timeline heads not in activity: {len(extra)} {[s[:8] for s in extra]}")
    else:
        print(f"activity unavailable: {err[:120]}")
        print(f"timeline-only heads: {len(T)}; with suite chains: {len(TP)}")
    print(f"force-pushes={len(fps)} (null before={sum(1 for f in fps if not f['beforeCommit'])}) "
          f"base-force-pushes={len(bfps)} current commits={len(commits)} "
          f"commits with check suites={sum(1 for c in commits if c['checkSuites']['nodes'])}")

if __name__ == "__main__":
    main()
```
