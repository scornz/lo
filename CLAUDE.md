# Lo

Lo (as in "lo and behold") is a native macOS app for reviewing GitHub pull requests, built on Zed's
crates as libraries. It's for people who read far more code than they write. It talks to GitHub
directly and has no backend. [`docs/design.md`](docs/design.md) is the approved design: build it as
written. Where it's silent, decide, record the call in [`docs/decisions.md`](docs/decisions.md) with
a line of reasoning, and keep going. [`docs/plan.md`](docs/plan.md) has the milestones and
[`docs/status.md`](docs/status.md) where they stand.

## The name

Text a person reads says "Lo", with a capital L, even mid-sentence: menus, window titles, messages,
logs, comments and docs. A lowercase l reads as a capital I in sans-serif fonts. Lowercase `lo` is
only for names that are code: the crate, binary and command, the repository (`scornz/lo`), git refs
(`refs/lo/`), the URL schemes (`lo://`, and `lo-dev://` in debug builds), `~/.config/lo`, the user
agent and environment variables (`LO_*`). `crates/lo/tests/visible_name.rs` fails on a string
literal that says lowercase `lo` as a word. The test org `rozed-dev`, its bot `rozed-bot` and the
Zed fork's first patch branch, `rozed/v1.27.0`, keep the project's first name.

`script/rename-to-lo.sh` renames a branch from before the rename: merge `origin/main`, run it,
then fix by hand the lines it lists, and the Markdown lines it says it changed. On a branch that
changes only docs, run it with `--list` and edit by hand instead: docs about the rename use both
names, and the rewrite can't tell those lines apart. A line that has to say the first name, such as one about
the rename, ends in `<!-- rename:keep -->`. CI runs the script with `--check`, which fails while
any other line outside the kept names says it.

Dev state from before the rename stays in the first name's folders, which Lo never reads. Delete
them and sign in again, or move them with these commands, in bash, with every copy of the app quit.
Release builds' Keychain items under the old `rozed://` scheme stay behind. <!-- rename:keep -->
Delete them in Keychain Access.

```sh
mv ~/.config/rozed ~/.config/lo # rename:keep
mv ~/Library/Application\ Support/rozed ~/Library/Application\ Support/Lo # rename:keep
mv ~/Library/Caches/rozed ~/Library/Caches/Lo # rename:keep
# The database's file, and the key sqlez stores its migrations under.
for db in ~/Library/Application\ Support/Lo/db/0-*/rozed.sqlite; do # rename:keep
  [ -f "$db" ] || continue
  sqlite3 "$db" 'PRAGMA wal_checkpoint(TRUNCATE)' "UPDATE migrations SET domain = 'lo' WHERE domain = 'rozed'" # rename:keep
  for f in "$db"*; do mv "$f" "${f/rozed.sqlite/lo.sqlite}"; done # rename:keep
done
# The clones' pins and files, and the --trust checkouts' links to their clone.
for repo in ~/Library/Caches/Lo/repos/github.com/*/*.git; do
  [ -d "$repo" ] || continue
  git --git-dir="$repo" for-each-ref --format='create refs/lo/%(refname:strip=2) %(objectname)%0adelete %(refname) %(objectname)' refs/rozed/ | git --git-dir="$repo" update-ref --stdin # rename:keep
  for name in last-used unavailable; do
    if [ -e "$repo/rozed-$name" ]; then mv "$repo/rozed-$name" "$repo/lo-$name"; fi # rename:keep
  done
done
for checkout in ~/Library/Caches/Lo/checkouts/github.com/*/*/pull-*; do
  [ -d "$checkout" ] || continue
  repo=${checkout%/pull-*}
  git --git-dir="${repo/checkouts/repos}.git" worktree repair "$checkout"
done
# The debug token store, re-keyed without printing a token.
(cd ~/.config/lo && umask 077 && jq 'with_entries(.key |= sub("^rozed-dev://"; "lo-dev://"))' credentials.json > credentials.json.new && mv credentials.json.new credentials.json) # rename:keep
```

## Principles

These bind every change (design §1).

- **Read-only.** Lo never edits files or pushes commits. It writes to GitHub only to review and
  merge: comments and their attachments, reviews, viewed marks, merges, queue and auto-merge changes,
  and changing a PR's base branch to form a stack.
