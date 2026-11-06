# rozed design

Approved on 2026-11-05.

| # | Section | Status |
|---|---|---|
| 1 | Overview and scope | Approved |
| 2 | Architecture | Approved |
| 3 | GitHub, git and accounts | Approved |
| 4 | Stacks | Approved |
| 5 | Diffs | Approved |
| 6 | Revisions and viewed files | Approved |
| 7 | Code features | Approved |
| 8 | Comments, reviews and the timeline | Approved |
| 9 | Merging and the merge queue | Approved |
| 10 | Keyboard, windows and opening links | Approved |
| 11 | Errors and edge cases | Approved |
| 12 | Testing | Approved |
| 13 | Decisions and trade-offs | Approved |

## 1. Overview and scope

rozed ("read-only Zed") is a native macOS app for reviewing GitHub pull requests. It's for people
who read far more code than they write, such as someone reviewing the PRs their coding agents open.
It talks to GitHub directly and has no backend of its own.

### Where it beats GitHub's web UI

- **Stacks.** Any chain of PRs is a stack: native GitHub stacks, chains of base branches, and PRs
  that contain another PR's commits. Stacks show as a tree, you review them layer by layer, and
  they merge as one unit wherever GitHub allows.
- **Diffs.** Side-by-side by default, with moved-code detection, a syntax-aware view, files in
  review order and rebase-aware "changes since my last review."
- **Code.** Syntax-based navigation everywhere, plus types, hovers and exact references from
  language servers on repos you trust.
- **Merging.** Approve and ship on one key, a clear "why can't this merge" panel, and full
  merge queue support.

### Principles

- **Read-only.** rozed never edits files or pushes commits. It writes to GitHub only to review and
  merge: comments and their attachments, reviews, viewed marks, merges, queue and auto-merge
  changes, and changing a PR's base branch to form a stack.
- **No AI.** No AI features, and none of Zed's AI crates.
- **No backend, no telemetry.** rozed talks only to GitHub and to language servers' official
  download sources. Updates come from GitHub Releases.
- **Never runs a repo's code.** No dependency installs or build steps, and language servers run
  only on repos you trust.
- **Keyboard first.** Every action has a key.

### Scope

- **v1:** everything above, plus several GitHub accounts, opening PRs from links, comments and
  reviews, and the full conversation timeline.
- **Later:** a cross-repo inbox, reactions, @-mention autocomplete, a browser extension, dependency
  types, GitHub Enterprise, and Linux and Windows. v1 is macOS only.
