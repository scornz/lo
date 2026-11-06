# e2e fixtures

`fixtures.sh` sets up the pull requests in [`rozed-dev/e2e`](https://github.com/rozed-dev/e2e) that
rozed's revisions (milestone 6) and stacks (milestone 7) are checked against. It runs as
`rozed-bot` and builds nothing.

```sh
(set -a; . ~/.config/rozed-dev/bot.env; set +a; script/e2e/fixtures.sh setup)
```

| Command | What it does |
|---|---|
| `setup` | Creates any fixture that's missing. Run twice, the second run changes nothing. |
| `status [--json]` | Prints each fixture's PR numbers, branches and heads. Tests read `--json`. |
| `push-r4` | Pushes E1's r4, a normal push touching `e1/a.txt` and `e1/b.txt`. |
| `rebase-r5` | Moves `main` one commit, then rebases E1's PR on GitHub (`updatePullRequestBranch`, `REBASE`). |
| `push-r6` | Pushes E1's r6 after the rebase, a normal push changing line 3 of `e1/a.txt`. |
| `reset-e1` | Closes E1's PR and opens a fresh one, at r3, on the next branch (`e1/revisions-2`, `-3`, …). |

- **Credentials.** The token comes from `ROZED_BOT_TOKEN`, and the script checks that it belongs to
  `rozed-bot` before doing anything.
  - It runs `/opt/homebrew/bin/gh` (or `ROZED_E2E_GH`) with an empty config, and gives gh the token
    on each call.
  - Git runs in a scratch clone, deleted on exit, without your git config or `GIT_*` variables.
    It pushes over HTTPS through a credential helper that reads the token from the environment.
  - It refuses to run under `bash -x`, which would print the token.
- **What it writes,** only to `rozed-dev/e2e`:
  - branches under `e1/`, `e2/` and `e3/`, and their PRs;
  - E1's review;
  - E2's native stack;
  - squash merges of `e3/x` and `e1/move-main*`.
- **What it never does:** push to `main`, force-push or delete a branch. `main` moves only through
  PRs it opened, which it checks by author, base and title before merging.
- **Finding what exists.** It finds fixtures by branch, PR and commit subject, and counts only
  rozed-bot's PRs from `rozed-dev/e2e` itself, never a fork's. A failed GitHub request stops it
  before it writes anything.
- **If `reset-e1` fails partway,** finish it with `setup`. Running `reset-e1` again starts yet
  another E1.
- **E2 and E3 have no reset.** `setup` accepts their PRs in any state, so once a later test merges
  or closes them, they're not rebuilt.
- **Not yet:** E4 (merge states, rulesets, merge queue) and E5 (PRs the bot can approve).
  - E4's protected bases will be separate branches: `gated` (required checks, an approval and
    CODEOWNERS) and `queued` (the merge queue).
  - `main` stays unprotected, so `rebase-r5` and E3's merge keep working.

## Fixture map

The numbers are from 2026-11-06. `reset-e1` gives E1 a new number, so tests read `status --json`.

| Fixture | PR | Branch → base | Checks |
|---|---|---|---|
| **E1** revisions | #16 | `e1/revisions-4` → `main` | Milestone 6. **Picker:** r1–r3, with "your last review" on r2 (rozed-bot's COMMENT review), opening r2→r3. **Markers:** reopening shows "Nothing new since r3"; after `push-r4`, it opens r3→r4 ("last seen here"). **Changed since viewed:** `a.txt` viewed at r3 shows r3→r4 with the note. **Rebase:** after `rebase-r5` and `push-r6`, r4→r6 spans the rebase with the banner and shows only r6's one hunk in `e1/a.txt`. The plain r4..r6 diff also has `e1/base.txt`, and r4→r5 alone shows no files. |
| | #14 | `e1/revisions-3` → `main` | Closed by `reset-e1` after a full run: r1–r6, with the server-side rebase between r4 and r5. |
| | #15 | `e1/move-main-3` → `main` | #14's `rebase-r5` moved `main` with this PR, merged. It touches only `e1/base.txt`, which E1 never changes, so the rebase can't conflict. |
| | #1, #13 | `e1/revisions`, `e1/revisions-2` → `main` | Earlier E1s, closed. #1 ran to r5, with `main` moving twice under it (#12, see below). |
| **E2** stack tree | #2 | `e2/a` → `main` | Milestone 7. **Tree:** the base-branch chain a, b, c, with children in the order they were opened. **`]s`/`[s`:** walks the chain depth-first. **Layer diffs:** a multi-layer `a..c` diff. |
| | #3 | `e2/b` → `e2/a` | Has two children: c, then e. |
| | #4 | `e2/c` → `e2/b` | |
| | #5 | `e2/e` → `e2/b` | A sibling of c, opened after it. |
| | #6 | `e2/d` → `main` | Built on c's head (and so containing a's and b's) but targeting `main`. Only the shared-commits rule, nearest parent first, links it to c, and its layer diff shows only its own change. |
| | #7, #8 | `e2/s1` → `main`, `e2/s2` → `e2/s1` | A native GitHub stack (#9), separate from the chain. s2 → s1 also matches the base-branch rule, so the tree looks the same if native-stack detection breaks. Only the rule tooltip tells them apart. |
| **E3** needs rebase | #10 | `e3/x` → `main` | Two commits, head `e86aa48`, squash-merged into `main` as `04bdf22`. Its branch is kept. |
| | #11 | `e3/y` → `e3/x` | One commit on x. It still targets `e3/x`, because GitHub retargets only when the base branch is deleted. `setup` fails if that changes, so "Automatically delete head branches" must stay off. It shows "needs rebase" and compares against x's head, `e86aa48`, not the squash commit, so only y's change shows. |

### E1 revisions

Each revision is one push; `status --json` lists them under `e1.revisions`. r1 adds `e1/a.txt` and
`e1/b.txt` (12 lines each), r2 changes `a.txt`, r3 changes `b.txt`, r4 changes both, r5 is GitHub's
rebase and r6 changes line 3 of `a.txt`.

- **#16, now:** r1 `60005cf`, r2 `b88cc8b` (reviewed), r3 `34427a8`, on `main` at `a45754b`.
- **#14, the full run:**
  - **Pushes:** r1 `857b788`, r2 `2e2d4be` (reviewed), r3 `864ede1` and r4 `28b21f2`, on
    `99a9e80`.
  - **The rebase:** #15 moved `main` to `a45754b`, and GitHub rebased r4 into r5 `7513db3`. It shows
    as a `HeadRefForcePushedEvent` by rozed-bot, `28b21f2` → `7513db3`, and as a `force_push` in the
    branch's activity.
  - **r6:** `adb7208`. r5..r6 is the one hunk in `e1/a.txt`. The plain r4..r6 diff has that hunk
    and `e1/base.txt`.
- **#1, the first run:**
  - **Revisions:** r1 `91f269e`, r2 `344f0ef` (reviewed), r3 `cd66165`, r4 `b8b515a` and r5
    `f927e87`. r1–r3 fork from `03fa28b`.
  - **`main` moved twice under it:** E3's squash merge (`04bdf22`) landed before r4, and #12
    (`99a9e80`) before r5.
  - **Why that matters:** a revision's base is its merge base with `main`, which is `03fa28b` for
    r1–r4, not `main`'s tip when it was pushed. With the tip, r4 would show `e3/x.txt`.
  - `setup` now builds E3 before E1, so a new E1's base moves only under `rebase-r5`.
