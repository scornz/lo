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

- **PR descriptions say what the PR implements,** its code, behaviour and goal, and leave out how
  the work is run (sessions, agents, why PRs stack). The user asked for this; the repo is public.

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

## GitHub layer

- **`graphql_client` with an `apollo-compiler` validation test,** over `cynic`: `graphql_client`
  adds 5 crates and builds in a fraction of a second, and the test catches the argument and
  variable-type mistakes it misses. `cynic` costs about 19 crates, duplicate versions, and its repo
  moved off GitHub. (Research §4.)
- **`sqlez`, not `rusqlite`:** Zed links `libsqlite3-sys` with `links = "sqlite3"`, so a second
  SQLite would conflict. rozed opens its own database file rather than Zed's `db`.
- **Debug builds keep tokens in a mode-600 file** (`~/.config/rozed/credentials.json`), as the
  handoff allows, because unsigned rebuilds re-prompt for Keychain access. Zed's own dev fallback
  isn't mode 600, so rozed has its own.
- **The GraphQL schema is vendored from `github/docs`,** pinned by commit, because
  `octokit/graphql-schema` stopped updating in early 2025 and lacks stacks.
- **Shared desktop:** agents take `mkdir ~/src/rozed-worktrees/.desktop.lock` before using the
  desktop and remove it after, so two agents never drive it at once.

## Local copy

- **rozed runs the `git` CLI with an isolated environment** (`GIT_CONFIG_GLOBAL=/dev/null`,
  `GIT_CONFIG_NOSYSTEM=1`, no credential helper). Design §3 says the user's git config is never
  touched; ignoring it also stops `insteadOf` rewrites and helpers from changing how rozed fetches.
- **Every fetched PR head is pinned at `refs/rozed/pull/<n>/revs/<sha>`,** so force-pushed revisions
  survive `gc` (design §3).
- **git 2.40 or newer is required,** for `git merge-tree --write-tree --merge-base` (rebase-aware
  comparison, design §6). macOS's `/usr/bin/git` is 2.54.

## Building on Zed (PR #3 and its review)

- **Zed's unwanted crates are compiled in but never started.** `project`, `editor` and `workspace`
  pull in `language_model`, `agent_settings`, `cloud_llm_client`, `terminal`, `dap`, `remote` and
  `context_server`. Cutting them would take a large patch set on Zed that fights every bump, so
  rozed accepts them, as design §2's "comes along unused" allows. It never calls their `init`,
  never binds their actions, and forces `disable_ai`, which keeps context servers off. A CI check
  fails when the set of Zed crates changes, so new arrivals get looked at.
- **A forced settings layer** (Zed's server-settings layer, merged above user settings) turns off
  telemetry, auto-update, AI and extension auto-install, and points `server_url` at
  `https://rozed.invalid`. Tests apply contrary user settings and check the forced values win.
- **Read-only is enforced by the keymap and trust, not just by read-only buffers.** rozed loads
  Zed's keymap without bindings to open, new, save or save-as actions (a test checks), and
  initialises Zed's worktree trust with nothing trusted, so no language server starts and no
  repo `.zed/settings.json` applies until rozed trusts the worktree. The spike trusts its own PR
  checkout explicitly; milestone 10 adds the trust gate.
- **The HTTP client follows redirects itself,** checking and logging every hop, so a redirect can't
  reach Zed's hosts unseen and tokens aren't forwarded across hosts.
- **`rozed/v*` patch branches are append-only.** Rewriting a patch makes a new branch
  (`rozed/v1.27.0-2`), so commits pinned by older rozed commits stay reachable.
- **The submodule is fetched with `git submodule update --init --depth 1`** (7 s, 32 MB); without
  `--depth`, git fetches all of Zed's history.
- **`xtask` is its own Cargo workspace,** so `cargo xtask sync-zed` runs even when rozed's workspace
  can't resolve (an empty submodule, or a Zed bump that needs a new patch).
- **`--seed-lock` merges lockfiles:** Zed's versions for Zed's packages, rozed's existing pins for
  the rest, so a Zed bump doesn't silently upgrade rozed's own dependencies.
- **`ZED_DISABLE_STAFF=1`** is set at startup so Zed's staff-only feature flags stay off in debug
  builds, and **`ROZED_PARENT_PID`** stops rozed from starting under another rozed (Zed re-runs the
  executable for `--printenv` and `--askpass`, which rozed handles).

## Revisions

- **Revisions come from the branch's push history, not only the PR timeline.** Design §6 says the
  timeline of pushed commits and force-pushes. The timeline's commits carry no push time
  (`pushedDate` is null), so it can't separate two normal pushes. The repository activity API
  (`GET /repos/{o}/{r}/activity?ref=refs/heads/<branch>`) lists every push with before/after SHAs,
  so rozed builds revisions from it first, then fills gaps from force-push events, review commits,
  check-suite pushes and heads its own poller saw. This keeps the design's intent (one revision per
  push) with a source that has the data. (Research: `research/revisions.md`.)
- **Revisions are stored by head SHA and numbered r1, r2… only for display,** since a late source
  can insert one.
- **A revision's base is the merge-base of its head with the base branch as it was then,** computed
  locally; for stacked or retargeted PRs, the base tip comes from base-branch force-push events,
  the base branch's activity log and rozed's pinned refs.
- **Rebase-aware comparison is skipped when both revisions share a base;** a `merge-tree` exit of 1
  (conflict) falls back to the plain diff with a note, and any other failure is an error.

## Review order (PR #6)

- **`linguist-generated` has three states;** an explicit `-linguist-generated` keeps a lockfile
  expanded and lists it with config, as GitHub does.
- **Group precedence is generated, then config, then test, then source.** Anything not named
  (docs, images, `go.mod`, `Dockerfile`, `Makefile`) is source.
- **Every dot-directory counts as config,** not only `.github/` (`.husky/`, `.vscode/`).
- **A test follows the source with its name in any directory,** preferring the most shared
  directories, then the same extension, then path order. JS and TS extensions match each other.
- **"More than 5,000 changed lines" means additions plus deletions,** strictly more; generated and
  large are reported separately so a large lockfile is both collapsed and loaded on a key.
