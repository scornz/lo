# e2e fixtures

`fixtures.sh` sets up the pull requests in [`rozed-dev/e2e`](https://github.com/rozed-dev/e2e) that
Lo is checked against: image previews (milestone 5), revisions (6), stacks (7), comments and reviews
(8), code features and trust (10), moved code and the syntax-aware view (11), the file picker (12),
and the nightly suite, performance budgets and app check (13). It runs as `rozed-bot` and builds
nothing.

`trees.sh` writes the files of E6 onwards. It reads no network or token, and writes the same bytes
each time. Its awk is checked on macOS's awk only so far, though it uses nothing gawk or mawk
would read differently.

```sh
(set -a; . ~/.config/rozed-dev/bot.env; set +a; script/e2e/fixtures.sh setup)
```

| Command | What it does |
|---|---|
| `setup` | Creates any fixture that's missing, and brings E8 up to date. Run twice, the second run changes nothing. |
| `status [--json]` | Prints each fixture's PR numbers, branches and heads. For E6 onwards and #17, also the head of the branch its PR targets (`base_head`) and, except for E10, its changed files, each with the SHA-256 of its path that GitHub's `#diff-<hash>` links use. E9 adds its review and comment IDs, and E10 its size instead of its files. Tests read `--json`. |
| `push-r4` | Pushes E1's r4, a normal push touching `e1/a.txt` and `e1/b.txt`. |
| `rebase-r5` | Moves `main` one commit, then rebases E1's PR on GitHub (`updatePullRequestBranch`, `REBASE`). |
| `push-r6` | Pushes E1's r6 after the rebase, a normal push changing line 3 of `e1/a.txt`. |
| `reset-e1` | Closes E1's PR and opens a fresh one, at r3, on the next branch (`e1/revisions-2`, `-3`, …). |

- **Credentials.** The token comes from `ROZED_BOT_TOKEN`, and the script checks that it belongs to
  `rozed-bot` before doing anything.
  - It runs `/opt/homebrew/bin/gh` (or `LO_E2E_GH`) with an empty config, and gives gh the token
    on each call.
  - Git runs in a scratch clone, deleted on exit, without your git config or `GIT_*` variables,
    and with a `HOME` of its own, so your global ignore and attributes files and `~/.netrc` don't
    apply. It pushes over HTTPS through a credential helper that reads the token from the
    environment.
  - A commit that leaves any file out stops the script, so an ignore rule can't drop a fixture's
    file.
  - It refuses to run under `bash -x`, which would print the token.
- **What it writes,** only to `rozed-dev/e2e`:
  - branches under `e1/`, `e2/`, `e3/`, `e5/`, `e6/`, `e8/`, `e9/`, `e10/`, `e11/` and `e12/`, and
    their PRs;
  - E1's review, and E9's review with its two threads;
  - E2's native stack;
  - squash merges of `e3/x` and `e1/move-main*`;
  - E8's updates: a new commit on `e8/hostile`, and its PR's base and description.
- **What it never does:** push to `main`, force-push or delete a branch. `main` moves only through
  PRs it opened, which it checks by author, base and title before merging.
- **Base branches.** E6, E9–E12 change files that have to exist first, so each has a base
  branch, `e<n>/base`, that adds their base side to `main`, and its PR targets that branch. E8 has
  one too, `e8/base`, holding only what `main` held when E8 was made, so merging E8 can't put its
  baits on `main`. `main` never moves for any of them, so E1's base still moves only under
  `rebase-r5`. #17 only adds harmless files, so it targets `main`.
- **E8 is kept up to date.** Its baits follow Lo's hardening, so when `trees.sh` changes them,
  `setup` pushes the change to `e8/hostile` as a new commit, a normal push, and sets the PR's base
  and description to match. It stops if E8 was ever merged into anything but `e8/base`.
- **Finding what exists.** It finds fixtures by branch, PR, commit subject and review text, and
  counts only rozed-bot's PRs from `rozed-dev/e2e` itself, never a fork's. A failed GitHub request
  stops it before it writes anything.
- **If `reset-e1` fails partway,** finish it with `setup`. Running `reset-e1` again starts yet
  another E1.
- **E2, E3 and E6 onwards have no reset.** `setup` accepts their PRs in any state, so once a later
  test merges or closes them, they're not rebuilt. E8 is the exception, as above.
- **Not yet:**
  - **E4** (merge states, rulesets, merge queue). Its protected bases will be separate branches:
    `gated` (required checks, an approval and CODEOWNERS) and `queued` (the merge queue). `main`
    stays unprotected, so `rebase-r5` and E3's merge keep working.
  - **E5** (PRs the bot can approve). #17's branch, `e5/images`, predates it, so E5's branches need
    other names.
  - **E7** (a fork PR, `rozed-bot/e2e:e7/fork` → `rozed-dev/e2e:main`) waits for the user's OK: the
    fork would live under `rozed-bot`, outside `rozed-dev`. Until then `sharkdp/fd#2212`, already
    from a fork, stands in, read-only.

## Fixture map

The numbers are from 2026-11-06. `reset-e1` gives E1 a new number, so tests read `status --json`.

| Fixture | PR | Branch → base | Checks |
|---|---|---|---|
| **E1** revisions | #16 | `e1/revisions-4` → `main` | Milestone 6. **Picker:** r1–r3, with "your last review" on r2 (rozed-bot's COMMENT review), opening r2→r3. **Markers:** reopening shows "Nothing new since r3"; after `push-r4`, it opens r3→r4 ("last seen here"). **Changed since viewed:** `a.txt` viewed at r3 shows r3→r4 with the note. **Rebase:** after `rebase-r5` and `push-r6`, r4→r6 spans the rebase with the banner and shows only r6's one hunk in `e1/a.txt`. The plain r4..r6 diff also has `e1/base.txt`, and r4→r5 alone shows no files. |
| | #14 | `e1/revisions-3` → `main` | Closed by `reset-e1` after a full run: r1–r6, with the server-side rebase between r4 and r5. |
| | #15 | `e1/move-main-3` → `main` | #14's `rebase-r5` moved `main` with this PR, merged. It touches only `e1/base.txt`, which E1 never changes, so the rebase can't conflict. |
| | #1, #13 | `e1/revisions`, `e1/revisions-2` → `main` | Earlier E1s, closed. #1 ran to r5, with `main` moving twice under it (#12, see below). |
| **E2** stack tree | #2 | `e2/a` → `main` | Milestone 7. **Tree:** the base-branch chain a, b, c, with children in the order they were opened. **`]s`/`[s`:** walks the chain depth-first. **Layer diffs:** a multi-layer `a..c` diff. |
| | #3 | `e2/b` → `e2/a` | Has two children: c, then e. |
| | #4 | `e2/c` → `e2/b` | |
| | #5 | `e2/e` → `e2/b` | A sibling of c, opened after it. |
| | #6 | `e2/d` → `main` | Built on c's head (and so containing a's and b's) but targeting `main`. Only the shared-commits rule, nearest parent first, links it to c, and its layer diff shows only its own change. |
| | #7, #8 | `e2/s1` → `main`, `e2/s2` → `e2/s1` | A native GitHub stack (#9), separate from the chain. s2 → s1 also matches the base-branch rule, so the tree looks the same if native-stack detection breaks. Only the rule tooltip tells them apart. |
| **E3** needs rebase | #10 | `e3/x` → `main` | Two commits, head `e86aa48`, squash-merged into `main` as `04bdf22`. Its branch is kept. |
| | #11 | `e3/y` → `e3/x` | One commit on x. It still targets `e3/x`, because GitHub retargets only when the base branch is deleted. `setup` fails if that changes, so "Automatically delete head branches" must stay off. It shows "needs rebase" and compares against x's head, `e86aa48`, not the squash commit, so only y's change shows. |
| **E6** code | #18 | `e6/code` → `e6/base` | Milestones 10 and 13; [below](#e6-code). **Untrusted:** `gd` on `parse` lists both definitions, same folder first, and `enter` jumps into the diff (10d); `g r` lists both and their callers (10f); the left side's `g f` shows the base version (10e). **Trust (10g):** the banner; trusting checks it out and starts rust-analyzer, the trust survives a relaunch, and removing E6 from the list stops it. **Trusted (10h):** `gd`, `g r` and `K` come from servers in all four languages, labelled "syntax" until they're ready, and `gd` on `Deserialize` shows its `use` line. **13d** drives `gd` and `g r` untrusted; **13f** opens it untrusted, trusts it and waits for rust-analyzer, vtsls and basedpyright. |
| **E8** hostile files | #19 | `e8/hostile` → `e8/base` | Milestone 10 (10h's hardening, behind 10g's trust); [below](#e8-hostile-files). Trusted, after 2 minutes of scrolling and hovering, no marker exists: `lo-e8-rustc-wrapper` (`.cargo/config.toml`), `lo-e8-rust-analyzer`, `-gopls`, `-vtsls` and `-basedpyright` (`.zed/settings.json`), `lo-e8-tsserver` (`node_modules/typescript`) and `lo-e8-python` (`.venv/`). The two toolchain baits, `e8/pinned/rust-toolchain.toml` and `go.mod`, leave none, so the check reads the servers' environments instead. |
| **E9** moves and syntax | #20 | `e9/moves` → `e9/base` | Milestones 11 and 13; [below](#e9-moves-and-syntax). **11a:** the cross-file move shows both links, only its edited line has word highlights, `g m` round-trips, the within-file move works and the 2-line block isn't a move. **11b:** the renamed variable highlights only the name; the reordered arguments; the wrapped block marks only the `if`; the JSON reorder; the parse error and the 1.2 MB file fall back. **11c:** the thread on a moved line sits under the same line in both views. **13d** drives the moves and the syntax view. |
| **E10** large | #21 | `e10/large` → `e10/base` | Milestones 10, 12 and 13; [below](#e10-large). **10b:** the debug log shows both trees indexed with their times, and reopening reuses blobs. **12f:** ⌘P filters its 300 files at once. **13e:** the budgets for the first file (1.5 s warm) and all files (6 s), moved-code detection (200 ms), and memory with `fd#2212` open too (1 GB). |
| **E11** review writes | #23 | `e11/writes` → `e11/base` | Milestone 8; [below](#e11-review-writes). `github`'s recordings of comments, replies and reviews, written as rozed-bot by `crates/github/tests/record_fixtures.rs` (`review_writes_e2e_23`), which deletes the comments it can afterwards. Not for checks that need it unchanged: each run leaves two reviews GitHub can't delete. |
| **E12** extension languages | #22 | `e12/languages` → `e12/base` | Milestones 10 (10i) and 13 (13f). One file each in Swift, Kotlin, Terraform, Ruby, Java, C#, PHP, Elixir, TOML, HTML, a Dockerfile and a Makefile, each changed. **Untrusted:** every file is highlighted on both sides, and the debug log shows no request at launch. **Trusted:** the Terraform server downloads from `releases.hashicorp.com`. |
| **#17** images | #17 | `e5/images` → `main` | Milestone 5 (5d's image previews); 8g's timeline images go through the same preview helper. SVGs whose `<image>`s point at files on the reviewer's disk and at `/dev/zero` draw only their frame and dot; the 316-byte pattern bomb (a 20000 × 20000 tile) fails with "couldn't preview this image" and Lo stays responsive; the 48 × 48 PNG previews. Opened by hand first; `setup` would recreate the same files, two of whose SVG comments use the project's first name, as on GitHub. |

### E1 revisions

Each revision is one push; `status --json` lists them under `e1.revisions`. r1 adds `e1/a.txt` and
`e1/b.txt` (12 lines each), r2 changes `a.txt`, r3 changes `b.txt`, r4 changes both, r5 is GitHub's
rebase and r6 changes line 3 of `a.txt`.

- **#16, now:** r1 `60005cf`, r2 `b88cc8b` (reviewed), r3 `34427a8`, on `main` at `a45754b`.
- **#14, the full run:**
  - **Pushes:** r1 `857b788`, r2 `2e2d4be` (reviewed), r3 `864ede1` and r4 `28b21f2`, on
    `99a9e80`.
  - **The rebase:** #15 moved `main` to `a45754b`, and GitHub rebased r4 into r5 `7513db3`. It shows
    as a `HeadRefForcePushedEvent` by rozed-bot, `28b21f2` → `7513db3`, and as a `force_push` in the
    branch's activity.
  - **r6:** `adb7208`. r5..r6 is the one hunk in `e1/a.txt`. The plain r4..r6 diff has that hunk
    and `e1/base.txt`.
- **#1, the first run:**
  - **Revisions:** r1 `91f269e`, r2 `344f0ef` (reviewed), r3 `cd66165`, r4 `b8b515a` and r5
    `f927e87`. r1–r3 fork from `03fa28b`.
  - **`main` moved twice under it:** E3's squash merge (`04bdf22`) landed before r4, and #12
    (`99a9e80`) before r5.
  - **Why that matters:** a revision's base is its merge base with `main`, which is `03fa28b` for
    r1–r4, not `main`'s tip when it was pushed. With the tip, r4 would show `e3/x.txt`.
  - `setup` now builds E3 before E1, so a new E1's base moves only under `rebase-r5`.

### E6 code

#18, head `e184ac6`, on `e6/base` at `a958353`. It changes six files and adds two.

- **Rust,** `e6/rust/`:
  - `src/render/parse.rs` (added) defines a second `parse`, colliding with `src/parse.rs`'s
    (unchanged).
  - `src/lib.rs` has `use serde::Deserialize` on line 3. `Cargo.toml` never declares serde, so it's
    never installed.
  - `summarize` (line 6 on the left) becomes `describe` (line 18 on the right).
  - `lib.rs` calls `parse::count` in the unchanged `parse.rs`, and `render::parse::parse` in the
    added file.
- **TypeScript,** `e6/ts/src/`: `index.ts` calls `isAdmin` in `user.ts` (both changed) and
  `initials` in `names.ts` (unchanged).
- **Python,** `e6/py/`: `app.py` calls `truncate`, which `util.py` adds.
- **Go,** `e6/go/`: `main.go` calls `shout`, which `util.go` adds.

### E8 hostile files

#19, head `a35edcf`, on `e8/base` at `a45754b`, which is where `main` was when E8 was made. It
adds 18 files. Each bait that can run something runs only `e8/evil.sh`, which creates one empty
marker file, `$TMPDIR/lo-e8-<name>`, and does nothing else: no network, no deletes, no reads. It
refuses a marker that's already there, so it can't write through a planted link. The config files
sit where each tool looks for them, mostly at the repository's root, and `e8/` holds a small Rust,
TypeScript, Python and Go project, so each server has something to open.

| Bait | What it tries | Marker |
|---|---|---|
| `.cargo/config.toml` | `rustc-wrapper = "e8/evil.sh"`, so cargo runs it in place of rustc, in `e8/rust/` and `e8/pinned/` | `lo-e8-rustc-wrapper` |
| `.zed/settings.json` | points rust-analyzer, gopls, vtsls and basedpyright at `e8/evil.sh` | `lo-e8-rust-analyzer`, `lo-e8-gopls`, `lo-e8-vtsls`, `lo-e8-basedpyright` |
| `node_modules/typescript/lib/tsserver.js` | a workspace TypeScript, which Zed's vtsls adapter uses whenever it exists, whose server runs `e8/evil.sh` | `lo-e8-tsserver` |
| `.venv/` and `pyrightconfig.json` | a virtual environment whose `bin/python` runs `e8/evil.sh`. pet, Zed's environment finder, looks for `.venv` in the worktree's root and takes a folder with a `pyvenv.cfg`; Zed hands it to basedpyright as its interpreter | `lo-e8-python` |
| `e8/pinned/rust-toolchain.toml` | toolchain `1.999.0`, which doesn't exist, so rustup tries to download it from `static.rust-lang.org` | none |
| `go.mod` | `toolchain go1.99`, which doesn't exist, so go tries to download it from `proxy.golang.org` | none |

The check: trusted, after 2 minutes of scrolling and hovering, no `$TMPDIR/lo-e8-*` marker exists,
and the two toolchain baits show in the servers' environments (`ps -E -p <pid>`) and the network:

- rust-analyzer's environment has `RUSTUP_TOOLCHAIN` set and `RUSTUP_AUTO_INSTALL=0`;
- gopls's has `GOTOOLCHAIN=local` and `GOPROXY=off`;
- nothing connects to `static.rust-lang.org` or `proxy.golang.org`.

The toolchain baits can't leave a marker. rustup refuses a toolchain `path` relative to its file
(rustup 1.29.3 says "relative path toolchain"), and a checkout has no fixed absolute path. go runs
only a toolchain it downloads or finds on `PATH`. The toolchain file has a crate of its own,
`e8/pinned/`, so it can't stop cargo in `e8/rust/` before cargo reaches the wrapper.

### E9 moves and syntax

#20, head `243ff70`, on `e9/base` at `25c5f34`. It changes ten files.

- **Moves:**
  - `fullName` moves from `e9/api/user.ts` (lines 14–19) into the `Names` class in
    `e9/api/names.ts` (lines 8–13), one level deeper. Its first line changes, so the move runs from
    the next line, 15–19 → 9–13, with its return line edited (18 → 12). The code is the example in
    `diff_extras::moved`'s docs.
  - `checksum` moves up `e9/lib.rs`, unchanged: 22–28 → 8–14.
  - `TIMEOUT_SECONDS` and `RETRY_LIMIT` move down `e9/short.py`, 3–4 → 12–13: two lines, too few to
    be a move.
- **Syntax cases,** in `e9/syntax/`: `rename.rs` renames `total` to `sum`, `args.ts` reorders
  `clamp`'s arguments in its signature and at its call, `wrap.py` wraps a block in a new `if`, and
  `config.json` reorders keys at two levels.
- **Fallbacks:** `e9/broken.ts` gains a parse error, and `e9/large.ts` is 1,229,024 bytes on the
  left and 1,229,048 on the right, both past `diff_extras`'s 1,000,000-byte limit and 1 MiB.
- **Threads:** rozed-bot's COMMENT review (`e9.review` in `status --json`) has two threads:
  - on a moved line, `e9/api/names.ts:10` on the right (`const middle = …`);
  - on a line only the old side has, `e9/api/user.ts:26` on the left, in `legacyName`, which the PR
    deletes.

### E10 large

#21, head `1591774`, on `e10/base` at `8cac962`. 300 files, +19,545 −5,798: 25,343 changed lines.
`trees.sh` generates it from fixed seeds, so it's the same each time.

- **298 files** of TypeScript, TypeScript tests, Rust, Python, Go and TOML, under `e10/src`,
  `e10/rust`, `e10/py`, `e10/go` and `e10/config`. 253 are changed, 29 added and 16 deleted. Every
  ninth file moves its first function to its end, and a few move one to a file of the same language.
  With the two files below, the PR changes 254 files, adds 30 and deletes 16.
- **`e10/src/big/catalog.ts`:** added, 6,000 lines, over the 5,000 changed lines past which Lo
  collapses a file.
- **`e10/package-lock.json`:** a lockfile of 450 packages, collapsed as generated. The PR bumps every
  fourth, drops every 31st and adds 40 (+570 −414).

### E11 review writes

#23, head `5a48d22`, on `e11/base` at `5055651`. `e11/review.txt` holds `line 1` to `line 60`,
and the PR changes lines 10, 11, 30 and 50, so its diff has three hunks (lines 7–14, 27–33 and
47–53) and lines outside them; `e11/notes.md` changes one line, for file-level comments. It has
no review of its own.

`cargo test -p github --test record_fixtures -- --ignored --exact review_writes_e2e_23` writes to
it as rozed-bot, checking `GET /user` first, and records four cassettes, `review_writes_e2e_23_*`:

- **pending:** a pending review started with a line thread, then a range from the base's line 10
  to the head's 11, a file-level thread and a reply; a line outside the diff and a file the PR
  doesn't change, which GitHub answers with `thread: null`; a comment posted at once, refused
  while the review is pending; then the review submitted as a comment.
- **posted:** a reply, a line thread, a REST file-level comment and a review with only a summary,
  all posted at once; approving, requesting changes and an empty comment, which GitHub refuses
  its author; a reply that starts an empty pending review, submitted without a summary.
- **threads:** a thread resolved and unresolved, a reply edited, edited blank and deleted twice.
- **conversation:** two comments on the PR itself, the first edited, then both deleted.

Afterwards it deletes every comment of rozed-bot's starting `Fixture E11:`, so the PR keeps no
threads. GitHub drops a review without a summary once its last comment goes, so each run leaves
two submitted reviews, the two with a summary, which can't be deleted. A run stops before writing
anything if rozed-bot has a pending review there.
