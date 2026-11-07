# Decisions

Judgment calls made where the design is silent, newest last. Each has a line of reasoning.

## Process

- **Task worktrees live in `~/src/rozed-worktrees/<branch-slug>`.** The Agent tool's worktree
  isolation makes worktrees of the overseer's own repository (srcs), so subagents create Lo
  worktrees themselves with `git -C ~/src/rozed worktree add`.
- **The overseer's docs (`status.md`, `plan.md`, `decisions.md`) live on branch `docs/status`,** in
  the worktree `~/src/rozed-worktrees/docs`, and land through one open docs PR that a subagent
  updates. Nobody commits to `main`, and the overseer doesn't commit.
- **Merging resumed on 2026-11-06:** the user explicitly allowed the overseer to `gh pr merge` in
  `scornz/lo`. The stack lands bottom first (see `status.md`).
- **PRs are stacked while merging is blocked.** The auto-mode classifier refuses `gh pr merge` from
  the overseer ("Merge Without Review"), so merging waits for the user. Each new PR branches from
  the PR below it and targets that PR's branch, so every PR's diff shows only its own change and
  work keeps moving. Parallel tasks merge the new base into their branch (never rebase or
  force-push) before retargeting. Getting around the classifier, by merging through a subagent or
  the REST API, was rejected.