- **No AI.** No AI features, and none of Zed's AI, agent or model crates.
- **No backend, no telemetry.** Lo talks only to GitHub and to language servers' official download
  sources. Never configure a Zed endpoint or start Zed's account, telemetry or auto-update code.
- **Never runs a repo's code.** No dependency installs or build steps. Language servers run only on
  repos the user trusts.
- **Keyboard first.** Every action has a key.

## Layout

- `crates/<name>/`: Lo's crates, one Cargo workspace. `lo` is the app. Design §2 lists the
  rest (`github`, `repo_cache`, `stacks`, `review`, `diff_extras`, `review_ui`), plus
  `symbol_index`, the syntax-based symbol index (design §7).
- `crates/xtask/`: repo tasks, run as `cargo xtask <task>` from the repository's root. It's its own
  Cargo workspace, with its own `Cargo.lock`, so it runs even when Lo's workspace can't resolve
  (an empty `third_party/zed`, or a Zed update that needs a new patch).
- `third_party/zed/`: Zed, as a submodule. See "Zed submodule".
- `script/`: repo scripts, including the git hooks (`script/hooks/`) and CI checks.
- `docs/`: the design, plan, status and decisions.
- `brand/`: Lo's visual identity: the mark, wordmark, lockups, the README's banner, and color and
  type tokens. `brand/README.md` holds the rules for the mark, colors and type; follow it in
  anything that shows them.
- `.claude/skills/`: skills for every agent. `.agents/skills` links to it for Codex.

## Set up

```sh
export PATH="$HOME/.cargo/bin:$PATH"  # rustup's cargo, if your shell doesn't add it
mise install                          # Node, pnpm and cmake, pinned in mise.toml
pnpm install                          # commitlint and lefthook; installs the git hooks
git submodule update --init --depth 1 # Zed, one commit deep; see "Zed submodule"
```

Rust comes from rustup. `rust-toolchain.toml` pins Zed's toolchain, and cargo installs it on first
use. Run `pnpm install` and `git submodule update --init --depth 1` in every new clone and
worktree: the `commit-msg` hook runs commitlint from its `node_modules`, and nothing builds without
Zed.

Building needs Xcode (not just the Command Line Tools), because gpui compiles Metal shaders, and
cmake, because Zed's tree-sitter builds wasmtime. If your shell doesn't activate mise, run cargo,
Node and pnpm through `mise exec --` (e.g. `mise exec -- cargo build -p lo`). mise's cmake shim
fails when a build script calls it from outside the repo, so cargo needs mise's real cmake on
`PATH`, which activation or `mise exec` provides.

## Build, test and run

```sh
cargo build -p lo
cargo run -p lo -- https://github.com/sharkdp/fd/pull/2212           # a public pull request
cargo run -p lo -- --trust https://github.com/sharkdp/fd/pull/2212   # with rust-analyzer
cargo test --workspace
```

`lo <pull request link>` shows GitHub's diff of the pull request at once, changed lines only,
while it copies the pull request into a partial clone per repository under
`~/Library/Caches/Lo/repos/`. Then the copy's diff of whole files replaces it in Zed's
side-by-side diff, at the same file and line, both sides read from git, with no language server. `--trust` also checks the head out under `checkouts/` and runs
rust-analyzer there. rust-analyzer runs cargo and rustc in the checkout, which a repository can
point at its own programs, so only pass `--trust` for a pull request whose code you'd run (see
`docs/spike.md`, "Trust risks"). Without `--trust`, no language server can start at all. Milestone
10's trust prompt replaces the flag. Each pull request opens in its own tab; `cmd-o` opens another
from its link. With none open, the window lists the pull requests opened recently, where enter
opens one, and Lo starts there without a link.

In the diff, generated files, lockfiles and files over 5,000 changed lines or 1 MiB start collapsed,
each header saying why; `alt-a` opens one. "Keys" lists every key.

