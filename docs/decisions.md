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

- **Cached PR data carries a version** (a hash of the query text, or the REST API version), so an
  app update that changes a query reads old rows as missing instead of serving the wrong shape.
- **"Clear cache" clears cached PR data only;** accounts, owner choices and recent PRs stay (design §3).
- **One request in flight per account,** writes spaced at least 1 s apart, through the poller's queue.
- **A watched PR that changed in the background gets a fresh summary; its full details are fetched
  when it's opened,** so background tabs don't spend GraphQL points.

- **A 200 to a conditional poll isn't a change by itself.** GitHub's ETag for a PR covers its
  embedded repo objects, so any push or star in the repo changes it. The poller compares the decoded
  summary (and, for the open-PR list, a projection of each PR's number, head, base, draft, title,
  update time and stack) before spending GraphQL points.
- **Requests start only once they hold the account's queue,** which serves waiters first in, first
  out, so a poll never overlaps a write.
- **rozed's own writes invalidate what they change,** and the open PR's details are refetched in full
  at least every 5 minutes, so viewed marks set elsewhere show up without a push.
- **Only `Accounts` decides an account is signed out;** any other error getting a client (a locked
  Keychain) backs off and retries. "Offline" clears on any HTTP response.
- **Waits are capped (60 s) and recomputed from the wall clock,** so pauses end on time after the Mac
  sleeps, and slowed or open-only tiers wake at the limit's reset.
- **The cheap check covers what the UI will need:** checks rollup, review decision, merge state,
  unresolved threads and queue entry, not only counts.
- **Cache versions come from each stored type's shape** (a hash checked by a snapshot test), not a
  hand-bumped string.

- **Pull requests are polled with `If-Modified-Since` their summary's `updated_at`.** Checked live: an
  unrelated push changes a PR's ETag but still answers `If-Modified-Since` with a free 304. The
  open-PR list has no `Last-Modified`, so each of its pages is polled with its own ETag.
- **Writes invalidate what they touched whatever their outcome,** since a failed write may still have
  reached GitHub. The 5-minute full refresh of the open PR runs only in the foreground and not below
  5% of the limit.
- **GraphQL-generated cache types are versioned by their query text;** rozed's own types by a hash of
  their JSON schema. A snapshot test pins every version.

## Local copy

- **rozed runs the `git` CLI with an isolated environment** (`GIT_CONFIG_GLOBAL=/dev/null`,
  `GIT_CONFIG_NOSYSTEM=1`, no credential helper). Design §3 says the user's git config is never
  touched; ignoring it also stops `insteadOf` rewrites and helpers from changing how rozed fetches.
- **Every fetched PR head is pinned at `refs/rozed/pull/<n>/revs/<sha>`,** so force-pushed revisions
  survive `gc` (design §3).
- **git 2.41 or newer is required:** `merge-tree --write-tree --merge-base` needs 2.40 (rebase-aware
  comparison, design §6), and `--attr-source`, which keeps a PR's `.gitattributes` from changing
  checked-out bytes, needs 2.41 (`attr.tree` needs 2.43). macOS's `/usr/bin/git` is 2.54.

- **Base branches are fetched per PR** (`refs/rozed/pull/<n>/base`, tips pinned under `bases/<sha>`),
  never under a branch-name ref; arbitrary commits pin at `refs/rozed/commits/<sha>`.
- **`repo_cache`'s API blocks;** it's split into fetch, changed files and checkout, `Repo` is `Send +
  Sync`, and the app calls each step from a background task.
- **A merged PR's diff needs its merge commit's first parent as the base** (the base tip already
  contains it); milestone 6 handles merged PRs.

- **The token goes to git only through a credential helper scoped to `https://github.com`,** not a
  `GIT_ASKPASS` that answers any prompt, and redirects are off (`http.followRedirects=false`), so
  neither a proxy's password prompt nor a redirected host can get it.
- **Every git command names its repository** (`--git-dir`, plus `--work-tree` for checkouts), so git
  never discovers a repository above the cache; clones are deleted by moving them aside under the
  lock first, and a clone without `HEAD` is treated as garbage.
- **Paths are bytes:** a file name that isn't UTF-8 is shown with a badge, never failing the PR.
- **Clones get incremental repacks at launch,** and checkouts unused for a while are removed on their
  own, so repos reviewed every day stay fast and small.