- **Each line of PRs lands as a GitHub-native stack** (the user's suggestion): one async merge of the
  top layer lands every layer below as its own squash commit, and GitHub retargets the rest. Layers
  with merge commits merged fine. `gh pr merge` refuses stacked PRs; the async merge API is the way.
- **Landing the stack:** after each squash-merge GitHub retargets the PRs above to `main`. Before the
  next merge, its branch merges `origin/main`. Where the branch already contains everything `main`
  gained (its own lower layers), conflicts resolve to the branch's side and the merged tree must equal
  the branch's tree; where a sibling landed first, conflicts are resolved by hand (the integration
  branch's merges hold the same resolutions) and the diff against the branch's old tree may only touch
  the sibling's files.

- **PR descriptions say what the PR implements,** its code, behaviour and goal, and leave out how
  the work is run (sessions, agents, why PRs stack). The user asked for this; the repo is public.

## Repository

- **Node and pnpm are pinned in `mise.toml`, as in srcs,** so hooks and commitlint run the same way
  in both repos.
- **Scripts live in `script/`,** Zed's convention, since Lo sits beside Zed's tree.
- **Bootstrap ships a stub `lo` crate** so the Rust CI jobs check a real workspace from the first
  PR.
- **CI skips the Rust jobs when only docs, agent files or the license change.** `ci-ok` stays the
  single required check and passes when they're skipped. Building Zed's crates on a hosted macOS
  runner takes long enough that docs PRs shouldn't pay for it.
- **No `guard-bash` hook.** The `.claude/settings.json` deny rules plus lefthook cover the rules the
  user asked for; srcs's hook exists for Terraform.

## Spike

- **The spike is two stacked PRs:** 2a gets Zed's crates building locally and on CI, 2b opens a PR
  in the diff. A build or CI problem then surfaces before the diff work depends on it.
- **Zed patch branches are named `lo/v<zed version>`,** one per Zed release, so old submodule
  pointers stay reachable after a bump.
- **The Cargo sync is a Rust `xtask` (`cargo xtask sync-zed`, using `toml_edit`),** not a shell or
  Python script: it edits TOML structurally, needs no extra toolchain, and gets tested like the rest.
- **Zed's hosts are refused in the HTTP client as well as turned off in settings.** Settings alone
  depend on every Zed code path honouring them; refusing `zed.dev` and its subdomains in the one
  shared client catches anything missed and logs it.
- **Lo fetches into its own namespace, `refs/lo/...`,** so fetched revisions never collide
  with branch names and survive force-pushes (design §3).
- **Verification screenshots stay on the devbox,** under `~/src/rozed-worktrees/evidence/<branch>/`,
  and PR descriptions describe them. Uploading them would publish them on a public repo.

## GitHub layer

- **`graphql_client` with an `apollo-compiler` validation test,** over `cynic`: `graphql_client`
  adds 5 crates and builds in a fraction of a second, and the test catches the argument and
  variable-type mistakes it misses. `cynic` costs about 19 crates, duplicate versions, and its repo
  moved off GitHub. (Research §4.)
- **`sqlez`, not `rusqlite`:** Zed links `libsqlite3-sys` with `links = "sqlite3"`, so a second
  SQLite would conflict. Lo opens its own database file rather than Zed's `db`.
- **Debug builds keep tokens in a mode-600 file** (`~/.config/lo/credentials.json`), as the
  handoff allows, because unsigned rebuilds re-prompt for Keychain access. Zed's own dev fallback
  isn't mode 600, so Lo has its own.
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
- **Lo's own writes invalidate what they change,** and the open PR's details are refetched in full
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
- **GraphQL-generated cache types are versioned by their query text;** Lo's own types by a hash of
  their JSON schema. A snapshot test pins every version.

## Local copy

- **Lo runs the `git` CLI with an isolated environment** (`GIT_CONFIG_GLOBAL=/dev/null`,
  `GIT_CONFIG_NOSYSTEM=1`, no credential helper). Design §3 says the user's git config is never
  touched; ignoring it also stops `insteadOf` rewrites and helpers from changing how Lo fetches.
- **Every fetched PR head is pinned at `refs/lo/pull/<n>/revs/<sha>`,** so force-pushed revisions
  survive `gc` (design §3).
- **git 2.41 or newer is required:** `merge-tree --write-tree --merge-base` needs 2.40 (rebase-aware
  comparison, design §6), and `--attr-source`, which keeps a PR's `.gitattributes` from changing
  checked-out bytes, needs 2.41 (`attr.tree` needs 2.43). macOS's `/usr/bin/git` is 2.54.

- **Base branches are fetched per PR** (`refs/lo/pull/<n>/base`, tips pinned under `bases/<sha>`),
  never under a branch-name ref; arbitrary commits pin at `refs/lo/commits/<sha>`.
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

- **Lo's clones pin `core.precomposeUnicode=false`,** since `git init` on a Mac turns it on and git
  then reads a decomposed file name as its composed twin.
- **Busy clones are repacked in the background after launch** (geometric on git 2.52+, full before,
  because geometric repack mishandled promisor packs until 2.52), and files over 1 MiB are left
  unloaded like files over 5,000 changed lines.

## Building on Zed (PR #3 and its review)

- **Zed's unwanted crates are compiled in but never started.** `project`, `editor` and `workspace`
  pull in `language_model`, `agent_settings`, `cloud_llm_client`, `terminal`, `dap`, `remote` and
  `context_server`. Cutting them would take a large patch set on Zed that fights every bump, so
  Lo accepts them, as design §2's "comes along unused" allows. It never calls their `init`,
  never binds their actions, and forces `disable_ai`, which keeps context servers off. A CI check
  fails when the set of Zed crates changes, so new arrivals get looked at.
- **A forced settings layer** (Zed's server-settings layer, merged above user settings) turns off
  telemetry, auto-update, AI and extension auto-install, and points `server_url` at
  `https://lo.invalid`. Tests apply contrary user settings and check the forced values win.
- **Read-only is enforced by the keymap and trust, not just by read-only buffers.** Lo loads
  Zed's keymap without bindings to open, new, save or save-as actions (a test checks), and
  initialises Zed's worktree trust with nothing trusted, so no language server starts and no
  repo `.zed/settings.json` applies until Lo trusts the worktree. The spike trusts its own PR
  checkout explicitly; milestone 10 adds the trust gate.
- **The HTTP client follows redirects itself,** checking and logging every hop, so a redirect can't
  reach Zed's hosts unseen and tokens aren't forwarded across hosts.
- **Zed patch branches are append-only.** Rewriting a patch makes a new branch
  (`lo/v1.27.0-2`), so commits pinned by older Lo commits stay reachable.
- **The submodule is fetched with `git submodule update --init --depth 1`** (7 s, 32 MB); without
  `--depth`, git fetches all of Zed's history.
- **`xtask` is its own Cargo workspace,** so `cargo xtask sync-zed` runs even when Lo's workspace
  can't resolve (an empty submodule, or a Zed bump that needs a new patch).
- **`--seed-lock` merges lockfiles:** Zed's versions for Zed's packages, Lo's existing pins for
  the rest, so a Zed bump doesn't silently upgrade Lo's own dependencies.
- **`ZED_DISABLE_STAFF=1`** is set at startup so Zed's staff-only feature flags stay off in debug
  builds, and **`LO_PARENT_PID`** stops Lo from starting under another Lo (Zed re-runs the
  executable for `--printenv` and `--askpass`, which Lo handles).

## Revisions

- **Revisions come from the branch's push history, not only the PR timeline.** Design §6 says the
  timeline of pushed commits and force-pushes. The timeline's commits carry no push time
  (`pushedDate` is null), so it can't separate two normal pushes. The repository activity API
  (`GET /repos/{o}/{r}/activity?ref=refs/heads/<branch>`) lists every push with before/after SHAs,
  so Lo builds revisions from it first, then fills gaps from force-push events, review commits,
  check-suite pushes and heads its own poller saw. This keeps the design's intent (one revision per
  push) with a source that has the data. (Research: `research/revisions.md`.)
- **Revisions are stored by head SHA and numbered r1, r2… only for display,** since a late source
  can insert one.
- **A revision's base is the merge-base of its head with the base branch as it was then,** computed
  locally; for stacked or retargeted PRs, the base tip comes from base-branch force-push events,
  the base branch's activity log and Lo's pinned refs.
- **A replayed tree is pinned once per process and never unpinned while it runs;** pins are cleared on
  the next open with no other Lo using the clone. No `Drop` runs git.
- **Rebase-aware comparison checks its inputs:** each base must be an ancestor of its head, and all
  four commits must already be in the copy (a missing one is its own error, never a silent download).
  A retargeted base, or an unchanged head on a moved base, is flagged rather than attributed to the
  author.
- **When the replay conflicts, the plain comparison still shows (design §6), plus every conflicted
  file,** marked "resolved a conflict", with the diff from the conflicted replay to the new head
  available for it, so an author's resolution (including dropping a base change) is never hidden.
  Directory renames on the base are followed (`merge.directoryRenames=true`).
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

- **Lo never applies a repository's own Zed settings** (`.zed/settings.json`, tasks, debug
  configs), trusted or not. A trusted checkout's local settings merge over Lo's forced layer, so a
  PR could otherwise pick its own language-server binary. Trust means "run this repo's language
  servers", not "let this repo configure Lo". Fixed on PR #4.
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
  runs; Lo keeps its own trust list (milestone 10).
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
  profile, because Zed's release channel reads `stable` in every Lo build.