The local copy is the `repo_cache` crate, which runs git with an environment of its own: none of
your git config, HTTPS only. A pull request opens with the GitHub account that can see its
repository ("Accounts…" in the app menu signs in or imports from `gh`), whose token also signs git
in, and without signing in when none can and GitHub gave no other reason, which works for public
repositories. When several accounts can see it, Lo asks once per owner which to use, or for that
repository alone when the owner's account can't see it. Offline, a pull request whose saved head and
base the local copy has shows its diff from the copy, read without downloading, and the tab loads
from GitHub by itself once it answers again.

Lo keeps its files in `~/.config/lo`, `~/Library/Application Support/Lo`,
`~/Library/Caches/Lo` and `~/Library/Logs/Lo`, never Zed's. It logs to
`~/Library/Logs/Lo/Lo.log`, and to the terminal when it has one. `RUST_LOG` sets the levels:
`RUST_LOG=info,lo=debug` also logs the host and path of every HTTP request and redirect.

## Keys

Lo binds its keys over Zed's. Each module binds its own in its `bind_keys`: `review_ui`'s
`bind_keys` (`crates/review_ui/src/lib.rs`) calls those of `review_ui`'s modules, the PR tab's calls
its parts' (`pr_tab/keys.rs`), and the Accounts modal binds its own (`crates/lo/src/accounts_ui.rs`).
Keys for reading the diff go through `pr_tab::keys::reading`. Tests in both crates read the
context stack of each place in the window that takes keys, from a window showing it. They fail when
one of Lo's bindings holds in none of the places, so a binding in a new context needs a place, and
when two of Lo's bindings both hold in one with the same keystrokes, or with one's starting the
other's (`review_ui::key_clashes`). A key that's added, changed or removed changes its row here.

Where a key works, in the PR tab (`crates/review_ui/src/pr_tab/keys.rs`): *tab* is anywhere in it
(`KEY_CONTEXT`), *reading* is the tab outside any text box in it (`READING_CONTEXT`), *diff* is the
diff's editors (`DIFF_CONTEXT`), and *file list* is the file list (`FILE_LIST_CONTEXT`). The tab's
context says what its middle shows, `middle=diff` or `middle=view`, and keys that act on the diff
hold only while it shows: *diff*, *reading the diff* (`DIFF_READING_CONTEXT`) and *file list, the
diff showing* (`DIFF_FILE_LIST_CONTEXT`). Elsewhere:
*window* is anywhere in Lo's window (`Workspace`), *open box* is the box `cmd-o` opens
(`OpenPullRequest > Editor`), *recent list* is the list of recent pull requests
(`RecentPullRequests`), *account picker* is the question about which account to use
(`AccountPicker`), and *accounts* is the Accounts modal (`AccountsModal`).

