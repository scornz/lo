# Decisions

Judgment calls made where the design is silent, newest last. Each has a line of reasoning.

## Process

- **Task worktrees live in `~/src/rozed-worktrees/<branch-slug>`.** The Agent tool's worktree
  isolation makes worktrees of the overseer's own repository (srcs), so subagents create Lo
  worktrees themselves with `git -C ~/src/rozed worktree add`.
- **The overseer's docs (`status.md`, `plan.md`, `decisions.md`) land through small docs PRs** opened
  by a subagent, since nobody commits to `main` and the overseer doesn't commit.

## Repository

- **Node and pnpm are pinned in `mise.toml`, as in srcs,** so hooks and commitlint run the same way
  in both repos.
- **Scripts live in `script/`,** Zed's convention, since Lo sits beside Zed's tree.
- **Bootstrap ships a stub `lo` crate** so the Rust CI jobs check a real workspace from the first
  PR.
- **CI skips the Rust jobs when only docs, agent files or the license change.** `ci-ok` stays the
  single required check and passes when they're skipped. Building Zed's crates on a hosted macOS
  runner takes long enough that docs PRs shouldn't pay for it.
- **No `guard-bash` hook.** The `.claude/settings.json` deny rules plus lefthook cover the rules the
  user asked for; srcs's hook exists for Terraform.