- **`gh` accounts whose token comes from an environment variable aren't offered for import.** That's
  how a wrapper injects a token for one shell, not a stored account (and on this machine, it would
  be the user's own account).
- **Trust is opt-in until milestone 10's gate exists.** The spike trusted every PR checkout, and a
  trusted checkout runs the PR's code (`.cargo/config.toml` wrappers through rust-analyzer's cargo
  calls, `.zed/settings.json` binary paths, rustup auto-installing a pinned toolchain). Without
  trust, Lo makes no checkout, starts no language server, and reads both sides from git.
- **Untrusted and trusted alike, the right side's text comes from git.** A trusted checkout's file is
  used for the language server only when its bytes match the head blob; otherwise Lo shows git's
  content with a warning. APFS folds case and Unicode forms, so `Lib.rs` and `lib.rs` collide on
  disk, and `.gitattributes` filters change checked-out bytes.
- **Even in a trusted repo, Lo hardens what it runs:** a forced allowlist of language servers
  (only those Lo ships support for), no project-local Zed settings, no interactive login shell
  in the checkout (the project gets Lo's own environment), and `RUSTC_WRAPPER`,
  `RUSTC_WORKSPACE_WRAPPER`, `RUSTUP_TOOLCHAIN` and friends pinned for rust-analyzer. Design §7
  accepts that trusted repos' language servers may run repo code; these keep a PR's files from
  choosing *which* programs run. Milestone 10 also evaluates a macOS sandbox profile for language
  servers.
- **No changed file is ever dropped from the diff.** Anything Lo can't render gets a placeholder
  saying why. Submodule bumps show "Subproject commit a → b", and mode-only and line-ending-only
  changes get a badge.
- **Git runs with an allowlisted environment** (not inherited and then patched), plus
  `core.hooksPath=/dev/null`, `core.fsmonitor=false`, `core.attributesFile=/dev/null`,
  `protocol.allow=never` with `https` allowed, and `GIT_NO_REPLACE_OBJECTS=1`.
- **Cache refs are keyed by PR, not branch name** (`refs/lo/pull/<n>/base`), the fetched head is
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
  that revoking the grant happens on GitHub (Lo has no client secret to revoke it).
- **Debug builds that opt into the Keychain use their own item names** (`lo-dev://…`), so they
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
  same window needs a way in. (Since 5e, launching without a link shows the recent list instead.)
- **Every editor the workspace opens is read-only,** which closes "Open File" and any other path into
  an ordinary editor tab.
- **Default panel keys:** `⌘B` file list, `⌘⌥B` threads, `alt-o` review/path order. Milestone 12's
  keymap and `?` sheet revisit all keys.
- **A PR opened anonymously (public repo, no account) doesn't poll and isn't added to recent PRs.**

- **Lo never opens a path it didn't create:** dropped files and folders are swallowed everywhere
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
- **GitHub's diff never shows a hunk somewhere GitHub didn't put it.** Rows between hunks are filled
  with a marker unique to each row and identical on both sides, so Zed's re-diff can't pair them with
  changes, and after the diff is built every hunk is checked against GitHub's rows; a file that fails
  the check shows GitHub's raw patch text instead. GitHub's diff is tied to the head it was read at,
  and the switch to the local copy says so when the heads differ.
- **Anonymous opens read at most a few pages of GitHub's file list** through one app-wide anonymous
  client, so two big public PRs can't use up the 60-requests-an-hour limit; anonymity is explicit
  (`Option<Token>`), never inferred from an empty token.
- **The PR tab owns and draws its banners** under its header (checking out, checkout failed, head
  mismatch, retrying), since its diff editor is created and replaced while the tab is open.

- **Viewed marks belong to each user and need only read access,** so `e` works on any PR the
  account can see. The local record of where each file was marked lives in `viewed_files` (per
  account and PR); clearing the cache keeps it, removing the account removes it.
- **`e` moves to the next unviewed file in review order, wrapping;** `]f`/`[f` and `]c`/`[c` stop at
  the ends and always follow the diff's (review) order. On a failed write only the mark reverts.

- **A file is marked viewed only when GitHub's head is the head on screen** (`markFileAsViewed`
  takes no commit). When the PR moved on, `e` refuses to mark, says there's a newer push, and offers to
  load it; clearing a mark always works. A push that lands inside the poll interval can still slip
  through; that's accepted.