| Key | Where | Action | What it does |
|---|---|---|---|
| `cmd-o` | window | `review_ui::OpenPullRequest` | Asks for a pull request's link and opens it in a tab, or switches to its tab |
| `enter` | open box | `menu::Confirm` | Opens the link typed |
| `escape` | open box | `menu::Cancel` | Closes the box |
| `cmd-b` | tab | `review_ui::ToggleFileList` | Shows or hides the file list |
| `cmd-alt-b` | tab | `review_ui::ToggleThreads` | Shows or hides the threads panel |
| `cmd-r` | tab | `review_ui::ToggleThreads` | Shows or hides the threads panel |
| `ctrl-o` | tab | `pane::GoBack` | Goes back to where the diff's cursor was before it jumped |
| `ctrl-i` | tab | `pane::GoForward` | Goes forward again |
| `e` | reading the diff | `review_ui::ToggleViewed` | Marks the current file viewed and moves to the next unviewed file, or clears a viewed file's mark |
| `] f` | reading the diff | `review_ui::NextFile` | Moves to the next file in the diff |
| `[ f` | reading the diff | `review_ui::PreviousFile` | Moves to the previous file in the diff |
| `] c` | reading the diff | `review_ui::NextChange` | Moves to the next change, in whichever file it is |
| `[ c` | reading the diff | `review_ui::PreviousChange` | Moves to the previous change, in whichever file it is |
| `alt-o` | reading | `review_ui::ToggleFileOrder` | Switches the file list between review order and path order |
| `tab` | diff | `review_ui::SwitchSide` | Side by side, moves the cursor to the diff's other side |
| `alt-v` | diff; file list, the diff showing | `review_ui::ToggleDiffLayout` | Shows the diff inline or side by side |
| `alt-w` | diff; file list, the diff showing | `review_ui::ToggleWhitespace` | Hides changes to whitespace alone, or shows them again |
| `alt-a` | diff; file list, the diff showing | `review_ui::ExpandFile` | Opens the file at the cursor, or the one selected in the file list, when it's collapsed, loads it when it's too large to have loaded, and otherwise switches it between its changes and its whole text |
| `o` | file list | `review_ui::ToggleFileOrder` | Switches the file list between review order and path order |
| `j` | file list | `menu::SelectNext` | Selects the next file, and scrolls the diff to it |
| `k` | file list | `menu::SelectPrevious` | Selects the previous file, and scrolls the diff to it |
| `enter` | file list | `menu::Confirm` | Scrolls the diff to the selected file and moves to the diff |
| `escape` | file list | `menu::Cancel` | Moves back to the diff |
| `j` | recent list | `menu::SelectNext` | Selects the next pull request |
| `down` | recent list | `menu::SelectNext` | Selects the next pull request |
| `k` | recent list | `menu::SelectPrevious` | Selects the previous pull request |
| `up` | recent list | `menu::SelectPrevious` | Selects the previous pull request |
| `enter` | recent list | `menu::Confirm` | Opens the selected pull request, with the account it was opened with if it's still signed in |
| `cmd-w` | recent list | `workspace::CloseWindow` | Closes the window, as closing an empty pane does |
| `j` | account picker | `menu::SelectNext` | Selects the next account |
| `down` | account picker | `menu::SelectNext` | Selects the next account |
| `k` | account picker | `menu::SelectPrevious` | Selects the previous account |
| `up` | account picker | `menu::SelectPrevious` | Selects the previous account |
| `enter` | account picker | `menu::Confirm` | Uses the selected account, and remembers it |
| `escape` | account picker | `menu::Cancel` | Closes the picker without choosing |
| `space` | accounts | `accounts::ToggleChosen` | Picks or unpicks the selected `gh` account to import |
| `cmd-backspace` | accounts | `accounts::RemoveSelected` | Removes the selected account and its token, on a second press |

## Verify

Run these before every push. CI runs the same on macOS.

```sh
cargo fmt --check  # not --all, which formats Zed's crates too
cargo xtask sync-zed --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
script/check-symlinks.sh
script/check-zed-crates.sh
```

`script/check-zed-crates.sh` fails when the set of Zed crates compiled into Lo changes. If the
change is intended, run it with `--update`, update the crate list in `docs/spike.md`, and commit
both.

xtask is a separate workspace, so check it too when you change it:

```sh
cargo fmt --check --manifest-path crates/xtask/Cargo.toml
cargo clippy --manifest-path crates/xtask/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path crates/xtask/Cargo.toml
```

## Zed submodule

