# Decisions

Judgment calls made where the design is silent, newest last. Each has a line of reasoning.

## Process

- **Task worktrees live in `~/src/rozed-worktrees/<branch-slug>`.** The Agent tool's worktree
  isolation makes worktrees of the overseer's own repository (srcs), so subagents create rozed
  worktrees themselves with `git -C ~/src/rozed worktree add`.
- **The overseer's docs (`status.md`, `plan.md`, `decisions.md`) live on branch `docs/status`,** in
  the worktree `~/src/rozed-worktrees/docs`, and land through one open docs PR that a subagent
  updates. Nobody commits to `main`, and the overseer doesn't commit.
- **PRs are stacked while merging is blocked.** The auto-mode classifier refuses `gh pr merge` from
  the overseer ("Merge Without Review"), so merging waits for the user. Each new PR branches from
  the PR below it and targets that PR's branch, so every PR's diff shows only its own change and
  work keeps moving. Parallel tasks merge the new base into their branch (never rebase or
  force-push) before retargeting. Getting around the classifier, by merging through a subagent or
  the REST API, was rejected.

## Repository

- **Node and pnpm are pinned in `mise.toml`, as in srcs,** so hooks and commitlint run the same way
  in both repos.
- **Scripts live in `script/`,** Zed's convention, since rozed sits beside Zed's tree.
- **Bootstrap ships a stub `rozed` crate** so the Rust CI jobs check a real workspace from the first
  PR.
- **CI skips the Rust jobs when only docs, agent files or the license change.** `ci-ok` stays the
  single required check and passes when they're skipped. Building Zed's crates on a hosted macOS
  runner takes long enough that docs PRs shouldn't pay for it.
- **No `guard-bash` hook.** The `.claude/settings.json` deny rules plus lefthook cover the rules the
  user asked for; srcs's hook exists for Terraform.

## Spike

- **The spike is two stacked PRs:** 2a gets Zed's crates building locally and on CI, 2b opens a PR
  in the diff. A build or CI problem then surfaces before the diff work depends on it.
- **Zed patch branches are named `rozed/v<zed version>`,** one per Zed release, so old submodule
  pointers stay reachable after a bump.
- **The Cargo sync is a Rust `xtask` (`cargo xtask sync-zed`, using `toml_edit`),** not a shell or
  Python script: it edits TOML structurally, needs no extra toolchain, and gets tested like the rest.
- **Zed's hosts are refused in the HTTP client as well as turned off in settings.** Settings alone
  depend on every Zed code path honouring them; refusing `zed.dev` and its subdomains in the one
  shared client catches anything missed and logs it.
- **rozed fetches into its own namespace, `refs/rozed/...`,** so fetched revisions never collide
  with branch names and survive force-pushes (design §3).
- **Verification screenshots stay on the devbox,** under `~/src/rozed-worktrees/evidence/<branch>/`,
  and PR descriptions describe them. Uploading them would publish them on a public repo.
