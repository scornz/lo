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
  along.
- The network check: method, hosts seen, verdict.
- GitHub findings: attachments and stacks (summarised from `research/github-checks.md`).
- Numbers: clean and incremental local build times, `target/` size, binary size, launch to first
  diff on `fd#2212`, CI times.
- Risks and follow-ups for later milestones.

**Done when:** PR 2b's checks are green and its description links the screenshots and the numbers.