- **`e` acts on the file the diff shows:** the cursor's file when it's in view, else the file at the
  top of the viewport, and never a file that hasn't loaded or that GitHub doesn't list.
- **Lo's diff keys win over vim's inside the PR diff** (milestone 12 binds them in a context deeper
  than vim's), and comment boxes never receive Lo's single-key bindings.

- **A token GitHub refuses signs that account out,** and then it counts as not signed in; it doesn't
  by itself block opening a public PR anonymously.
- **Drops are stopped at the window** (GPUI's `on_drag_move` plus `stop_active_drag` on the workspace
  root), so no Zed patch was needed for empty panes.
- **Offline, a PR whose head is already in the local copy shows its diff from the copy;** without a
  copy, the tab shows the saved details only (5e).

- **With no PR open, the window shows a "Recent pull requests" tab** across all accounts; launching
  without a link opens it, and closing the last PR brings it back.
- **The account picker answers once per owner;** dismissing it opens nothing, and reopening the link
  asks again. The window title is `owner/repo#n — title`, or "Lo" (Zed's `window_title_format` is
  forced so its workspace never overwrites it).
- **Offline, the diff comes from the copy only if the copy fetched the saved head,** compared from
  where that head left the last-fetched base, untrusted and without fetching; reopening the link
  retries online.

- **An owner's account choice is about the repository's current owner** (from GitHub's `full_name`,
  so transferred repos follow), and a picker's answer applies only to tabs whose candidate accounts
  include it. When the remembered account can't see one repo, Lo asks for that repo alone and
  remembers it per repository, leaving the owner's choice alone. Pickers queue per owner; another
  dialog or click never cancels a waiting tab.
- **Only failed connections and DNS lookups count as offline.** TLS failures and proxy refusals
  are errors, and timeouts and lost connections are retryable. A tab that has shown the local copy
  keeps it through any later failure, says why and keeps checking; "failed" is only for a load that
  never showed a copy.
- **Lo retries by itself only what waiting can fix** (network failures, 5xx, 429, a proxy's 503).
  Failures only the user can fix (no account chosen, refusals, SSO) keep the copy, stop retrying and
  show their help links; an automatic reload never opens the account picker, it offers a button.
- **`e` marks only the head on screen.** When GitHub reports a newer head than the diff shown, the
  newer head is recorded as the latest and `e` is refused with the "newer push" message until that
  head's diff is up.
- **A `0-dev` database from a single milestone 5 branch may not open in the combined build**
  ("Migration changed at step 2"); unreleased, so delete `~/Library/Application Support/Lo/db/0-dev/lo.sqlite`.
- **The account picker asks only for the active tab** and keeps focus until it's answered or
  dismissed; a background tab says "Waiting to ask…" until it's selected.
- **Offline means Lo reads only what the copy already holds:** the saved head and saved base when
  both are pinned (else the details-only view), with lazy downloads off (`GIT_NO_LAZY_FETCH=1` on git
  2.44+, a missing-object check before reading on older git) and no write lock. An offline tab keeps
  checking and switches back online by itself; only connection failures count as offline, not slow
  answers or one account's failing check.

- **While the poller knows GitHub is unreachable or the account is signed out, `e` marks nothing;**
  a failed write reverts only the mark, never the selection. "Load the newer push" reopens the tab at
  the new head in the same place.
- **The file list follows the diff:** the cursor's file while it's in view, otherwise the file at the
  top of the view; `e` acts on that file.
- **Local viewed records follow marks set elsewhere,** recorded at the poll's head, and are forgotten
  when GitHub says the file is unviewed.
- **The tab's single-key bindings are scoped so text boxes in the tab type them** (`e`, `]f`, `[c`,
  `alt-o` and the rest); milestone 12 sorts out vim's own contexts.

- **Zed patch 4 keeps a split diff's expand buttons hidden when Zed rebuilds the split**
  (`SplittableEditor::disable_expand_excerpt_buttons`).
- **Signed out, GitHub's diff reads at most 3 pages and none while 10 or fewer requests remain;**
  GitHub's diff retries (5 s doubling to 5 minutes, or until the rate limit resets) until the local
  copy's diff replaces it. Unknown copy failures stop after 5 tries with a Retry button; network and
  rate-limit failures keep retrying. Hunks past line 50,000 show as GitHub's raw patch.

- **`linguist-generated` comes from `git check-attr --source=<head>` in the bare clone,** not a
  `.gitattributes` parser; a failed lazy download is an error, not "unspecified".
- **Diff view keys:** `tab` switches side, `alt-v` toggles inline/side-by-side (kept per tab), `alt-w`
  hides whitespace-only changes, `alt-a` opens a folded file, loads a large one, or switches between
  its changes and the whole file. Files whose text didn't change fold with "no changes". Milestone 12
  revisits keys with vim.
- **PR image previews are decoded in a helper process** (a hidden mode of the `lo` binary), never in
  the app. It has a big-stack thread, a deadline of about 3 s and a memory watchdog of about
  512 MiB, and at most two run at once; closing the tab kills them. A crafted SVG under 2 KB can
  overflow a worker's stack, take 6 GB or burn a CPU for hours inside usvg/resvg, and no in-process
  cap stops that. The app itself reads only capped image headers.
- **PR images are decoded by Lo, never by gpui's loaders.** SVGs are parsed with Lo's own
  `usvg::Options` that resolve no file or remote hrefs (gpui's defaults read any local path), and
  rasterized with `resvg`. Every image is capped from its header (8192 px a side, 40 MP), decoded
  with explicit `image::Limits`, GIFs to frame 0 only, and kept downscaled to the panel's size.
  Milestone 8's comment images must go through the same safe path.
- **Size limits:**
  - `alt-a` refuses files over 50 MiB or with a line over 1 MiB ("too large to show").
  - `.gitattributes` files are found with `ls-tree` and downloaded in one batch. Past 200 of them,
    attributes are skipped with a warning.
- **Zed's base keymap loads before Lo's,** so Lo's keys win at equal depth. `tab` binds only in
  full-size diff editors, and the `alt` view keys bind only in the diff and the file list.
- **Hiding whitespace ignores only space, tab, CR and LF,** as `git diff -w` and GitHub do; other
  Unicode whitespace (U+2028, NBSP) can change meaning and stays visible.
- **Images are read from git only (never the trusted checkout), decoded in the background,** and shown
  level side by side; other binary files say "binary file changed" with both sizes and never fold for
  size. In GitHub's diff, `alt-a` on a large file only opens it.

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
  depends only on tree-sitter, so it's tested without compiling Zed.
- **Departure from design §7's mechanism (behaviour kept): the index answers inside the app, not as a
  built-in language server.** Zed attaches language servers only to buffers backed by files on disk,
  and neither side of Lo's diff is one (the right side is read from git on untrusted repos; the
  left side is the diff's base text with no file). So Lo sets its own `SemanticsProvider` (a public
  Zed trait, no patch) on both diff editors and on the peek and file views: it answers from
  `symbol_index`, and on a trusted repo's right side wraps the project's language-server provider so
  the server's answers win once it's ready. Everything §7 asks for stays: syntax navigation on every
  repo and both sides, server answers winning, syntax filling in while a server starts, peek, the
  read-only file view and `⌃o` back, jumping into the diff, no changes to Zed. Only "Lo starts a
  copy of itself in a server mode" goes. (Research: `research/code-navigation.md`.)
- **When a ready language server finds nothing,** Lo shows the import if the name is imported
  (design §7 "Dependencies"), otherwise the syntax answer, labelled "syntax". On untrusted repos `K`
  shows the definition's line from the index, labelled the same way.
- **Cmd-click opens the peek,** like `gd`, rather than Zed's new tab (one window, a tab per PR); a small
  Zed patch is fine if no hook exists. Lo re-applies its provider when the split rebuilds by
  observing the diff editor, with a patch only if that proves unreliable.

- **The index uses each grammar's own `tags.scm`, plus small Lo additions appended after it**
  (consts, fields, type references, components and the like), since the vendored queries miss
  much; the vendored tag wins when both match. JavaScript is parsed with the TSX grammar, as Zed does.
- **Files over 1 MB, `linguist-generated` files, minified files and files with a "generated" header
  are skipped.** Ranking is same file, same directory, an import's target, then the rest; import
  matching is a per-language heuristic over the asking file's text, never reading project manifests.
- **`tree-sitter-tags` needs `thiserror` 2.0.18 where Zed's lock has 2.0.17,** so Lo's lock moves
  that one crate; a `--seed-lock` after a Zed bump lets cargo raise it again.

## Merging (PR #12 and its review)

- **Approve and ship acts on GitHub's verdict, not Lo's prediction.** It approves first, then
  re-reads the PR and merges or joins the queue only if GitHub reports `CLEAN`, `HAS_HOOKS` or
  `UNSTABLE`. If GitHub still blocks it, it turns on auto-merge when every remaining blocker is one
  waiting fixes, and otherwise stops and shows why. Lo's prediction only words the confirmation
  ("will likely merge" or "will likely wait"). Since it merges only on those states, approve and ship
  never bypasses a rule, even for an admin; the admin override stays in the merge panel, behind
  typing the PR's number (design §9).
- **GitHub's own decision wins over Lo's model:** an `APPROVED` or `CHANGES_REQUESTED` review
  decision stays unless the viewer's approval is provably what changes it (they were the only
  requester).
