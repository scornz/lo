# Lo implementation plan

**Design doc:** [`design.md`](design.md). Process and repo rules: [`handoff.md`](handoff.md). Progress:
[`status.md`](status.md). Judgment calls: [`decisions.md`](decisions.md).

**Goal:** Build Lo v1, the native macOS PR reviewer described in the design, as a Cargo workspace
on Zed's crates.

**Architecture:** Lo's own crates (`lo`, `github`, `repo_cache`, `stacks`, `review`,
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
| 1 | **Bootstrap** | Repo conventions from srcs adapted to Rust: `CLAUDE.md` + symlinks, skills, settings, lefthook + commitlint, PR template, `pr-title` and `ci` workflows with `ci-ok`, Dependabot, `LICENSE`, docs. A stub `lo` crate passes fmt, clippy and test on macOS CI. |
| 2 | **Spike** (design §2 "First milestone") | `third_party/zed` submodule on `scornz/zed` branch `rozed/v1.27.0` with a `paths` patch. `cargo run -p lo -- <pr-url>` opens a public PR in Zed's side-by-side diff, with rust-analyzer running on the new side. `docs/spike.md` lists the Zed crates compiled in and shows nothing contacted Zed's servers. Whether GitHub accepts Lo's OAuth token for attachment uploads is answered. Cargo settings copied from Zed are kept in sync by a script that CI checks. Performance numbers recorded. |
| 3 | **GitHub layer** (§3) | `github` crate: device-flow sign-in, `gh` account import, several accounts (Keychain in release, mode-600 file in debug), typed GraphQL checked against GitHub's schema, REST with ETags, polling with backoff and rate-limit tracking, SQLite cache. Unit tests on replayed responses. |
| 4 | **Local copy** (§3 "Local data", §11 "The local copy") | `repo_cache` crate: bare partial clones in `~/Library/Caches/Lo`, PR head/base fetching through `GIT_ASKPASS`, every fetched revision kept under Lo's own refs, checkouts for trusted repos, git check at launch, 30-day cleanup. Git tests on real temp repos, including force-push survival. |
| 5 | **The PR tab** (§5, §10 "Window") | `review_ui` PR tab: header, file list in review order (and path order), side-by-side diff from the local copy with the API diff shown first, generated/lockfile/large files collapsed, binary and image files, renames, whitespace toggle, GitHub viewed marks with `e`. Recent-PRs screen when nothing is open. |
| 6 | **Revisions** (§6) | `review` crate: revisions from the timeline, the revision picker with "your last review" and "last seen here", the default comparison, rebase-aware comparison via `git merge-tree` with fallback, changed-since-viewed. |
| 7 | **Stacks** (§4) | `stacks` crate: the three rules with nearest-parent, trees, native stacks winning, squash-merged layers; the stack tree sidebar, `]s`/`[s`, layer and multi-layer diffs, "needs rebase", one tab per stack. |
| 8 | **Comments and reviews** (§8) | Threads in the diff, comment box (Zed editor, Markdown, Vim, preview), ranges, suggestions, file comments, pending review, `C`, submit dialog, threads panel, `]t`/`[t`, outdated threads, timeline, attachments, unsent text saved locally. |
| 9 | **Merging** (§9, §11) | Merge status line and `m` panel explaining blocks, methods, queue, confirmations, approve and ship (`⌘⇧⏎`) with every outcome tested, stack merges (async), "Make this a GitHub stack" with rollback, queue view and controls, admin override. |
| 10 | **Code features** (§7) | Syntax index served as a built-in language server (Lo in server mode), trust banner and settings, language servers on trusted same-repo PRs only, `gd` peek, `gr`, `K`, jump into the diff, failure handling. Bundled extensions. |
| 11 | **Moved code and the syntax-aware view** (§5) | `diff_extras`: moved-block detection within and across files with links, difftastic-style syntax-aware view with real line numbers and comment mapping. |
| 12 | **Links, keys and settings** (§10) | `⌘O` with clipboard prefill, link-router registration, `lo://`, CLI install, PR link parsing (files/commits/checks/line/comment), keymap file and `?` sheet, settings UI. GPUI tests that every default key maps to an action. |
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

1. Write `CLAUDE.md`: what Lo is (one paragraph, link `docs/design.md`), the principles that bind
   code (read-only, no AI, no backend or telemetry, never runs a repo's code, keyboard first), the
   repo rules from `docs/handoff.md` "Repo rules the user asked for", the workspace layout, how to
   set up (`mise install`, `pnpm install`, Rust from `rust-toolchain.toml`, `PATH` with
   `~/.cargo/bin`), how to build, test and run (`cargo build -p lo`, `cargo test --workspace`,
   `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`), and the
   secrets rule (never print, log or commit tokens, passwords or two-factor secrets). Leave a
   "Zed submodule" section stub for the spike to fill.
2. `.claude/settings.json`: attribution off (`commit: ""`, `pr: ""`, `sessionUrl: false`) and the
   deny rules from srcs's `.claude/settings.json` for pushing `main`, force-pushing and
   `--no-verify` (only those; none of the terraform or nx rules).
3. Copy the `pr-writing` skill verbatim, then fix anything srcs-specific in it (nx, pnpm targets,
   product names) so it reads right for Lo.
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
- Create: `crates/lo/Cargo.toml`, `crates/lo/src/main.rs` (a stub `main` that does nothing
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
