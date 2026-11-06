# rozed handoff

Written 2026-11-06 by the session that designed rozed with the user. Read this, then
[`design.md`](design.md), before anything else. The design is approved: build it as written, and don't
reopen decisions in it. Where it's silent, decide, write the decision down, and keep going.

## Your role: overseer

You oversee the build, and subagents do the work. Your context is for planning, decisions, review and
verification, so don't write code yourself.

- **Plan.** Keep `docs/plan.md` current, and break the current milestone into tasks small enough for one
  subagent each.
- **Delegate on Opus 5.5.** Every code change, commit, push and GitHub change is done by a subagent
  running Opus 5.5: pass `model: "opus"` when you spawn it. Read-only research, such as reading Zed's
  source, may use a smaller model.
- **Brief precisely.** Each brief gives:
  - the goal and the design sections it implements
  - the files involved and the acceptance criteria
  - how to verify it, and the branch to use
  - what to hand back: a PR link plus the verification evidence (test output, screenshots)
- **Run independent tasks in parallel,** each in its own worktree (`isolation: "worktree"`). Keep at
  most two compiling at once, because each worktree's build of Zed's crates takes many minutes and
  about 16 GB. Only one agent controls the desktop at a time.
- **Review before merging.** Read each PR's diff against the design, or have a fresh subagent review
  it without telling it what you expect. Check CI and the verification evidence. If it falls short,
  send it back with specific fixes.
- **Verify every change** yourself or through a subagent. Build it, run the tests, and run the app to
  check the change actually works. Check UI changes on screen. Nothing merges unverified, and every PR
  says how it was verified.
- **Merge.** Squash-merge once checks pass and you're satisfied.
- **Report in `docs/status.md`.** Keep it current with the milestone, what merged, what's next, and
  anything blocked on the user. Record judgment calls in `docs/decisions.md`.
- **Use the computer to test.** The user explicitly allows you and your subagents to take control of
  this devbox's desktop (the `computer` MCP tool) to test and verify. Leave windows you didn't open
  alone (another agent's Codex window, and the user's Chrome and Terminal), and close the ones you
  open when you're done.

## Keep going until it's done

The user is away and won't answer questions. Keep overseeing until every milestone in `docs/plan.md`
is merged and verified.

- **Don't stop to wait.** Never end your turn to ask a question or wait for input, and don't stop after
  a milestone, a merge or a long build. Start the next piece of work.
- **Decide for yourself.** When the design is silent, make the call, record it in `docs/decisions.md`
  with a line of reasoning, and continue.
- **Route around blockers.** If something needs the user, add it to "Waiting on the user" in
  `docs/status.md` and move to the next unblocked work. Come back to it if it unblocks.
- **Change approach when stuck.** After a few failed attempts at the same problem, record what was
  tried in `docs/status.md`, then try a different approach or move on.
- **Keep work flowing.** While subagents, builds and test suites run in the background, plan, brief
  or review the next piece of work.
- **Stay resumable.** Keep `docs/status.md` accurate enough that a fresh session could pick up from it
  alone.
- **Stop only when it's finished:** every milestone is merged and verified, or everything left is
  blocked on the user. Then write a final report in `docs/status.md`, close the windows you opened,
  and stop any app instances, language servers or daemons you started.

## Repo rules the user asked for

- **PRs.** Subagents work on branches and open PRs. You squash-merge them once checks pass and you've
  reviewed and verified them. Nobody commits to or pushes `main`.
- **Conventional Commits** for every commit and PR title, enforced locally (commitlint via lefthook)
  and in CI, like srcs.
- **PR descriptions** follow the `pr-writing` skill. Copy it from srcs
  (`~/src/ash-mac/.claude/skills/pr-writing/`, including `references/examples.md`) into rozed's
  `.claude/skills/pr-writing/`.
- **No AI attribution** in commits or PRs (no Co-Authored-By, no "Generated with").
- **One set of rules for every agent:**
  - `CLAUDE.md` is the source, with an `AGENTS.md -> CLAUDE.md` symlink beside every `CLAUDE.md`.
  - `.claude/skills/` holds the skills, with an `.agents/skills -> ../.claude/skills` symlink, the
    folder Codex reads.
  - CI checks that both symlinks exist and resolve.

