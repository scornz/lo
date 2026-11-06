# rozed status

Kept current by the overseer session. A fresh session should be able to resume from this file,
[`plan.md`](plan.md) and [`decisions.md`](decisions.md). The live copy is in the worktree
`~/src/rozed-worktrees/docs` (branch `docs/status`), which can be ahead of what's pushed.

**Updated:** 2026-11-06

## Current milestone

**2. Spike** and **3. GitHub layer**, in parallel.

- PR #3 (2a, `build/zed-workspace`): submodule, Cargo sync, first window. Green (CI ~10 min per
  run). Window verified on screen. The independent review found real gaps (Zed's open/save keys and
  untrusted language servers, redirects past the HTTP guard, lockfile pins, xtask bootstrapping,
  untested forced settings); fixes are in progress on the same branch.
- 2b (`feat/spike`, on `build/zed-workspace`): PR diff with rust-analyzer, network check,
  `docs/spike.md`. In progress.
- 3a (`feat/github-client`, on `build/zed-workspace`): `github` crate, typed GraphQL, accounts,
  device flow, `gh` import, sign-in UI. In progress.
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
| [#4](https://github.com/scornz/rozed/pull/4) `feat: open a pull request in Zed's side-by-side diff` | `feat/spike` | `build/zed-workspace` | In progress. |
| [#5](https://github.com/scornz/rozed/pull/5) `feat(github): sign in to GitHub and call its API` | `feat/github-client` | `build/zed-workspace` | In progress. |
| [#6](https://github.com/scornz/rozed/pull/6) `feat(review): order files for review…` | `feat/review-order` | `build/zed-workspace` | Reviewed; 60 tests; green. Ready after #3. |
| [#7](https://github.com/scornz/rozed/pull/7) `feat(stacks): find stacks…` | `feat/stacks-logic` | `build/zed-workspace` | Awaiting hand-back and review. |

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
