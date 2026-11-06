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
| `reset-e1` | Closes E1's PR and opens a fresh one, at r3, on the next branch (`e1/revisions-2`, `-3`, …). |

- **Credentials.** The token comes from `ROZED_BOT_TOKEN`. The script checks that it belongs to
  `rozed-bot` before doing anything. It runs `/opt/homebrew/bin/gh` (or `ROZED_E2E_GH`) with an
  empty config, and runs git in a scratch clone, deleted on exit, with none of your git config. Git
  pushes over HTTPS through a credential helper that reads the token from the environment.
- **What it writes,** only to `rozed-dev/e2e`: branches under `e1/`, `e2/` and `e3/`, their PRs,
  E1's review, E2's native stack, and squash merges of `e3/x` and `e1/move-main*`. It finds what
  exists by branch, PR and commit subject. It never pushes to `main`, force-pushes or deletes a
  branch, so `main` moves only through merged PRs.
- **Not yet:** E4 (merge states, rulesets, merge queue) and E5 (PRs the bot can approve).

## Fixture map

The numbers are from the first `setup` on 2026-11-06. `reset-e1` gives E1 a new number, so tests
read `status --json`.

| Fixture | PR | Branch → base | Checks |
|---|---|---|---|
| **E1** revisions | #13 | `e1/revisions-2` → `main` | Milestone 6. **Picker:** r1–r3, with "your last review" on r2 (rozed-bot's COMMENT review), opening r2→r3. **Markers:** reopening shows "Nothing new since r3"; after `push-r4`, it opens r3→r4 ("last seen here"). **Changed since viewed:** `a.txt` viewed at r3 shows r3→r4 with the note. **Rebase:** after `rebase-r5`, r4→r5 spans a rebase and shows only the author's changes, with the banner. |
| | #1 | `e1/revisions` → `main` | The first E1, closed by `reset-e1` after a full run, so it holds r1–r5 and a server-side rebase. |
| | #12 | `e1/move-main` → `main` | #1's `rebase-r5` moved `main` with this PR, merged. It touches only `e1/base.txt`, which E1 never changes, so the rebase can't conflict. |
| **E2** stack tree | #2 | `e2/a` → `main` | Milestone 7. **Tree:** the base-branch chain a, b, c, with children in the order they were opened. **`]s`/`[s`:** walks a, b, c, d, e depth-first. **Layer diffs:** a multi-layer `a..c` diff. |
| | #3 | `e2/b` → `e2/a` | Has two children: c, then e. |
| | #4 | `e2/c` → `e2/b` | |
| | #5 | `e2/e` → `e2/b` | A sibling of c, opened after it. |
| | #6 | `e2/d` → `main` | Built on c's head but targets `main`: only the shared-commits rule links it to c, and its layer diff shows only its own change. |
| | #7, #8 | `e2/s1` → `main`, `e2/s2` → `e2/s1` | A native GitHub stack (#9), separate from the chain. |
| **E3** needs rebase | #10 | `e3/x` → `main` | Two commits, squash-merged into `main`. Its branch is kept. |
| | #11 | `e3/y` → `e3/x` | One commit on x. It still targets `e3/x`: GitHub retargets only when the base branch is deleted. Shows "needs rebase", comparing against x's squash commit. |

### E1 revisions

Each revision is one push; `status --json` lists them under `e1.revisions`. r1 adds `e1/a.txt` and
`e1/b.txt` (12 lines each), r2 changes `a.txt`, r3 changes `b.txt`, and r4 changes both.

- **#13, now:** r1 `f8ba244`, r2 `f69d085` (reviewed), r3 `26b329f`.
- **#1, after `push-r4` and `rebase-r5`:** r1 `91f269e`, r2 `344f0ef` (reviewed), r3 `cd66165`,
  r4 `b8b515a`, r5 `f927e87`. `main` moved to `99a9e80` (#12). GitHub's rebase shows as a
  `HeadRefForcePushedEvent` by rozed-bot, `b8b515a` → `f927e87`, and as a `force_push` in the
  branch's activity. r5's four commits sit on `99a9e80` and change only `e1/a.txt` and `e1/b.txt`.