## The machine

The `ash-mac` Mac devbox on `scornz-mini` has 64 GB of memory, 18 cores and about 600 GB free. srcs's
`CLAUDE.md` "Devboxes" section applies.

- **Rust 1.98.1** (Zed v1.27.0's pinned toolchain), installed with rustup in `~/.cargo`. It isn't on
  `PATH` by default: `export PATH="$HOME/.cargo/bin:$PATH"`.
- **`cmake` and `protoc`** are installed per user with mise (`~/.local/share/mise/installs`).
  Homebrew isn't writable by this user.
- **Zed fork.** `scornz/zed` is a public fork with all tags, cloned blobless at `~/src/zed` and checked
  out at `v1.27.0` (latest stable, 2026-11-04). Use it to read Zed's source. It's the submodule source
  in the design (`third_party/zed`), and rozed's patch branch goes on it.
- **Xcode 27.2** is selected, with the Metal compiler. git is at `/usr/bin/git`.
- **gh** is signed in as `scornz` through the Coder GitHub App, with access to all of the user's repos.
  Its token has no OAuth scopes, because it's a GitHub App user token.
- **The rozed clone** is at `~/src/rozed`, on `main`. It holds the user's README commit plus these two
  uncommitted docs. Commit them in the bootstrap PR.

## GitHub

- **`scornz/lo`** is public. Its `main` has the user's README commit.
- **The `protect-main` ruleset** is active:
  - PRs only, merged by squash, with linear history
  - no force-pushes and no deleting `main`
  - required checks `pr-title` and `ci-ok`, with branches up to date
- **Merge settings** are applied: squash only (title from the PR title, message from the PR body),
  and merged branches are deleted.
- **`ci-ok`** is an aggregate job that depends on every other CI job, so adding jobs never changes the
  ruleset.
- **OAuth app client ID:** `Ov23liBORqwRdiPzqsPP`. Device flow is on and there's no secret. It's
  public, so commit it.

## Test org and bot

- **The bot** is `rozed-bot` (email `rozed-test@scornz.com`), an Owner of the `rozed-dev` org, which
  the user owns.
- **Its token** is in `~/.config/rozed-dev/bot.env` as `ROZED_BOT_TOKEN`: a classic token with
  `repo`, `workflow`, `read:org` and `delete_repo`.
- **Its password** is in `~/.config/rozed-dev/password`.
- **Its two-factor codes** come from `~/.local/bin/rozed-bot-totp`, which reads
  `~/.config/rozed-dev/totp-secret`.
- **Browser.** Use a Chrome profile named "rozed-bot", signed in as the bot, to approve device-flow
  sign-ins through the GUI. If the profile doesn't exist or is signed out, create it or sign in using
  the password and a code from `rozed-bot-totp`.
- **Never** print, log or commit the token, password or two-factor secret, and never sign in as the
  user.

## Bootstrap PR

Copy srcs's conventions, adapted to a Rust repo:

- `CLAUDE.md` and the symlinks above. Include the rules in this doc, the read-only and no-AI
  principles, and how to build, test and run rozed.
- `.claude/settings.json`: attribution off, plus deny rules for pushing `main`, force-pushing and
  `--no-verify`, as in srcs.
- `lefthook.yml`:
  - commit-msg: commitlint
  - pre-commit: block commits on `main`, and run `cargo fmt` on staged Rust files
  - pre-push: block pushes to `main`
- `commitlint.config.mjs` extending `@commitlint/config-conventional`, and a `package.json` for
  commitlint and lefthook, with Node pinned in `mise.toml`.
- `.github/pull_request_template.md`, copied from srcs.
- `.github/workflows/pr-title.yml` and `ci.yml`:
  - commitlint on the PR's commits
  - on macOS: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`
  - the symlink check
  - the aggregate `ci-ok` job
- `.github/dependabot.yml` for Actions.
- `LICENSE` (GPL-3.0-or-later; copy `~/src/zed/LICENSE-GPL`), the `README.md`, and `docs/design.md`,
  `docs/handoff.md`, `docs/plan.md`, `docs/status.md`.

## Plan

Use the `planning` skill to write `docs/plan.md`:

- a roadmap of milestones with acceptance criteria
- bite-sized detail for the current milestone only

Write each later milestone's detail when it starts, once the earlier ones have shown how Zed's crates
really behave. A suggested order:

1. **Bootstrap** (above).
2. **Spike** (design section 2, "First milestone"): a bare app opening a PR in Zed's side-by-side diff
   with a language server running.
   - Write `docs/spike.md` listing which Zed crates get compiled in, and confirm nothing contacts Zed's
     servers.
   - Check whether GitHub accepts rozed's OAuth token for attachment uploads (design section 8).
   - Read `crates/zed/src/main.rs`, Zed's split diff (`SplittableEditor`) and `git_ui`'s commit view
     first.
3. **GitHub layer:** device-flow sign-in, importing `gh` accounts, several accounts, typed GraphQL, REST,
   polling and the SQLite cache.
4. **Local copy:** partial clones, PR refs, keeping fetched revisions under rozed's own refs, and the
   git check at launch.
5. **The PR tab:** the diff from the local copy, file list in review order, generated files, binaries
   and viewed marks.
6. **Revisions and rebase-aware comparison.**
7. **Stacks:** detection, the tree, layer diffs and stale layers.
8. **Comments, reviews, threads, timeline and attachments.**
9. **Merging:** the merge panel, the queue, approve and ship, stack merges and converting chains to
   GitHub stacks.
10. **Code features:** the syntax index served as a language server, trust, language servers and peek.
11. **Moved code and the syntax-aware view.**
12. **Opening links** (`⌘O`, link routers, `lo://`, the CLI), the keymap and `?`, and settings.
13. **End-to-end tests** on `rozed-dev`, the network allowlist check, performance, and the release
    workflow.

## Learned from a first attempt

A first implementation session ran for about 20 minutes before the user stopped it to change how the
work is done. Its work was deliberately discarded. Start fresh, but keep what it learned:

- **The approach works.** It reached a window showing a real PR's diff (`sharkdp/fd#2212`) in Zed's
  side-by-side view, with rust-analyzer downloaded and running.
- **Cargo reads some settings only from the root workspace.** It ignores a dependency's `[patch]` and
  `[profile]` sections and its `.cargo/config.toml`. rozed needs copies of Zed's, kept in sync by a
  script that CI checks. Zed's `.cargo/config.toml` sets `--cfg tokio_unstable`,
  `-C symbol-mangling-version=v0` and `MACOSX_DEPLOYMENT_TARGET=12.0`.
- **Zed's `paths` crate hardcodes Zed's data, config and cache folders.** rozed needs a patch on its
  Zed patch branch so it uses its own.
- **Some GitHub admin calls get blocked.** The auto-mode permission classifier blocked a repo-settings
  `PATCH` and a remote branch delete. If that happens, record it under "Waiting on the user" and
  carry on.

## Waiting on the user

- **Apple Developer account,** for signed releases, later. Release workflow secrets are
  `MACOS_CERT_P12`, `MACOS_CERT_PASSWORD`, `NOTARY_KEY_ID`, `NOTARY_ISSUER_ID` and `NOTARY_KEY_P8`.
  Until then, builds stay unsigned on the devbox.
- **Zed's trademark terms,** which the user checks before the name goes public in a release.

## Notes from the design session

- **Keychain prompts.** Unsigned rebuilds change the code signature, so macOS asks for Keychain access
  on every rebuild. Debug builds may keep tokens in a file under `~/.config/lo/` with mode 600.
  Release builds use the Keychain.
- **Stacked PRs** are in public preview. Check that `GET /repos/{owner}/{repo}/stacks` works on a
  `rozed-dev` repo. If GitHub needs an opt-in, add it to "Waiting on the user."
- **Attachments.** `gh` 2.99+ `--attach` uploads through an undocumented endpoint. Read how `cli/cli`
  does it.
- **Merge queues** need an org-owned repo, which is why `rozed-dev` exists.