`third_party/zed` is Zed, pinned to branch `rozed/v1.27.0` of
[`scornz/zed`](https://github.com/scornz/zed): Zed's release tag `v1.27.0` plus Lo's patches, one
commit each. Lo's crates depend on Zed's by path. Don't edit files in `third_party/zed`; change a
patch on the branch instead.

Fetch it with `git submodule update --init --depth 1`, which fetches only the pinned commit (32 MB,
about 7 seconds). Always pass `--depth 1`. `.gitmodules` sets `shallow = true`, but git still
fetches all of Zed's history (400 MB, about a minute) when the pinned commit isn't on the fork's
default branch, and Lo's patch commits never are.

### Patches

On top of `v1.27.0`, in order. To change, reorder or drop one, make a new branch (see "Updating
Zed").

1. `paths: use rozed's folders`. Sets `APP_NAME` in `crates/paths/src/paths.rs` to the <!-- rename:keep -->
   project's first name, which patch 5 changes to `"Lo"`, so Lo uses `~/.config/lo`,
   `~/Library/Application Support/Lo`, `~/Library/Caches/Lo` and `~/Library/Logs/Lo`, never Zed's
   folders. A new branch makes patches 1 and 5 one commit, `paths: use Lo's folders`.
2. `project: let an app ignore worktrees' own settings files`. Adds
   `SettingsObserver::ignore_worktree_settings_files` and `Project::settings_observer`. Lo calls
   it on every project, so a repository's `.zed/settings.json`, `.zed/tasks.json`,
   `.zed/debug.json`, `.vscode/tasks.json` and `.vscode/launch.json` never apply, trusted or not.
3. `project: let an app keep worktrees out of Recent Items`. Adds
   `WorktreeStore::keep_out_of_recent_documents`, so Lo's checkouts never reach macOS's Recent
   Items. Adding them there also kept Lo from quitting on some desktops.
4. `editor: keep a split diff's expand buttons hidden when it splits again`. Adds
   `SplittableEditor::disable_expand_excerpt_buttons`, which also hides them on the left editor
   that `split()` makes when the diff widens or its style changes, and
   `Editor::expand_excerpt_buttons_disabled`. GitHub's diff of a pull request shows hunks only,
   and its excerpts must never expand.
5. `paths: name the app Lo`. Sets `APP_NAME` to `"Lo"`, renaming the folders patch 1 set. Only
   `rozed/v1.27.0` has it; see patch 1. Both change the same line, so a Zed release that changes
   it fails patches 1 and 5 together, for one cause.

### Cargo settings copied from Zed

Cargo ignores a dependency's `[patch]` sections, profiles and `.cargo/config.toml`, so Lo keeps
copies of Zed's. `cargo xtask sync-zed` (`crates/xtask`) writes them between `BEGIN generated` and
`END generated` markers; don't edit inside the markers.

- `Cargo.toml`: Zed's `[patch.*]` sections, with `path` entries pointed into `third_party/zed`, and
  its `[profile.*]` sections. Patches and per-package profile settings for crates that aren't in
  Lo's `Cargo.lock` are left out, because cargo warns about each one on every build.
- `.cargo/config.toml`: the macOS parts of Zed's, its `rustflags` and `MACOSX_DEPLOYMENT_TARGET`.
- `rust-toolchain.toml`: Zed's toolchain channel.

`cargo xtask sync-zed --check` changes nothing and fails, naming each stale section; CI runs it.
`cargo xtask sync-zed --seed-lock` first merges Zed's `Cargo.lock` into Lo's: Zed's entries
for every package in Zed's lockfile, Lo's own entries for the rest. Packages Zed uses then
resolve to the versions Zed tested, and Lo's other dependencies keep their versions. Then it
lets cargo prune the lockfile and syncs. Run it after updating Zed or adding a dependency on
another Zed crate, and review the `Cargo.lock` diff.

### Updating Zed

Each Zed release gets its own branch, `lo/v<version>`; the first, `rozed/v1.27.0`, predates the
name. Patch branches are append-only: never force-push or delete one, because older commits here
pin commits on it. To rewrite a patch on the same Zed release, push a new branch with a suffix,
such as `lo/v1.28.0-2`, and point the submodule at it as in step 2.

To see what a release breaks first, run `script/zed-latest.sh --tag v<version>` (under `mise exec
--`). In a worktree of its own, it replays the patches onto the release, then runs `cargo xtask
sync-zed --seed-lock`, `rustup toolchain install`, `cargo fetch`, `cargo fmt --check`, `cargo
build`, `cargo clippy` and `cargo test`. It reports each patch that no longer applies and each
failing step's first errors, and pushes nothing. The `weekly-zed` workflow runs it against Zed's
latest release every week and keeps one issue labelled `zed-update` open, with the latest report,
while that release breaks Lo.

1. In a full Zed clone, branch `lo/v<version>` from tag `v<version>`, cherry-pick the patches
   above in order, with patches 1 and 5 as one commit, and push the branch to `scornz/zed`.
2. Point the submodule at it:

   ```sh
   git config -f .gitmodules submodule.third_party/zed.branch lo/v<version>
   git -C third_party/zed fetch --depth 1 origin lo/v<version>
   git -C third_party/zed checkout FETCH_HEAD
   ```

3. Run `cargo xtask sync-zed --seed-lock`.
4. Build, fix what breaks, and update the patch list here.
5. Run `script/check-zed-crates.sh` and review any Zed crates that come or go.

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
