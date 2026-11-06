# rozed

rozed ("read-only Zed") is a native macOS app for reviewing GitHub pull requests, built on Zed's
crates as libraries. It's for people who read far more code than they write. It talks to GitHub
directly and has no backend. [`docs/design.md`](docs/design.md) is the approved design: build it as
written. Where it's silent, decide, record the call in [`docs/decisions.md`](docs/decisions.md) with
a line of reasoning, and keep going. [`docs/plan.md`](docs/plan.md) has the milestones and
[`docs/status.md`](docs/status.md) where they stand.

## Principles

These bind every change (design §1).

- **Read-only.** rozed never edits files or pushes commits. It writes to GitHub only to review and
  merge: comments and their attachments, reviews, viewed marks, merges, queue and auto-merge changes,
  and changing a PR's base branch to form a stack.
- **No AI.** No AI features, and none of Zed's AI, agent or model crates.
- **No backend, no telemetry.** rozed talks only to GitHub and to language servers' official download
  sources. Never configure a Zed endpoint or start Zed's account, telemetry or auto-update code.
- **Never runs a repo's code.** No dependency installs or build steps. Language servers run only on
  repos the user trusts.
- **Keyboard first.** Every action has a key.

## Layout

- `crates/<name>/`: rozed's crates, one Cargo workspace. `rozed` is the app. Design §2 lists the
  rest (`github`, `repo_cache`, `stacks`, `review`, `diff_extras`, `review_ui`).
- `third_party/zed/`: Zed, as a submodule. See "Zed submodule".
- `script/`: repo scripts, including the git hooks (`script/hooks/`) and CI checks.
- `docs/`: the design, plan, status and decisions.
- `.claude/skills/`: skills for every agent. `.agents/skills` links to it for Codex.

## Set up

```sh
export PATH="$HOME/.cargo/bin:$PATH" # rustup's cargo, if your shell doesn't add it
mise install                         # Node and pnpm, pinned in mise.toml
pnpm install                         # commitlint and lefthook; installs the git hooks
```

Rust comes from rustup. `rust-toolchain.toml` pins Zed's toolchain, and cargo installs it on first
use. If your shell doesn't activate mise, prefix Node and pnpm commands with `mise exec --`. Run
`pnpm install` in every new clone and worktree, because the `commit-msg` hook runs commitlint from
its `node_modules`.

## Build, test and run

```sh
cargo build -p rozed
cargo run -p rozed
cargo test --workspace
```

## Verify

Run these before every push. CI runs the same on macOS.

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
script/check-symlinks.sh
```

## Zed submodule

_Not added yet. The spike (plan milestone 2) adds `third_party/zed` on `scornz/zed`'s patch branch
and fills in this section: the patch list, the Cargo settings copied from Zed and the script that
keeps them in sync, and how to update Zed._

## Agent rules

There's one set of rules for every agent:

- `CLAUDE.md` is the source. Every `CLAUDE.md` has an `AGENTS.md -> CLAUDE.md` symlink beside it, so
  Codex reads it too.
- Skills live in `.claude/skills/`, with an `.agents/skills -> ../.claude/skills` symlink, the folder
  Codex reads.
- CI checks that both symlinks exist and resolve (`script/check-symlinks.sh`).

## Git

- All changes land through squash-merged PRs. Never commit to or push `main`.
- Commits and PR titles follow Conventional Commits. The PR title becomes the commit on `main`.
  commitlint checks both, in the `commit-msg` hook and in CI.
- Write PR descriptions with the `pr-writing` skill, and say how the change was verified.
- Never bypass the git hooks (`--no-verify`, `LEFTHOOK=0`) and never force-push. To update a branch,
  merge `origin/main` into it.
- No AI attribution (Co-Authored-By, "Generated with") in commits or PRs.
- Never change repository settings, rulesets or branch protection.

## Secrets

Never print, log or commit tokens, passwords or two-factor secrets, including the test bot's. Read
them into the process or environment that needs them without echoing them.