- **Auto-merge and later pushes.** The snapshot records whether stale approvals are dismissed. When
  they aren't, the confirmation warns that a later push could merge with this approval, and while
  Lo runs it turns auto-merge off when it sees a newer head than the one approved, and says so.
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
  `CLEAN`, `HAS_HOOKS` or `UNSTABLE`. When GitHub says `CLEAN` but Lo sees a blocker waiting won't
  fix (a stale failed check), approve and ship stops rather than merges.
- **"Lo may not see every rule" shows on any partly explained block** until the GitHub layer
  confirms it read the branch's rules (milestone 9 wires `sees_every_rule`).

## Comments and reviews (PR #21)

- **Comments go only on lines of the PR's diff** (checked live: GitHub refuses lines outside hunks,
  and `addPullRequestReviewThread` refuses by returning a null thread with no error, which Lo treats
  as a refusal). A range's two ends must each be a diff line but may sit in different hunks, and may
  run from a removed line to an added one. Suggestions only on the head's side. GitHub's web UI has
  allowed comments outside hunks since 2025-09, but the API doesn't, so Lo offers a file-level
  comment instead. (Live checks: `research/review-api-checks.md`.)
- **New threads land on the PR's current head,** whatever revision a pending review was started on
  (checked live). So a line comment on an older revision is possible only by starting a new review at
  that revision (`addPullRequestReview` with its `commitOID`); with a review already pending, line
  comments are offered only when the right column shows the current head, and otherwise the UI offers
  to submit the pending review or switch to the latest revision. Replies join from any revision.
