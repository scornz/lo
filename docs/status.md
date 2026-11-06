# rozed status

Kept current by the overseer session. A fresh session should be able to resume from this file,
[`plan.md`](plan.md) and [`decisions.md`](decisions.md). The live copy is in the worktree
`~/src/rozed-worktrees/docs` (branch `docs/status`), which can be ahead of what's pushed.

**Updated:** 2026-11-06

## Current milestone

**2. Spike:** done (#3, #4). **3. GitHub layer:** done (#5, #10 reviewed, fixed, verified). **4. Local
copy:** #13 review fixes in progress. **5. The PR tab:** 5a in progress on `integration/pr-tab`.

- PR #3 (2a, `build/zed-workspace`): submodule, Cargo sync, first window. Green (CI ~10 min per
  run). Window verified on screen. The independent review found real gaps (Zed's open/save keys and
  untrusted language servers, redirects past the HTTP guard, lockfile pins, xtask bootstrapping,
  untested forced settings); fixes are in progress on the same branch.
- PR #4 (2b, `feat/spike`): the spike's acceptance criteria are met (diff, rust-analyzer, network
  check, crate list with a CI check, numbers in `docs/spike.md`). The review's trust and diff
  findings are being fixed on the branch; its git findings went to milestone 4.
- PR #13 (milestone 4, `feat/repo-cache`): `repo_cache` crate. Reviewed, fixed, verified (private repo on screen; a live credential-helper test is being added).
- PR #15 (5a, `feat/pr-tab`, on `integration/pr-tab`): the PR tab. Under review.
- PR #17 (5b, `feat/api-diff`) and PR #18 (5c, `feat/viewed-marks`): under review.
- 5d (`feat/diff-display`, on `feat/api-diff`): in progress. 5e waits for #15's fixes.
- PR #5 (3a, `feat/github-client`): `github` crate, typed GraphQL, accounts, device flow, `gh`
  import, Accounts modal. Reviewed, fixed, verified.
- PR #10 (3b, `feat/github-polling`): polling scheduler and cache. Reviewed, fixed.
- Early logic, no Zed compile: `feat/stacks-logic` (Task 7.1) and `feat/review-order` (Task 5.1),
  both on `build/zed-workspace`.
- Research for milestone 6 (push boundaries, markers, viewed marks, `git merge-tree`) running;
  report goes to `docs/research/revisions.md`.
- Agents share the desktop through `mkdir ~/src/rozed-worktrees/.desktop.lock`.

## The PR stack

Merging is blocked on the user (see below), so PRs stack: each targets the branch of the PR below
it. Merge them bottom to top.

