# GitHub review API checks

Run on 2026-11-06 against `rozed-dev/playground` as `rozed-bot` (classic token), to check the review
model in scornz/rozed#21. Every call went through GraphQL (`POST /graphql`) or REST as noted. Tokens
are elided. Responses are trimmed to the fields that matter.

PR #21 has no "Still to check against a live PR" section (body last updated 2026-11-06T15:51:03Z, no
comments), so the spec choices below are those in its "What GitHub allows" section and in the doc
comments of `crates/review/src/pending.rs` and `threads.rs` on `feat/threads-logic` (`334643c`).

## Setup

- `review.txt` holds `line 1` … `line 60`. Commit **c1** `cc1ebf5` adds it on top of `main`
  (`6ce218e`). All commits were made through the REST git data API (blob, tree, commit, ref).
- **Deviation:** the PR targets a new branch, `review-checks-base` (at c1), not `main`. With `main` as
  the base, `review.txt` would be a new file and the whole of it one hunk, so checks 5 and 6 would
  test nothing. Pushing `review.txt` to `main` instead would have put `stack-a` behind `main`.
- Branch `review-checks`:
  - **c2** `fc0f4b4`: lines 10, 11, 30 and 50 changed.
  - **c3** `2104a3c`: line 10 changed again (check 1).
  - **c4** `b405bfc`: line 55 changed (extra for check 4).
- PR **#4** "Review API checks", `review-checks` → `review-checks-base`, node `PR_kwDOVk6v9M8AAAABNWhMpQ`.
  Its diff at c2 had three hunks:

  ```
  @@ -7,8 +7,8 @@    lines 7–14   (10, 11 changed)
  @@ -27,7 +27,7 @@  lines 27–33  (30 changed)
  @@ -47,7 +47,7 @@  lines 47–53  (50 changed)
  ```

- Mutations used (selection sets trimmed here):
  - `addPullRequestReview(input: AddPullRequestReviewInput!) { pullRequestReview { id state body commit{oid} comments{…} } }`
  - `addPullRequestReviewThread(input: AddPullRequestReviewThreadInput!) { thread { line originalLine startLine originalStartLine diffSide startDiffSide isOutdated comments{ commit{oid} originalCommit{oid} line originalLine pullRequestReview{state commit{oid}} } } }`
  - `addPullRequestReviewThreadReply(input: …) { comment { state commit{oid} originalCommit{oid} pullRequestReview{id state} } }`
  - `submitPullRequestReview(input: …) { pullRequestReview { state commit{oid} comments{…} } }`
- GraphQL errors came back as HTTP 200 with `data.<mutation>: null` and `errors[].type: "UNPROCESSABLE"`.

## 1. Pending review across a push

**Sent**

1. `addPullRequestReview {pullRequestId: PR, commitOID: c2}`, no `event`, no threads.
2. `addPullRequestReviewThread {pullRequestReviewId: R1, path: "review.txt", line: 10, side: RIGHT}` (P1).
3. Push c3 (line 10 changed again), wait for `headRefOid` = c3, query P1.
4. After the push, add `addPullRequestReviewThread` on R1 at line 50 RIGHT (P2, unchanged between
   c2 and c3) and line 10 RIGHT (P3, changed between c2 and c3).
5. `submitPullRequestReview {pullRequestReviewId: R1, event: COMMENT, body: "…"}`.

**Response**

```
R1        state PENDING, commit c2
P1 before push   line 10, originalLine 10, isOutdated false, comment commit c2, originalCommit c2
P1 after push    line null, startLine null, originalLine 10, isOutdated true,
                 comment commit c2, originalCommit c2
P2 (after push)  line 50, isOutdated false, comment commit c3, originalCommit c3, review commit c2
P3 (after push)  line 10, isOutdated false, comment commit c3, originalCommit c3, review commit c2
R1 after push    state PENDING, commit c2 (unchanged)
submit           state COMMENTED, commit c2, 5 comments: P1 on c2, P2 and P3 on c3,
                 plus the reply from 3b and the thread from 8's extra
```

**Extra: a review started on an older revision.** With head at c3:

- `addPullRequestReview {commitOID: c2, event: COMMENT, threads: [line 10 RIGHT]}` posted, with the
  thread on c2 (commit c2, originalCommit c2).
- `addPullRequestReview {commitOID: c2, threads: [line 10 RIGHT]}`, no event, started R3 with its
  thread on c2. Then `addPullRequestReviewThread {pullRequestReviewId: R3, line: 11}` put that thread
  on **c3** (originalCommit c3), while R3's `commit` stayed c2.
- Submitting R3 with `body: ""` showed the review on c2.

