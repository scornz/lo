# rozed implementation plan

**Design doc:** [`design.md`](design.md). Process and repo rules: [`handoff.md`](handoff.md). Progress:
[`status.md`](status.md). Judgment calls: [`decisions.md`](decisions.md).

**Goal:** Build rozed v1, the native macOS PR reviewer described in the design, as a Cargo workspace
on Zed's crates.

**Architecture:** rozed's own crates (`rozed`, `github`, `repo_cache`, `stacks`, `review`,
`diff_extras`, `review_ui`) depend by path on Zed's crates in a `third_party/zed` submodule, pinned
to a patch branch on `scornz/zed`. One process; git and language servers run as child processes.

**Tech stack:** Rust 1.98.1 (Zed v1.27.0's toolchain), GPUI and Zed's editor crates, SQLite,
GitHub GraphQL and REST, git. Tooling: Node and pnpm (commitlint, lefthook) pinned in `mise.toml`.

---

## How the work runs

- The overseer plans, briefs, reviews, verifies and merges. Subagents on Opus 5.5 write every change,
  commit, push and open PRs.
- Each task is one branch and one PR, in its own worktree at `~/src/rozed-worktrees/<branch-slug>`
  (see `decisions.md`). At most two worktrees compile Zed at once.
- Each milestone's detailed tasks are written when it starts. Later milestones stay at roadmap level
  until the ones before them show how Zed's crates behave.

## Roadmap

Each milestone is done when every PR in it is merged, CI is green, and its acceptance criteria are
verified by building and running the app (on screen for UI).

| # | Milestone | Acceptance criteria |
|---|---|---|
| 1 | **Bootstrap** | Repo conventions from srcs adapted to Rust: `CLAUDE.md` + symlinks, skills, settings, lefthook + commitlint, PR template, `pr-title` and `ci` workflows with `ci-ok`, Dependabot, `LICENSE`, docs. A stub `rozed` crate passes fmt, clippy and test on macOS CI. |
| 2 | **Spike** (design §2 "First milestone") | `third_party/zed` submodule on `scornz/zed` branch `rozed/v1.27.0` with a `paths` patch. `cargo run -p rozed -- <pr-url>` opens a public PR in Zed's side-by-side diff, with rust-analyzer running on the new side. `docs/spike.md` lists the Zed crates compiled in and shows nothing contacted Zed's servers. Whether GitHub accepts rozed's OAuth token for attachment uploads is answered. Cargo settings copied from Zed are kept in sync by a script that CI checks. Performance numbers recorded. |
| 3 | **GitHub layer** (§3) | `github` crate: device-flow sign-in, `gh` account import, several accounts (Keychain in release, mode-600 file in debug), typed GraphQL checked against GitHub's schema, REST with ETags, polling with backoff and rate-limit tracking, SQLite cache. Unit tests on replayed responses. |
| 4 | **Local copy** (§3 "Local data", §11 "The local copy") | `repo_cache` crate: bare partial clones in `~/Library/Caches/rozed`, PR head/base fetching through `GIT_ASKPASS`, every fetched revision kept under rozed's own refs, checkouts for trusted repos, git check at launch, 30-day cleanup. Git tests on real temp repos, including force-push survival. |
| 5 | **The PR tab** (§5, §10 "Window") | `review_ui` PR tab: header, file list in review order (and path order), side-by-side diff from the local copy with the API diff shown first, generated/lockfile/large files collapsed, binary and image files, renames, whitespace toggle, GitHub viewed marks with `e`. Recent-PRs screen when nothing is open. |
| 6 | **Revisions** (§6) | `review` crate: revisions from the timeline, the revision picker with "your last review" and "last seen here", the default comparison, rebase-aware comparison via `git merge-tree` with fallback, changed-since-viewed. |
| 7 | **Stacks** (§4) | `stacks` crate: the three rules with nearest-parent, trees, native stacks winning, squash-merged layers; the stack tree sidebar, `]s`/`[s`, layer and multi-layer diffs, "needs rebase", one tab per stack. |
| 8 | **Comments and reviews** (§8) | Threads in the diff, comment box (Zed editor, Markdown, Vim, preview), ranges, suggestions, file comments, pending review, `C`, submit dialog, threads panel, `]t`/`[t`, outdated threads, timeline, attachments, unsent text saved locally. |
| 9 | **Merging** (§9, §11) | Merge status line and `m` panel explaining blocks, methods, queue, confirmations, approve and ship (`⌘⇧⏎`) with every outcome tested, stack merges (async), "Make this a GitHub stack" with rollback, queue view and controls, admin override. |
| 10 | **Code features** (§7) | Syntax index served as a built-in language server (rozed in server mode), trust banner and settings, language servers on trusted same-repo PRs only, `gd` peek, `gr`, `K`, jump into the diff, failure handling. Bundled extensions. |
| 11 | **Moved code and the syntax-aware view** (§5) | `diff_extras`: moved-block detection within and across files with links, difftastic-style syntax-aware view with real line numbers and comment mapping. |
| 12 | **Links, keys and settings** (§10) | `⌘O` with clipboard prefill, link-router registration, `rozed://`, CLI install, PR link parsing (files/commits/checks/line/comment), keymap file and `?` sheet, settings UI. GPUI tests that every default key maps to an action. |
| 13 | **End-to-end and release** (§12) | Scripted scenarios on `rozed-dev` driven by `rozed-bot`, nightly; proxy-recorded host allowlist check; performance budgets; weekly Zed-latest build job; release workflow (signing gated on the user's Apple account). |

---

## Milestone 1: Bootstrap

**Branch:** `chore/bootstrap`, in the main clone at `~/src/rozed` (it already holds the untracked
docs this PR commits). **PR title:** `chore: bootstrap the repository`.

### Task 1.1: Agent rules and skills

**Depends on:** None

**Files:**

- Create: `CLAUDE.md`, `AGENTS.md -> CLAUDE.md`
- Create: `.claude/settings.json`
- Create: `.claude/skills/pr-writing/SKILL.md`, `.claude/skills/pr-writing/references/examples.md`
  (copied from `~/src/ash-mac/.claude/skills/pr-writing/`)
- Create: `.agents/skills -> ../.claude/skills`

**Steps:**

1. Write `CLAUDE.md`: what rozed is (one paragraph, link `docs/design.md`), the principles that bind
   code (read-only, no AI, no backend or telemetry, never runs a repo's code, keyboard first), the
   repo rules from `docs/handoff.md` "Repo rules the user asked for", the workspace layout, how to
   set up (`mise install`, `pnpm install`, Rust from `rust-toolchain.toml`, `PATH` with
   `~/.cargo/bin`), how to build, test and run (`cargo build -p rozed`, `cargo test --workspace`,
   `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`), and the
   secrets rule (never print, log or commit tokens, passwords or two-factor secrets). Leave a
   "Zed submodule" section stub for the spike to fill.
2. `.claude/settings.json`: attribution off (`commit: ""`, `pr: ""`, `sessionUrl: false`) and the
   deny rules from srcs's `.claude/settings.json` for pushing `main`, force-pushing and
   `--no-verify` (only those; none of the terraform or nx rules).
3. Copy the `pr-writing` skill verbatim, then fix anything srcs-specific in it (nx, pnpm targets,
   product names) so it reads right for rozed.
4. Create the two symlinks with relative targets: `ln -s CLAUDE.md AGENTS.md`,
   `ln -s ../.claude/skills .agents/skills`.

**Done when:** `readlink AGENTS.md` prints `CLAUDE.md`, `ls .agents/skills/pr-writing` lists
`SKILL.md`, and `CLAUDE.md` contains no srcs-only instructions.

### Task 1.2: Toolchain pins and the stub crate

**Depends on:** None

**Files:**

- Create: `rust-toolchain.toml` (channel `1.98.1`, profile `minimal`, components `rustfmt`,
  `clippy`, `rust-analyzer`, `rust-src`)
- Create: `Cargo.toml` (workspace: `members = ["crates/*"]`, `exclude = ["third_party"]`,
  `resolver = "2"`, edition 2024, `[workspace.package]` with `license = "GPL-3.0-or-later"` and
  `publish = false`)
- Create: `crates/rozed/Cargo.toml`, `crates/rozed/src/main.rs` (a stub `main` that does nothing
  but exist; one unit test so `cargo test` runs something real, e.g. a `version()` helper
  returning `env!("CARGO_PKG_VERSION")`)
- Create: `mise.toml` (`node = "24.22"`, `pnpm = "12.14.0"`, as in srcs)
- Create: `.gitignore` (`/target`, `node_modules/`, `.DS_Store`)

**Steps:**

1. Write the files above.
2. Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo test --workspace` with `PATH="$HOME/.cargo/bin:$PATH"`. Expected: all pass.
3. Commit: `chore: add the Cargo workspace and toolchain pins`.

**Done when:** all three cargo commands pass locally.

### Task 1.3: Git hooks and commitlint

**Depends on:** 1.2

**Files:**

- Create: `package.json` (private, `packageManager` pinned like srcs, devDependencies
  `@commitlint/cli`, `@commitlint/config-conventional`, `lefthook`; `prepare` runs
  `lefthook install`), `pnpm-lock.yaml`
- Create: `commitlint.config.mjs`:
  `export default { extends: ['@commitlint/config-conventional'] };`
- Create: `lefthook.yml` with `commit-msg: commitlint`, `pre-commit: block-main-commit` then
  `cargo fmt` on staged `*.rs` files with `stage_fixed: true`, and `pre-push: block-main-push`
  (`use_stdin: true`)
- Create: `script/hooks/block-main-commit.sh`, `script/hooks/block-main-push.sh` (adapted from
  `~/src/ash-mac/tools/scripts/hooks/`), plus a toolchain wrapper if hooks can't find mise's tools
  otherwise

**Steps:**

1. Write the files and run `pnpm install` (installs the hooks).
2. Check each hook: a commit with message `bad message` is rejected; a commit on `main` is
   rejected (try on a throwaway branch named `main` in a temp clone, never the real `main`); a push
   to `main` is rejected (`git push --dry-run origin HEAD:main` from the temp clone); a staged
   badly formatted `.rs` file is reformatted.
3. Commit: `chore: add commitlint and lefthook`.

**Done when:** each of the four hook checks behaves as described.

### Task 1.4: GitHub configuration and CI

**Depends on:** 1.2, 1.3

**Files:**

- Create: `.github/pull_request_template.md` (copied from srcs)
- Create: `.github/dependabot.yml` (Actions, weekly, `chore` prefix, as in srcs)
- Create: `.github/workflows/pr-title.yml` (as in srcs, without the composite setup action: install
  mise's tools with `jdx/mise-action` or `actions/setup-node` + pnpm)
- Create: `.github/workflows/ci.yml` with jobs:
  - `changes`: decides whether Rust jobs run (skip them when only `docs/**`, `*.md`, `.claude/**`,
    `.agents/**` or `LICENSE` changed)
  - `commitlint`: lints every commit in the PR (`pnpm exec commitlint --from <base sha> --to <head sha>`)
  - `symlinks`: fails unless every `CLAUDE.md` has an `AGENTS.md` symlink beside it resolving to
    it, and `.agents/skills` resolves to `.claude/skills`. Put the check in
    `script/check-symlinks.sh` so it also runs locally.
  - `rust` on `macos-latest`, only when `changes` says so: `cargo fmt --all --check`,
    `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, with
    `Swatinem/rust-cache`. Check out submodules recursively (none yet; the spike adds Zed).
  - `ci-ok`: `if: always()`, needs every other job, fails if any needed job failed or was
    cancelled (skipped is fine)
- Pin every action by full commit SHA with a version comment, as srcs does (look up SHAs with
  `gh api repos/<owner>/<repo>/git/ref/tags/<tag>`).

**Steps:**

1. Write the files. Run `script/check-symlinks.sh` locally (expected: pass), then break a symlink
   in a scratch copy and check it fails.
2. Commit: `ci: add the pr-title and ci workflows`.

**Done when:** on the PR, `pr-title`, `commitlint`, `symlinks`, `rust` and `ci-ok` all pass.

### Task 1.5: License, README and docs, then the PR

**Depends on:** 1.1–1.4

**Files:**

- Create: `LICENSE` (copy of `~/src/zed/LICENSE-GPL`)
- Modify: `README.md` (keep the user's text; add one line on the license and a link to
  `docs/design.md`)
- Add: `docs/design.md`, `docs/handoff.md`, `docs/plan.md`, `docs/status.md`, `docs/decisions.md`
  as they are on disk

**Steps:**

1. Commit: `docs: add the design, handoff, plan and status`.
2. Push `chore/bootstrap` and open the PR with a description written using the `pr-writing` skill
   (from srcs: `~/src/ash-mac/.claude/skills/pr-writing/SKILL.md`).
3. Wait for CI. Fix failures with new commits (never force-push).

**Done when:** every check is green and the PR description says how each piece was verified.

---

## Milestone 2: Spike

Design §2 "First milestone". Research: [`research/zed-crates.md`](research/zed-crates.md) (Zed's
startup, split diff, `paths`, Cargo, crate list, traps — read it before starting),
[`research/github-api.md`](research/github-api.md) and
[`research/github-checks.md`](research/github-checks.md) (attachments and stacks, already
answered: rozed's OAuth token uploads attachments; stacks need no opt-in).

Two PRs, stacked:

- **2a** `build: build rozed on Zed's crates` — branch `build/zed-workspace`, base
  `chore/bootstrap`. Tasks 2.1–2.5.
- **2b** `feat: open a pull request in Zed's side-by-side diff` — branch `feat/spike`, base
  `build/zed-workspace`. Tasks 2.6–2.10.

### Task 2.1: Zed patch branch

**Depends on:** None

- In a worktree of `~/src/zed` (don't move `~/src/zed`'s own checkout), create branch
  `rozed/v1.27.0` from tag `v1.27.0`.
- One commit: `paths: use rozed's folders` setting `APP_NAME` to `"rozed"` in
  `crates/paths/src/paths.rs` (research §4).
- Push the branch to `scornz/zed`. A later Zed bump makes a new `rozed/v<version>` branch, so old
  submodule pointers stay reachable.

**Done when:** `git ls-remote https://github.com/scornz/zed refs/heads/rozed/v1.27.0` shows the
patch commit, whose parent is `v1.27.0`.

### Task 2.2: The submodule

**Depends on:** 2.1

- `third_party/zed` as a submodule of `https://github.com/scornz/zed.git`, branch
  `rozed/v1.27.0`, at the patch commit. Clone it without downloading Zed's whole history (blobless
  or shallow, e.g. clone `--filter=blob:none` into place before `git submodule add`, or
  `git submodule update --init --depth 1`).
- `Cargo.toml`: `exclude = ["third_party/zed"]` (research §5; keep `third_party` excluded).
- CLAUDE.md "Zed submodule": how to initialise it quickly in a new clone or worktree, the patch
  list, and how to bump Zed (new `rozed/v<version>` branch, rebase the patches, run the sync, fix
  breakages).
- `.gitignore`: add `.build/` (a SwiftPM cache some tool drops in the clone).

### Task 2.3: Cargo settings copied from Zed, kept in sync

**Depends on:** 2.2

- A `crates/xtask` binary (`cargo xtask sync-zed` via an alias in `.cargo/config.toml`, using
  `toml_edit`) that copies from `third_party/zed` into rozed's root: the `[patch.*]` sections (paths
  rewritten to `third_party/zed/...`), the `[profile.*]` sections, and the parts of Zed's
  `.cargo/config.toml` that apply on macOS (rustflags `-C symbol-mangling-version=v0` and
  `--cfg tokio_unstable`, `MACOSX_DEPLOYMENT_TARGET = "12.0"`). Copied content sits between clear
  "generated from Zed; run `cargo xtask sync-zed`" markers. `--check` exits non-zero, naming what
  differs, when the copies are stale.
- `cargo xtask sync-zed --seed-lock` copies `third_party/zed/Cargo.lock` to the root, for the first
  build and each Zed bump (research §5 "Cargo.lock").
- Unit tests for the copy (feed it small TOML fixtures).
- CI: a step in the `rust` job runs `cargo xtask sync-zed --check`.

### Task 2.4: A window built from Zed's crates

**Depends on:** 2.3

- `crates/rozed` depends by path on the Zed crates the spike needs (research §6), with features
  `gpui_platform/font-kit`, `languages/load-grammars` and `util/debug-embed` (research §5 "Required
  features"). A `build.rs` as in research §5 "Binary build.rs". Don't depend on `git_ui`,
  `git_ui_core` or `extension_host`.
- Startup modelled on `crates/component_preview/examples/component_preview.rs` (research §1
  "Minimal init sketch"): `Client::new` without `client::init` or authentication, settings with the
  forced overlay (telemetry and the rest off, research §1 "Forced settings overlay"), theme, assets,
  default keymap loaded with partial failure allowed.
- **The HTTP client refuses Zed's hosts.** Wrap the one shared HTTP client so any request to
  `zed.dev` or a subdomain fails with an error and is logged at `warn`. Every request's host is
  logged at `debug`, for the network check in 2.9. Unit-test the host matching.
- `cargo run -p rozed` opens one window showing a read-only Zed `Editor` with some Rust text,
  highlighted, in Zed's default theme.
- Check on screen and take a screenshot. Check that `~/.config/rozed`,
  `~/Library/Application Support/rozed` and `~/Library/Logs/rozed` are used, and that nothing
  under Zed's own folders changed.

### Task 2.5: CI builds it

**Depends on:** 2.4

- The `rust` job checks out the submodule and passes fmt, `sync-zed --check`, clippy and test on
  `macos-latest`. If the hosted runner runs out of disk or time, fix it (e.g. no debuginfo and no
  incremental in CI, freeing runner disk, caching) and record what was needed in `docs/spike.md`.
- Record clean and cached CI times.
- Open PR 2a. **Done when:** checks are green and the screenshot shows the window.

### Task 2.6: PR links

**Depends on:** 2a

- `crates/rozed/src/pr_link.rs`: parse `https://github.com/<owner>/<repo>/pull/<n>` (with or without
  scheme, trailing slash, `/files`, `/commits`, `/checks`) into `{ owner, repo, number }`. TDD: tests
  first, including rejects (issues links, other hosts, non-numeric numbers).

### Task 2.7: A local copy, spike version

**Depends on:** 2a

Milestone 4 replaces this with the `repo_cache` crate; keep it small and tested.

- Bare partial clone (`--filter=blob:none`) at `<cache dir>/repos/github.com/<owner>/<repo>.git`.
- Fetch `refs/pull/<n>/head` and the base branch into rozed's own refs
  (`refs/rozed/pull/<n>/head`, `refs/rozed/base/<branch>`). The base branch name comes from
  unauthenticated `GET /repos/{owner}/{repo}/pulls/{n}` through the app's HTTP client (public
  repos only, for now).
- Base commit = merge-base of head and base. Changed files from
  `git diff --name-status -M <base> <head>`.
- A detached checkout of the head at `<cache dir>/checkouts/github.com/<owner>/<repo>/pull-<n>` (a
  `git worktree` of the bare clone).
- Tests on real temp repos: a bare "remote" with a `refs/pull/1/head`, a rename, an added and a
  deleted file.

### Task 2.8: The PR diff with a language server

**Depends on:** 2.6, 2.7

- `cargo run -p rozed -- <pr-url>`: prepares the local copy, creates a local `Project` on the
  checkout, and shows every changed file in one `SplittableEditor`, side by side. Right side: project
  buffers opened from the checkout. Left side: base text from `git show <base>:<path>`. Follow
  `git_ui_core/src/file_diff_view.rs` (research §2 "Sketch"), including its late-language-load fix.
  Added files have an empty left side; deleted and binary files must not crash (a placeholder is
  fine).
- rust-analyzer starts on the right side (it's downloaded from GitHub releases on first run).
- **Trust:** there's no trust gate yet (milestone 10). Only run the spike on PRs you choose. Note
  research §9's trust risks in `docs/spike.md`.

**Done when:** `sharkdp/fd#2212` opens side by side, and hovering a symbol on the right side shows
rust-analyzer's type hover. Screenshots of both.

### Task 2.9: Network check

**Depends on:** 2.8

- Run the app on the PR from a clean state (rust-analyzer not yet downloaded, empty rozed folders)
  for at least two minutes, scrolling and hovering. Collect every HTTP host from the debug log, and
  sample `lsof -i -n -P -a -p <pid>` every second for the rozed process's sockets.
- **Done when:** no Zed-owned host appears, every host is GitHub or a language-server download
  source, and the refused-host warning never fired (or, if it fired, the code path is found and
  disabled).

### Task 2.10: `docs/spike.md` and PR 2b

**Depends on:** 2.9

`docs/spike.md` records:

- The Zed crates compiled in, generated from `cargo metadata`, grouped, with the unwanted ones
  (AI, collab, telemetry, terminal, debugger, extensions, auto-update) flagged and why they come
  along. A committed list (`script/zed-crates.txt`) and a CI check that fails when the set
  changes.
- The network check: method, hosts seen, verdict.
- GitHub findings: attachments and stacks (summarised from `research/github-checks.md`).
- Numbers: clean and incremental local build times, `target/` size, binary size, launch to first
  diff on `fd#2212`, CI times.
- Risks and follow-ups for later milestones.

**Done when:** PR 2b's checks are green and its description links the screenshots and the numbers.

---

Carried forward from PR #3: Zed's keymap no longer binds any open, new, save or repo-changing
action, but the pane tab bar's "+" menu and Zed's welcome page still offer New File and Open by
mouse. The PR tab (milestone 5) replaces both; vim's `:w`/`ZZ` and the command palette get the
same filtering when vim lands (milestone 12).

---

## Milestone 3: GitHub layer

Design §3, §11 "GitHub", §12 "GitHub API". Research:
[`research/github-layer.md`](research/github-layer.md) — read its "Recommendations" first; the
tasks below follow them. Everything here lives in a new `crates/github`, except the sign-in UI.

Two PRs, stacked on the spike:

- **3a** `feat(github): sign in to GitHub and call its API` — branch `feat/github-client`. Tasks
  3.1–3.5.
- **3b** `feat(github): poll pull requests and cache them` — branch `feat/github-polling`, base
  `feat/github-client`. Tasks 3.6–3.8.

### Task 3.1: The crate, transport and fixtures

**Depends on:** 2a

- `crates/github` depending by path on Zed's `http_client`, `gpui`, `credentials_provider`,
  `sqlez` and whatever else the research names; the client takes an `Arc<dyn HttpClient>`.
- REST: `GET`/`POST`/`PUT`/`PATCH`/`DELETE` with `Authorization: token <token>`,
  `Accept: application/vnd.github+json`, `X-GitHub-Api-Version`, a rozed `User-Agent`, conditional
  requests (`If-None-Match`) and typed errors that keep GitHub's message, `documentation_url`, the
  `X-GitHub-SSO` link (research §5 "SAML SSO") and the OAuth-app-restriction case (§5 "Blocked by
  an org").
- Rate-limit tracking from `x-ratelimit-*` headers and the GraphQL `rateLimit` field, per account.
- `ReplayHttpClient` (research §8): serves recorded responses in order, keyed by method, URL and
  body hash; fails a test on an unexpected request. Recording keeps only allowlisted headers and
  refuses to write a fixture containing the token's bytes. Fixtures live in
  `crates/github/tests/fixtures/`.
- Tests: error mapping, conditional requests, rate-limit parsing, and the token-leak guard.

### Task 3.2: Typed GraphQL

**Depends on:** 3.1

- Vendor GitHub's schema at `crates/github/graphql/schema.graphql` with
  `script/update-github-schema` (pinned to a `github/docs` commit, research §4).
- Queries as `.graphql` files in `crates/github/graphql/`, generated into Rust types with
  `graphql_client`. A test validates every query against the schema with `apollo-compiler`, so an
  unknown field, argument or variable type fails `cargo test`.
- First queries (later milestones add theirs): `Viewer` (login, name, avatar), `PullRequest` (title,
  number, author, state, draft, base/head refs and OIDs, head repository and whether it's a fork,
  `viewerViewedState` per file with pagination, `updatedAt`), `PullRequestCheck` (the cheap poll in
  research §7: `updatedAt`, timeline `updatedAt`, head OID, counts), and `OpenPullRequests` (the
  repo's open PRs with base/head refs, head OIDs and stack membership, paginated, capped at 1,000).
- Decode responses with rozed's own envelope so GraphQL errors keep their `type`.
- Tests on recorded fixtures from `rozed-dev/playground` (PRs #1 and #2, stack #3), recorded with
  the bot's OAuth token from `~/.config/rozed-dev/oauth-token`.

### Task 3.3: Credentials and accounts

**Depends on:** 3.1

- Two `CredentialsProvider`s (research §2 "Recommended design"): Keychain via GPUI in release
  builds, a mode-600 JSON file at `~/.config/rozed/credentials.json` (atomic writes) in debug
  builds, `ROZED_USE_KEYCHAIN=1` forcing the Keychain. Keychain items are keyed
  `rozed://github.com/<login>`.
- rozed's SQLite database through `sqlez` at `<data dir>/rozed.sqlite`, failing loudly rather than
  falling back to memory. First migration: `accounts` (login, name, avatar URL, scopes, source:
  device flow or `gh`, added at) and `owner_accounts` (owner → account login, design §3 "Which
  account").
- An `Accounts` model: list, add, remove, pick the account for an owner (the one that can see the
  repo; if several can, the remembered choice; else "ask"), and mark an account signed out on a
  401 (design §11 "Signed out").
- Tests with `FakeCredentialsProvider` and a temp database.

### Task 3.4: Device flow and `gh` import

**Depends on:** 3.3

- Device flow (research §5 "Recommended loop"): client ID `Ov23liBORqwRdiPzqsPP`, scopes
  `repo read:org workflow`, polling at the interval × 1.2, `slow_down` adopting the returned
  interval, `expired_token` and `access_denied` ending the flow, then `GET /user` and a scope check.
  Tests on synthetic fixtures for every branch.
- `gh` import (research §6 "Cleanest import"): find `gh` (including Homebrew paths), list accounts
  with `gh auth status --json hosts`, read each token with
  `gh auth token --hostname github.com --user <login>` through a pipe with `GH_TOKEN` and
  `GITHUB_TOKEN` removed from its environment; note when a token lacks `workflow`. Tests with a fake
  `gh` script.

### Task 3.5: Sign-in UI and PR 3a

**Depends on:** 3.4

- In `crates/rozed`: an "Accounts" modal (from the command palette and the app menu) listing
  accounts, with "Sign in with GitHub" (shows the code, copies it, opens
  `https://github.com/login/device` in the default browser, waits, then adds the account) and
  "Import from gh" (lists gh's accounts, imports the chosen ones). Blocked-org and SSO errors show
  GitHub's link.
- **Verify on screen:** sign in as `rozed-bot` through the device flow (approve in Chrome's
  "rozed-bot" profile), see the account listed, restart and see it still there, remove it. For gh
  import, use a throwaway `GH_CONFIG_DIR` signed in as the bot (`gh auth login --with-token` fed
  from the bot's token file), never the user's own gh account.
- Open PR 3a. **Done when:** checks are green, every test passes, and the screenshots show the
  sign-in code, the signed-in account and the import list.

### Task 3.6: Polling

**Depends on:** 3a

- A per-account scheduler on GPUI's background executor (research §7 "Recommended scheme"): the
  open PR every 20 s (free conditional `GET /pulls/{n}`, then the 1-point `PullRequestCheck`, then
  details only when something changed), other open PRs every 2 min, everything every 5 min while
  the app is in the background. Requests per account are serialized; writes are spaced at least 1 s
  apart.
- Rate limit: slow down at 20% left, only the open PR at 5%, pause at zero until the reset, and
  expose the state for a banner (design §3 "Rate limit", §11).
- Tests with a fake clock and the replay client: intervals, change detection, backoff, pause and
  resume.

### Task 3.7: The cache

**Depends on:** 3.6

- Migration: `pr_cache` (account, owner, repo, number, query name → JSON, ETag, fetched at) and
  `recent_prs` (owner, repo, number, title, account, opened at). Only the cache tables are ever
  cleared.
- Opening a PR serves the cached copy at once and refreshes it; offline, the cached copy is
  returned with its age (design §11 "Offline").
- Tests: round-trip, staleness, clearing the cache leaves accounts alone.

### Task 3.8: PR 3b

**Depends on:** 3.7

- Open PR 3b with the polling scheduler and the cache, tested in the `github` crate. Wiring PR
  loading in the app to the `github` crate moves to milestone 5's first task, where the spike's
  diff, `repo_cache` and `github` first meet.
- **Done when:** checks are green and the polling and cache tests cover the cases above.

---

## Milestone 4: Local copy

Design §3 "Local data", §11 "The local copy", §12 "Git". A new `crates/repo_cache` that runs the
`git` CLI as child processes (no libgit2), replacing the spike's local-copy module.

One PR: `feat(repo_cache): keep a partial clone of each repository` — branch
`feat/repo-cache`, stacked on the top of the stack when it starts.

### Task 4.1: Running git

- A `Git` runner: finds `git` (`/usr/bin/git`, then `PATH`), runs it with a clean, predictable
  environment: `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null`, `GIT_TERMINAL_PROMPT=0`,
  `-c credential.helper=`, `LC_ALL=C`. The user's git config is never read or written.
- Authentication through `GIT_ASKPASS`: a small helper script written once into the cache dir
  answers the username prompt with `x-access-token` and the password prompt with the token from an
  environment variable set only on that git child. The token never appears in argv, files or logs.
  Partial clones fetch blobs lazily, so every git call against a repo gets the same environment.
- The git check at launch: `git` missing or older than 2.40 (needed for `merge-tree --write-tree --merge-base`)
  produces an error the app shows with the fix (install the Xcode Command Line Tools).
- Tests: environment isolation (a hostile global config with `url.*.insteadOf` has no effect), the
  askpass helper answering both prompts, version parsing.

### Task 4.2: Clones and fetches

- `RepoCache::open(owner, repo)` → a bare clone with `--filter=blob:none` at
  `<cache dir>/repos/github.com/<owner>/<repo>.git`, created on first use, with a `last_used` stamp.
- `fetch_pull(n)`: fetches `refs/pull/<n>/head` (works for PRs from forks) into
  `refs/rozed/pull/<n>/head` and pins the commit at `refs/rozed/pull/<n>/revs/<sha>`, so every
  revision ever fetched survives force-pushes and `gc`.
- `fetch_branch(name)` into `refs/rozed/branches/<name>`, and `fetch_commit(sha)` for a revision
  rozed knows by SHA but hasn't fetched (a missing one reports "no longer available", design §11).
- Reads: `merge_base`, `changed_files(a, b)` (name-status with renames, binary detection),
  `read_blob(rev, path)` through a long-lived `git cat-file --batch` per repo, `is_ancestor`.
- Progress events for the first clone (design §11 "First open").
- Tests on real temp repos served over `file://` with `uploadpack.allowFilter`: clone, a PR ref,
  a fork PR (a second remote repo's commits under `refs/pull/<n>/head`), a force-push after which
  the old revision is still readable, rename and binary detection.

### Task 4.3: Checkouts and cleanup

- `checkout(pr, sha)`: a detached `git worktree` at
  `<cache dir>/checkouts/github.com/<owner>/<repo>/pull-<n>`, moved to a new head on each push. Only
  called for trusted repos (milestone 10 adds the gate; until then the caller decides).
- `size()` for settings, `clear()` for "clear cache", and `remove_unused(30 days)` run at launch.
- Failure handling: a failed fetch returns a typed error the UI can show and retry (design §11
  "Fetch fails"); a full disk is its own error.
- Tests: checkout moves with a new head, cleanup removes only stale repos and their checkouts.

### Task 4.4: Use it in the app

- Replace the spike's local-copy module in `crates/rozed` with `repo_cache`, passing the account's
  token when one can see the repo.
- **Verify:** `sharkdp/fd#2212` and `rozed-dev/playground#2` (private, as `rozed-bot`) open as
  before, with the head pinned under `refs/rozed/pull/2/revs/`. Force-push survival is proved by
  Task 4.2's tests on local repos; nothing force-pushes to GitHub.

---

## Early logic tasks

Pure decision logic that doesn't compile Zed's crates, built ahead of its milestone so the UI work
later only wires it up. Each is one PR stacked on `build/zed-workspace`, tested with
`cargo test -p <crate>` locally (CI runs the whole workspace). Each crate defines its own input
types; the GitHub and git layers map into them later.

### Task 7.1: Stack detection (`crates/stacks`)

PR: `feat(stacks): find stacks among a repository's pull requests` — branch `feat/stacks-logic`.
Design §4 "What makes a stack", "After a merge", "The stack tree"; §12 "Stacks".

- Input: the repo's PRs (number, state open/merged/closed, base ref, head ref, whether the head is
  in the base repo, head OID, recent commit OIDs oldest→newest, created at, native stack number and
  position if any), plus, for merged PRs, whether their head commit is in the base branch (the
  caller computes it with git).
- Parent of B, first rule that applies: (1) native stack: the PR one position below in the same
  stack; (2) base branch: B's base ref is open PR A's head ref, and A's head is in the base repo;
  (3) shared commits: B's commits contain open PR A's head OID; if several do, the nearest (A's
  head OID closest to B's head). No parent: B's root is its base branch (`main`, `feature-x`).
- Squash-merged lower layer: if B's parent by rule 2 or 3 is a merged PR whose head isn't in the
  base branch, B hangs off the root, marked "needs rebase", with A's head OID as its comparison
  base.
- Output: a forest of trees per root branch; children ordered by creation; `stack_of(pr)`; the
  pre-order walk for `]s`/`[s` (finish one branch before the next); the rule that linked each edge.
  Cycles are broken and reported, never looped on.
- Tests for each rule, each precedence pair, nearest-parent, trees with several children, fork PRs
  (rule 2 never links to a fork head), squash-merged layers, a merge-committed lower layer (no
  rebase needed), cycles, and 1,000 PRs in under 50 ms.

### Task 5.1: Review order and generated files (`crates/review`)

PR: `feat(review): order files for review and spot generated ones` — branch
`feat/review-order`. Design §5 "File order", "Generated files", "Very large files".

- `review_order(files)`: each source file followed by its tests (matched by name: `foo.ts` ↔
  `foo.test.ts`/`foo.spec.ts`, `foo.go` ↔ `foo_test.go`, `foo.py` ↔ `test_foo.py`/`foo_test.py`,
  `foo.rs` ↔ `tests/foo.rs`, and the same for `.tsx`, `.js`, `.jsx`, `.mjs`, `.cjs`), then config
  (JSON, YAML, TOML, dotfiles, anything under `.github/`), then generated files. Unmatched tests go
  after the source files, before config. Ties keep path order. `path_order(files)` too.
- `is_lockfile(path)`: `pnpm-lock.yaml`, `package-lock.json`, `yarn.lock`, `Cargo.lock`, `go.sum`,
  `uv.lock`, `poetry.lock`, `Gemfile.lock`, `composer.lock`, `Podfile.lock`, `Package.resolved`,
  `pubspec.lock`, `mix.lock`, `flake.lock`, `bun.lock`, `bun.lockb`, `packages.lock.json`.
- `initial_state(file)`: collapsed when generated (`linguist-generated` from the caller, or a
  lockfile), or with more than 5,000 changed lines; binary files flagged.
- Tests for every rule above, with realistic mixed PRs.

### Task 6.1: Revisions, the default comparison and viewed marks (`crates/review`)

PR: `feat(review): build revisions and choose what to compare` — branch `feat/revisions-logic`,
stacked on `feat/review-order` (same crate). Design §6; research
[`research/revisions.md`](research/revisions.md); decisions "Revisions".

- Input: push records from several sources, each tagged with its source: activity-API pushes
  (before, after, time, forced), force-push events (before, after, time), review commits (SHA,
  time), check-suite pushes (previous, next, time), and heads rozed's poller saw (SHA, time); plus
  the PR's current commits in order (from git).
- `build_revisions`: merge the sources into revisions keyed by head SHA, ordered by time and by
  ancestry, deduplicated, each with its previous head, whether it was forced and which sources saw
  it. Heads that GitHub no longer has are kept and flagged by the caller as unavailable. Numbering
  (r1, r2, …) is a display helper over the ordered list.
- `default_comparison(revisions, last_review_sha, last_seen_sha)`: the more recent of the two
  markers → latest; neither → full diff; marker is latest → full diff with "Nothing new since rN".
- `viewed_file_comparison(file, viewed_state, local_viewed_at, last_review)`: for a file GitHub
  says changed since viewed, compare from the revision recorded locally when it was marked viewed;
  with no local record, from the last review; viewed and unchanged files are skipped.
- Tests from realistic fixtures: normal pushes only, force-pushes, a rebase, pushes only the poller
  saw, conflicting sources, a review on a since-force-pushed commit, and every branch of both
  comparison functions.

### Task 11.1: Moved-code detection (`crates/diff_extras`)

PR: `feat(diff_extras): detect moved blocks of code` — branch `feat/moved-code`, on
`build/zed-workspace`. Design §5 "Moved code".

- Input: the PR's per-file line changes: path (and previous path), deleted lines with old line
  numbers and text, added lines with new line numbers and text (as a line diff produces them).
- A move is a block of at least 3 non-blank lines deleted in one place and added in another, in the
  same file or another file of the PR. Lines are compared with leading and trailing whitespace
  ignored, so re-indented code still counts. Blank lines can sit inside a block but don't count
  toward the 3.
- Small edits inside a moved block keep it a move: a block may contain changed lines as long as
  most of its non-blank lines match (choose and document the rule; e.g. matched lines are at least
  two thirds of the block and no run of changed lines is longer than 2). The changed lines are
  reported, so the UI highlights only those.
- Each deleted or added line belongs to at most one move; longer matches win. Output per move: from
  (file, old line range), to (file, new line range), and the edited line pairs inside it.
- Tests: within a file, across files, re-indented, with an inner edit, too short (2 lines), blank
  lines not counting, two candidate destinations (the longer wins), a deleted block added twice
  (one move), and a 20,000-line diff in under 100 ms in release mode (or a reasonable debug bound).

### Task 9.1: Why a PR can't merge, and approve and ship (`crates/review`)

PR: `feat(review): explain merge blocks and plan approve and ship` — branch `feat/merge-logic`,
stacked on `feat/revisions-logic`. Design §9 "Merge status", "Merging", "Approve and ship", §11
"Acting on what you saw".

- Input: a merge snapshot the GitHub layer fills from GraphQL and REST: state, draft, `mergeable`,
  `mergeStateStatus`, `reviewDecision`, reviews by user, review requests (with `asCodeOwner`),
  required approving review count and whether code-owner review is required (from branch rules),
  required checks with state and URL (and checks that haven't reported), unresolved thread count,
  whether conversation resolution is required, whether the base requires a merge queue, whether the
  PR is in the queue, auto-merge state, the repo's allowed merge methods, the viewer's permissions
  (`viewerDidAuthor`, can approve, can merge, can merge as admin, can enable auto-merge), whether the
  viewer's approval would count (write access, code owner of which paths), the head SHA GitHub has,
  and the head SHA on screen.
- `blockers(snapshot)`: every reason it can't merge (missing approvals with which code owners,
  changes requested and by whom, each required check failed/running/missing with its URL,
  unresolved conversations, conflicts, out of date, draft, other rules), each marked as something
  waiting will fix (running or missing checks, others' approvals) or not. If GitHub says blocked and
  nothing explains it, an "unexplained" blocker that links to the PR.
- `merge_action(snapshot)`: merge with a method, add to the merge queue, or not possible.
- `approve_and_ship(snapshot)`: the exact plan one confirmation lists: stop before anything if the
  head moved since what's on screen; approve (skipped on your own PRs or when already approved by
  you); then merge, add to queue, turn on auto-merge (only waiting blockers left after your
  approval), or stop after approving with the blockers. `approve` uses the on-screen SHA.
- Tests for every outcome of approve and ship, every blocker kind, queue vs direct merge, own PRs,
  and the moved-head stop.
- Read GitHub's schema for exact field and enum names (it's vendored on `feat/github-client` at
  `crates/github/graphql/schema.graphql`).

### Task 7.2: Planning "Make this a GitHub stack" (`crates/stacks`)

PR: `feat(stacks): plan turning a chain into a GitHub stack` — branch `feat/stack-conversion`,
stacked on `feat/stacks-logic`. Design §4 "Turning chains into GitHub stacks", §9 "Stacks", §11
"Conversion fails partway"; research `research/github-api.md` (Stacks API).

- `plan_conversion(stacks, layer, facts)`: the path to convert runs from the stack's bottom layer up
  through `layer`, then on up while each layer has exactly one open child (a fork in the tree ends
  it). The plan lists every base-branch change (`retarget_to` from 7.1, in bottom-to-top order) and
  then the stack to create, bottom to top. With the path already a native stack, nothing to do; with
  its lower part already a native stack, the plan adds the rest to that stack (`POST
  /stacks/{n}/add`) instead of creating one.
- Not offered, with the reason, when: the path has fewer than 2 PRs, any PR in it is from a fork,
  is in a merge queue, or is in a different native stack, or the repo doesn't have stacks (the
  caller says so after a 404).
- `rollback(plan, done)`: given which steps succeeded, the steps that put base branches back, in
  reverse order, and what can't be undone (a created stack can be unstacked; say so).
- `merge_one_layer_at_a_time(stacks, layer)`, for chains that can't become a GitHub stack: merge the
  bottom PR, then point the next at the root branch and mark it "needs rebase" (design §9 "Stacks").
- Tests for every branch above, including trees, partial native stacks and rollback after each
  failing step.
