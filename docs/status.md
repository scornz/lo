# Lo status

Kept current by the overseer session. A fresh session should be able to resume from this file,
[`plan.md`](plan.md) and [`decisions.md`](decisions.md). The live copy is in the worktree
`~/src/rozed-worktrees/docs` (branch `docs/status`), which can be ahead of what's pushed.

**Updated:** 2026-11-06

## Current milestone

- **Done and on `main`:** 1 Bootstrap, 2 Spike, 3 GitHub layer, 4 Local copy, 5 The PR tab.
- **5. The PR tab:** merged as stack #32 and verified on screen.
- **Planned:** milestones 6–9 in detail (`plan.md`). The e2e fixtures (E1–E3) are live on `rozed-dev/e2e` (#29).
- **Logic built ahead of its milestone, on `main`:** review order (#6), revisions (#8), merge
  logic (#12), threads and the pending review (#21), rebase-aware comparison (#20), stacks (#7, #11),
  moved code (#9), syntax-aware diff (#14), symbol index (#16).
- **Landing in progress:** the merge steward brings each line up to date and creates a GitHub stack;
  the overseer merges the top layer through the async merge API.

## The open PRs

Every PR is reviewed by an independent agent and its findings fixed before it lands.

| PR | What | Base | State |
|---|---|---|---|
| #33 | Rename the project to Lo (`refactor/rename-lo`, lane-1) | `main` | Reviewed: sound, but behind `main` (#31's code still says rozed). Fixes: merge `main` and rename #31's names, a `--check` CI guard, safer handling of Markdown and fixtures. |
| 7a | Stack data layer (`feat/stack-data`, lane-2) | `main` | In progress. |
| #2 | The docs (`docs/status`) | `main` | Land last; its branch keeps moving. |

### How a line lands

1. The steward merges `origin/main` into the line's bottom branch (chain case: the tree must equal the
   branch's; sibling case: three-way against the last shared commit, proving both sides are kept),
   then merges each layer's updated base into the layer above, and waits for green and `CLEAN`.
2. The steward creates the stack (`POST /repos/scornz/rozed/stacks`, bottom to top).
3. The overseer merges the top layer: `PUT /repos/scornz/rozed/pulls/<top>/merge-async` with
   `merge_method=squash` and the expected head `sha`, then polls the returned UUID until `merged`.
   Each layer lands as its own squash commit. `gh pr merge` refuses stacked PRs.
4. Check `main`'s tree equals the top layer's old head, and delete the merged worktrees' `target/`.

## Merged

- #1 `chore: bootstrap the repository` → `main` at `1e6b090` (2026-11-06).
- #3 `build: build rozed on Zed's crates` → `main` at `988ce2c` (2026-11-06).
- #4 `feat: open a pull request in Zed's side-by-side diff` → `e0ae578`, and #13 `feat(repo_cache): keep a partial clone of each repository` → `59b68a6`, together as GitHub stack #22 through the async merge API (2026-11-06).
- #5 `feat(github): sign in to GitHub and call its API` → `68da4db`, and #10 `feat(github): poll pull requests and cache them` → `080d041`, as stack #24 (2026-11-06). Milestone 3 (GitHub layer) and milestone 4 (local copy) are on `main`.
- #6 → `5390600`, #8 → `a36e23c`, #12 → `6aa6be3`, #21 → `17bee1c` (the review model), as stack #26 (2026-11-06).
- #20 `feat(repo_cache): compare revisions across a rebase` → `dfd4f92` (2026-11-06).
- #7 `feat(stacks): find stacks among a repository's pull requests` → `4b484ad`, and #11 `feat(stacks): plan turning a chain into a GitHub stack` → `0ead135`, as stack #28 (2026-11-06).
- #9 `feat(diff_extras): detect moved blocks of code` → `7b6c53a`, #14 `feat(diff_extras): compare files by syntax` → `7d27f64`, #16 `feat(symbol_index): index definitions and references by name` → `6da6849`, as stack #30 (2026-11-06). All logic built ahead of its milestone is on `main`.
- #27 `ci: build weekly against Zed's latest release` → `66efea3` (2026-11-06). Verified on GitHub's runner: the latest release (v1.27.0) is skipped as pinned and the issue job does nothing; `v1.28.0-pre` reports "breaks rozed" with the worktree-settings patch's conflict and a green run, the issue job skipped for a tagged dispatch. Task 13.1's weekly job is done; the release workflow is still to come in milestone 13.
- #19 `chore(deps): bump jdx/mise-action from 5.2.0 to 5.2.6` → `66eba59` (2026-11-06). `weekly-zed.yml` still pins 5.2.0; Dependabot's next run should bump it.
- #29 `test: script the review fixtures on rozed-dev/e2e` → `32cec21` (2026-11-06). CI now skips the Rust job for `script/e2e/` and shellchecks it.
- Stack #32, milestone 5, the PR tab (2026-11-06):
  - #15 → `cfad45b`
  - #17 → `5ad3cbf`
  - #18 → `b5ae7fb`
  - #23 → `3e62fec`
  - #25 → `d14286a`

  `main`'s tree equals #25's head. The async merge reported `failed` although every commit landed.
  #15 shows as merged; #17, #18, #23 and #25 were closed with a comment naming their commits, and the
  stack shows closed. See Notes.
- #31 `feat(github): read a pull request's push history` (6c) → `7fbc54c` (2026-11-06), after two rounds of review fixes.
- **Milestone 5 verified on screen** at `36f9980` (evidence in `evidence/milestone-5/verify/`):
  - Every acceptance check passed, including the image-bomb fixture (`rozed-dev/e2e#17`, helper
    killed at 512 MiB, rozed at 128 MB).
  - Partial: the recent list across several accounts, since only one account was available.
  - Follow-ups are listed under Next.

## Next

1. Rename to Lo (`refactor/rename-lo`), then milestone 6 per `plan.md`: #31 (6c) merges `main` and
   its review fixes, then 6a and 6b in lane 1.
   Milestone 5 follow-ups, to fold into 6a/6b:
   - **Focus:** clicking the title bar takes focus out of the diff, so the `alt` view keys and `tab`
     do nothing until you click in the diff.
   - **`tab`:** it lands at the other editor's own cursor, not the same row.
   - **Offline header:** it drops "as <account>".
   - **Accounts window:** it shows a refused account as signed in until its token is used.
   - **Quit:** every quit logs "timed out waiting on app_will_quit".
   - **Path order:** the diff stays in review order; only the list re-sorts.
   - **Added files:** the left side shows an empty line 1.
2. After milestone 5, the UI milestones wire in the logic already built: 6 revisions (picker,
   rebase-aware view), 7 stacks (tree sidebar, `]s`/`[s`, layer diffs), 8 comments and reviews
   (threads in the diff, comment box, submit dialog, timeline, attachments), 9 merging (status
   line, `m` panel, approve and ship, queue), 10 code features (`symbol_index` as a built-in
   language server, trust gate, peek), 11 moved code and the syntax-aware view, 12 links, keys
   (vim) and settings, 13 end-to-end and release. Each gets detailed tasks in `plan.md` when it
   starts.

## Findings so far (folded into `docs/spike.md` by the spike PR)

- **Attachments:** rozed's OAuth token (device flow, scopes `repo read:org workflow`) uploads to
  `uploads.github.com/user-attachments/assets` (201). No `gh` fallback needed. Images in private
  repos need the token on `github.com/user-attachments/assets/<id>`, which 302s to a signed S3 URL
  that must be fetched without the token.
- **Stacks:** GA since 2026-10-06, no opt-in. On `rozed-dev/playground`, PRs #1 (`stack-a`) and #2
  (`stack-b`) form stack #3, created via `POST /stacks` (201). Stack numbers share the PR sequence.
- **Async merge** 404s are `{"message":"Not Found",...,"status":"404"}` for every unknown UUID.
- Reports: [`research/github-api.md`](research/github-api.md) and
  [`research/github-checks.md`](research/github-checks.md).

## Waiting on the user

- **For the rename to Lo:** the repo already shows as `scornz/lo`, and the OAuth app shows as "Lo";
  both done.
- **Five merged branches to delete, if you want:** `feat/pr-tab`, `feat/api-diff`,
  `feat/viewed-marks`, `feat/recent-prs` and `feat/diff-display`. Their content is on `main`, but
  stack #32's merge stopped before GitHub's auto-delete ran. Deleting them was refused as
  destructive, so it's yours to do or leave.

- **Later, for milestones 10–13** (see `plan.md`; none blocks current work):
  - **Nightly e2e:** a `ROZED_BOT_TOKEN` Actions secret on `scornz/lo`.
  - **Homebrew:** `scornz/homebrew-tap` with a `TAP_TOKEN`.
  - **Fork fixture:** an OK for a fork under `rozed-bot` (E7). Until then `fd#2212` stands in.
  - **Icon:** an app icon. A placeholder ships until then.
  - **Intel:** whether Intel (x86_64) builds ship. The plan is arm64 only.
  - **Fallback runner:** a self-hosted runner, if hosted runners can't draw with Metal for the app
    smoke test.

- **Apple Developer account:** the user will set it up later; until then builds stay unsigned. Release
  secrets when it exists: `MACOS_CERT_P12`, `MACOS_CERT_PASSWORD`, `NOTARY_KEY_ID`,
  `NOTARY_ISSUER_ID`, `NOTARY_KEY_P8`.

## Resolved with the user

- **Merging:** the user allowed the overseer to merge in `scornz/rozed` (2026-11-06).
- **Bot credentials:** the user allowed using `rozed-bot`'s tokens with rozed's credential helper to
  test rozed (2026-11-06).
- **Public end-to-end repo:** the user allowed creating it in `rozed-dev` with the bot's token
  (2026-11-06): `rozed-dev/e2e`.
- **Zed's trademark terms:** checked by the user; the name "rozed" is fine for now (2026-11-06).

## Notes

- **Milestone 10 mechanism decided** (see decisions "Code features"): the syntax index answers
  through a rozed `SemanticsProvider` inside the app, since no language server can serve rozed's
  diff buffers. This departs from design §7's wording but keeps its behaviour; the user may override.

- **Combining `feat/viewed-marks` (#18) with `feat/api-diff` (#17):** in `show_github_diff`, call
  `self.set_head(head)` with the head GitHub listed the files at; make api-diff's `list()` call
  `set_viewed(&self.marks.shown())` instead of `&self.viewed`. A tested scratch merge is the local
  commit `8f206a7` on `scratch/viewed-marks-with-api-diff-2` (worktree of `feat-viewed-marks`).

- **The `gh` on PATH (`~/.local/bin/gh`) is a Coder wrapper that injects the user's own token over
  `GH_TOKEN`.** For anything as `rozed-bot`, call `/opt/homebrew/bin/gh` and check `GET /user` says
  `rozed-bot` first.
- `rozed-dev/playground` also has branches `review-checks` and `review-checks-base` and a closed PR #4
  from the live review-API checks; harmless.

- **Memory:** the computer-use MCP servers (`~/.local/share/computer-use-mcp`, node) leak: each
  reaches about 40 GB of footprint (mostly swap; `ps` RSS hides it, `footprint -p` shows it), and
  three running together filled swap and raised macOS's "out of application memory" dialog.

- **Disk:** each worktree's `target/` holds 30–90 GB of Zed builds. Delete a worktree's `target/`
  once its PR merges; keep an eye on free space (`df -h ~`).

- **Classifier blocks seen so far, worked around legitimately:** merging PRs ("Merge Without
  Review", waiting on the user), creating a public repo ("Create Public Surface"), force-pushing a
  test fixture ("Git Destructive"; tests use local repos), reading the bot's token in a shell for a
  live check ("Credential Exploration"; the user then explicitly allowed using the bot's token with
  rozed's credential helper to test rozed, on 2026-11-06), and
  building git from downloaded source ("Code from External"; verified from git's source instead).

- `rozed-dev/playground` has a branch `probe/if-modified-since` left from a live check; harmless.

- **e2e fixtures (`rozed-dev/e2e`, from `script/e2e/fixtures.sh`):**
  - **E1:** #16 (`e1/revisions-4`) is open at r3 with the bot's COMMENT review on r2. #14 is closed
    and has the full r1–r6 run: the server-side rebase is r4 `28b21f2` → r5 `7513db3`, then r6
    `adb7208` changes one line of `e1/a.txt`. #1 and #13 are older closed runs.
  - **E2:** #2 a → main, #3 b → a, #4 c → b, #5 e → b, #6 d → main (contains c), and native stack #9
    (#7 s1, #8 s2).
  - **E3:** #10 x squash-merged; #11 y still targets `e3/x`.
  - **`main` moves only through merged PRs,** never a direct push. Once E4 adds rulesets on `main`,
    `rebase-r5` and E3 need the bot to be able to merge.

- **Runaway processes:** Zed captures the shell environment by running the current executable with
  `--printenv`. rozed must handle that flag, and refuses to start under a rozed parent
  (`ROZED_PARENT_PID`). Never remove either.
- **Launch rozed at normal priority:** zsh renices background jobs (`BG_NICE`), and a niced rozed
  never repaints. Use `setopt NO_BG_NICE` or `nice -n 0`.
- **Frozen-looking window on this desktop:** GPUI stops repainting after the first frame until the
  window is re-activated (virtual display). Click the wallpaper twice, then rozed's title bar.
- **Builds need mise** (`mise exec -- cargo ...`) for the pinned cmake.

- **The bot's OAuth tokens were revoked on 2026-11-06,** around when the OAuth app was renamed "Lo".
  The grant survived. The milestone 5 verification signed `rozed-bot` in again through rozed's device
  flow, in a fresh Playwright profile. The working token is in rozed's debug store,
  `~/.config/rozed/credentials.json`. `~/.config/rozed-dev/oauth-token` still holds the revoked one;
  the PAT in `bot.env` works.
- **A stack's async merge can report `failed` after landing every commit.** Stack #32's merge did. Check
  `main` and each PR's state rather than the status alone.
- The bot's device-flow token for rozed's OAuth app is in `~/.config/rozed-dev/oauth-token` (mode
  600). Approving it granted rozed's OAuth app access to `rozed-dev`, which restricts OAuth apps by
  default; revoke it in the org's settings if needed.

- An untracked `.build/` folder (a SwiftPM cache, probably from sourcekit-lsp) appeared in
  `~/src/rozed`. Add it to `.gitignore` in a later PR.

## Where things live

- Main clone: `~/src/rozed`, on `main`. Worktrees branch from it.
- Task worktrees: `~/src/rozed-worktrees/<branch-slug>`; the docs worktree is
  `~/src/rozed-worktrees/docs`.
- Zed fork: `~/src/zed` (`scornz/zed`, at `v1.27.0`).