**Conclusion:** `addPullRequestReviewThread` puts a new thread on the pull request's current head,
not on the pending review's commit. Only the threads passed to `addPullRequestReview` use its
`commitOID`. A review shows the `commitOID` it was started with, even when it holds threads on a
later head.

## 2. COMMENT review without a body

**Sent:** `addPullRequestReview {pullRequestId: PR, commitOID: c2, event: COMMENT, threads: [{path: "review.txt", line: 30, side: RIGHT, body: "…"}]}`, no `body`.

**Response:** `pullRequestReview {state: COMMENTED, body: "", commit: c2, comments.totalCount: 1}`.

**Extras:**

| Sent | Response |
| --- | --- |
| `addPullRequestReview {event: COMMENT}`, no body, no threads | `errors: [{type: UNPROCESSABLE, message: ""}]` |
| Same with `body: ""` | `errors: [{type: UNPROCESSABLE, message: ""}]` |
| REST `POST /pulls/4/reviews {commit_id: c2, event: COMMENT}` | `422 {"message": "Unprocessable Entity", "errors": [""]}` |
| REST `POST /pulls/4/reviews {event: REQUEST_CHANGES}`, no body, as author | `422 errors: ["Review Can not request changes on your own pull request"]`: the author check wins |
| `submitPullRequestReview {event: COMMENT, body: ""}` on R3, which held 2 comments | `COMMENTED`, body `""` |

**Conclusion:** a COMMENT review with threads posts without a body. A COMMENT review with neither
body nor threads is refused, and GitHub's error message is an empty string.

## 3. Reply without a review id

**3a. No pending review.** `addPullRequestReviewThreadReply {pullRequestReviewThreadId: <thread on line 30>, body: "…"}`, no `pullRequestReviewId`.

```
comment.state SUBMITTED
pullRequestReview {databaseId 5803319208, state COMMENTED, commit c2}   (a new review of its own, empty body)
```

**3b. R1 pending.** Same call on the same thread.

```
comment.state PENDING
pullRequestReview {id R1, state PENDING}   (joined R1; no error)
```

It was posted when R1 was submitted.

**Conclusion:** with no pending review, a reply without a review id posts at once as its own
COMMENTED review. With a pending review open, GitHub quietly adds it to that review instead of
posting it or refusing it.

## 4. Outdated threads

**Response (P1 after c3):** `line: null`, `startLine: null`, `originalLine: 10`,
`originalStartLine: null`, `isOutdated: true`. The first comment's `commit` stayed **c2**: for an
outdated thread it equals `originalCommit`.

**Control (thread on line 30, unchanged by c3):** `line: 30`, `isOutdated: false`. The comment's
`commit` moved to c3, then to c4, while `originalCommit` stayed c2.

