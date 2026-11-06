# rozed status

Kept current by the overseer session. A fresh session should be able to resume from this file,
[`plan.md`](plan.md) and [`decisions.md`](decisions.md). The live copy is in the worktree
`~/src/rozed-worktrees/docs` (branch `docs/status`), which can be ahead of what's pushed.

**Updated:** 2026-11-06

## Current milestone

**2. Spike** — research done or running; implementation brief next.

## The PR stack

Merging is blocked on the user (see below), so PRs stack: each targets the branch of the PR below
it. Merge them bottom to top.

| PR | Branch | Base | State |
|---|---|---|---|
| [#1](https://github.com/scornz/rozed/pull/1) `chore: bootstrap the repository` | `chore/bootstrap` | `main` | Reviewed and verified; checks green. Ready to merge. |

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
- Reports: `github-research.md` and `github-checks.md` in the overseer's scratchpad.

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