- **Checkouts are keyed by head** (`pull-<n>/<sha>`), so two windows or processes on one PR never
  move each other's checkout.
- **The diff never waits for a checkout:** the git-only view shows first, and a trusted checkout
  swaps in project buffers when ready, or a banner says why it failed.

- **rozed's clones pin `core.precomposeUnicode=false`,** since `git init` on a Mac turns it on and git
  then reads a decomposed file name as its composed twin.
- **Busy clones are repacked in the background after launch** (geometric on git 2.52+, full before,
  because geometric repack mishandled promisor packs until 2.52), and files over 1 MiB are left
  unloaded like files over 5,000 changed lines.

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

- **The latest revision is always the PR's current head** (`headRefOid`), passed in, never inferred
  from inexact times. Heads outside the current commits sort before the latest force-push's result.
  Without this a late review or a skewed clock made a stale head "latest" and said "Nothing new".
- **Revisions start where the PR opened:** heads older than the newest head pushed at or before
  `createdAt` are dropped, so pushes made before the PR existed aren't r1.
- **A review's commit is a revision** (it was the head when the reviewer looked). Only submitted
  reviews count; pending ones have no time and are ignored.
- **A branch re-created at another SHA counts as forced unless ancestry says otherwise,** and a check
  suite whose previous SHA isn't a known head or current commit (a backport on another branch) never
  names a revision's previous head.
- **Revision building is O(n log n),** with a head → position index, so 20,000 pushes stay fast.

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

## Spike results (PR #4)

- **rozed never applies a repository's own Zed settings** (`.zed/settings.json`, tasks, debug
  configs), trusted or not. A trusted checkout's local settings merge over rozed's forced layer, so a
  PR could otherwise pick its own language-server binary. Trust means "run this repo's language
  servers", not "let this repo configure rozed". Fixed on PR #4.
- **PR checkouts are kept out of macOS Recent Items.** Zed adds every visible worktree there, and
  on this desktop the update blocks quitting. Fixed on PR #4.
- **rust-analyzer is downloaded from GitHub for now,** with build scripts, proc macros,
  check-on-save and dependencies off. Design §2 prefers an installed copy; milestone 10 decides
  how to find one that doesn't follow the repo's `rust-toolchain.toml` (rustup would install it).
- **Live GitHub fixtures are only read, never force-pushed.** The classifier treats force-pushes as
  destructive, and local test repos prove force-push survival just as well.

- **Zed patches 2 and 3:** `SettingsObserver::ignore_worktree_settings_files()` (skips a worktree's
  `.zed/` and `.vscode/` settings, tasks and debug files; `.editorconfig` still applies) and
  `WorktreeStore::keep_out_of_recent_documents()`. Neither setting nor API existed.
- **Prettier and inline git blame are forced off.** Prettier tried to install itself with `--trust`,
  and blame in a partial clone fetched each file's history one request at a time (899 requests in
  2.5 minutes).
- **Zed's `trusted_worktrees` table is emptied at startup,** so trust never carries over between
  runs; rozed keeps its own trust list (milestone 10).
- **rust-analyzer gets `RUSTUP_AUTO_INSTALL=0`** and a pinned `RUSTUP_TOOLCHAIN`, so a repo's
  `rust-toolchain.toml` can't make rustup download a toolchain. A trusted repo's `.cargo/config.toml`
  still steers cargo; that's within design §7's accepted cost for trusted repos.

## Stacks (PR #7 and its review)

- **Trunk branches are never a layer.** The caller passes the repo's trunks (the default branch plus
  protected or ruleset-covered branches); a PR whose head is a trunk (a deploy PR
  `production <- main`, a back-merge `develop <- main`) is never anyone's parent, by any rule.
  Without this one such PR swallows the whole tree. Roots list trunks first, in the given order.
- **The base-branch check uses the base branch's live tip** (`baseRef.target.oid`, null once
  deleted), not `baseRefOid`, which is stale on most open PRs. The tip check applies only when the
  upper PR is open.
- **A squash- or rebase-merged lower layer is matched by any of its commits,** the newest one the
  layer contains, and the layer is compared against that commit. The common flow (the lower layer
  got review fixes after the upper was cut) then still finds the link and hides what landed. Open
  lower layers keep design §4's rule: B must contain A's latest commit.