| PR | Branch | Base | State |
|---|---|---|---|
| [#1](https://github.com/scornz/rozed/pull/1) `chore: bootstrap the repository` | `chore/bootstrap` | `main` | Reviewed and verified. Ready. |
| [#2](https://github.com/scornz/rozed/pull/2) `docs: plan the spike…` | `docs/status` | `chore/bootstrap` | The project docs; keeps getting commits. Merge any time after #1. |
| [#3](https://github.com/scornz/rozed/pull/3) `build: build rozed on Zed's crates` | `build/zed-workspace` | `chore/bootstrap` | Reviewed; review fixes in (16 commits); verified on screen; green. Ready after #1. |
| [#4](https://github.com/scornz/rozed/pull/4) `feat: open a pull request in Zed's side-by-side diff` | `feat/spike` | `build/zed-workspace` | Reviewed; fixes in (opt-in `--trust`, right side from git, Zed patches to ignore `.zed` settings and Recent Items, server allowlist, no login shell, blame and Prettier off); verified on screen; network GitHub-only; green. Ready after #3. |
| [#5](https://github.com/scornz/rozed/pull/5) `feat(github): sign in to GitHub and call its API` | `feat/github-client` | `build/zed-workspace` | Security-reviewed; 18 fix commits (removal always works, only not-found means not visible, token only to GitHub API hosts, newest PRs first, device-flow hardening, safe token file); verified on screen; green. Ready after #3. |
| [#6](https://github.com/scornz/rozed/pull/6) `feat(review): order files for review…` | `feat/review-order` | `build/zed-workspace` | Reviewed; 60 tests; green. Ready after #3. |
| [#8](https://github.com/scornz/rozed/pull/8) `feat(review): build revisions and choose what to compare` | `feat/revisions-logic` | `feat/review-order` | Reviewed; fixes in (head is always latest, revisions start at PR open, O(n log n)); 148 tests incl. 20,000 random histories; green. Ready after #6. |
| [#9](https://github.com/scornz/rozed/pull/9) `feat(diff_extras): detect moved blocks of code` | `feat/moved-code` | `build/zed-workspace` | Reviewed; 56 tests; 1.6 ms on a 21,000-line diff; green. Ready after #3. |
| [#10](https://github.com/scornz/rozed/pull/10) `feat(github): poll pull requests and cache them` | `feat/github-polling` | `feat/github-client` | Reviewed; 24 fix commits (If-Modified-Since polling, FIFO queue spawned inside the lock, writes invalidate, sleep-safe waits, extended check); 35 polling tests ×30 seeds; green. Ready after #5. |
| [#11](https://github.com/scornz/rozed/pull/11) `feat(stacks): plan turning a chain into a GitHub stack` | `feat/stack-conversion` | `feat/stacks-logic` | Reviewed; 42 new tests; green. Ready after #7. |
| [#13](https://github.com/scornz/rozed/pull/13) `feat(repo_cache): keep a partial clone of each repository` | `feat/repo-cache` | `feat/spike` | Reviewed; 18 fix commits (URL-scoped credential helper, `--git-dir` everywhere, byte paths, per-head checkouts, diff before checkout, maintenance); green. Private `playground#2` verified on screen through the new credential helper. Ready after #4. |
| [#14](https://github.com/scornz/rozed/pull/14) `feat(diff_extras): compare files by syntax` | `feat/syntax-diff` | `feat/moved-code` | Reviewed; difftastic port with its tests; 10,000 lines in ~160 ms (release); green. Ready after #9. |
| [#12](https://github.com/scornz/rozed/pull/12) `feat(review): explain merge blocks and plan approve and ship` | `feat/merge-logic` | `feat/revisions-logic` | Reviewed; redesigned and fixed (two-step approve and ship acting on GitHub's verdict, never bypassing, stale-approval warning, auto-merge off after an unseen push); 268+ tests; green. Ready after #8. |
| [#15](https://github.com/scornz/rozed/pull/15) `feat(review_ui): open pull requests in tabs` | `feat/pr-tab` | `integration/pr-tab` | Review found a dropped file starts rust-analyzer without trust, plus account fallbacks, no offline open, palette writes; fixes in progress. |
| [#17](https://github.com/scornz/rozed/pull/17) `feat(review_ui): show GitHub's diff while the local copy loads` | `feat/api-diff` | `feat/pr-tab` | Green; GitHub's diff in 3.5–6.6 s on a 56-file rust-lang PR, switch-over keeps the line; verified on screen. Independent review running. |
| [#18](https://github.com/scornz/rozed/pull/18) `feat(review_ui): mark files viewed` | `feat/viewed-marks` | `feat/pr-tab` | Green; `e`, `]f`/`[f`, `]c`/`[c` verified on screen, GitHub shows the marks. Independent review running. |
| [#16](https://github.com/scornz/rozed/pull/16) `feat(symbol_index): index definitions and references by name` | `feat/symbol-index` | `feat/syntax-diff` | Reviewed; 76 tests; VS Code's 11,500 files in 1.2 s, re-index 1.8 ms (release); green. Ready after #14. |
| [#7](https://github.com/scornz/rozed/pull/7) `feat(stacks): find stacks…` | `feat/stacks-logic` | `build/zed-workspace` | Reviewed; review fixes in (trunks, live base tip, squash-merged matching, `retarget_to`); 82 tests; green. Ready after #3. |

#4–#7 all sit on #3 and don't depend on each other, except where noted when they land. Whichever
merges second resolves `Cargo.lock` and `crates/rozed` conflicts by merging `origin/main`.

### How to merge the stack

1. Squash-merge the bottom PR. GitHub deletes its branch and retargets the next PR to `main`.
2. In the next PR's branch, merge `origin/main` and push (the ruleset wants branches up to date).
   The squash commit and the branch's own copy of the lower PR's commits can conflict where the
   upper PR changed lines the lower one added; the upper branch's side is always right there,
   because it already contains everything below it.
3. Wait for checks, squash-merge, repeat.

## Merged

Nothing yet.

## Next

- **2. Spike:** detailed tasks go into `plan.md` from the Zed research, then one implementer on
  branch `feat/spike` stacked on `chore/bootstrap`.

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

- **Merging PRs.** The auto-mode permission classifier refuses `gh pr merge` from the overseer
  ("Merge Without Review"). Either merge the stack yourself (bottom to top, see above) or add a
  permission rule that allows `gh pr merge` for `scornz/rozed`, and a session can merge the reviewed
  stack.
- **A public end-to-end repo.** The classifier refuses creating a public repo ("Create Public
  Surface"). Merge queues need a public org repo (design §12), so milestone 13 needs the user to
  create `rozed-dev/e2e` (public) or allow it. Until then checks use the private
  `rozed-dev/playground`.
- **Apple Developer account,** for signed releases. Release secrets: `MACOS_CERT_P12`,
  `MACOS_CERT_PASSWORD`, `NOTARY_KEY_ID`, `NOTARY_ISSUER_ID`, `NOTARY_KEY_P8`. Builds stay unsigned
  until then.
- **Zed's trademark terms,** checked before the name goes public in a release.

## Notes

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
