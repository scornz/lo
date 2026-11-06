# rozed status

Kept current by the overseer session. A fresh session should be able to resume from this file,
[`plan.md`](plan.md) and [`decisions.md`](decisions.md). The live copy is in the worktree
`~/src/rozed-worktrees/docs` (branch `docs/status`), which can be ahead of what's pushed.

**Updated:** 2026-11-06

## Current milestone

- **Done and on `main`:** 1 Bootstrap, 2 Spike, 3 GitHub layer, 4 Local copy.
- **5. The PR tab:** all five PRs written and reviewed. #15 (5a), #17 (5b) and #18 (5c) are fixed
  and green, and are being brought up to date as the bottom of one stack. #23 (5e) is in its second
  round of fixes, and #25 (5d) is in fixes after review.
- **Planned:** milestones 6–9 in detail (`plan.md`). The e2e fixtures script (E1–E3) is in progress.
- **Logic built ahead of its milestone, on `main`:** review order (#6), revisions (#8), merge
  logic (#12), threads and the pending review (#21), rebase-aware comparison (#20), stacks (#7, #11),
  moved code (#9), syntax-aware diff (#14), symbol index (#16).
- **Landing in progress:** the merge steward brings each line up to date and creates a GitHub stack;
  the overseer merges the top layer through the async merge API.

## The open PRs

Every PR is reviewed by an independent agent and its findings fixed before it lands.

| PR | What | Base | State |
|---|---|---|---|
| #15 | PR tab (5a) | `integration/pr-tab` | Ready; lands once its base's branches are on `main`. |
| #17 | GitHub's diff first (5b) | `feat/pr-tab` | Ready. |
| #18 | Viewed marks (5c) | `feat/pr-tab` | Ready; combine with #17 using the two edits in Notes. |
| #23 | Recent PRs, account picker, window title, offline diff (5e) | `feat/pr-tab` | All 11 findings fixed; the re-review found 8 more (an offline reload can drop the local diff, a sticky account, offline classification, picker focus). Round-two fixes in progress. Its migration becomes step 3 when it lands above #18. |
| #25 | Collapse, images, renames, view keys (5d) | `feat/api-diff` | Reviewed: 13 findings (SVG local-file reads, image bombs, `alt-w` unfolding, Unicode whitespace). Fixes in progress. |
| #19 | Dependabot: `jdx/mise-action` 5.2.6 | `main` | Being brought up to date; lands next. |
| #27 | Weekly build against Zed's latest release (13.1) | `main` | Reviewed (no blockers; two medium: infrastructure failures read as "Zed breaks rozed", relative `--dir`). Fixes in progress. |
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

## Next

1. Land #19 and #27, then milestone 5 as one stack, **#15 → #17 → #18 → #23 →
   #25**, then #2. Building the stack:
   - #18 merges `feat/api-diff` in, with the two edits in Notes;
   - #23 merges `feat/viewed-marks` in and moves `repo_accounts` to migration index 3;
   - #25 merges `feat/recent-prs` in.
   This order lets #18 and #23 be combined while #25's fixes are still running.
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

- **Disk:** each worktree's `target/` holds 30–90 GB of Zed builds. Delete a worktree's `target/`
  once its PR merges; keep an eye on free space (`df -h ~`).

- **Classifier blocks seen so far, worked around legitimately:** merging PRs ("Merge Without
  Review", waiting on the user), creating a public repo ("Create Public Surface"), force-pushing a
  test fixture ("Git Destructive"; tests use local repos), reading the bot's token in a shell for a
  live check ("Credential Exploration"; the user then explicitly allowed using the bot's token with
  rozed's credential helper to test rozed, on 2026-11-06), and
  building git from downloaded source ("Code from External"; verified from git's source instead).

- `rozed-dev/playground` has a branch `probe/if-modified-since` left from a live check; harmless.

- **Runaway processes:** Zed captures the shell environment by running the current executable with
  `--printenv`. rozed must handle that flag, and refuses to start under a rozed parent
  (`ROZED_PARENT_PID`). Never remove either.
- **Launch rozed at normal priority:** zsh renices background jobs (`BG_NICE`), and a niced rozed
  never repaints. Use `setopt NO_BG_NICE` or `nice -n 0`.
- **Frozen-looking window on this desktop:** GPUI stops repainting after the first frame until the
  window is re-activated (virtual display). Click the wallpaper twice, then rozed's title bar.
- **Builds need mise** (`mise exec -- cargo ...`) for the pinned cmake.

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