- **Each layer says which base branch "Make this a GitHub stack" must set** (`retarget_to`), instead
  of the converter inferring it from the linking rule.
- **Merged PRs' `head_in_base` comes from GraphQL:** the head is one of `mergeCommit`'s parents or
  equals it. Callers pass open PRs plus recently merged ones and the merged PRs whose head branch is
  an open PR's base, following merged layers down to a trunk; a helper lists the branches still to
  look up. A shared commit belongs to the oldest merged PR that lists it.
- **A closed middle layer of a GitHub stack doesn't split it:** the layer above links to the nearest
  lower position still present.
- **Duplicates prefer open, then merged, then closed;** cycles list only open PRs.
- **App wiring for the GitHub layer moves to milestone 5.** PR 3b keeps polling and the cache inside
  the `github` crate, because the app's PR loading lives on the spike's branch and the local copy's,
  which 3b doesn't sit on. Milestone 5's first task joins them.
- **The database lives in `0-dev` for debug builds and `0-stable` for release,** keyed on the build
  profile, because Zed's release channel reads `stable` in every rozed build.
- **`gh` accounts whose token comes from an environment variable aren't offered for import.** That's
  how a wrapper injects a token for one shell, not a stored account (and on this machine, it would
  be the user's own account).
- **Trust is opt-in until milestone 10's gate exists.** The spike trusted every PR checkout, and a
  trusted checkout runs the PR's code (`.cargo/config.toml` wrappers through rust-analyzer's cargo
  calls, `.zed/settings.json` binary paths, rustup auto-installing a pinned toolchain). Without
  trust, rozed makes no checkout, starts no language server, and reads both sides from git.
- **Untrusted and trusted alike, the right side's text comes from git.** A trusted checkout's file is
  used for the language server only when its bytes match the head blob; otherwise rozed shows git's
  content with a warning. APFS folds case and Unicode forms, so `Lib.rs` and `lib.rs` collide on
  disk, and `.gitattributes` filters change checked-out bytes.
- **Even in a trusted repo, rozed hardens what it runs:** a forced allowlist of language servers
  (only those rozed ships support for), no project-local Zed settings, no interactive login shell
  in the checkout (the project gets rozed's own environment), and `RUSTC_WRAPPER`,
  `RUSTC_WORKSPACE_WRAPPER`, `RUSTUP_TOOLCHAIN` and friends pinned for rust-analyzer. Design §7
  accepts that trusted repos' language servers may run repo code; these keep a PR's files from
  choosing *which* programs run. Milestone 10 also evaluates a macOS sandbox profile for language
  servers.
- **No changed file is ever dropped from the diff.** Anything rozed can't render gets a placeholder
  saying why. Submodule bumps show "Subproject commit a → b", and mode-only and line-ending-only
  changes get a badge.
- **Git runs with an allowlisted environment** (not inherited and then patched), plus
  `core.hooksPath=/dev/null`, `core.fsmonitor=false`, `core.attributesFile=/dev/null`,
  `protocol.allow=never` with `https` allowed, and `GIT_NO_REPLACE_OBJECTS=1`.
- **Cache refs are keyed by PR, not branch name** (`refs/rozed/pull/<n>/base`), the fetched head is
  checked against the head SHA GitHub reported, and repo copies are keyed by the API's canonical
  `owner/repo` (repository ID once the GitHub layer is wired in). Writes to a repo copy hold a
  per-repo lock.

## Stack conversion (PR #11)

- **Conversion is refused with every reason at once,** bottom to top, so the user sees all of them;
  "already a GitHub stack" is a refusal, not an empty plan.
- **Layers already in the GitHub stack keep their base branches;** only joining PRs are retargeted.
- **A GitHub stack with other open layers above the path can't be extended** (the API only appends
  at the top), and a PR in a merge queue anywhere on the path blocks conversion.
- **Rollback restores base branches, last changed first, and reports a created stack as kept**
  (it can be unstacked by number); layers added to an existing stack can't be removed alone.
- **Merging one layer at a time moves every layer on the merged bottom to the root** and marks it
  "needs rebase", so none is left targeting a dead branch, the way GitHub does when a branch is
  deleted.

## GitHub client (PR #5 and its review)