- **Never:** editing code, applying suggestions (that's a commit), AI features, a rozed backend.

### Distribution

Open source under GPL-3.0, which building on Zed's crates requires. rozed ships as a signed,
notarized download and a Homebrew cask. It skips the Mac App Store, whose rules forbid
downloading and running code, which language servers need.

## 2. Architecture

rozed is a Rust app built on Zed's crates, which it uses as libraries.

### Pinning Zed

Zed sits in the repo as a git submodule at `third_party/zed`. The submodule points at a fork of Zed
holding rozed's patch branch: a Zed release tag plus a short list of patches, one commit each.
rozed's crates depend on Zed's by path. To update, rebase the patch branch onto a newer Zed release
and fix whatever breaks in rozed. Cargo ignores a dependency's `[patch]` section, so rozed copies
Zed's into its own workspace.

### Zed crates

- **Used:** `gpui`, `ui`, `theme`, `settings`, `editor` (including its side-by-side diff),
  `multi_buffer`, `buffer_diff`, `language`, `languages`, `lsp`, `project`, `workspace`, `vim`,
  `command_palette`, `file_finder`, `markdown` and `extension_host`.
- **Left out:** Zed's agent and model crates, edit prediction providers, collaboration and calls,
  the terminal, the debugger, the extension store and Zed's auto-updater.
- **Comes along unused:** some account, telemetry and AI-settings code that `editor` and
  `workspace` depend on. rozed never starts it and configures no Zed endpoint.

### rozed's crates

| Crate | Does |
|---|---|
| `lo` | The app: startup, windows, menus, settings and update checks |
| `github` | Sign-in, accounts, GraphQL and REST calls, polling and the local cache |
| `repo_cache` | Partial clones, fetching PR commits, checkouts for trusted repos and the git check |
| `stacks` | Finding stacks, the stack tree and turning chains into GitHub stacks |
| `review` | The PR model: revisions, rebase-aware comparison, viewed marks, threads and the pending review |
| `diff_extras` | Moved-code detection and the syntax-aware diff |
| `review_ui` | The PR tab: stack tree, file list, diff views, threads, timeline and merge panel |

### Languages

- **Built in:** the languages in Zed's `languages` crate, including TypeScript, JavaScript, Python,
  Rust and Go.
- **Bundled extensions:** Swift, Kotlin and Terraform, plus other popular languages such as Ruby,
  Java, C#, PHP and Elixir, run through Zed's extension runtime. The set is fixed when rozed is
  built. There's no extension store, and nothing comes from Zed's servers.
- **Language servers:** downloaded on first use from each one's official source, and only for
  trusted repos. rozed uses an installed copy when it finds one.

### Runtime

One process. Git and language servers run as child processes, and GitHub calls run on GPUI's
background executor.

### First milestone

The first build is a bare app that opens a PR in Zed's side-by-side diff with a language server
running. It proves the approach before anything else is built: it lists which Zed code is compiled
in and confirms that nothing contacts Zed's servers. It also checks whether GitHub accepts rozed's
sign-in for attachment uploads.

## 3. GitHub, git and accounts

### Sign-in

- **Accounts.** Each account signs in through GitHub's device flow: rozed shows a code and opens
  GitHub's sign-in page. rozed can also import accounts from the `gh` CLI. Tokens are stored in
  the Keychain, one per account.
- **Scopes.** rozed asks for `repo` (read and review private repos, and merge), `read:org` (team
  review requests and code owner teams) and `workflow` (GitHub refuses to merge a PR that changes
  `.github/workflows` without it).
- **Which account.** A PR opens with the account that can see its repo. If several can, rozed asks
  once per owner and remembers the answer.
- **Blocked by an org.** If an organization blocks third-party apps, rozed shows GitHub's link to
  request approval.

### GitHub API

- **GraphQL** covers PRs, files, reviews, threads, viewed marks, the timeline, stack membership and
  the merge queue.
- **REST** covers the Stacks API, async merges, a branch's rules and commit comparisons.
- **Polling.** The open PR and its queue are checked every 15–30 seconds, everything else less
  often, and everything slows to every 5 minutes while rozed is in the background. Each poll asks
  first whether anything changed, and only then fetches the details. REST calls use ETags, so
  unchanged answers don't count against the rate limit.
- **Rate limit.** rozed tracks how much of the limit is left, slows polling when it runs low and
  tells you when it does.

### Local data

- **Database.** A SQLite file in Application Support stores cached PR data for instant reopening,
  "last seen" revisions, which account to use per owner, trusted repos and recent PRs. Only the
  cached PR data can be thrown away safely.
- **Repo copies.** Each repo gets a bare partial clone in `~/Library/Caches/Lo`. rozed fetches
  PR heads (`refs/pull/<n>/head`, including PRs from forks) and base branches as it needs them. It
  keeps every revision it has fetched under its own refs, so force-pushed commits survive. Git
  signs in with the account's token through `GIT_ASKPASS`, and your git config is never touched.
- **Checkouts.** Trusted repos get a checkout at the PR head for language servers. The old side is
  read straight from git.
- **Cleanup.** Settings shows how much space the cache uses and can clear it. Repos unused for 30
  days are removed.

## 4. Stacks

### What makes a stack

PR B sits on PR A when any of these is true. When more than one is, the first in the list wins.

1. **GitHub stack.** GitHub says they're adjacent layers of a native stack.
2. **Base branch.** B's base branch is A's head branch.
3. **Shared commits.** B contains A's latest commit. If B contains several open PRs' commits, its
   parent is the nearest one: the PR whose latest commit is closest to B's.

A base branch with no PR, such as `feature-x`, is the stack's root, just like `main`. A PR can have
several children, so a stack is a tree, with children in the order they were opened.

### Finding stacks

When you open a PR, rozed loads its repo's open PRs, with their branches and recent commits, in a
few GraphQL pages. It builds the stack graph from that and refreshes it as it polls.

### Layer diffs

- **One layer.** Each layer is compared against the layer below it, not the stack's root, using
  the local copy of the repo.
- **Several layers.** The whole stack, or any run of layers, can be viewed as one diff.
- **After a merge.** When a lower layer is squash-merged, the layer above is compared against the
  merged layer's last commit. Changes that already landed stay hidden, and the layer is marked
  "needs rebase."

### The stack tree

A sidebar shows the tree. Each layer shows its title, number, author, review state, checks, queue
state, any "needs rebase" mark, and how many of its files you've viewed. `]s` and `[s` walk the
tree from top to bottom, finishing one branch before starting the next. A link to any layer opens
the whole stack in one tab, at that layer.

### Turning chains into GitHub stacks

"Make this a GitHub stack" lists what will change and asks before doing anything:

1. Any PR found through shared commits gets its base branch pointed at the head branch of the PR
   below it.
2. rozed creates the stack through the Stacks API, from the bottom PR to the top.

GitHub stacks can't branch, so for a tree rozed converts the path through the layer you're on. The
option isn't offered when a PR comes from a fork, when the repo doesn't have stacks turned on, or
when a PR is already in a merge queue.

## 5. Diffs

### The diff view

rozed uses Zed's side-by-side diff, which keeps both sides aligned.

- **Layout.** Side-by-side by default. `Tab` switches sides, and one key switches to inline.
- **Changes.** Changed words are highlighted within changed lines.
- **Context.** Unchanged code is collapsed and expands in place. One key shows the whole file.
- **Whitespace.** Whitespace changes are shown, and a key hides them.
- **Renames.** A renamed file shows as a rename, with any changes to its contents.
- **Generated files.** Files the repo's `.gitattributes` marks `linguist-generated`, and common
  lockfiles (`pnpm-lock.yaml`, `package-lock.json`, `yarn.lock`, `Cargo.lock`, `go.sum`,
  `uv.lock`, `poetry.lock`, `Gemfile.lock` and others), start collapsed.
- **Binary files.** Images show before and after, side by side. Other binary files just say
  "binary file changed."
- **Very large files.** Files with more than 5,000 changed lines start collapsed and load on a key.

### Moved code

A block of at least 3 non-blank lines that's deleted in one place and added in another shows as a
move, within a file or across files in the PR. Changes in indentation don't count. Both ends link
to each other ("moved from `api/user.ts:40`"), and only edits inside the block are highlighted.

### Syntax-aware view

An optional view for each file, switched with one key. It uses Zed's tree-sitter grammars with
difftastic's algorithm, whose MIT license lets rozed use it. Each column shows real lines with
real line numbers, so a comment lands on the line you pick. If GitHub won't take a comment on that
line, rozed offers a file-level comment instead. Files with no grammar, or too large for the
algorithm, stay in the normal diff.

### File order

Files are listed in review order: each source file followed by its tests (matched by name, such as
`foo.ts` and `foo.test.ts`, `foo_test.go` or `test_foo.py`), then config (JSON, YAML and TOML
files, dotfiles and `.github/`), then generated files. One key switches to path order. The file
list shows viewed marks and comment counts.

## 6. Revisions and viewed files

### Revisions

Each push to a PR's branch is a revision, numbered r1, r2 and so on, taken from the PR's timeline
of pushed commits and force-pushes. A revision's base is where its head branched off the base
branch.

- **Picker.** Compare any two revisions. Two markers show where you left off: "your last review"
  (the commit your latest review was made on) and "last seen here" (the newest revision on screen
  when you last left the PR, saved on this Mac only).
- **Default.** rozed compares the more recent of those two markers with the latest revision. With
  neither marker, it shows the full diff. If nothing changed since the marker, it shows the full
  diff with a note: "Nothing new since r4."
- **File list.** When comparing two revisions, the file list shows only files that changed between
  them.

### Rebase-aware comparison

When the base moved between two revisions, rozed replays the older revision's changes onto the
newer base in its local copy (`git merge-tree`). It then compares the result with the newer
revision, so only the author's changes show. If the replay conflicts, rozed falls back to a plain
comparison and says so. The same applies to a stacked layer when the layer below it gets a new
push.

### Viewed marks

- **GitHub's marks.** rozed reads and sets GitHub's own per-file viewed marks, so they match the
  web UI and other Macs. Each PR in a stack has its own marks.
- **Changed since viewed.** When a push changes a viewed file, GitHub resets it to "changed since
  viewed." rozed remembers, on this Mac, the revision at which you marked each file viewed, and
  opens the file to what changed since then. For a file marked viewed elsewhere, it shows what
  changed since your last review instead.
- **Keys.** `e` marks a file viewed and moves to the next unviewed file. Pressing it again on a
  viewed file clears the mark.

## 7. Code features

### Syntax-based, on every repo

Highlighting, a file outline, and go-to-definition and find-references by name across the whole
repo. The right side of a diff navigates the new commit, and the left side the old one.

- **Index.** rozed builds a symbol index from tree-sitter tag queries, reading files from its local
  copy. The first index of a repo downloads its files at that commit, and later commits reuse most
  of them.
- **How the editor uses it.** The index runs as a built-in language server: rozed starts a copy of
  itself in a server mode. Zed's editor talks to it like any other language server, so this needs
  no changes to Zed.
- **Name collisions.** When a name matches several definitions, the peek lists them all: same file
  first, then same directory, then a matching import.

### Language servers, on trusted repos

- **What they add.** Type hovers, inline type hints (off by default, toggled with a key), and exact
  definitions and references. They run against a checkout of the PR head, so they serve the new
  side only. The old side keeps syntax-based navigation.
- **Precedence.** Language server answers win. Syntax-based answers fill in while a language server
  starts up.
- **Dependencies.** Dependencies are never installed, so a symbol from one shows its import and
  nothing more.

### Trust

Language servers run only when the repo is trusted and the PR's branch lives in that repo. A PR
from a fork is someone else's code, even in a repo you trust.

- **Untrusted repo.** A banner says "Language servers are off for this repo," with buttons to trust
  the repo or dismiss the banner. You're asked once per repo.
- **PR from a fork.** The banner offers to trust that one PR instead.
- **Settings.** Settings lists trusted repos and PRs, and you can remove any of them.

### Navigating

- **Peek.** `gd` opens the definition in a peek under the line. One key opens the full file, read
  only, in the same tab, and `⌃o` brings you back to the diff.
- **Into the diff.** If the definition is in a file the PR changes, rozed jumps to that spot in the
  diff instead of the plain file.
- **Other keys.** `gr` lists references in a panel. `K` shows the type hover. `⌃o` and `⌃i` go back
  and forward.

## 8. Comments, reviews and the timeline

### Comments

- **Where.** `c` comments on the current line, and `v` then `c` on a range of lines, on either side
  of the diff. A key on a file's header leaves a file-level comment.
- **In the diff.** Threads appear under their line. Unresolved threads start open, and resolved
  ones start collapsed.
- **Threads.** You can reply, resolve and unresolve, and edit or delete your own comments.
- **Writing.** The comment box is a Zed editor with Markdown highlighting, Vim keys and a preview
  toggle.
- **Suggestions.** `s` opens the comment box with the selected lines in a `suggestion` block, ready
  to edit. It works on the new side only, because GitHub can only apply suggestions there. Applying
  them happens on GitHub.
- **Outdated threads.** A thread whose line has since changed appears where it was written when you
  view that revision, and is labeled outdated elsewhere.
- **Attachments.** Drag, paste or pick a file (PNG, JPEG, GIF, WebP, SVG, MP4, MOV or WebM) into
  any comment box, including review comments and the review summary. rozed uploads it the way
  `gh --attach` does and inserts the Markdown. Images and GIFs can be up to 10 MB, and videos 10 MB
  on free plans or 100 MB on paid ones. Uploading needs write access to the repo. Where you don't
  have it, rozed says so, and you can still paste or drop a GIF's web address, which goes in as an
  image. If GitHub won't take rozed's sign-in for uploads, rozed uses an imported `gh` sign-in, and
  without one it accepts web addresses only. There's no built-in GIF search.

### Reviews

- **Pending review.** Comments collect into a pending review by default. It's stored on GitHub, so
  it survives quitting rozed and shows as pending on the web. `C` posts one comment right away
  instead.
- **Submitting.** `⌘⏎` opens the submit dialog. It has a summary box, a choice of Comment, Approve
  or Request changes, the number of pending comments, and the account you're acting as. On your own
  PRs only Comment is offered, since GitHub allows nothing else there.
- **Approve and ship.** `⌘⇧⏎`, described in section 9.

### Threads panel

Lists every thread on the PR. It can show unresolved threads, all threads or only yours, and
labels outdated ones. `]t` and `[t` jump between threads.

### Timeline

The PR's full GitHub timeline: the description, comments, reviews, pushes and force-pushes, CI
results, labels, assignments, review requests, cross-references, merges, queue events and
deployments. Markdown is rendered, GIFs play, and images load with the account's token so images in
private repos show. A comment box at the bottom posts a comment on the PR itself. The PR's title,
description, labels, reviewers and draft status are read-only in rozed.

## 9. Merging and the merge queue

### Merge status

Every PR tab shows a one-line merge status in its header, and `m` opens the merge panel. When a PR
can't merge, the panel lists why: missing approvals (including which code owners), required checks
that failed, are running or haven't reported (each linked to its run), unresolved conversations,
conflicts, an out-of-date branch, draft status, and other rules on the branch. rozed pieces this
together from several API calls. If GitHub reports a block rozed can't explain, it says so and
links to the PR on GitHub.

### Merging

- **Method.** Only the methods the repo allows are offered. rozed remembers the last one you used
  in each repo. A squash commit's title and message start from the repo's setting and can be edited.
- **Queue.** If the base branch requires a merge queue, the button becomes "Add to merge queue,"
  and the queue's own settings decide the method and message.
- **Confirmation.** Every merge asks first. The confirmation names the account, the method, the
  target branch and, for a stack, every layer that will merge.
- **Branches.** rozed never deletes branches. The repo's own auto-delete setting applies.
- **Out-of-date branches.** rozed shows that a branch is out of date but doesn't offer GitHub's
  "Update branch," because that adds a commit to the PR. The author or agent updates it.

### Approve and ship

`⌘⇧⏎` approves (skipped on your own PRs), then merges or joins the queue if nothing blocks it. If
it's only waiting on checks or other approvals, rozed turns on auto-merge so the PR goes in once
ready. If something waiting won't fix blocks it (conflicts, failed checks, changes requested), it
stops after approving and shows why. One confirmation lists exactly what will happen.

### Stacks

Merging a layer of a GitHub stack merges every layer below it, as one unit: they merge, join the
queue or don't merge at all. rozed submits it through the async merge API and shows progress until
it's done. Other chains are offered "Make this a GitHub stack" first (section 4). When that isn't
possible, rozed merges one layer at a time: it merges the bottom PR, points the next one at the
root branch and marks it "needs rebase."

### The merge queue

- **Queue view.** For each base branch: every entry with its position, state, time in the queue and
  estimated time to merge, which entries are being tested together, and your PRs highlighted.
- **Removals.** When a PR leaves the queue, rozed says why (failed checks, a conflict, a timeout or
  someone removed it) and links to the failing check.
- **Controls.** Remove a PR from the queue, jump it to the front if you're allowed, and turn
  auto-merge on or off.
- **Admin override.** Offered only to people allowed to bypass the rules. To confirm, you type the
  PR's number.

## 10. Keyboard, windows and opening links

### Keys

Vim-style single keys by default, from Zed's `vim` crate. Arrow keys also work. Nothing in a diff is
editable, so Vim's insert mode exists only in comment boxes. Every key can be remapped in a keymap
file in Zed's format, kept with the settings in `~/.config/lo/`. `?` shows every key.

| What | Keys |
|---|---|
| Move | `j`/`k` line, `w`/`b` word, `gg`/`G` top and bottom |
| Jump | `]c`/`[c` change, `]f`/`[f` file, `]t`/`[t` thread, `]s`/`[s` stack layer |
| Act | `v` select lines, `c` comment, `C` comment right away, `s` suggest, `e` mark viewed, `Tab` switch side |
| Code | `gd` definition, `gr` references, `K` type hover, `⌃o`/`⌃i` back and forward |
| Review | `m` merge panel, `⌘⏎` submit review, `⌘⇧⏎` approve and ship |
| Find | `⌘O` open PR, `⌘P` files, `⌘⇧P` command palette, `/` search |

### Window

One window, with a tab per PR. A whole stack lives in one tab. Opening a link to a PR that's
already open switches to its tab, at that layer.

- **Header.** The PR's title, number and author, the account you're acting as, the merge status
  and the revision picker.
- **Left.** The stack tree, with the file list below it.
- **Middle.** The diff. One key switches it to the timeline and back.
- **Right.** The threads panel.

Each panel opens and closes with a key. With no PR open, rozed shows the PRs you opened recently,
across all accounts.

### Opening links

- **`⌘O`** opens a box that's already filled in when the clipboard holds a PR link.
- **Link routers.** rozed registers as an app that can open web links, without becoming the
  default browser, so Finicky, Velja or Choosy can send PR links to it. rozed's docs include
  ready-made rules. Any link rozed can't show goes back to the default browser.
- **`lo://` and the CLI.** `lo://github.com/<owner>/<repo>/pull/<n>` opens a PR. "Install
  command-line tool" in the app menu adds `lo <url>`.
- **What counts as a PR link.** `github.com/<owner>/<repo>/pull/<n>`, plus its `/files`,
  `/commits` and `/checks` pages. Links to a file, a line, a commit or a comment open at that
  spot.

## 11. Errors and edge cases

### Acting on what you saw

- **Reviews** are recorded against the commit you were looking at, so GitHub shows exactly what you
  approved.
- **Merges** also carry the commit you were looking at. If someone pushed after it, GitHub refuses
  the merge, and rozed shows the new revision with the changes since the one you saw.
- **Approve and ship** checks for a newer push before it does anything, and if there is one it
  stops before approving.

### GitHub

- **Offline.** Cached PRs open read-only, with a banner showing when the data is from. Commenting,
  reviewing and merging are off until you're back online.
- **Unsent text.** Text in comment boxes is saved on this Mac until it's posted, so a crash or
  restart doesn't lose it.
- **Rate limit used up.** A banner shows when it resets. Polling pauses until then.
- **Signed out.** If an account's token is revoked, that account's tabs ask you to sign in again.
  Other accounts keep working.
- **Single sign-on.** If an org requires SSO approval for the token, rozed shows GitHub's link to
  grant it.
- **PR state changes.** When a PR is closed, merged or reopened while it's open in rozed, the tab
  updates and the actions change to match.

### The local copy

- **First open.** The diff appears straight away from GitHub's API. Whole-file context, navigation
  and rebase-aware comparison turn on when the local copy is ready, with progress shown.
- **Fetch fails.** Those features stay off, a banner explains why, and rozed retries.
- **Disk full.** A banner offers to clear the cache.
- **Missing revisions.** A force-pushed revision that GitHub no longer has, and that rozed never
  fetched, is shown as "no longer available."

### Stacks

- **Busy repos.** Stacks are found among the 1,000 most recently updated open PRs. A note says so
  when a repo has more.
- **Conversion fails partway.** rozed lists what it changed and offers to put the base branches back.
- **Async merge fails.** Branch rules are checked when the merge runs, so a failure can arrive
  late. rozed shows GitHub's reason.

### Language servers

If one fails to download, crashes or hangs, syntax-based navigation carries on, the status bar
shows the problem, and a button restarts it. A repo's language servers stop when its last tab
closes.

## 12. Testing

### Logic

Unit tests cover the parts that make decisions, each fed from fixtures:

- **Stacks:** base-branch chains, shared commits, the nearest-parent rule, trees, native stacks
  winning, and layers left behind by a squash merge.
- **Revisions:** building revisions from a timeline, the default comparison, and viewed marks.
- **Diffs:** moved-code detection, and mapping a line in the syntax-aware view back to a real line
  for comments.
- **Everything else:** review order, parsing PR links, explaining why a PR can't merge, and every
  outcome of approve and ship.

### Git

Tests build real repos in temporary folders, scripting commits, rebases, force-pushes and squash
merges. They check rebase-aware comparison, fetching PR refs into a partial clone, and that fetched
revisions survive a force-push.

### GitHub API

- **Typed queries.** GraphQL queries are checked against GitHub's published schema and generate
  Rust types, so a change on GitHub's side fails the build rather than the app.
- **Replayed responses.** Responses recorded from a real test repo are served by a fake server, so
  tests run offline and fast.

### UI

GPUI's test harness sends keystrokes and checks what changes. It also checks that every default key
maps to an action, and that `?` lists them all.

### End to end

A public repo in a GitHub org made for rozed, driven by a bot account, holds scripted scenarios:
stacks, a merge queue, force-pushes and fork PRs. Merge queues need an org-owned repo, which is why
it lives in an org. These tests run nightly and on demand, not on every PR. While they run, a proxy
records every host rozed contacts, and the run fails if anything other than GitHub and the
language-server download sources shows up.

### CI and Zed updates

GitHub Actions on macOS runs tests, `clippy` and `rustfmt` on every PR, and builds, signs and
notarizes on release tags. A weekly job builds against Zed's latest release and opens an issue if
it breaks, so each monthly update starts with a known list of breakages.

### Performance

Time to the first diff and scrolling smoothness are measured on a large fixture PR. Budgets are set
after the first milestone, once there are real numbers.

## 13. Decisions and trade-offs

| Decision | Rejected | Why | Cost we accept |
|---|---|---|---|
| Build on Zed's crates as libraries | A new GPUI app; copying Zed crates; a full fork | The GPUI app's code view and language features would trail Zed's for a long time. Copied crates fall behind. A fork carries all of Zed and endless merges into code we deleted. | GPL-3.0, breakages on each monthly Zed update, unused Zed code compiled in |
| OAuth app | GitHub App | A GitHub App can't comment, approve or merge where it isn't installed, which rules out most open-source repos | Broad `repo` scope, and orgs that restrict apps must approve rozed |
| No backend, polling | A server receiving webhooks | Nothing to run, pay for or trust | Up to 15–30 seconds of lag, and a rate limit to budget |
| A local partial clone | GitHub's API alone | Whole-file context, navigation, rebase-aware comparison and language servers all need one | git is required, and repos take disk space |
| Three stack rules, including shared commits | Base-branch chains only | Catches agents branching off another branch but targeting `main`, and stacks from forks | More data per repo, and the nearest-parent rule can guess wrong |
| Convert chains to GitHub stacks to merge them | Merging one layer at a time | Stacks merge as one unit, including through the queue | It changes PR base branches for everyone, on a feature in preview |
| Language servers only for trusted repos and same-repo PRs | Language servers everywhere | Some language servers run code from the repo | One click per repo, and PRs from forks get syntax-based navigation only |
| No dependency types | Installing dependencies | Every repo installs differently, and rozed never runs a repo's code | Symbols from dependencies show their import only |
| Language extensions bundled at build time | Zed's extension store | The store contacts Zed's servers | A larger app, and new languages arrive only with rozed releases |
| Attachments uploaded the way `gh --attach` does | No attachments | You want GIFs in reviews | The upload endpoint is undocumented and needs write access |
| Comments collect into a pending review | Posting each one right away | Agents act on each comment as it lands | One extra key to post a single comment |
| Read-only: no "Update branch," applying suggestions, PR edits or branch deletion | Matching GitHub's web UI | rozed changes nothing but reviews, comments and merges | Those actions stay on GitHub |
| Several accounts in v1 | One account | You work across several GitHub accounts | Choosing an account for each owner, plus account names shown everywhere |
| Signed download and Homebrew | Mac App Store | App Store rules forbid downloading and running code | No App Store discovery or updates |
