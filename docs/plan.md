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
mouse, and with `--trust` a file header's "Open File" opens an ordinary editor tab that must stay
read-only. The PR tab (milestone 5) replaces all three; vim's `:w`/`ZZ` and the command palette get the
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

### Task 11.2: The syntax-aware diff's core (`crates/diff_extras`)

PR: `feat(diff_extras): compare files by syntax` — branch `feat/syntax-diff`, stacked on
`feat/moved-code`. Design §5 "Syntax-aware view".

- Port difftastic's structural diff (github.com/Wilfred/difftastic, MIT): turn two tree-sitter trees
  into difftastic's syntax nodes (lists with delimiters, atoms), find the cheapest route through its
  graph of matched and novel nodes (Dijkstra), and mark each side's novel nodes. Keep difftastic's
  MIT copyright notice in the ported files and add it to a `THIRD_PARTY_NOTICES.md`.
- Input: old and new source with their tree-sitter `Tree`s (the caller parses with Zed's grammars);
  depend only on the `tree-sitter` crate at Zed's version, plus a few grammar crates as
  dev-dependencies for tests (Rust, JSON, TypeScript), at the versions in Zed's workspace.
- Output for display: aligned rows of (old line number, new line number), either side possibly
  absent, with the novel byte ranges on each line. Every row uses real line numbers, so a comment
  can be placed on a real line (design §5).
- Limits like difftastic's (graph size, byte size); past them, or with no grammar, return "too
  large" or "no grammar" so the caller keeps the normal diff.
- `nearest_commentable_line(row, side, commentable_lines)`: maps a row to a line GitHub will take
  a comment on, or `None`, so the UI can offer a file-level comment instead.
- Tests ported from difftastic's own where they fit, plus: a renamed variable, a reordered argument
  list, a re-indented block (no change), a moved function, and the size fallbacks.

---

## Milestone 5: The PR tab

Design §5, §6 "Viewed marks", §10 "Window", §11 "The local copy". A new `crates/review_ui` holds the
PR tab; `crates/rozed` keeps startup, windows and menus. This is the first milestone that needs the
GitHub layer, the local copy and the review model together.

**Base:** an integration branch, `integration/pr-tab`, made only of merges of `feat/repo-cache`
(the spike and the local copy), `feat/github-polling` (the GitHub layer) and `feat/revisions-logic`
(the review model: review order and revisions). It gets no commits of its own besides
conflict resolutions, and is updated by merging those branches again when they move. Milestone 5's
PRs target it, so each PR's diff shows only its own change.

Five PRs, in order (5c and 5d can run in parallel once 5b is open):

### 5a `feat(review_ui): open pull requests in tabs` — branch `feat/pr-tab`

- **Wiring:** `rozed <pr-url>` picks the account (`Accounts::account_for_repo`; unauthenticated for
  public repos when none can see it), starts that account's `Poller`, opens the PR from the cache
  at once and refreshes it, prepares the local copy with `repo_cache` using the account's token,
  and builds revisions (`review::revisions`, with the PR's head and `createdAt`). `--trust` stays
  the only way to get a checkout and language servers until milestone 10.
- **The tab** (a `workspace::Item` in `review_ui`): header with title, number, author, the account
  acting, and placeholders for the merge status line and revision picker; left panel with the file
  list in review order (`review::files`) showing viewed marks, a key for path order; middle the
  side-by-side diff (the spike's `pr_diff`, moved into `review_ui`); right an empty threads panel.
  Each panel opens and closes with a key.
- **One window, a tab per PR;** opening a PR that's already open switches to its tab.
- **Read-only gaps from the spike closed:** no welcome page, no tab-bar "+" menu, no "Open File" on
  file headers (or it opens a read-only view).
- **Verify on screen:** `sharkdp/fd#2212` (public, unauthenticated) and `rozed-dev/playground#2`
  (private, signed in as `rozed-bot`) open in two tabs; reopening one switches to it.

### 5b `feat(review_ui): show GitHub's diff while the local copy loads` — branch `feat/api-diff`

- The diff appears straight away from GitHub's per-file patches (`GET /pulls/{n}/files`), read-only
  and without whole-file context, then switches to the local copy's diff when it's ready, keeping
  the scroll position. Progress shows while the copy loads; a failed fetch shows a banner and
  retries (design §11).

### 5c `feat(review_ui): mark files viewed` — branch `feat/viewed-marks`

- `e` marks the file viewed through GraphQL `markFileAsViewed` (via `Poller::write`) and moves to
  the next unviewed file; on a viewed file it clears the mark. The revision at which each file was
  marked is stored locally (SQLite) for `viewed_file_comparison`. `]f`/`[f` move between files,
  `]c`/`[c` between changes. Viewed marks show in the file list.

### 5d `feat(review_ui): collapse, expand and switch how files show` — branch `feat/diff-display`

- Generated files and lockfiles start collapsed; files over 5,000 changed lines start collapsed and
  load on a key (`review::files::initial_state`). Binary files say "binary file changed"; images
  show before and after side by side. Renames show as renames. Keys: `Tab` switches side, one key
  toggles inline/side-by-side, one hides whitespace changes, one expands the whole file.

### 5e `feat(review_ui): list recent pull requests` — branch `feat/recent-prs`

- With no PR open, the window lists PRs opened recently across all accounts (`recent_prs`), opened
  with `enter`.
- When several accounts can see a repo, ask once per owner which to use and remember it (design §3
  "Which account"); 5a takes the first account.
- The window title names the open PR (it reads "empty project" after 5a).
- Offline, a PR whose head the local copy already has shows its diff from the copy (design §11);
  5a's fixes show only the saved details offline.

**Milestone done when:** all five PRs are green, every key above works on screen, and both test PRs
open from a clean state.

### Task 10.1: The syntax index (`crates/symbol_index`)

PR: `feat(symbol_index): index definitions and references by name` — branch `feat/symbol-index`,
stacked on `feat/syntax-diff` (it reuses that branch's tree-sitter workspace dependencies). Design §7
"Syntax-based, on every repo".

- Per file: run the language's tree-sitter tags query (`tree-sitter-tags`, from the same
  tree-sitter git revision Zed pins) over its bytes, giving definitions (name, kind, range, the
  enclosing definition) and references (name, range). Results are keyed by blob ID and reusable
  across commits, so a new commit re-indexes only the blobs it changed.
- Tags queries for Rust, TypeScript/TSX, JavaScript, Python and Go, vendored from each grammar's own
  `queries/tags.scm` at Zed's grammar versions (licenses noted in `THIRD_PARTY_NOTICES.md`). The
  caller passes the `tree_sitter::Language` and query, so the app can supply Zed's grammars.
- `Index` over a commit's files: `definitions(name, from_path, from_text)` ranked per design §7
  (same file first, then same directory, then files a matching import names, then the rest, each
  group in path order); `references(name)`; `symbols_in(path)` for the outline.
- Import matching is a small per-language heuristic (Rust `use`/`mod`, JS/TS `import`/`require`,
  Python `import`/`from`, Go `import`) that maps the requesting file's imports to candidate paths.
- Budgets: skip files over a size limit and generated files; index thousands of files quickly
  (measure on a real repo's files, e.g. this repo's own Rust and a TypeScript project).
- Tests: definitions and references in each language, the ranking with collisions, reuse by blob
  ID, the size skip, and timing.

### Task 6.2: Rebase-aware comparison (`crates/repo_cache`)

PR: `feat(repo_cache): compare revisions across a rebase` — branch `feat/rebase-compare`, stacked on
`feat/repo-cache`. Design §6 "Rebase-aware comparison"; research `research/revisions.md` §5.

- `compare_revisions(old: (base, head), new: (base, head))`: when the bases differ, replay the old
  revision's changes onto the new base with `git merge-tree --write-tree --merge-base=<old base>
  <new base> <old head>`, then diff that tree against the new head, so only the author's changes
  show. When the bases match, a plain `diff old head..new head`. A conflict (exit 1) falls back to
  the plain comparison and says so, with the conflicted paths; any other failure is an error.
- Returns the same `ChangedFile` list the app already renders, plus where each side's text comes
  from (a tree ID for the replayed side), so `read_file` works on it.
- Works in the bare partial clone (merge-tree fetches blobs lazily; batch when possible) and never
  writes refs for the replayed tree beyond a pin that keeps it alive while shown.
- A stacked layer whose lower layer got a new push uses the same function with the lower layer's
  old and new heads as bases.
- Tests on real temp repos: a rebase onto a moved base with one extra change shows only that change;
  a conflicting rebase falls back with the paths; same base is a plain diff; a stacked layer after
  its lower layer's force-push; renames across the rebase.

### Task 8.1: Threads and the pending review (`crates/review`)

PR: `feat(review): place review threads and keep a pending review` — branch `feat/threads-logic`,
stacked on `feat/merge-logic`. Design §8.

- Thread placement: given a thread (path, `line`/`startLine`, `originalLine`/`originalStartLine`,
  `diffSide`, `isOutdated`, `isResolved`, the commit it was written on) and the comparison on screen
  (two revisions), say where it shows: on a line of either side, or "outdated" in the threads panel
  only. A thread shows where it was written when viewing that revision (design §8 "Outdated
  threads"). Unresolved threads start open, resolved ones collapsed.
- Commentable lines: which lines GitHub accepts a comment on for a comparison (read GitHub's docs:
  lines in the diff's hunks, either side, and single- vs multi-line rules), so the UI can offer a
  file-level comment otherwise.
- The pending review model: comments added locally collect into GitHub's pending review (create it
  on the first comment, add to it after), `C` posts one comment at once, the submit dialog's choices
  (Comment, Approve, Request changes; only Comment on your own PR), and the count of pending
  comments.
- Suggestions: `suggestion_block(selected_lines)` builds the ```` ```suggestion ```` block, new side
  only.
- Unsent text: a small store keyed by (account, PR, place) holding drafts until posted (the app
  persists it).
- Tests for placement across revisions (written on r1, viewed at r3 with and without the line
  changing), multi-line threads, commentable lines, the pending review's lifecycle, suggestions and
  own-PR choices.

### Task 13.1: The weekly build against Zed's latest release

PR: `ci: build weekly against Zed's latest release` — branch `ci/weekly-zed`, on `main`. Design §12
"CI and Zed updates".

- A scheduled workflow (weekly, plus `workflow_dispatch`) on macOS: find Zed's latest stable release
  tag (`gh api repos/zed-industries/zed/releases/latest`), check out rozed, move the submodule to that
  tag with rozed's patches replayed on top (cherry-pick the patch commits listed in `CLAUDE.md`'s Zed
  section from `rozed/v<current>`; a patch that no longer applies is itself a breakage to report), run
  `cargo xtask sync-zed --seed-lock`, then fmt, clippy and tests.
- On failure, open or update one issue (label `zed-update`) naming the Zed release, the failing step
  and the first errors; on success, close it. Never pushes branches or changes the repo.
- `script/zed-latest.sh` holds the steps so they run locally too.
- `ci-ok` is unaffected (the weekly job isn't a PR check).
- **Verify:** run `script/zed-latest.sh` locally against the current latest Zed release and report what
  breaks (that list is the start of the next Zed bump); run the workflow once via `workflow_dispatch`
  on the PR branch if GitHub allows dispatching from a branch, else show it with `act`-style dry
  reasoning and actionlint.


---

## Milestones 6–9: how they run

These milestones wire the logic already on `main` into the PR tab. They start once milestone 5 has
landed, from `main`.

- **Lanes.** Builds run in two lane worktrees, `~/src/rozed-worktrees/lane-1` and `lane-2`. Each
  PR in a lane starts a new branch in that worktree from its dependency, so Zed's crates build
  once per lane, not once per branch. A shared `CARGO_TARGET_DIR` wouldn't help: Zed's crates are
  path dependencies, and a different path is a different build. Fixes to a PR under review check
  out its branch in its lane, and only rozed's crates rebuild.
- **Migration slots** in `github::db::RozedDb::MIGRATIONS` are fixed ahead, so parallel PRs never
  take the same index:
  - 2: `viewed_files` (5c)
  - 3: `repo_accounts` (5e)
  - 4: `seen_heads` and `last_seen` (6c)
  - 5: `drafts` (8b)
  - 6: `merge_methods` (9a)

  A slot changes only through this plan, and changing one means deleting the debug database
  (`0-dev`).
- **Writes go only to `rozed-dev` repos.** `sharkdp/fd#2212` is a real third-party PR: it gets no
  comments, reviews, approvals or merges, ever. Milestones 6–9 are checked on it signed in as
  `rozed-bot`, read-only.
- **PRs opened without signing in** keep milestone 5's behaviour. GraphQL needs a token, so
  revisions, stacks, threads and merging say "Sign in to see …".
- **Waves.** Two lanes; the data-layer PRs (6c, 7a, 8a, 8b, 9a, 9b) compile `gpui` but not
  `editor`, so they're quick.

  | Wave | Lane 1 | Lane 2 | Milestone done |
  |---|---|---|---|
  | 1 | 6a | 6c, then F | |
  | 2 | 6b | 8c | |
  | 3 | 6d | 7a, then 8a | |
  | 4 | 6e | 7b | 6 |
  | 5 | 8d | 7c | |
  | 6 | 8b, then 8e | 7d | 7 |
  | 7 | 8f | 9a, then 9b | |
  | 8 | 8g | 9c | |
  | 9 | 8h | 9d | 8 |
  | 10 | 9f | 9e | 9 |

- **Branching.** Each PR stacks on its last dependency. A PR with two open dependencies branches
  from one and merges the other; it never rebases.
- **Conflict hot spots,** and what keeps PRs apart:
  - `pr_tab.rs` is split by 6a. Each feature becomes an entity in its own file, and adds one field
    and one render call to `PrTab`.
  - The header has one fixed slot per feature.
  - Keys: one `bind_keys` per module, plus a GPUI test that fails on duplicate bindings (6a). The
    `PrDiff` context is settled early (8c).
  - `ShownComparison` and `DiffDecorations` (6b) mean later PRs never edit `show_local_copy` or
    the file-header badges directly.
  - `github`'s `queries.rs` becomes `queries/`, with one generic `Poller::read_cached` (6c).
  - `test_support.rs` is split into per-feature request handlers (6a).
  - `CLAUDE.md` gets a "Keys" table, one row per key (6a).

### Fixtures: `test: script the review fixtures on rozed-dev/e2e` — branch `test/e2e-fixtures`

An idempotent script under `script/e2e/`, run as `rozed-bot` through `/opt/homebrew/bin/gh` (check
that `GET /user` returns `rozed-bot` first). It builds nothing.

- **E1 (revisions):** a PR with three normal pushes and the bot's own COMMENT review on r2; the
  script pushes r4 on demand. A rebased revision (r5) comes from GitHub's own server-side rebase
  (GraphQL `updatePullRequestBranch(updateMethod: REBASE)` after `main` moves), so nothing is
  force-pushed. A rebase that resolved a conflict can't be made that way. It's checked read-only
  on a public PR that has one (`cli/cli#14745` was force-pushed and retargeted), and with GPUI
  tests.
- **E2 (stack tree):**
  - a base-branch chain: `a` → `main`, `b` → `a`, `c` → `b`;
  - a sibling `e` → `b`;
  - `d` targeting `main` while containing `c`'s head;
  - a separate two-layer native stack.
- **E3 (needs rebase):** `x` → `main` and `y` → `x`, then `x` squash-merged.
- **E4 (merge states):**
  - two protected bases, kept off `main` so the E1 and E3 scripts can still merge there: `gated`
    (a required `ci` check, one approval, CODEOWNERS) and `queued` (a merge-queue ruleset);
  - a `ci` workflow whose result comes from a file: pass, fail or slow;
  - PRs for a conflict, a draft, and an unresolved thread under required conversation resolution.
- **E5 (PRs the bot can approve):** opened by `github-actions[bot]` through a `workflow_dispatch`
  workflow.

Merging fixture PRs and setting e2e's rulesets happen only in `rozed-dev/e2e`, as `rozed-bot`, to
test rozed.

## Rename to Lo — `refactor: rename the project to Lo` — branch `refactor/rename-lo`

Lands right after milestone 5, before 6a, as one mechanical PR (see decisions, "Project").

- **Fork:** add a patch on the Zed fork branch (append-only) setting `paths`' `APP_NAME` to "Lo", and
  move the submodule pin.
- **Code:** rename the crate and binary `rozed` → `lo` (`crates/lo`), and every identifier, ref,
  environment variable, folder, `rozed://` and the debug Keychain prefix `rozed-dev://` → `lo-dev://`.
  Keep `rozed-dev` only for the test org and `~/.config/rozed-dev/`, plus `rozed-bot` and the fork
  branch `rozed/v1.27.0`.
- **Visible text by hand, not by the script:** every string a person reads says "Lo" with a capital
  L: menus and the Quit item, window titles, error and status messages, sign-in prompts, Keychain
  item labels, and docs prose, even mid-sentence (`Quit rozed` → `Quit Lo`, "rozed couldn't find gh"
  → "Lo couldn't find gh"). Lowercase `lo` only for code names (crate, binary, command, cask, repo,
  refs, URL scheme, `~/.config/lo`, user agent). The script renames code names only, and lists
  every remaining string literal and doc line that mentions the name for a hand pass; a test or check
  fails on a user-visible string containing lowercase `lo` as a word where "Lo" is meant, where
  that's practical.
- **Script:** the rename is a committed, re-runnable script (`script/rename-to-lo.sh`, deleted in a
  follow-up once every open branch has it). Open branches such as 6c and the docs branch merge
  `main` first, then run the script, then fix the lines it lists by hand. Running the script first
  conflicts far more, because the rename PR's hand pass edits the same lines. The script also renames
  hyphenated prose ("rozed-specific" → "lo-specific"), so check each branch's diff for that.
- **Local state:** move `~/Library/Application Support/rozed` and `~/Library/Caches/rozed` to the new
  names with `mv` (never reading token files), or recreate them.
- **Verify:** the full suite; `lo <pr-url>` opens `fd#2212` and `playground#2` with the bot signed in;
  `git grep -i rozed` lists only the kept names; on screen, the menu bar, Quit item, window title,
  sign-in prompt and an error message all say "Lo".
- **The user** renames the GitHub repo to `scornz/lo` (GitHub redirects the old URLs) and the OAuth
  app to "Lo".

## Milestone 6: Revisions

Design §6, §11 "Missing revisions". This milestone wires `review::revisions` (`build_revisions`,
`default_comparison`, `viewed_file_comparison`, `Revisions::mark_unavailable`) and
`repo_cache::Repo::compare_revisions` into the tab. It replaces 5a's stand-in in `PrTab::copied`.
6a and 6b are groundwork that milestones 7–9 also use.

### 6a `refactor(review_ui): give each part of the PR tab its own file` — branch `refactor/pr-tab-parts`

- **Starts only when no other `review_ui` branch is open.**
- **The split:**
  - `pr_tab.rs` becomes `pr_tab/{mod.rs, header.rs, left.rs, middle.rs, right.rs, keys.rs}`.
  - The tests move to `pr_tab/tests/…`.
  - `test_support.rs` becomes `test_support/{github,git}.rs`, with a list of request handlers.
- **Extension points:**
  - the header's ordered slots: stack, revision picker, pending review, merge status;
  - a left slot above the file list, for the stack tree;
  - a middle switch between the diff and the timeline;
  - a right panel host, for threads.
- **Keys:** `keys.rs` holds the context names and calls each feature's `bind_keys`. A GPUI test
  fails when two bindings share a keystroke and a context. CLAUDE.md gets a "Keys" table.
- **Verify on screen:** the test count is unchanged, and `git diff -M --color-moved` shows only
  moves outside the new code. `fd#2212` and `playground#2` behave as in milestone 5, checked with
  every milestone 5 key.

### 6b `refactor(review_ui): show any comparison in the diff and let features decorate it` — branch `refactor/shown-comparison`

- **`ShownPull`** groups the per-PR state. `PrTabEvent` gains `PullChanged`, `HeadChanged` and
  `ComparisonChanged`.
- **`ShownComparison`** (`old` commit or replayed tree, `new`, files, kind `Full` / `Between` /
  `Layer`) replaces `LocalPullRequest`'s fixed `base..head`.
- **`DiffDecorations`:** decorators re-run after each file is added, after a split rebuild
  (`alt-v`), after `alt-w`, and when the local copy replaces GitHub's diff. File headers get a list
  of badges.
- **Verify on screen:** as 6a. `alt-v` and `alt-w` keep the file headers.

### 6c `feat(github): read a pull request's push history` — branch `feat/push-history`

- **Reads:**
  - REST `GET /repos/{head_owner}/{head_repo}/activity?ref=refs/heads/{ref}`, for the head and base
    branches, paged with ETags.
  - GraphQL `PullRequestRevisions`:
    - `createdAt`, `merged`, `mergeCommit { oid parents }`;
    - force-push, base-change and retarget timeline items;
    - submitted reviews with `commit`;
    - your last review;
    - `CheckSuite.push { previousSha nextSha }`.
- **Mapping:** reads map into `review::revisions::PushRecord`. `github` depends on `review`.
- **Storage:** tables `seen_heads` and `last_seen` (slot 4), in `github::revision_marks`.
- **`Poller::read_cached(pull, name, request)`:** refreshes when named fields of the open PR's
  check change, and on the 5-minute refresh. 7a, 8a and 9a reuse it. `queries.rs` becomes
  `queries/`.
- **`repo_cache`:**
  - `Repo::pull_commits(base, head)` (`rev-list --topo-order --reverse`);
  - `Repo::fetch_commits(&[Oid])`, one fetch that pins under `refs/rozed/pull/<n>/revs/`, and treats
    "not our ref" as unavailable.
- **`review::revisions::base_tip_at`** (pure): the base branch's tip when a revision was pushed.
  For a PR merged with a merge commit, it's the first parent.
- **Verify:** recorded fixtures (E1, and `fd#2212` as `rozed-bot`, read from the fork).

### 6d `feat(review_ui): pick the revisions to compare` — branch `feat/revision-picker`

- **Revisions:** `PrTab::copied` builds real revisions, fetches every head and marks the missing ones
  unavailable.
- **The picker** goes in the header slot. Rows show:
  - each rN with its time, a forced badge and its sources;
  - "your last review" and "last seen here";
  - disabled "no longer available" rows;
  - a commit reviewed but never pushed (`Source::ReviewedCommit`) shows as "reviewed, never pushed",
    not as a numbered rN;
  - a "Whole pull request" row.
- **Opening:**
  - The tab opens at `default_comparison`.
  - `Full { nothing_new_since }` shows "Nothing new since rN".
  - `Between` runs through `ShownComparison` with `compare_revisions`.
  - `e` refuses unless the right side is the latest head.
- **"Last seen here"** is written when the tab deactivates or closes. Before the local copy is
  ready, the picker says comparisons wait for it.
- **Keys:** `alt-r` opens the picker. In it: `j`/`k`, `space` sets the left side, `enter` sets the
  right side and applies, and `escape` closes.
- **Verify on screen:**
  - `fd#2212`: r1 (`15635d6`, seen through the maintainer's review) and r2; r1→r2 shows only the
    second commit.
  - E1:
    - it opens r2→r3 with "your last review" on r2;
    - reopening shows "Nothing new since r3";
    - after r4 it opens r3→r4 ("last seen here").

### 6e `feat(review_ui): compare across rebases and open changed files where you left them` — branch `feat/rebase-aware-view`

- **Bases:** each revision's base is the base as it was (`base_tip_at`).
- **How `Replay` shows:**
  - `Clean`: a banner says the comparison spans a rebase and shows only the author's changes.
  - `Retargeted`: worded as decisions says.
  - `Conflicted`: the plain comparison with a banner, and conflicted files badged "resolved a
    conflict". `alt-a` there switches to the replay → head diff.
- **Changed since viewed:** each dismissed file opens at `viewed_file_comparison`, replacing 5c's
  stand-in note with "Changed since you viewed it in rN" or "since your last review in rN".
- **Merged PRs:** the base is the merge commit's first parent.
- **Verify on screen:**
  - E1: mark `a.txt` viewed at r3, then push r4 touching `a.txt` and `b.txt`. `a.txt` shows r3→r4
    with the note.
  - E1's server-side rebase: after `rebase-r5` and `push-r6`, r4→r6 spans the rebase, shows the
    rebase banner, and shows only r6's one hunk in `e1/a.txt`, with no `e1/base.txt`, which the plain
    r4..r6 diff has. r4→r5 alone shows no files.
  - The conflicted case on the public fallback.

**Milestone 6 is done when** 6a–6e are green and the checks above pass on screen.

## Milestone 7: Stacks

Design §4. This milestone wires in `stacks::Stacks::find` and its layers, rules, `needs_rebase` and
`next`/`previous`.

### 7a `feat(github): read what finding stacks needs` — branch `feat/stack-data`

- **`OpenPullRequests` gains:**
  - `id`, `createdAt`, `author`, and `baseRef { target { oid } }`;
  - `commits(last: 20)`;
  - `reviewDecision`, `statusCheckRollup`, `isInMergeQueue` and `mergeQueueEntry`.
- **`MergedPullRequests($headRefName)`** for the branches `branches_to_look_up` returns.
- **Trunks:** from `defaultBranchRef`, branch protection patterns and ruleset `refName` conditions
  (`~DEFAULT_BRANCH`, `~ALL`, fnmatch).
- **`StackLayers(ids)`** reads each layer's review, checks, queue and "n of m viewed" state, and
  refreshes with the open PR.
- **Mapping** into `stacks::PullRequest`, including `State::Merged { head_in_base }`.
- **Verify:** fixtures recorded from playground stack #3 and E2.

### 7b `feat(review_ui): show a pull request's stack as a tree` — branch `feat/stack-tree`

- **The tree** (`StackTree`) goes in the left slot, indented by depth, with children in the order they
  were opened. Each row shows:
  - the title, number and author;
  - review, checks and queue state;
  - "needs rebase";
  - "n of m viewed".
- **Around the rows:**
  - The linking rule shows in a tooltip, and cycles are listed.
  - A note appears when the open list is truncated.
  - The tree is hidden outside a stack.
- **Keys:** `alt-s` shows, hides and focuses the tree. In it: `j`/`k` and `enter`, which for now
  opens a new tab.
- **Verify on screen:** playground#1 (two rows, a native stack), E2 (branches in order), and
  `fd#2212` (no tree).

### 7c `feat(review_ui): keep a whole stack in one tab, walked with ]s and [s` — branch `feat/stack-tab`

- **`TabKey` holds the stack's layers:** a link to any layer switches to that tab at that layer.
- **Switching layers:**
  - It swaps `ShownPull`, keeping each layer's scroll and built diff.
  - The poller follows the shown layer.
  - The header shows the root and "layer k of n", and the titles name the stack.
- **Keys:** `]s`/`[s` walk the stack depth-first. In the tree, `enter` switches layers in place.
- **Verify on screen:**
  - open playground#1, then #2's link: the same tab switches;
  - `]s`/`[s` walk E2 depth-first;
  - a middle layer's link opens the stack at that layer.

### 7d `feat(review_ui): compare each layer with the one below, or several at once` — branch `feat/layer-diffs`

- **A layer's diff** starts from:
  - the lower layer's head that this layer contains;
  - `NeedsRebase::compare_against` when the lower layer was squash-merged;
  - for a bottom layer, the merge-base with its root.
- **GitHub's diff** is skipped (or labelled) first for layers not linked by `BaseBranch`.
- **"Needs rebase"** shows as a banner. A force-pushed lower layer uses `compare_revisions` with its
  old and new heads.
- **Multi-layer diffs** run from a run's bottom base to its top head, along one path. They're
  read-only: no comments and no `e`.
- **Keys:** in the tree, `shift-j`/`shift-k` extend a range and `enter` shows it. `alt-shift-s` shows
  the stack from the bottom up to this layer.
- **Verify on screen:**
  - playground#2 shows only `stack-b`;
  - E2's shared-commits layer `d` shows only its own change;
  - a multi-layer `a..c` diff;
  - E3's "needs rebase".

**Milestone 7 is done when** 7a–7d are green and the checks above pass on screen.

## Milestone 8: Comments and reviews

Design §8, §11 "Unsent text". This milestone wires in `review::threads`, `review::pending` and
`review::drafts`.

### 8a `feat(github): read review threads, your pending review and the timeline` — branch `feat/threads-data`

- **`ReviewThreads`** (paged) maps into `Thread`, adding the `viewer_can_*` fields and comment URLs.
- **`PendingReview($login)`.**
- **`Timeline`** pages the item types design §8 names; other types are read by `__typename` only.
- **Refreshes** go through `read_cached`.
- **`repo_cache::Repo::patch(base, head, path)`** (myers, user config ignored) gives commentable
  lines when GitHub leaves out a patch, and for older revisions.
- **Verify:** recorded fixtures from `fd#2212` and the playground.

### 8b `feat(github): send comments, replies and reviews` — branch `feat/review-writes`

- **An executor for every `review::pending::Mutation`.** `thread: null` counts as a refusal, and
  `UNPROCESSABLE` maps to readable messages.
- **Other writes:** resolve, unresolve, edit and delete comments, plus `addComment`, all through
  `Poller::write`.
- **A `drafts` table** (slot 5).
- **Verify:** fixtures recorded on the playground as the bot.

### 8c `feat(review_ui): turn on vim, with rozed's keys above it` — branch `feat/vim-keys`

- **Vim:**
  - Adds the `vim` crate (with `search`) and updates `script/zed-crates.txt`.
  - Calls `vim::init`.
  - `vim_mode: true` is a default the user can change.
- **A `PrDiff` key context** on the diff editors, through an `Addon`. Reading keys bind at editor
  depth with `Editor && PrDiff && !(Editor && mode != full)`, loaded after vim's keymap.
- **Removed bindings:** `editor::NewlineBelow` and `NewlineAbove`, which would take `cmd-enter`.
- **Comment editors** turn on modal editing when created.
- **Verify on screen:**
  - GPUI tests: every milestone 5 key reaches rozed with vim on.
  - On `fd#2212`: `j`, `k`, `w`, `b`, `gg` and `G` move; `e` marks viewed; `v` starts visual mode.

### 8d `feat(review_ui): show review threads in the diff and the threads panel` — branch `feat/threads-view`

- **Spike first:** at the start, try editors in blocks under removed lines in the inline layout,
  and blocks surviving a split rebuild.
- **In the diff:**
  - `place(thread, &screen)` follows the comparison on screen. `Placement::Lines` puts a block under
    the line, on the right editor or the left one; `Placement::File` puts it on the file header.
  - Resolved threads start collapsed.
  - Bodies are Markdown through Zed's `markdown` crate.
- **The panel** lists `panel(threads, filter, viewer)` with Unresolved, All and Yours, including
  outdated threads and those not on screen. The file list shows counts.
- **Keys:**
  - `]t`/`[t` move between threads.
  - In the panel: `j`/`k`, `enter`, and `u`/`a`/`y` for the filters.
  - `alt-enter` opens or collapses a thread.
- **Verify on screen:**
  - `fd#2212`: threads under their lines, outdated labels, filters, `]t`;
  - a playground PR with a resolved thread: it starts collapsed.

### 8e `feat(review_ui): write comments and suggestions under the line` — branch `feat/comment-box`

- **The comment box** (`CommentBox`): an auto-height editor with Markdown, vim and a preview.
- **Where comments go:**
  - `c` on a line;
  - `v` … `c` on a range (with `RangeError` explained);
  - `alt-c` for the file;
  - `s` for a suggestion.
- **Posting:**
  - `C` posts at once, or joins the pending review and says so.
  - `cmd-enter` in the box sends.
  - `NotLatest` offers to submit, or to switch to the latest revision.
  - A line GitHub won't take offers a file comment.
- **Hunks** come from GitHub's patch, kept after the swap, or from `Repo::patch`.
- **Drafts** save on every change; drafts stranded on older revisions are listed.
- **Keys** in the box: `cmd-enter`, `alt-p` for the preview, and `escape`, which keeps the draft.
- **Verify on screen** on an E4 PR:
  - a line comment;
  - ranges across hunks, and from a removed line to an added one;
  - a suggestion and a file comment;
  - pending shown on GitHub;
  - a half-written box survives a relaunch.

### 8f `feat(review_ui): reply, resolve and submit your review` — branch `feat/submit-review`

- **Threads:** reply, edit and delete in thread blocks, and resolve or unresolve. The header's
  pending-review slot shows the count.
- **The submit dialog** comes from `YourReview::submit_dialog`, with the summary as a draft. It
  explains `OtherRevision`, `NothingToSubmit` and `OwnPullRequest`.
- **Keys:**
  - `cmd-enter` opens and submits the dialog.
  - In a thread: `r` replies, `shift-r` resolves or unresolves, `alt-e` edits, and
    `alt-backspace` deletes after a confirmation.
- **Verify on screen:**
  - On E5 PRs: Request changes, then Approve.
  - On the bot's own PR: Comment only.
  - With a pending review on r1, then a push: Approve is refused.

### 8g `feat(review_ui): show the timeline` — branch `feat/timeline`

- **The timeline** is a virtualized list of the item types. Markdown images load through rozed's own
  loader:
  - the token goes only to `https://github.com/user-attachments/assets/*`;
  - the redirect is followed to S3 without it;
  - other hosts are fetched anonymously.
- **Media:** GIFs play; videos show as links. SVGs and other images go through 5d's preview helper
  process (no file or remote hrefs, pixel caps, deadline, memory cap), never gpui's default
  `SvgRenderer` or image loaders. GIF animation needs the helper to return frames within the same
  budget.
- **A conversation comment box** sits at the bottom.
- **Keys:** `alt-t` switches between the diff and the timeline.
- **Verify on screen:** `fd#2212`'s timeline; a private playground image loads; a comment posted on
  E4.

### 8h `feat(review_ui): attach images and videos to comments` — branch `feat/attachments`

- **Adding a file:** drop, paste or pick into any comment box. Drops are allowed onto comment boxes
  only, reading only the file's bytes; they stay stopped everywhere else.
- **Checks:** type and size (images 10 MB; videos 10 or 100 MB), and `viewerPermission` WRITE or
  above.
- **Upload:** to `uploads.github.com/user-attachments/assets`, inserting the Markdown.
- **Keys:** `alt-shift-a` picks a file.
- **Verify on screen:**
  - The playground (private): a PNG, a GIF and an MP4, in a line comment, the summary and the
    conversation.
  - `fd#2212`: it says write access is needed, and nothing is posted.

**Milestone 8 is done when** 8a–8h are green and the checks above pass on screen.

## Milestone 9: Merging

Design §9, §11. This milestone wires in `review::merge` (`blockers`, `status`, `merge_action`,
`approve_and_ship`, `after_approving`, `after_push`) and `stacks::convert` / `stacks::merge`. Every
merge goes through `merge-async` with `sha` set to the head on screen.

### 9a `feat(github): read what decides a merge` — branch `feat/merge-data`

- **Reads:**
  - a `MergeSnapshot` GraphQL query;
  - REST `rules/branches/{branch}`;
  - `mergeQueue(branch:)`, both its configuration and its entries.
- **Mapping** into `review::merge::MergeSnapshot`:
  - `out_of_date` comes from the local copy under strict status checks;
  - `merge_methods` table (slot 6).
- **A gap to close in `review`:** GitHub refuses a second review while yours is pending, so
  `approve_and_ship` gains `Approval::SubmitPending` when the pending review is on the head on
  screen, and a refusal when it's on another revision.
- **Verify:** recorded fixtures for the E4/E5 states.

### 9b `feat(github): merge, queue and change stacks through GitHub's API` — branch `feat/merge-writes`

- **Async merges:**
  - Submit, then poll the UUID, handling 202 and 200, a 409 that carries the existing UUID, 400 and
    404.
  - The statuses are `pending`, `merged`, `enqueued` and `failed`.
- **Queue and auto-merge:** enqueue (with `expectedHeadOid` and `jump`), dequeue, and enable or
  disable auto-merge.
- **Stacks API:** list (a 404 means the API is off), create, add and unstack, and change a PR's base.
- **`MergeJobs`:** a global model that backs off from 1 s to 10 s and survives the tab closing.
- **Verify:** fixtures recorded on e2e.

### 9c `feat(review_ui): show where a merge stands and why` — branch `feat/merge-panel`

- **Header:** the status line.
- **The panel (`m`):**
  - Blockers, grouped by who they wait on, with links.
  - A note when the account may not see every rule.
  - A method choice (remembered per repository) or "Add to merge queue".
  - An editable squash title and message.
  - "Out of date" with no Update branch button.
- **Merging:**
  - A confirmation names the account, method and target.
  - If the head moved, the tab reopens at the new revision, compared from the one seen.
  - Merging is off offline.
- **Keys** in the panel: `j`/`k`, `tab` cycles methods, `enter` merges after the confirmation, and
  `escape` closes.
- **Verify on screen:**
  - E4's states;
  - a squash merge, then the same method preselected next time;
  - `fd#2212`, read-only.

### 9d `feat(review_ui): approve and ship with one key` — branch `feat/approve-and-ship`

- **The flow on `cmd-shift-enter`:**
  1. A fresh snapshot, then `approve_and_ship`.
  2. A confirmation, with the wording and the stale-approval warning.
  3. Approve, or submit the pending review as an approval.
  4. `after_approving`.
- **After that:** a "clean status" auto-merge refusal reads again and re-runs; after a push,
  `after_push` turns auto-merge off and says so.
- **Tests:** GPUI tests drive every outcome against `FakeGithub`.
- **Verify on screen** on E5:
  - merges;
  - auto-merge, then merges;
  - joins the queue;
  - stops after approving on a failed check;
  - skips approving its own PR;
  - stops before approving when a push lands while the confirmation is open;
  - a pending review is submitted as the approval.

### 9e `feat(review_ui): merge stacks and make chains GitHub stacks` — branch `feat/stack-merges`

- **Merging a layer of a native stack** merges everything below it. The confirmation lists each
  layer, and progress shows per layer. A `failed` result reads the stack again. Bypassing rules is
  allowed for the bottom layer only.
- **"Make this a GitHub stack":** `plan_conversion`, with every `Reason` listed when it's not
  offered. The steps are listed first. It stops at the first failure and offers `rollback`,
  explaining what `Kept` leaves in place.
- **Merging one layer at a time** when conversion isn't offered: `MergeBottom`'s retargets around
  each merge.
- **Keys:** `enter` in the merge panel on a stack; `alt-shift-g` converts.
- **Verify on screen:**
  - an E2-style chain becomes a native stack;
  - merging the top of a three-layer stack lands every layer;
  - rollback is checked in GPUI tests.

### 9f `feat(review_ui): show the merge queue and act on it` — branch `feat/merge-queue`

- **The queue view, per base branch:**
  - entries with position, state and estimate, yours highlighted;
  - what's tested together;
  - removal reasons with the failing check's link.
- **Controls:** remove, jump (when allowed), and auto-merge on or off.
- **Admin override:** you type the PR's number, then it merges with `bypass_rules: true`.
- **Polling:** the poller polls the queue every 15–30 s while the view is open.
- **Keys:** `alt-q`; in the view `j`/`k`, `d`, `J` and `a`. In the merge panel, `alt-enter` is the
  admin override.
- **Verify on screen** on e2e's `queued` base:
  - three PRs queued;
  - remove one;
  - a failed `ci` shows its reason;
  - jump one;
  - the admin override on `main`.

**Milestone 9 is done when** 9a–9f are green and the checks above pass on screen.

### Risks noted while planning

- **Editors in blocks under removed lines, in the inline layout,** and blocks surviving `alt-v`.
  8d starts with a spike.
- **Vim's normal-mode keys** (`c`, `C`, `e`, `v`, `s`, `m`) sit at editor depth and beat tab-level
  keys, hence 8c. Vim's `]` sequences need a test of GPUI's pending timeout.
- **Async stack merges** can land partway. `enqueued` is final before the PR merges.
- **Zed's `markdown`** isn't fully GitHub-flavoured, so the timeline is approximate. The timeline
  union has about 80 types; select only those listed.

---

## Milestones 10–13: how they run

These milestones start as milestone 9 ends. A few PRs move earlier: the ones that need only groundwork 6–9 already lay, and the ones that compile no Zed crate.

- **Lanes.** The same two lane worktrees, and still at most two Zed builds at once.
  - Some work compiles no Zed crate: `repo_cache`, the new `links` crate, `symbol_index`, `xtask`, scripts and workflows. It runs in its own worktree (`~/src/rozed-worktrees/<branch-slug>`) at any time and doesn't count as a build.
  - 13a's local check bundles a lane's existing debug build, and CI builds the release.
- **Waves.** Waves 1–10 are milestones 6–9 as planned. Two of those waves take one extra PR each, in bold.

  | Wave | Lane 1 | Lane 2 | Elsewhere (no Zed build) | Milestone done |
  |---|---|---|---|---|
  | Any time after the rename | | | 10a, 12a, 13a, 13b, 13c, F2 | |
  | 3 | 6d | 7a, then 8a, then **11a** | | |
  | 7 | 8f | 9a, then 9b, then **10g** | | |
  | 11 | 10b | 13d | | |
  | 12 | 10c | 10i | | |
  | 13 | 10d | 10h | | |
  | 14 | 10e | 10f | | 10 |
  | 15 | 11b | 12b | | |
  | 16 | 11c | 12c | | 11 |
  | 17 | 12f | 12d | | |
  | 18 | 12e | 13e | | 12 |
  | 19 | 13f | 13g | | 13 |

  - **11a in wave 3:** it needs only 6b's `DiffDecorations` and 8c's `PrDiff` context. It moves earlier so 6b's decorator hook is tried on a real feature before milestones 7–9 lean on it.
  - **10g in wave 7:** it needs only 6a's banners and header.
  - **If either slips,** it goes first in lane 2 in wave 11, and lane 2 runs two waves later (ending in wave 21).
- **Migration slot** in `LoDb::MIGRATIONS`:
  - 7: `trusted_repos`, `trusted_pulls` and `trust_prompts` (10g).

  The update check (13g) keeps its state in Zed's key-value store (`db::kvp`), so it needs no table.
- **Zed patches** go on `rozed/v1.27.0`, append-only:
  - **6:** `extension_host: let an app load only the extensions it ships` (10i).
  - **7, only if needed:** `editor: let a split diff remember its semantics provider`, about 15 lines in the shape of `02b1c42d6f`. It's needed only if 10c's observer proves unreliable.

  Patch 6 touches a leaf crate. Once it lands, the other lane rebuilds only `extension_host` and `lo`.
- **Writes** still go only to `rozed-dev` repos. Trusting a repository or pull request is local to the Mac, so trusting `sharkdp/fd#2212` writes nothing to GitHub. A fork fixture under `rozed-bot/e2e` would be a write outside `rozed-dev`, which needs the user's OK (see F2).
- **Conflict hot spots,** and what keeps PRs apart:
  - **`pr_diff.rs`:**
    - 10b and 10c touch only `new_diff_editor` and `Target::add_file`.
    - 10g first extracts the trusted part of `show_local_copy` into `start_language_servers`.
    - 11a adds one decorator registration.
  - **6a's middle switch:** 10e adds `Middle::File`, and 11b adds `Middle::Syntax`, in that order.
  - **`crates/lo/src/forced_settings.rs`:**
    - 10d adds `lsp_results_location`;
    - 10g changes the server model;
    - 10h adds Node and hardening;
    - 10i adds extension capabilities.

    Each change adds keys to `FORCED_SETTINGS` and a test row.
  - **`crates/lo/src/main.rs`:**
    - 10g removes `--trust`;
    - 10h adds `NodeRuntime`;
    - 10i adds extension init;
    - 12c adds `on_open_urls` and menus;
    - 12d adds the settings and keymap files;
    - 13g adds the update check.

    Each puts its code in its own module and adds one call in `main`.
  - **`script/zed-crates.txt`:** 10e adds `outline` and `picker_preview`, and 10i adds `extension_host` and `language_extension`. A conflict here is resolved by re-running `script/check-zed-crates.sh --update`.
  - **Keys:** until 12e, PRs bind keys through 6a's per-module `bind_keys`, and 6a's duplicate-binding test applies. 12e moves every module to one catalog. It starts only when no other open branch adds keys.

### Fixtures: `test: script the code, moves and large-PR fixtures on rozed-dev/e2e` (F2), branch `test/e2e-fixtures-2`

These go in `script/e2e/fixtures.sh`, idempotent and run as `rozed-bot` like E1–E5. `status --json` gains every number, head, path hash and comment ID the tests read.

- **E6 (code, same repo, to trust):** a PR on branch `e6/code` that adds and changes a small tree under `e6/`:
  - Rust: `e6/rust/{Cargo.toml, src/lib.rs, src/parse.rs, src/render/parse.rs}`. The two `parse` definitions collide. `lib.rs` has `use serde::Deserialize`, a dependency that's never installed.
  - TypeScript: `e6/ts/src/{index.ts, user.ts, names.ts}` with imports.
  - Python: `e6/py/{app.py, util.py}`.
  - Go: `e6/go/{go.mod, main.go, util.go}`.
  - The PR calls functions defined in files it changes and in files it doesn't, and renames one function, so the old name resolves on the left.
- **E7 (fork PR):** `rozed-bot/e2e:e7/fork` → `rozed-dev/e2e:main`.
  - **Needs the user's OK:** the fork lives outside `rozed-dev`.
  - **Until then:** `sharkdp/fd#2212`, which is already from a fork, stands in, read-only.
- **E8 (hostile, to trust):** a same-repo PR whose files try to run code or steer language servers. Each harmless `e8/evil.sh` writes `$TMPDIR/lo-e8-<name>` if it ever runs:
  - `.cargo/config.toml` with `rustc-wrapper = "e8/evil.sh"`;
  - `rust-toolchain.toml` naming a toolchain that doesn't exist;
  - `.zed/settings.json` pointing rust-analyzer at `e8/evil.sh`;
  - a committed `node_modules/typescript/lib/tsserver.js`;
  - a `pyrightconfig.json` whose venv names `e8/venv/bin/python`;
  - a `go.mod` with `toolchain go1.99`.
- **E9 (moves and syntax):**
  - Moves:
    - a function moved from `e9/api/user.ts` to `e9/api/names.ts`, re-indented, with one line edited;
    - a block moved within `e9/lib.rs`;
    - a 2-line block moved, which isn't a move.
  - Syntax cases:
    - a renamed variable;
    - reordered arguments;
    - a block wrapped in a new `if`;
    - a JSON key reorder.
  - Fallbacks: a file with a parse error, and a 1.2 MB file.
  - A review thread on a moved line, and one on a line only the old side has.
- **E10 (large):** a seeded, deterministic PR with 300 files and about 25,000 changed lines, including one 6,000-line file and a lockfile. Used for performance (13e).
- **E12 (extension languages):** one PR changing a Swift, Kotlin, Terraform, Ruby, Java, C#, PHP, Elixir, TOML, HTML, Dockerfile and Makefile file each.
- **#17 (image bomb):** scripted. It exists on `rozed-dev/e2e` but isn't in `fixtures.sh` on `main` (I grepped and found nothing).

---

## Milestone 10: Code features

Design §7 and §11 "Language servers"; decisions "Code features" and "Spike results"; research `research/code-navigation.md`. This milestone wires `symbol_index` (`Store::missing`, `Store::index`, `Index::definitions`, `Index::references`, `Index::symbols_in`) into the PR tab, and replaces the spike's `--trust` with a trust gate.

**How the index reaches the editor (decided: in the app, no language-server mode).**
- **The provider:** `review_ui::code_semantics::CodeSemantics` implements Zed's public `editor::SemanticsProvider` (`editor.rs:11875`). It wraps the project's own provider (`Rc<dyn SemanticsProvider>`, from the public `impl SemanticsProvider for WeakEntity<Project>`, `editor.rs:11959`).
- **Setting it:** `Editor::set_semantics_provider` (`editor.rs:3315`) sets it:
  - on `SplittableEditor::rhs_editor()` when `pr_diff::new_diff_editor` builds it;
  - on every new `lhs_editor()` (`split.rs:565`).
- **Why the left editor needs watching:** `split()` (`split.rs:717`) builds a new left editor each time the diff splits (on `toggle_split` and when the pane widens). Lo watches for it the way `shown_files::sync_side_by_side` already tracks `ShownFiles.lhs` for image blocks, through 6b's split-rebuild hook.
- **Patches:** none. Patch 7 is the fallback.
- **What the left editor still gets:** it runs with `disable_lsp_data()` (`split.rs:761`). That turns off the cmd-hover underline there but not go-to-definition, hover or highlights, so `gd` and `K` work on both sides.

**What design §7 asks that this approach can't do.**
- **The mechanism sentence.** Neither "rozed starts a copy of itself in a server mode" nor "Zed's editor talks to it like any other language server" holds; decisions already reword this.
- **Zed's own paths that skip the provider.** `editor::FindAllReferences` calls `project.references` directly (`navigation.rs:1363`), and workspace-symbol search has its own path. Both reach only real servers, so Lo captures them and sends them to its own `gr`.
- **Crash isolation.** The index runs in Lo's process. Zed already parses everything in process, so this matters little.
- **Languages from bundled extensions get no syntax navigation (independent of mechanism).** Their grammars are WebAssembly, and `tree-sitter-tags` parses with its own `Parser`, which has no WebAssembly store **(inferred)**. "On every repo" holds per repo, not per language: Rust, TypeScript/TSX, JavaScript, Python and Go are covered.

### 10a `feat(repo_cache): list a commit's files and download them in batches` — branch `feat/repo-tree`

No Zed build; it can run at any time.

- **`Repo::tree(rev) -> Vec<TreeEntry { path, oid, size, mode }>`** runs `git ls-tree -r -z -l --full-tree`.
- **`Repo::download_files(rev, paths)`** generalizes `attributes.rs`'s `download_attribute_files`:
  - it prefetches with `git diff --numstat` from the empty tree, in chunks of paths so argv stays bounded;
  - it then checks `cat-file --batch-check` with `GIT_NO_LAZY_FETCH=1`;
  - offline it downloads nothing and reports what's missing.
- **`Repo::attribute`** answers `linguist-generated` for a whole tree in one `check-attr --stdin` call **(inferred: today it takes a path list)**.
- **Tests:** real temp repos served over `file://` with `uploadpack.allowFilter`:
  - 5,000 files download in a few fetches;
  - an offline open fails cleanly;
  - sizes match `cat-file -s`.

### 10b `feat(review_ui): index the trees on screen by syntax` — branch `feat/code-index`

- **`review_ui::code_index::CodeIndex`,** a global entity:
  - per repo, an `Arc<Mutex<symbol_index::Store>>`;
  - per tree OID, a `Shared<Task<Result<Arc<Index>>>>`.
  - It's keyed by tree, so a replayed merge-tree result from `compare_revisions` indexes too.
- **Grammars:** from Zed's `grammars::native_grammars()` with `symbol_index::queries::{RUST, TYPESCRIPT, TSX, JAVASCRIPT, PYTHON, GO}`. The grammar is chosen through `LanguageRegistry` by path, so matching follows Zed's.
- **Building an index:**
  1. `Repo::tree`.
  2. `Store::missing`.
  3. `Repo::download_files`.
  4. `Store::index` with `Repo::read_blob`, on the background executor.
- **Limits** (a decision to record):
  - the 1 MB and generated skips that `Store` already does;
  - at most 20,000 indexable files or 200 MB downloaded per tree, past which the index is marked partial.

  Offline, only blobs already in the clone are indexed.
- **Wiring:** on 6b's `PrTabEvent::ComparisonChanged`, index `ShownComparison.old` and `.new`. A stack layer's left side is the lower layer's head tree.
- **Memory:** a repo's `Store` is dropped when its last tab closes.
- **Tests:** two commits sharing blobs read each blob once; partial index; offline; skipped files.
- **Verify:** `RUST_LOG=review_ui::code_index=debug` on `fd#2212` and E10 shows both trees indexed with their times, and reopening reuses blobs. There's no UI yet.

### 10c `feat(review_ui): go to definitions by name on both sides of the diff` — branch `feat/code-semantics`

- **`BufferSources`** (per tab):
  - It maps `BufferId → { side, tree, path }`, recorded in `pr_diff::Target::add_file` for the right buffer and for `BufferDiff::base_text_buffer()`. The left side's base-text buffer has no file, and `GitFile` has no revision, so a registry is needed.
  - It maps `(tree, path)` back to that buffer, so a target in a changed file reuses the diff's own buffer, which makes "into the diff" an identity check.
  - `open_buffer(tree, path)` makes read-only in-memory `GitFile` buffers through `memory_buffer` for files outside the diff.
- **`CodeSemantics`:**
  - **`definitions`:** the word at the position (`BufferSnapshot::surrounding_word`, `buffer.rs:4495`), then `Index::definitions(name, from_path, from_text)`, mapped to `LocationLink`s with anchors from `name_range`.
  - **`document_highlights`:** the name's occurrences in that file, from `FileSymbols`.
  - **`hover`:** the definition's line as a code `HoverBlock`, plus "From the syntax index: `src/parse.rs:12`" (decisions).
  - **On a trusted checkout's project buffers:** forward everything to the project provider (10h adds precedence).
  - **Inlay hints, semantic tokens and rename:** forwarded unchanged (`range_for_rename` answers none; Lo is read-only).
- **Wiring:** described under "How the index reaches the editor" above. A test toggles `alt-v` twice and still gets answers on the left.
- **Keys:**
  - **`g d`** → `review_ui::GoToDefinition` in the `PrDiff` context, above vim's `editor::GoToDefinition`. In this PR it jumps into the diff when the top-ranked definition is in a changed file on the same side; otherwise it says how many it found. 10d turns it into the peek.
  - **Jumping:**
    - right side: `MultiBuffer::buffer_point_to_anchor`, with `update_excerpts_for_path` when the point sits in a folded unchanged region (blocked on GitHub's diff);
    - left side: through `base_text_point_to_buffer_point`;
    - then `change_selections(… .nav_history(true))`.
  - **Captured at `PrTab`** so Zed never opens a tab: `editor::GoToDefinition`, `GoToDeclaration`, `GoToTypeDefinition`, `GoToImplementation` and `FindAllReferences` (vim's `g shift-d`, `g y`, `g shift-a`).
- **Verify on screen:**
  - **`fd#2212`, untrusted:**
    - `gd` on a call jumps to its definition in another changed file;
    - the same name on the left resolves in the base tree;
    - `K` shows the definition's line labelled "syntax";
    - resting on a name highlights its uses;
    - `alt-v` twice, then `gd` on the left, still works.
  - **E1 #16:** `gd` in `e1/a.txt` says there's no grammar for this file.

### 10d `feat(review_ui): peek at definitions under the line` — branch `feat/peek`

- **`Peek`:**
  - **The block:** `Editor::insert_blocks` (`editor.rs:8881`, `BlockPlacement::Below`) under the cursor's line, on whichever side asked. In the split, the other side gets Zed's automatic balancing spacer (`block_map.rs:1751`).
  - **Its content:** a read-only `Editor` over a `MultiBuffer` of definition excerpts (±3 lines). It's grouped by `symbol_index::Rank`: "This file", "Same folder", "Imported" and "Elsewhere". Each excerpt is labelled with where the answer came from ("syntax", or the server's name after 10h). Its height is capped at 15 lines, scrolling inside.
  - **Nesting:** the peek's editor has `CodeSemantics`, so `gd` inside it replaces the peek and pushes a nav entry.
  - **Closing:** a split rebuild closes a peek on the left, detected the same way as in 10c.
- **`gd`** now always opens the peek.
- **Cmd-click → peek, with no patch:**
  - Force `"lsp_results_location": "picker"`.
  - `PrTab` then captures `editor::hover_links::OpenDefinitionLocations` (`hover_links.rs:30`, public). `reveal_clicked_links` (`hover_links.rs:394`) dispatches it when every link is a text link and the editor has LSP data on, which the right editor has.
  - The left editor gets no cmd-click, since `disable_lsp_data` removes the underline; `gd` still works there.
- **Keys:**
  - `g d` and cmd-click open the peek.
  - In the peek:
    - `j`/`k` move between definitions;
    - `enter` goes into the diff when the definition is in a changed file on that side, and otherwise (from 10e) opens the file view;
    - `g d` nests;
    - `escape`/`q` closes.
- **Verify on screen:**
  - E6, untrusted:
    - `gd` on `parse` lists both definitions, same folder first;
    - `enter` on one in a changed file jumps into the diff;
    - cmd-click on the right opens the same peek.
  - `fd#2212`: a peek on the left keeps the two sides aligned.

### 10e `feat(review_ui): open whole files read-only in the tab and come back with ⌃o` — branch `feat/file-view`

- **`Middle::File(FileView)`** in 6a's middle switch:
  - a read-only `Editor` over the diff's buffer (changed files) or `BufferSources::open_buffer`;
  - `CodeSemantics` on it;
  - a header naming the path, revision and side, with "Back to the diff (⌃o)".
- **Navigation:**
  - `PrTab` pushes its own `ItemNavHistory` payload, `PrTabNav::{Diff(DiffPosition), File { side, tree, path, point }}`.
  - `PrTab::navigate` (today it forwards to the diff, `pr_tab.rs:3594`) restores its own payloads and passes any other to the visible editor.
  - `⌃o`/`⌃i` (`pane::GoBack`/`GoForward`, already bound) then cross views.
- **Outline:** `g s` calls `outline::toggle(focused_editor, …)` (`outline.rs:31`, public). Adds the `outline` crate.
- **Keys:**
  - `g f` in the diff opens the cursor's file whole, at that side's revision. It replaces vim's `OpenSelectedFilename`, which is already a captured file action.
  - `enter` in the peek.
  - `g s` outline.
  - `⌃o`/`⌃i`.
  - `g d`, `g r` and `K` work inside the file view.
- **Verify on screen:**
  - `fd#2212`:
    - peek a definition in an unchanged file and press `enter`: the file view opens;
    - `⌃o` returns to the same diff line, and `⌃i` goes forward;
    - `g s` lists `src/main.rs`'s outline;
    - `g f` on a changed file shows its whole new version.
  - E6: the left side's `g f` shows the base version.

### 10f `feat(review_ui): list references by name` — branch `feat/references`

- **`ReferencesPanel`** in 6a's right panel host, beside threads (`escape` returns to threads):
  - references grouped by file, each with its line, text, kind and side;
  - the count;
  - "By name: may include other symbols called this" for syntax answers.
- **Sources:**
  - **Trusted and ready:** `project.references(buffer, position)`.
  - **Otherwise:** `Index::references(name)` plus `Index::definitions`.
- **Keys:**
  - `g r` in the diff, the file view and the peek.
  - In the panel: `j`/`k`, `enter` (into the diff, or the file view) and `escape`.
- **Verify on screen:**
  - `fd#2212`: `g r` on a changed function lists uses in changed and unchanged files; `enter` on an unchanged one opens the file view.
  - E6: both `parse` definitions and their callers show.

### 10g `feat: trust a repository before running its language servers` — branch `feat/trust`

- **Store (slot 7)**, keyed by GitHub repository ID, not name, so renames and transfers keep trust:
  - `trusted_repos(repo_id, owner, name, trusted_at)`;
  - `trusted_pulls(repo_id, number, trusted_at)`;
  - `trust_prompts(repo_id, dismissed_at)`.
- **Repository IDs:** `github_api::BaseRepo` gains `id`, and `Head` gains `repo: Option<HeadRepo { id, full_name }>`.
- **The decision:**
  - `trust::TrustStore` (global) gives `decide(base_repo_id, head_repo_id, number)`.
  - Same repo means equal IDs. A deleted fork counts as a fork.
  - Trusted means the repo is trusted and the PR is same-repo, or that PR itself is trusted.
  - It's decided per `ShownPull`, so each layer of a stack tab (7c) decides for itself.
- **Replacing `--trust`:**
  - `main.rs` loses `Options.trust`, `Servers` chosen at launch, and the `USAGE` text.
  - `pr_diff::Trust` is computed per tab.
  - `show_local_copy`'s trusted half becomes `pr_diff::start_language_servers`, callable when trust is granted mid-session.
- **Allowlist and checkout:**
  - The forced layer keeps every language's `language_servers` at `[]`.
  - For a trusted checkout's worktree only, Lo sets local settings that name the allowed servers through `SettingsStore::set_local_settings(worktree_id, InWorktree(RelPath::empty()), Settings, …)` (`settings_store.rs:1064`). Local settings merge over the server layer (`recompute_values`, `settings_store.rs:1369`). Rust gets `rust-analyzer` and the toolchain from `rust_toolchain::find`.
  - This keeps a second line: an untrusted worktree can't start anything. A GPUI test proves the precedence, and that Zed patch 2 (`ignore_worktree_settings_files`) doesn't clear it **(inferred until then)**.
  - The checkout and `trust::trust_checkout` work as today.
- **The banner:**
  - `BannerKind::LanguageServersOff` says "Language servers are off for this repo." It offers "Trust owner/repo", or for a fork "Trust this pull request", and "Dismiss".
  - Dismissing records `trust_prompts`, so the banner asks only once per repo.
  - `pr_banner::BannerMessage.action` becomes `actions: Vec<BannerAction>` to fit two buttons.
  - Trusting first shows a confirmation that says what trust means.
- **The trusted list:**
  - "Trusted Repositories…" in the app menu, and as a command, opens the `TrustList` modal: rows for repos and PRs, with when each was trusted.
  - Removing one stops servers in its open tabs (the checkout's worktree goes) and reopens them from git. 12d embeds this list in Settings.
- **Keys:**
  - `alt-shift-t` opens the trust confirmation for the active tab.
  - In the list: `j`/`k`, `d` (removes after a confirmation) and `escape`.
- **Verify on screen:**
  - E6: the banner shows; trusting checks the PR out and starts rust-analyzer (Zed's own hover, through the forwarding in 10c); the trust survives a relaunch.
  - Dismissing on E1 means it isn't asked again.
  - `fd#2212` offers "Trust this pull request".
  - `playground#2` (private) is trusted and checked out with the bot's token.
  - Removing E6 from the list stops rust-analyzer.

### 10h `feat(review_ui): let language servers answer first, and say when one fails` — branch `feat/language-servers`

- **Precedence in `CodeSemantics`** (research §4 and decisions):
  - **Ready and non-empty:** the server's answer wins.
  - **Starting, errored, or slower than 2 s:** the index answers, labelled "syntax" (and "rust-analyzer is indexing" while `pending_work` is non-empty, `lsp_store.rs:4780`).
  - **Ready but empty:** the import line if the name is imported, otherwise the syntax answer, labelled. This needs `symbol_index::Index::import_of(name, from_path, from_text)`, which this PR adds (`imports::targets` already finds it).
  - **`K`:** the server's type hover; the syntax hover while it starts.
- **Inlay hints** are off by default and toggled with `editor::ToggleInlayHints` (`actions.rs:910`).
- **Status:** a "language servers" slot added after the merge status in 6a's header (design §11 says "status bar"; the tab header is Lo's status line).
  - **States:** downloading, starting, indexing, ready, failed with the error, stopped, not answering after 3 timeouts in a minute.
  - **Sources:** `LanguageRegistry::language_server_binary_statuses()` (`language_registry.rs:1092`) and `Project::language_server_statuses()` (`project.rs:4420`).
  - **Restart** uses `Project::restart_language_servers_for_buffers` (`project.rs:4258`).
  - Syntax navigation keeps working throughout. Servers still stop with the last tab (existing).
- **Servers:**
  - **Allowlist:** Rust → rust-analyzer; TypeScript, TSX and JavaScript → vtsls; Python → basedpyright; Go → gopls (an installed copy first). Never `"..."`, ESLint (it loads the repo's plugins), Tailwind, YAML, JSON or Bash.
  - **Node:** `NodeRuntime::unavailable()` in `main.rs` becomes `NodeRuntime::new`, used only by trusted checkouts' servers.
- **Hardening** (forced settings, key names **(inferred)**, each checked against E8):
  - vtsls never uses the workspace's TypeScript or tsserver plugins;
  - basedpyright never runs an interpreter from the repo;
  - gopls gets `GOTOOLCHAIN=local`, `GOFLAGS=-mod=readonly` and `GOPROXY=off`;
  - rust-analyzer keeps today's environment.
- **Sandbox evaluation** (decisions ask for it): a `sandbox-exec` profile wrapped around server binaries through `lsp.<server>.binary`. The result goes in decisions, and it's built only if it's cheap.
- **Tests:** `project` test-support `FakeLanguageServer`s, to check:
  - a slow server gives the labelled syntax answer, then the server's;
  - a crash gives "stopped" and Restart;
  - an empty answer gives the import line.
- **Keys:**
  - `alt-i` toggles inlay hints.
  - `alt-shift-l` restarts the tab's language servers.
- **Verify on screen:**
  - **E6, trusted:**
    - `gd`, `g r` and `K` in Rust, TypeScript, Python and Go come from servers, with no "syntax" label;
    - just after trusting, they're labelled "syntax", then the label goes;
    - `gd` on `Deserialize` shows its `use` line;
    - `alt-i` shows hints;
    - after `kill -9` of rust-analyzer, the header says stopped and Restart works.
  - **E8, trusted:** after 2 minutes of scrolling and hovering, no `$TMPDIR/lo-e8-*` marker exists.

### 10i `feat: bundle language extensions` — branch `feat/bundled-extensions`

- **Zed patch 6:** `ExtensionStore` takes `ExtensionStoreOptions { installed_dir, extension_api: bool }`.
  - **Why it's needed:** `ExtensionStore::new` always runs `auto_install_extensions` and `check_for_updates` (`extension_host.rs:463-469`), and the latter calls `fetch_extensions_from_api("/extensions/updates")`. In Lo that request would go to `lo.invalid` on every launch and fail the host check.
  - **With `extension_api: false`:** the store never calls the API or installs anything, and it reads a read-only installed folder.
- **Which extensions** (a proposal):
  - **Grammars and servers:** Swift, Kotlin, Terraform, Ruby, Java, C#, PHP and Elixir (design §2).
  - **Grammars only:** TOML, HTML, Dockerfile, Makefile and SQL. Zed's `languages` crate has no TOML or HTML.
- **How they're bundled, with no fetch at runtime:**
  1. `extensions/extensions.toml` pins each extension's repo, revision and licence.
  2. `script/build-extensions` clones each from GitHub at build time and builds it with Zed's `extension_cli` (it needs `wasm32-wasip2` and fetches wasi-sdk from GitHub).
  3. The output, `extension.toml`, `extension.wasm`, `grammars/*.wasm` and `languages/…`, goes into `target/extensions/`.
  4. 13a copies it into `Lo.app/Contents/Resources/extensions/`.
  - Debug builds read `LO_EXTENSIONS_DIR`, filled by `script/build-extensions --dev`.
  - `THIRD_PARTY_NOTICES.md` lists every extension and grammar licence.
- **At runtime,** `crates/lo/src/extensions.rs`:
  - starts the store with the patched options, then `language_extension::init`, which registers grammars, languages and server adapters;
  - forces `granted_extension_capabilities` to exactly the download hosts and npm packages these extensions need;
  - keeps their languages' `language_servers` at `[]` globally;
  - 10g's local settings add the extension servers for trusted checkouts.
- **Verify on screen:**
  - E12, untrusted: every file is highlighted on both sides, and the debug log shows no request at launch.
  - E12, trusted: the Terraform server downloads from `releases.hashicorp.com`.
  - Hardened runtime is checked in 13b.

**Milestone 10 is done when** 10a–10i are green and the checks above pass on screen.

## Milestone 11: Moved code and the syntax-aware view

Design §5 "Moved code" and "Syntax-aware view"; decisions "Moved code" and "Syntax-aware diff". This milestone wires `diff_extras::moved::Moves::find` and `diff_extras::syntax::compare` into the tab. Moves decorate the existing diff through 6b's `DiffDecorations`. The syntax-aware view is a new view in 6a's middle switch, for one file at a time.

### 11a `feat(review_ui): show moved code as moves, linked at both ends` — branch `feat/moved-code-view`

- **A `MovedCode` decorator,** registered with `DiffDecorations`:
  - It builds `FileChanges` from each file's `BufferDiff` hunks (deleted rows from the base text, added rows from the buffer, with their line numbers) and runs `Moves::find` in the background, once per `ShownComparison`.
  - It runs on the local copy's diff only, since GitHub's file list can be cut short.
  - It recomputes after `ComparisonChanged` and `alt-w`, and reapplies on every decorator run: each file added, split rebuilds, and the local-copy swap.
- **Drawing:**
  - moved rows on both sides get a "moved" background (`Editor::highlight_rows::<MovedRows>`, `editor.rs:9300`), toning down the add and delete colours on matching rows;
  - only `Move::edits` lines keep word highlights;
  - a one-line block at each end says "Moved from `api/user.ts:40`" or "Moved to `api/names.ts:12`", and is clickable;
  - a file-header badge counts the moves (6b's badges), and the file list shows a mark.
- **In the inline layout,** deleted rows are drawn by the right editor, so highlights there anchor into the base text **(inferred)**. The PR starts with a spike of whether row highlights paint over Zed's hunk backgrounds in both layouts.
- **Keys:** `g m` jumps to the other end of the move under the cursor, opening a folded file first. Vim's `]m`/`[m` keep their method jumps.
- **Verify on screen:**
  - E9:
    - the cross-file move shows both links;
    - only the edited line has word highlights;
    - `g m` round-trips;
    - the within-file move works;
    - the 2-line block isn't a move;
    - `alt-v` and `alt-w` keep everything.
  - `fd#2212` and E1 #16 show no false moves.

### 11b `feat(review_ui): compare a file by syntax` — branch `feat/syntax-view`

- **`Middle::Syntax(SyntaxView)`** for the cursor's file.
- **Two read-only `Editor`s, side by side,** over the same buffers the diff uses (the left over the file's `BufferDiff::base_text_buffer()`, the right over its buffer).
  - **Why two editors:** Zed has no API to feed its own alignment to `SplittableEditor`. Its companion is `pub(crate)` (`display_map.rs:243`), and `BufferDiff` takes no external hunks.
  - **What it gains:** real line numbers come from the editors' own gutters; `CodeSemantics`, vim, `/`, thread blocks and drafts all work; and `gd`, `g r` and `K` work here.
- **Alignment:**
  - `compare(Language::for_grammar(name), old, new, &Limits::default())` produces the rows.
  - A spacer block goes where a row has no line on one side, so both editors have the same number of display rows.
  - Scroll is locked by mirroring `ScrollPositionChanged` with `set_scroll_position`.
  - Novel ranges get `Editor::highlight_text::<SyntaxNovel>`, and their rows get backgrounds.
  - Unchanged runs over 8 lines fold.
- **Trees:** reparsed in the background with `tree_sitter::Parser` and the native grammar, because Zed's `SyntaxLayer` keeps its `Tree` private (`syntax_map.rs:245`).
- **Fallbacks:** `Fallback::{NoGrammar, TooLarge, ParseErrors}` shows a message with the normal diff for that file.
- **Keys:**
  - `alt-shift-v` opens and closes the view, in the diff and the file list.
  - `]f`/`[f` move to the next or previous file without leaving the view.
  - `⌃o` returns.
- **Verify on screen:**
  - E9: the renamed variable highlights only the name; the reordered arguments; the wrapped block marks only the `if`; the JSON reorder; the parse error and the 1.2 MB file fall back.
  - `fd#2212`: `src/main.rs`'s line numbers match the normal diff.
  - E1 #16: "no grammar".

### 11c `feat(review_ui): comment and show threads in the syntax-aware view` — branch `feat/syntax-view-comments`

This is the interaction with milestone 8.
- **Threads:**
  - `review::threads::place(thread, &screen)` decides as in the diff.
  - `Placement::Lines` puts 8d's thread block under that line, in that side's editor, with a mirrored spacer of the same height in the other editor. The spacer resizes with `resize_blocks` (`editor.rs:8898`) when the thread block does, because the two editors aren't companions.
  - `Placement::File` goes in the view's header; outdated threads stay panel-only.
  - **This asks 8d** to make its thread block hostable in any editor (an editor, buffer and row) and to report height changes. If 8d didn't, this PR refactors it.
- **Comments:**
  - `c`, `v`…`c`, `s` and `alt-c` open 8e's `CommentBox` at `diff_extras::syntax::nearest_commentable_line`.
  - It takes `impl Fn(Side, u32) -> bool` instead of its own `CommentableLines`. The predicate is backed by `review::threads::commentable::CommentableLines`, so 8's rules hold: comments go on the right column's revision, and the left column takes them only in a whole-PR comparison.
  - `None` says GitHub won't take a comment on that line and offers a file comment; the comment is never moved (decision).
  - Ranges use `RangeError`. Suggestions work on the right side only.
- **Drafts** are keyed by place (path, side, line), so a draft follows you between the diff and this view.
- **Keys:** `]t`/`[t` move between threads in the view.
- **Verify on screen:**
  - E9, writing as the bot on e2e: a comment on a novel line shows on GitHub at the same line; a row outside the hunks offers a file comment.
  - E9's existing thread on a moved line sits under the same line in both views.
  - A half-written comment carries between the two views.

**Milestone 11 is done when** 11a–11c are green and the checks above pass on screen.

## Milestone 12: Links, keys and settings

Design §10. The pieces that need macOS plumbing are:
- the URL types in `Info.plist` (13a's bundle);
- registering `lo://` with LaunchServices;
- opening other links with a specific browser;
- possibly pasteboard privacy.

None needs a Zed patch:
- GPUI already has `Application::on_open_urls` (`app.rs:326`), `App::register_url_scheme` (`app.rs:1924`; on macOS it needs a bundle ID and an installed app, `gpui_macos/platform.rs:811-830`), `read_from_clipboard` and `open_url`.
- Zed's `settings_ui` pulls in `agent`, `copilot`, `edit_prediction` and audio, so the settings UI is Lo's own.
- Zed's `install_cli` hard-codes `/usr/local/bin/zed` and its `cli` binary, so Lo writes its own (about 150 lines).

### 12a `feat(links): read every kind of pull request link` — branch `feat/links`

No Zed build.
- **The crate:** a new `crates/links` with no dependencies. `PrLink` moves there, and `review_ui::pr_link` re-exports it.
- **`Link { pull: PrLink, target: Target }`,** parsed from `https://github.com/…` or `lo://github.com/…`. Today `PrLink::from_str` throws away `?` and `#`. Targets:
  - `Files`;
  - `File { path_hash }` from `#diff-<sha256 of the path>` **(inferred, widely observed)**;
  - `Lines { path_hash, side: L|R, start, end }`;
  - `Commits`, and `Commit { sha, file? , line? }` from `/commits/<sha>`;
  - `Checks { check_run_id? }`;
  - `ReviewComment { id }` from `#discussion_r<id>` and `/files#r<id>`;
  - `IssueComment { id }` from `#issuecomment-<id>`;
  - `Review { id }` from `#pullrequestreview-<id>`.
- **GitHub's newer "Files changed" URLs** (for example `/changes`, **inferred**) are recorded live first and added.
- **Also:** `Link::to_https()`, for "Open on GitHub" and the browser fallback.
- **Tests:** a table of real links, and rejections.

### 12b `feat(review_ui): open links at their file, line, commit, check or comment` — branch `feat/open-at`

- **`PrTab::open_at(target)`**, used for new and already-open tabs:
  - files and lines: `pr_diff::scroll_to`, with the line selected on the right side;
  - a path hash is matched by hashing each changed file's old and new paths;
  - a commit opens a new read-only `ShownComparison` kind, `Commit { sha }` (old is its first parent; no comments and no `e`);
  - checks open 9c's merge panel at that check;
  - a review comment opens and scrolls to its thread (8d), or the threads panel when it's outdated;
  - issue comments and reviews open 8g's timeline at that item.
- **`⌘O` with clipboard prefill:** `OpenBox::new` reads `cx.read_from_clipboard()`; if it holds a link, the box is filled and selected.
  - **Risk (inferred):** recent macOS may show a paste-privacy alert for programmatic reads. If it does on the devbox, check `NSPasteboard` `detectPatterns` first, through `objc2`.
- **Keys:** `cmd-o` (existing). In the box, `enter` and `escape`.
- **Verify on screen:**
  - `fd#2212`:
    - a `/files#diff-…R10` link lands on line 10;
    - `/commits/<sha>` shows that commit;
    - a `#discussion_r…` link opens its thread;
    - `/checks` opens the merge panel.
  - E4: a `#issuecomment-…` link opens the timeline there.
  - `⌘O` with a copied PR link is filled in.

### 12c `feat: open links from other apps, lo:// and the lo command` — branch `feat/open-links`

- **`on_open_urls`** is registered before `app.run`. It feeds a channel that's drained once the window exists, because URLs can arrive at launch, as in Zed's `OpenListener`.
- **Handling a URL:**
  - PR links and `lo://` links go to `review_ui::open` and `open_at`.
  - Anything else opens in a browser. If the default handler is a link router (Finicky, Velja or Choosy, matched by bundle ID), it uses the "fallback browser" setting instead (default Safari), through `/usr/bin/open -b <id>`, so a router can't loop the link back to Lo.
- **`Info.plist`** (13a's template) gains `CFBundleURLTypes` for `lo`, `http` and `https`, each with role Viewer:
  - Lo is offered as a handler for web links but never claims to be the default browser;
  - at launch, a bundled Lo calls `register_url_scheme("lo")` only when LaunchServices names another default.
- **CLI:**
  - **The binary:** a new `crates/lo_cli`, binary `lo-cli`, depending only on `links`. It goes in the bundle at `Contents/MacOS/lo-cli`.
  - **What it does:**
    - `lo <link>…` checks each link and runs `/usr/bin/open -a <its own Lo.app> lo://…`, so LaunchServices sends it to the running instance. No IPC is needed, unlike Zed's `zed-cli://` and ipc-channel.
    - It also has `--version` and `--help`.
  - **"Install Command-Line Tool…"** in the app menu links `/usr/local/bin/lo` to the CLI, falling back to `osascript` with administrator rights, as Zed's `install_cli_binary.rs` does. It says when Homebrew already installed `lo`.
  - **Development:** `cargo run -p lo -- <link>` behaves as today.
- **Docs:** `docs/link-routers.md`, with Finicky, Velja and Choosy rules for `github.com/<owner>/<repo>/pull/<n>`.
- **Verify on screen:**
  - with a dev bundle at `~/Applications/Lo Dev.app`:
    - `open "lo://github.com/sharkdp/fd/pull/2212"` opens the PR in the running app;
    - `lo https://github.com/rozed-dev/e2e/pull/16/files` opens E1 #16;
  - a Finicky rule sends a PR link to Lo, and an issue link comes back to Safari;
  - the menu item installs `lo`.

### 12d `feat: settings in settings.json and a Settings tab` — branch `feat/settings`

- **User settings:** `~/.config/lo/settings.json` is loaded and watched (`settings::watch_config_file`), then `SettingsStore::set_user_settings`. Parse errors show as a notification naming the file, and the forced layer still wins.
  - Today `main.rs` reads no user settings file at all.
- **The `SettingsTab` item,** one per window:
  - **Accounts:** the `accounts_ui` list.
  - **Trusted repositories:** 10g's `TrustList`.
  - **Keys:** vim on or off (`vim_mode`), "Open keymap file" (opened in the default text editor with `open -t`, created from a template if missing), and "Show all keys".
  - **Diff:** the default layout, whether whitespace is hidden by default, and whether inlay hints start on.
  - **Appearance:** theme, UI font size, editor font size.
  - **Links:** CLI status and Install, whether `lo://` is Lo's, the fallback browser, and router rules to copy.
  - **Storage:** cache size and "Clear cache" (`RepoCache::size` and `clear`, design §3), and language-server downloads.
  - Writes go through `settings::update_settings_file` into Lo's own config.
- **Keys:**
  - `cmd-,` → `lo::OpenSettings`. Zed's `zed::OpenSettings` stays a captured file action.
  - In the tab: `j`/`k`, `space` and `enter`.
- **Verify on screen:** turning vim off makes `w` move by character in comment boxes; changing the theme; clearing the cache shrinks the shown size; removing a trusted repo works; editing `settings.json` by hand applies live.

### 12e `feat(review_ui): one key catalog, ? for every key, your keymap file, and vim in its place` — branch `feat/keys`

It starts only when no other open branch adds keys.

- **The catalog:** each module's `bind_keys` (6a) becomes `const KEYS: &[KeyRow { keystrokes, action, context, group, help }]`, collected in `keys.rs`.
  - One source binds the keys, renders the `?` sheet, and generates CLAUDE.md's "Keys" table.
  - A test compares that table; `UPDATE_KEYS=1` rewrites it.
- **The keymap file:** `~/.config/lo/keymap.json`, in Zed's format, is loaded after Lo's defaults with `KeybindSource::User` and watched.
  - `review_ui::keymap::load` gains this; today it loads only Zed's defaults minus `FileActions`.
  - Errors name the file.
- **`?` sheet:** a modal grouped as in design §10 (Move, Jump, Act, Code, Review, Find, Panels and View). It shows the live bindings, so your own changes appear.
- **GPUI tests,** with vim on and with vim off:
  - every catalog row's keys, typed in its context on a test `PrTab`, run its action (vim's pending timeout included);
  - every action in Lo's namespaces has at least one key;
  - the sheet lists every row.
- **Vim:**
  - **Lo's keys win in `PrDiff`, the file view, the peek and the syntax view.** Each loses a vim meaning, documented in the sheet:
    - `?` (vim's backward search; use `/`, then `N`);
    - `m` (marks);
    - `e`;
    - `c`/`s`/`v`;
    - `g f`, `g m` and `g r`.
  - **Kept:** `j`/`k`/`w`/`b`/`gg`/`G`, `/`, `shift-k`, `g s`, `]m`/`[m` and `⌃o`/`⌃i`.
  - **Edit keys** (`i a o O A I x d p u r R J . ~ >`) in read-only views are bound to `NoAction` if a test shows vim changes mode on read-only buffers **(inferred)**.
  - **Ex commands** that open, write, split or run a shell (`:e`, `:w`, `:wq`, `:x`, `ZZ`, `:sp`, `:r`, `:!`) are added to `read_only::FileActions` or hidden through `CommandPaletteFilter`, with a test for each.
  - **`/` search:** `BufferSearchBar` is added to pane toolbars if 8c didn't. `main` has no toolbar items yet.
- **Verify on screen:**
  - `?` on `fd#2212` shows every key;
  - remapping `e` to `x` in `keymap.json` takes effect live, and the sheet shows it;
  - `:w` and `:e foo` do nothing in the diff;
  - `/fn` then `n` finds matches.

### 12f `feat(review_ui): find any file with ⌘P` — branch `feat/file-picker`

- **A picker** built on Zed's `picker` and `fuzzy` (Zed's `file_finder` pulls in `channel` and searches only project worktrees, which untrusted PRs don't have). It lists:
  - the PR's changed files first, in review order;
  - then every file of the head tree, from `Repo::tree` through `CodeIndex`.
- **`enter`** on a changed file scrolls the diff to it; on any other file it opens 10e's file view.
- **Keys:** `cmd-p`, then `enter`, `escape` and `ctrl-n`/`ctrl-p`.
- **Verify on screen:**
  - `fd#2212`: `⌘P` "walk" finds `src/walk.rs`, and an unchanged file opens in the file view.
  - E10: 300 files filter instantly.

**Milestone 12 is done when** 12a–12f are green and the checks above pass on screen.

## Milestone 13: End to end and release

Design §12; decisions "Weekly Zed build" and "Project". The weekly job against Zed's latest release is done (#27).

### 13a `build: bundle Lo.app and an unsigned disk image` — branch `build/bundle-mac`

It can run at any time after the rename.
- **`script/bundle-mac [--release|--debug] [--install] [--target-dir <dir>]`** builds `lo` (and `lo-cli` once 12c lands) and puts `Lo.app` together by hand:
  - no `cargo-bundle` (Zed installs a fork of it from git);
  - no bundled git (Lo needs the Xcode Command Line Tools' git, 2.41 or newer).
- **Bundle contents:**
  - `Contents/Info.plist`, from `crates/lo/resources/Info.plist` with versions filled in by `PlistBuddy`. Keys:
    - `CFBundleIdentifier` `com.scornz.lo` (debug: `com.scornz.lo.dev`, named "Lo Dev");
    - `CFBundleName` and `CFBundleDisplayName` "Lo";
    - `CFBundleExecutable` `lo`;
    - `LSMinimumSystemVersion` 12.0;
    - `NSHighResolutionCapable`;
    - `LSApplicationCategoryType` developer tools;
    - the `lo` URL type.
  - `Contents/MacOS/{lo, lo-cli}`.
  - `Contents/Resources/{Lo.icns, extensions/…}`. There's no icon yet, so this ships a placeholder (see "Waiting on the user").
- **Signing:** an ad-hoc signature with hardened runtime and `crates/lo/resources/lo.entitlements`, which hold only `allow-jit` and `allow-unsigned-executable-memory` (wasmtime). None of Zed's camera, contacts or location entitlements.
- **The disk image:** `Lo-<version>-aarch64.dmg` from `hdiutil` (no npm `dmg-license`), with its `sha256`.
- **Release build settings:** `.cargo/bundle-config.toml` modelled on Zed's (`opt-level = "z"`, with the hot crates at 3, and one codegen unit for `lo`), plus `-Wl,-unexported_symbol,__R*`.
- **`script/check-bundle`:**
  - `plutil -lint` and the required keys;
  - `codesign --verify --strict`;
  - `lo --version`;
  - the disk image mounts.
- **`.github/workflows/release.yml`:**
  - runs on `workflow_dispatch` (dry run, any branch) and `push: tags: v*`;
  - a `bundle` job on `macos-26` builds, checks and uploads the artifact.
  - The release build's time on the hosted runner is measured here; the CI job's timeout is 120 minutes.
- **Verify:**
  - `script/bundle-mac --debug --install` bundles a lane's existing build, and `Lo Dev.app` opens `fd#2212` with `open -a "Lo Dev" --args <link>`;
  - a dispatch on the branch produces the disk image, which opens after `xattr -d com.apple.quarantine`.

### 13b `ci: sign, notarize and publish releases once the Apple account exists` — branch `ci/release-signing`
- **Keychain item labels say "Lo":** Zed's Keychain store (`gpui_apple/src/keychain.rs`) sets no
  label, so items show as `lo://github.com/<login>`. Release builds use a token store of Lo's own
  that sets the label "Lo — GitHub (<login>)" (decisions, "Project").
- **The embedded `Info.plist`** that `crates/lo/build.rs` links into the binary for `cargo run` must
  be dropped from the real bundle's executable.

CI only; it can run at any time after 13a.
- **Gated on secrets** `MACOS_CERT_P12`, `MACOS_CERT_PASSWORD`, `NOTARY_KEY_ID`, `NOTARY_ISSUER_ID` and `NOTARY_KEY_P8`:
  1. a temporary keychain imports the certificate;
  2. `codesign --timestamp --options runtime --entitlements` signs `lo-cli`, then `lo`, then the app, then the disk image;
  3. `notarytool submit --wait`;
  4. `stapler staple`;
  5. `spctl -a -vv`.
- **Checked now, without Apple:** a throwaway self-signed identity made in the job (`script/ci/self-signed-identity.sh`) exercises every `codesign` step. `codesign -dv` shows the runtime flag and entitlements, and the signed app launches and loads a bundled extension grammar under the hardened runtime. That proves `allow-jit` is enough. Only notarization and Gatekeeper's verdict wait for the account.
- **Publishing:** a `publish` job (ubuntu, `contents: write`) makes a draft release with the disk image, its `.sha256` and notes. Unsigned builds publish only as `-unsigned` prereleases.
- **The Homebrew cask `lo`:**
  - **The template,** `packaging/homebrew/lo.rb.in`:
    - `arch arm: "aarch64"`;
    - a `url` on the GitHub release;
    - `app "Lo.app"`;
    - `binary "#{appdir}/Lo.app/Contents/MacOS/lo-cli", target: "lo"`;
    - `depends_on macos: ">= :monterey"`;
    - `livecheck` with `github_latest`;
    - `zap` for `~/Library/{Application Support,Caches,Logs}/Lo`, `~/.config/lo` and the saved state.
  - **Publishing it:** the job renders the cask and opens a PR on a tap, `scornz/homebrew-tap`, gated on `TAP_TOKEN`.
  - **Checked now:** `brew style` and `brew audit --cask --strict`, on a local tap.
  - **Why a tap first:** homebrew/cask expects notarized apps (and notability) **(inferred)**. Also check `brew info lo` for a name clash first.

### 13c `build(xtask): record the hosts a run contacts and check them` — branch `build/host-check`

No Zed build; it can run at any time.
- **`cargo xtask host-proxy --log hosts.jsonl`:** a CONNECT proxy on `std::net`, threaded, that logs host, time and client. It refuses plain HTTP and logs it.
- **`cargo xtask check-hosts hosts.jsonl --allow script/e2e/allowed-hosts.toml --log <lo.log>`** fails on any host not listed, on any Zed host, and on any refused-host warning in Lo's log. It reports which listed hosts were seen.
- **The allowlist** names who may contact each host and why:
  - **GitHub:** `api.github.com`, `github.com`, `uploads.github.com`, `avatars.githubusercontent.com`, `release-assets.githubusercontent.com`, `objects.githubusercontent.com`, and the attachment S3 bucket (one exact bucket, **inferred** name to confirm).
  - **For language servers only:** `nodejs.org`, `registry.npmjs.org`, `proxy.golang.org`, and each bundled extension's sources (`releases.hashicorp.com` and the others).
- **`script/e2e/sample-sockets.sh`:** `lsof` of Lo's process tree every second, flagging any socket that bypasses the proxy, as the spike did.
- **Verify:** a debug run on `fd#2212` passes; a test-only request to `example.com` fails the check.

### 13d `test: drive scenarios against rozed-dev/e2e every night` — branch `test/nightly-e2e`

- **Can a GUI be driven headlessly on a hosted runner? Not by synthesizing OS input.** Keystrokes from AppleScript or `CGEvent` need Accessibility permission, and `screencapture` needs Screen Recording. A job can't grant either: both are TCC settings protected by SIP **(inferred)**.
- **What runs instead:** GPUI's own test harness (`TestAppContext` and `VisualTestContext`), which needs no window server or Metal, wired to live GitHub.
- **A new tests-only crate, `crates/e2e`,** behind feature `live`, so `cargo test --workspace` skips it. It builds the real stack:
  - `LoDb` and `RepoCache` in temporary folders;
  - a real `ReqwestClient` behind `ZedHostGuard`;
  - the bot's token from `ROZED_BOT_TOKEN`;
  - `gpui_tokio::init`;
  - `cx.executor().allow_parking()`. Zed's real-filesystem tests use this; real HTTP through it is proved in this PR's first commit **(inferred)**.

  The tests open pull requests through `review_ui::open`, drive them with `simulate_keystrokes`, and assert on `PrTab` state. Fixture numbers come from `script/e2e/fixtures.sh status --json`.
- **Scenarios:**
  - `fd#2212`, anonymous and signed in, read-only.
  - E1: the revision picker, `push-r4`, and changed-since-viewed.
  - E2 and E3: the stack tree, `]s`, and "needs rebase".
  - E4: a line comment, range, suggestion and file comment, submitted, then resolved and deleted.
  - E4 and E5: blockers and approve and ship. E5's PRs are recreated each night.
  - `queued`: the merge queue.
  - An E2-style chain: conversion and rollback.
  - E6: `gd` and `g r`, untrusted.
  - E9: moves and the syntax view.
  - The links in 12b.
  - Writes go only to e2e.
- **`.github/workflows/nightly.yml`:**
  - runs on a schedule and `workflow_dispatch`, never on `pull_request`;
  - holds the `nightly-e2e` concurrency group and `contents: read`;
  - needs a `ROZED_BOT_TOKEN` secret, which the user adds;
  - runs fixture setup, then the suite under `host-proxy`, then `check-hosts`, then uploads the logs;
  - on failure, one `e2e` issue is opened or updated, in the shape of `weekly-zed.yml`'s `issue` job.
- **Verify:** a dispatch on the branch is green; a deliberately broken assertion opens the issue.

### 13e `perf: measure time to the first diff and scrolling, against budgets` — branch `perf/budgets`

- **Timing events:** `lo::timing` emits structured `timing event=… ms=…` lines, replacing today's ad-hoc "after launch" log lines. Events:
  - `window_open`, `github_answer`, `first_file_github`, `local_copy_ready`, `first_file_local`, `all_files_local`;
  - `index_ready`, `syntax_view_ready`, `lsp_ready`.
- **Scroll bench:** `crates/review_ui/benches/diff_scroll.rs` with `#[gpui::bench(fps = 120)]`.
  - It uses `BenchAppContext` with the headless Metal renderer (`gpui` and `gpui_platform` `bench-support`; precedent: Zed's `crates/benchmarks/benches/editor_render.rs`).
  - It builds an E10-sized diff offline from a generated git repo, then pages through it split and inline.
  - It reads frame p95, p99, maximum and foreground work from `BenchReport`.
- **Budgets,** in `script/perf/budgets.toml`. These are proposals, set from the first measurements × 1.25, absolute on the devbox and with a ×2 factor on CI:
  - launch to window, release build: 400 ms;
  - `fd#2212` warm, to the first file: 1.0 s;
  - `fd#2212` cold, to GitHub's diff: 2.0 s; to the local copy's diff: 4 s;
  - E10 warm: first file 1.5 s, all files 6 s;
  - `alt-v`: 300 ms;
  - scrolling: p95 8.3 ms and p99 16.7 ms per frame, none over 33 ms;
  - a syntax answer after the index is ready: 100 ms;
  - `fd#2212`'s head tree indexed: 500 ms; a 10,000-file tree: 5 s (blobs present);
  - the syntax view of a 2,000-line file: 300 ms;
  - moved-code detection on E10: 200 ms;
  - memory with `fd#2212` and E10 open, untrusted: 1 GB.
- **How they're measured:** 13d's live suite and 13f's app run parse the timing events; the benches run nightly; memory comes from `footprint`. The job summary shows a table, and a seeded regression fails the run.

### 13f `test: run the bundled app under the host check` — branch `test/app-smoke`

This needs 12c, 13a and 13c.
- **A probe step first:** can GPUI open a window and render on `macos-26`? GitHub's runners have a logged-in session, so a window can open **(inferred)**. Metal inside their VM is the open question: the job checks for a Metal device, then renders one frame.
- **Driving the app:**
  - A cargo feature `e2e-driver`, never in release builds, reads `LO_E2E_SCENARIO`:
    - open a URL through the same path as `on_open_urls`;
    - wait for a tab to be ready, or a language server;
    - type keys with `window.dispatch_keystroke`;
    - dump `PrTab` state as JSON;
    - quit.
  - Trust is preseeded in a temporary `HOME`, as the spike did.
  - The app binary runs directly with `HTTPS_PROXY`, so it inherits the proxy; a separate `open lo://…` checks LaunchServices.
- **The scenario:**
  1. Open E6 untrusted.
  2. Trust it.
  3. Wait for rust-analyzer, vtsls and basedpyright.
  4. Press `K`.
  5. Open E12, trusted.
  6. Quit.

  Then `check-hosts` and the socket sampling run, and the timing events give launch to window.
- **If the probe fails,** this runs nightly on the devbox through `launchd` or a self-hosted runner instead (the user decides).

### 13g `feat: tell you when a newer Lo is out` — branch `feat/update-check`

- **The check** runs at launch and then daily: `GET api.github.com/repos/scornz/lo/releases/latest`, anonymous, with an ETag, compared with `version()`. Design §1 and §2 promise update checks, but no milestone held them.
- **The note** is a notification, "Lo 0.3.0 is out", with "Download" (the release page). For a Homebrew install it says `brew upgrade --cask lo` instead. There's no self-install; Zed's `auto_update` isn't used.
- **Settings:** `auto_update_check` is on by default. The state goes in `db::kvp`.
- **Verify:** `LO_FAKE_VERSION=0.0.1` shows the note; offline, nothing shows.

**Milestone 13 is done when** 13a–13g are green, the nightly run is green three nights in a row, and every budget holds.

---

### Gaps in the code on `main`, and the PR that closes each

| Gap | PR |
|---|---|
| No user `settings.json` or `keymap.json` is ever loaded (`main.rs`, `keymap::load`) | 12d, 12e |
| `--trust` and the forced `Servers` are global and fixed at launch | 10g |
| `NodeRuntime::unavailable()`, so the TypeScript and Python servers can't run | 10h |
| No `on_open_urls`, bundle, `Info.plist`, entitlements or icon | 12c, 13a |
| `PrLink::from_str` drops `?` and `#`; `OpenBox` has no prefill | 12a, 12b |
| `BannerMessage` has one action | 10g |
| `GitFile` has no revision; base-text buffers have no file | 10c (`BufferSources`) |
| `repo_cache` can't list a tree or download many blobs | 10a |
| `diff_extras::syntax::CommentableLines` duplicates `review::threads::commentable` with weaker rules | 11c |
| `symbol_index` can't say what an import names | 10h |
| No toolbar, so no `BufferSearchBar`, so no `/` search (8c may add it) | 12e |
| `⌘P` and update checks are in the design but in no milestone | 12f, 13g |
| `ShownComparison` has no single-commit kind | 12b |
| Image fixture #17 isn't scripted | F2 |
| No timing instrumentation beyond log lines | 13e |

### Risks noted while planning

- **`ExtensionStore` always calls Zed's API.** Patch 6 is needed. WebAssembly extensions need `allow-jit` under the hardened runtime, which 13b checks.
- **Metal on hosted macOS runners is unproven.** 13f probes first, with the devbox as the fallback.
- **The syntax view is two editors, not Zed's split.** Scroll sync and mirrored spacers for thread blocks of changing height are Lo's code (11b, 11c).
- **Row highlights over Zed's hunk colours,** in the inline layout above all, are untested. 11a spikes them.
- **Blocks under the line in the left editor don't survive a split rebuild.** The peek closes, as 8d's spike may also find.
- **Trusted language servers still run some repo code.** vtsls's workspace TypeScript, Python interpreters, the Go toolchain and module downloads, and Elixir and Swift builds through extensions. 10h hardens each against E8, and the sandbox is evaluated.
- **The first index downloads every blob of a tree.** It's capped per tree (10b), and the index memory has no eviction beyond closing the last tab.
- **Declaring `http`/`https`** lists Lo among the browsers in System Settings. Link routers could loop a link back, which the fallback browser prevents.
- **Clipboard prefill may trigger paste-privacy alerts (inferred).** 12b checks on the devbox.
- **Release builds on the 3-core, 7 GB hosted runners** may be slow or run out of memory. 13a measures; the options are a larger runner or the devbox.
- **The bundle ID `com.scornz.lo` can't change after the first signed release.** Release builds must be signed before anyone stores tokens, or Keychain access-control prompts follow.
- **Homebrew wants notarized apps.** Until the Apple account exists, the cask lives in a personal tap and needs a quarantine workaround.
- **Vim chords and GPUI's pending timeout** in the every-key test (12e).