- **The token only goes to GitHub's API hosts.** The client refuses absolute URLs to any host but
  `api.github.com` and `uploads.github.com`; images and links found in responses are fetched by a
  separate path (milestone 8) that decides per host.
- **Only "not found" means an account can't see a repo.** Rate limits, SSO, OAuth-app restrictions
  and server errors are reported with GitHub's link, so the UI can explain them (design §3, §11).
- **Open PRs are read newest first** (`CREATED_AT DESC`), so the 1,000-PR cap drops the oldest, not
  the newest.
- **The device flow opens only `https://github.com/login/device`,** and help links open only when
  they're `https` on `github.com`; server-supplied numbers are clamped; transient errors keep the
  flow polling until its deadline.
- **Removing an account always removes it,** even when its token is already gone, and tells the user
  that revoking the grant happens on GitHub (rozed has no client secret to revoke it).
- **Debug builds that opt into the Keychain use their own item names** (`rozed-dev://…`), so they
  never touch a release build's tokens.
- **A 401 is handled in one place:** the client's hook marks the account signed out, after one
  re-import from `gh` for accounts that came from `gh`.

## Moved code (PR #9)

- **Lines of only punctuation (`}`, `});`) don't count toward a move's 3 lines,** though they can sit
  inside one. Design §5 says 3 non-blank lines; without this, runs of closing braces show as moves.
  git's `--color-moved` has a similar floor.
- **Code deleted and re-added in the same place isn't a move,** so a re-indent or wrapping code in
  a block shows as an ordinary change rather than a move to the same spot.
- **A moved block may contain edits** if at least two thirds of its lines match and no more than 2
  edited lines come in a row; an inserted or removed line splits it into two moves.
- **Lines repeated all over a PR don't start a lineup** (deleted × added count above 64), which keeps
  detection linear on generated or repetitive diffs.

## The PR tab

- **Milestone 5 builds on an integration branch** (`integration/pr-tab`) made only of merges of the
  open PR branches it needs, so its PRs' diffs show only their own change while the lower PRs wait
  to merge. When the lower PRs land on `main`, the milestone's PRs are retargeted to `main`.

- **`integration/pr-tab` merges `feat/revisions-logic`, not `feat/merge-logic`;** the merge logic
  joins it when milestone 9 needs it.
- **`⌘O` arrives with 5a** (without clipboard prefill until milestone 12), since a second PR in the
  same window needs a way in; launching without a link opens it.
- **Every editor the workspace opens is read-only,** which closes "Open File" and any other path into
  an ordinary editor tab.
- **Default panel keys:** `⌘B` file list, `⌘⌥B` threads, `alt-o` review/path order. Milestone 12's
  keymap and `?` sheet revisit all keys.
- **A PR opened anonymously (public repo, no account) doesn't poll and isn't added to recent PRs.**

- **rozed never opens a path it didn't create:** dropped files and folders are swallowed everywhere
  (the PR tab, empty panes, the window), and every palette action that opens, creates, saves or adds
  a window is captured, not just unbound. Without `--trust`, every language's servers are forced off
  as a second line, since Zed treats invisible worktrees as trusted.
- **Opening a PR goes cache first:** cached details show at once and offline, with a banner of their
  age; the account check and fetch follow. A failed tab retries when its link is opened again.
- **A remembered account that's signed out or unreadable is reported, never swapped for another;**
  when no account can be used and GitHub explained why, the tab says so with GitHub's link rather
  than opening anonymously.
- **The poller watches a PR only once GitHub confirmed it exists, and only the active tab is its
  "open" PR.** A tab's poll state (signed out, rate-limited, offline) shows in its header.
- **A trusted checkout's worktree and language servers stop when the last tab using them closes**
  (design §11).

- **GitHub's diff shows hunks only, at their real line numbers, with expanding blocked** until the
  local copy's diff replaces it at the same file and line; if the local copy is ready first, GitHub's
  never shows. A retryable copy failure retries after 5 s, doubling to 5 minutes, with a banner.
- **The PR tab owns and draws its banners** under its header (checking out, checkout failed, head
  mismatch, retrying), since its diff editor is created and replaced while the tab is open.

- **Viewed marks belong to each user and need only read access,** so `e` works on any PR the
  account can see. The local record of where each file was marked lives in `viewed_files` (per
  account and PR); clearing the cache keeps it, removing the account removes it.