**Extra: threads written on an older commit.** The two threads written on c2 line 10 after c3 was
pushed (see check 1's extra):

| When | line | isOutdated | comment commit |
| --- | --- | --- | --- |
| After c3 was pushed, re-queried a minute later | 10 (c2's line 10, not c3's) | false | c2 |
| After c4 (line 55 only) was pushed | null | true | c2 |

**`startLine` quirk.** For single-line threads, the thread's `startLine` is sometimes equal to `line`
(threads whose comment `commit` is the head: lines 30, 13, 11, 50, 10) and sometimes `null` (the
threads on c2, and outdated ones). `startDiffSide`, `originalStartLine` and the comment's
`startLine` are null for every single-line thread.

**Conclusion:** an outdated thread has `line` null and keeps `originalLine`. More precisely, a
thread's `line` is on its first comment's `commit`. That is the latest head once GitHub has
recomputed it, but a thread written on an older commit keeps that commit's line and is not outdated
until the next push.

## 5. Lines outside hunks

| Sent | Response |
| --- | --- |
| `addPullRequestReviewThread {pullRequestReviewId: R1, line: 25, side: RIGHT}` | `{"data": {"addPullRequestReviewThread": {"thread": null}}}`: **no error**, nothing added |
| Same, `line: 40` | same: `thread: null`, no error |
| REST `POST /pulls/4/comments {commit_id: c2, path, line: 25, side: RIGHT}` | `422 {"message": "Validation Failed", "errors": [{"resource": "PullRequestReviewComment", "code": "custom", "field": "pull_request_review_thread.line", "message": "could not be resolved"}]}` |
| Same, `line: 40` | same 422 |
| `addPullRequestReview {commitOID: c3, event: COMMENT, threads: [line 25 RIGHT]}` | `errors: [{type: UNPROCESSABLE, message: "Line could not be resolved"}]` |
| Control: REST, `line: 13` (unchanged context line inside hunk 1) | `201 Created`, `diff_hunk` ends at `line 13` |

**Conclusion:** GitHub refuses lines outside the diff's hunks and accepts unchanged context lines
inside them. `addPullRequestReviewThread` refuses silently, with `thread: null` and no error.

## 6. Multi-line ranges

Sent as `addPullRequestReviewThread` on a fresh pending review R2 started on c3 (the head):

| Range sent | Response |
| --- | --- |
| `startLine 10 RIGHT, line 11 RIGHT` (within hunk 1) | accepted: `startLine 10, line 11, startDiffSide RIGHT, diffSide RIGHT` |
| `startLine 10 RIGHT, line 30 RIGHT` (across hunks 1 and 2) | **accepted**: `startLine 10, line 30, originalStartLine 10, originalLine 30` |
| `startLine 10 LEFT, line 10 RIGHT` (removed → added) | accepted: `startDiffSide LEFT, diffSide RIGHT, startLine 10, line 10` |
| `startLine 10 LEFT, line 11 LEFT` (removed lines only) | accepted: `startDiffSide LEFT, diffSide LEFT` |
| `startLine 25 RIGHT, line 30 RIGHT` (starts outside any hunk) | `thread: null`, no error |

The across-hunk range also went through the other two paths:

| Sent | Response |
| --- | --- |
| `addPullRequestReview {commitOID: c3, event: COMMENT, threads: [startLine 10, line 30, RIGHT]}` | accepted, COMMENTED |
| REST `POST /pulls/4/comments {commit_id: c3, start_line: 10, start_side: RIGHT, line: 30, side: RIGHT}` | `201`, `start_line 10, line 30`, `diff_hunk` holds only hunk 2 (`@@ -27,7 +27,7 @@ … +line 30 changed`) |

All of these stayed `isOutdated: false` through c4.

**Conclusion:** a range's two ends must each be on a line of the diff, but they don't have to share a
hunk. A range from a removed line to an added line is accepted and reported with
`startDiffSide: LEFT, diffSide: RIGHT`.

## 7. Author events

All sent with no pending review open:

| Sent | Response |
| --- | --- |
| `addPullRequestReview {commitOID: c2, event: APPROVE, body: "…"}` | `errors: [{type: "UNPROCESSABLE", path: ["addPullRequestReview"], message: "Review Can not approve your own pull request"}]` |
| `addPullRequestReview {commitOID: c2, event: REQUEST_CHANGES, body: "…"}` | `errors: [{type: "UNPROCESSABLE", message: "Review Can not request changes on your own pull request"}]` |
| REST `POST /pulls/4/reviews {event: APPROVE}` | `422 {"message": "Unprocessable Entity", "errors": ["Review Can not approve your own pull request"]}` |
| REST `POST /pulls/4/reviews {event: REQUEST_CHANGES}` | `422 {"message": "Unprocessable Entity", "errors": ["Review Can not request changes on your own pull request"]}` |

No pending review was left behind.

**Conclusion:** GitHub refuses an author's APPROVE and REQUEST_CHANGES, with the messages above.

## 8. One pending review per user

All sent with R1 pending:

| Sent | Response |
| --- | --- |
| `addPullRequestReview {commitOID: c2}`, no event | `errors: [{type: UNPROCESSABLE, message: "User can only have one pending review per pull request"}]` |
| `addPullRequestReview {commitOID: c2, event: COMMENT, threads: [line 50]}` (rozed's `C`) | same message |
| REST `POST /pulls/4/comments {line: 50, …}` | `422 {"message": "Validation Failed", "errors": [{"resource": "PullRequestReview", "code": "custom", "field": "user_id", "message": "user_id can only have one pending review per pull request"}]}` |
| REST `POST /pulls/4/reviews {event: COMMENT, body}` | `422 {"errors": ["User can only have one pending review per pull request"]}` |
| Extra: `addPullRequestReviewThread {pullRequestId: PR, line: 11}`, no review id | accepted, joined R1 (`pullRequestReview.state PENDING`, id R1) |

**Conclusion:** GitHub refuses a second review, including one posted at once, while one is pending.
A thread or reply sent without a review id joins the pending review.

## Cleanup

- PR #4 closed, not merged.
- Branches `review-checks` (at c4) and `review-checks-base` (at c1) left in place.
- PRs #1 and #2, `stack-a` and `stack-b` were not touched.
- Raw log: one JSON line per call, with request, status and full response, in
  `/private/tmp/claude-502/-Users-ash-mac-src-ash-mac--claude-worktrees-bridge-cse-01EVVyxT8EgHvzs43QBHC4x2/a45330b3-b34c-5629-895e-e953fca4bd5e/scratchpad/review-api-checks/out/log.jsonl`
  (scratch, not kept).