- **A current thread's `line` is on its first comment's commit** until GitHub recomputes it on the
  next push, so placement keys `line` to that commit, not to the latest head.
- **`C` while a review is pending adds to that review and says so,** because GitHub allows one pending
  review per user per PR.
- **New comments always go on the right column's revision;** between revisions and on a replayed
  comparison the left column takes no line comments.
- **Each thread shows once:** the right column wins over the left, and the latest head wins over where
  it was written. A current thread on an older revision it wasn't written on shows only in the panel.
- **Drafts are kept per account, PR and place** until posted; blank text deletes a draft.
- **Approve and Request changes are refused while the pending review sits on a revision other than
  the one on screen** (design §11: a review records the commit you looked at); Comment is allowed.
- **The left column takes line comments only in a revision's whole diff,** where it shows the base:
  design §8 says "either side", but GitHub only places LEFT comments against the PR's base, so between
  revisions and on replayed comparisons only the right column takes them.
- **File-level comments never go through `addPullRequestReview`'s threads** (its draft thread input
  has no subject type): a pending review is started empty, then `addPullRequestReviewThread` with
  `subjectType: FILE`; `C` on a file uses REST's file comment. A reply as the first pending comment is
  two calls too, so a failure can't lose it.
- **Commentable lines come from local git (`-U3`, the user's config ignored) when GitHub omits a
  file's `patch`,** which it does for some ordinary text files, and a file outside the PR's diff takes
  no comment at all.
- **A review started empty (for a file-level comment or a first reply) starts on the latest head,**
  so replying from an older revision never blocks approving the latest. File-level comments, like
  replies, join a pending review from any revision; GitHub places them on the head.
- **`Replay::Retargeted` is advisory:** `compare_revisions` flags any base that didn't move forward,
  which includes a correctly restacked layer whose lower layer was amended. The UI words the label from
  what it knows (the PR's base branch changed, or the layer below was force-pushed); the file lists
  are the same either way.

## Project

- **The project is renamed "Lo"** (as in "lo and behold", an old word for "look"), chosen by the user
  on 2026-11-06. The repository becomes `scornz/lo`, the app's name is "Lo", the CLI and crate are
  `lo`, the Homebrew name is `lo`; the user may get `getlo.dev` for a website and would like `lo.dev`
  eventually. It replaces the earlier "rozed" <!-- rename:keep -->
  (which the user had cleared against Zed's trademark terms). The rename lands as one PR right after
  milestone 5. Choices made for it:
  - **"Lo", capital L, in everything a person reads:** menus and the Quit item, window titles, error
    and status messages, sign-in prompts, Keychain item labels, and docs prose, even mid-sentence
    ("Lo couldn't find gh"). A lowercase l reads as a capital I in sans-serif fonts.
  - **Lowercase `lo` only for names that are code:** the crate, binary and command, the Homebrew
    cask, the repo, git refs (`refs/lo/…`), the URL scheme (`lo://`), the config folder
    (`~/.config/lo`), the user agent, and environment variables (`LO_*`).
  - macOS folders follow Zed's convention: `~/Library/Application Support/Lo` and
    `~/Library/Caches/Lo` (Zed's `paths` patch sets the app name to "Lo").
  - A binary run outside an app bundle embeds an `Info.plist` with `CFBundleName` "Lo", so the menu
    bar says "Lo". No bundle ID is set until the real bundle, since gpui turns on bundle-only
    features when one exists.
  - The CLI's own messages start with the command's name, lowercase (`lo: …`), and say "Lo" in the
    sentence; `lo --version` prints "Lo 0.1.0".
  - The bundle ID is `com.scornz.lo`, from `lo.scornz.com`, confirmed by the user (2026-11-06). It's
    code, so it's lowercase. Changing it after the first release would orphan users' Keychain items
    and preferences, so a move to a domain like `lo.dev` must happen before milestone 13's release.
  - The debug build's Keychain prefix `rozed-dev://` becomes `lo-dev://`. `rozed-dev` stays only <!-- rename:keep -->
    where it names the test org or its secrets folder, `~/.config/rozed-dev/`.
  - the Zed fork's branch `rozed/v1.27.0` keeps its name (older commits pin it); later bumps use
    `lo/v<version>`;
  - the test org `rozed-dev`, its secrets folder `~/.config/rozed-dev/` and the account `rozed-bot`
    keep their names: they're fixtures, and renaming the org would break their URLs.
  Nothing has shipped, so no saved data is migrated; local dev state is moved or recreated.
- **End-to-end fixtures live in the public `rozed-dev/e2e`** (merge queues need a public org repo);
  the private `rozed-dev/playground` stays for quick live checks.
- **Lo writes only to `rozed-dev` repos while testing.** `sharkdp/fd#2212` and other third-party
  PRs are read-only: no comments, reviews, approvals or merges. Fixture PRs in `rozed-dev/e2e` may
  be merged and its rulesets set, as `rozed-bot`, to test Lo.
- **Fixtures never force-push.** A rebased revision comes from GitHub's server-side rebase
  (`updatePullRequestBranch(updateMethod: REBASE)`); a conflicted rebase is checked read-only on a
  public PR and in GPUI tests.

## Push history (PR #31)

- **Both branches' activity is read newest first.** The head branch goes back to the push the PR
  opened with (at most 10 pages, marked truncated beyond); the base branch goes back to the PR's
  first revision (at most 3 pages). Read oldest first, a busy or long-lived branch reaches back
  years.
- **A merge commit's first parent is the base only when its second parent is the head.**
  Otherwise the base is the `before` of the push that merged it, or uncertain. It is never today's
  tip.
- **After a failure, a read waits** (the open PR's interval, doubling to 5 minutes) and an incomplete
  read is kept without a stamp, so it's retried.
- **The push history re-reads only when** `headRefOid`, `baseRefName`, `state`, the submitted-review
  count or a count of the four revision event types changes, not on every comment.
- **`304`s on `/activity` didn't count against the rate limit** in recordings, so pages are stored
  with their URL, ETag and next link and re-validated cheaply.
- **Head pushes after the PR closed are dropped** (a reused branch name would mix in another PR's
  pushes); check suites count only when `push.nextSha` is their own commit on the head branch.
- **A head or base repo that's gone or unreadable gives no pushes plus a reason, not an error**
  (404, SSO, OAuth restriction, 403/409/410/451).
- **`seen_heads` rows are per account** (removed with it) and merged across accounts on read; only
  watched PRs' heads are recorded.
- **`Poller::read_cached` names a read by its type's `CacheEntry::NAME`** and watches JSON-pointer
  fields of the open PR's check; the value is stored with those fields so a restart doesn't re-read.
- **`github` depends on `review`** (which has no dependencies), so GitHub's answers map straight into
  the review model.
- **Lost heads are remembered per PR in the clone,** so later opens don't refetch them; a force-push
  whose new commit GitHub lost shows as an unavailable revision. A review on a commit that was never
  pushed is kept as a `ReviewedCommit` record (for "your last review"), not a pushed revision.
- **`seen_heads` and `last_seen` are pruned** for PRs closed more than 30 days ago, when every
  account's cached summary agrees, keeping marks noted after the cutoff.
- **A lost force-push head is named from the REST issue timeline** (read with ETags), matched within
  the same second after dropping commits other events already name.
- **Check suites with no `push` count as "head by then"** only when the head branch's log is
  incomplete (deleted fork, unreadable repo or truncated read).
- **A base tip that already contains the revision's head is flagged uncertain,** as after a git-flow
  back-merge.

## Milestones 6–9 (planning)

- **Builds run in two reused lane worktrees** (`lane-1`, `lane-2`). Zed's crates are path
  dependencies, so a shared `CARGO_TARGET_DIR` across worktrees at different paths rebuilds them
  anyway.
- **Migration slots are fixed in `plan.md`** (2 `viewed_files`, 3 `repo_accounts`, 4 revision marks,
  5 drafts, 6 merge methods), so parallel PRs never take the same index.
- **PRs opened without signing in** keep milestone 5's read-only view; revisions, stacks, threads
  and merging say "Sign in to see …", since GraphQL needs a token.
- **Multi-layer stack diffs are read-only:** each PR keeps its own threads and viewed marks.
- **The PR tab is split into per-feature files first (6a),** with header slots, one `bind_keys` per
  feature and a test that fails on duplicate key bindings, so parallel UI PRs don't collide.

## Weekly Zed build (PR #27)

- **The weekly run stays green when Zed breaks Lo.** It reports through one `zed-update` issue
  (opened, updated or closed) and fails only when its script can't run, so nobody is told twice.
- **Two jobs:** the macOS build job runs Zed's and crates.io's build scripts with `contents: read`
  only; a separate ubuntu job holds `issues: write`.
- **The patches replayed are the ones `main` pins** (`<tag>..<gitlink>`), not the fork branch's head.
- **No cache at all in the weekly job** (`cache: false` on mise-action too): it's the first workflow
  that builds a new Zed release on `main`, and cache entries written there are ones every PR job
  restores. Unused caches are evicted within a week anyway.
- **The issue body is data:** fenced with a fence longer than any backtick run, `@` mentions
  defused, lines and the body capped under GitHub's limit.
- **Seen on `v1.28.0-pre`:** the worktree-settings patch conflicts in one context-only hunk
  (`project_settings.rs`), and a new crate, `github_releases` (language-server and debug-adapter
  downloads), is compiled in. The bump must review it and update `docs/spike.md`.