- **`e` moves to the next unviewed file in review order, wrapping;** `]f`/`[f` and `]c`/`[c` stop at
  the ends and always follow the diff's (review) order. On a failed write only the mark reverts.

## Syntax-aware diff (PR #14)

- **difftastic's structural diff is ported (MIT, from commit `87aad1ae`),** with its notice in every
  ported file and in `THIRD_PARTY_NOTICES.md`. It depends only on `tree-sitter` at Zed's pin: a
  Myers line diff stands in for imara-diff, and a bucket queue for the radix heap.
- **Files with parse errors fall back to the normal diff,** as difftastic does, alongside "no
  grammar" and "too large" (1 MB, a 3M-vertex graph).
- **A comment from the syntax-aware view lands only on the exact line it was written beside;** if
  GitHub won't take a comment there, the UI offers a file-level comment instead of moving it.
- **Sliders are ported** (difftastic's "prefer the inner/outer delimiter" fix-up), so wrapping code in
  a new block marks only the wrapper.

## Code features

- **The syntax index is its own crate (`symbol_index`),** beside the seven in design §2's table: it
  depends only on tree-sitter, so it's tested without compiling Zed, and both the app and rozed's
  language-server mode use it.

- **The index uses each grammar's own `tags.scm`, plus small rozed additions appended after it**
  (consts, fields, type references, components and the like), since the vendored queries miss
  much; the vendored tag wins when both match. JavaScript is parsed with the TSX grammar, as Zed does.
- **Files over 1 MB, `linguist-generated` files, minified files and files with a "generated" header
  are skipped.** Ranking is same file, same directory, an import's target, then the rest; import
  matching is a per-language heuristic over the asking file's text, never reading project manifests.
- **`tree-sitter-tags` needs `thiserror` 2.0.18 where Zed's lock has 2.0.17,** so rozed's lock moves
  that one crate; a `--seed-lock` after a Zed bump lets cargo raise it again.

## Merging (PR #12 and its review)

- **Approve and ship acts on GitHub's verdict, not rozed's prediction.** It approves first, then
  re-reads the PR and merges or joins the queue only if GitHub reports `CLEAN`, `HAS_HOOKS` or
  `UNSTABLE`. If GitHub still blocks it, it turns on auto-merge when every remaining blocker is one
  waiting fixes, and otherwise stops and shows why. rozed's prediction only words the confirmation
  ("will likely merge" or "will likely wait"). Since it merges only on those states, approve and ship
  never bypasses a rule, even for an admin; the admin override stays in the merge panel, behind
  typing the PR's number (design §9).
- **GitHub's own decision wins over rozed's model:** an `APPROVED` or `CHANGES_REQUESTED` review
  decision stays unless the viewer's approval is provably what changes it (they were the only
  requester).
- **Auto-merge and later pushes.** The snapshot records whether stale approvals are dismissed. When
  they aren't, the confirmation warns that a later push could merge with this approval, and while
  rozed runs it turns auto-merge off when it sees a newer head than the one approved, and says so.
- **Approve and ship does nothing on a draft** and refuses an empty head SHA.
- **A required check that never reported is a wait only while something on the head can still report
  it;** otherwise it blocks. A deployment waiting for a reviewer is its own kind of wait. Every check
  under a required name must pass.
- **Merge methods:** rebase isn't offered when the branch requires signed commits; a remembered
  method that's no longer allowed falls back to GitHub's default for the viewer.
- **The stale-approval warning, and turning auto-merge off after a push,** apply when a push wouldn't
  need a new approval: stale approvals aren't dismissed and the latest push needn't be approved by
  someone else, or no reviews are required at all. Neither applies to your own PR, or once someone
  else turned auto-merge on.
- **On a queue branch, `BEHIND` may join the queue** (the queue rebases); a direct merge still needs
  `CLEAN`, `HAS_HOOKS` or `UNSTABLE`. When GitHub says `CLEAN` but rozed sees a blocker waiting won't
  fix (a stale failed check), approve and ship stops rather than merges.
- **"rozed may not see every rule" shows on any partly explained block** until the GitHub layer
  confirms it read the branch's rules (milestone 9 wires `sees_every_rule`).
