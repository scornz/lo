<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/banner/banner-dark.svg">
  <img alt="Lo" src="brand/banner/banner-light.svg" width="100%">
</picture>

<p align="center">
  A fast, native macOS app for reviewing GitHub pull requests from the keyboard.
</p>

<p align="center">
  <a href="https://github.com/scornz/lo/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/scornz/lo/actions/workflows/ci.yml/badge.svg"></a>
  <a href="LICENSE"><img alt="License: GPL-3.0-or-later" src="https://img.shields.io/badge/license-GPL--3.0--or--later-blue"></a>
  <a href="#status"><img alt="Platform: macOS" src="https://img.shields.io/badge/platform-macOS-lightgrey"></a>
</p>

<p align="center">
  <a href="#about">About</a> ·
  <a href="#features">Features</a> ·
  <a href="#status">Status</a> ·
  <a href="#build-from-source">Build</a> ·
  <a href="#usage">Usage</a> ·
  <a href="#contributing">Contributing</a>
</p>

<!-- A screenshot of a pull request open in Lo goes here, then a short demo. -->
<p align="center"><i>A screenshot and a short demo are coming.</i></p>

## About

Lo (as in "lo and behold") is a native macOS app for reviewing GitHub pull requests. It's for
people who read far more code than they write: maintainers, leads, and anyone reviewing the pull
requests their coding agents open.

GitHub's web UI is fine for the odd review. When reviewing is most of your day, you want what an
editor gives you: a fast diff, real code navigation and a key for everything. Lo is built on
[Zed](https://zed.dev)'s crates, used as libraries, so its diff is Zed's side-by-side diff and its
code view is Zed's editor. Around them, Lo adds what reviewing many pull requests needs: review
order and viewed marks today, then stacks, revisions and merging.

Lo keeps to a few rules:

- **Read-only.** Lo never edits files or pushes commits. It writes to GitHub only to review and
  merge.
- **No AI, no backend, no telemetry.** Lo talks only to GitHub, and to language servers' official
  download sources.
- **Never runs a repository's code.** No dependency installs or build steps, and language servers
  run only on repositories you trust.
- **Keyboard first.** Every action has a key.

## Features

### Works today

- **Open by link.** `lo <link>` or `⌘O` opens a pull request in a tab of its own. With none open,
  Lo lists the ones you opened recently.
- **Instant, then whole.** GitHub's diff shows at once while Lo copies the pull request into a
  local partial clone. Then Zed's side-by-side diff of whole files takes over, at the same file and
  line.
- **Review order.** Each source file is followed by its tests, then come config files, then
  generated ones. One key switches to path order.
- **Less noise.** Generated files, lockfiles and very large files start collapsed, each saying why.
  One key shows the diff inline, and another hides changes to whitespace alone.
- **Viewed marks.** `e` sets GitHub's own viewed mark on a file and moves to the next unviewed one.
- **Several accounts.** Sign in through GitHub's device flow, or import your accounts from `gh`.
  Public repositories open without signing in.
- **Offline.** A pull request Lo has copied still opens from the copy, and catches up once GitHub
  answers again.

### Coming

Much of the logic behind these is already on `main` and tested, waiting for its place in the app.

- **Revisions.** Compare any two pushes, start from what changed since your last review, and see
  only the author's changes across a rebase.
- **Stacks.** Native GitHub stacks, chains of base branches and pull requests that share commits,
  shown as a tree, reviewed layer by layer and merged as one unit where GitHub allows.
- **Comments and reviews.** Threads in the diff, a pending review, suggestions, attachments and the
  full timeline.
- **Merging.** Why a pull request can't merge, approve and ship on one key, and the merge queue.
- **Code navigation.** Go to definition and find references on every repository, from a syntax
  index, plus types and hovers from language servers on repositories you trust.
- **Moved code and a syntax-aware diff.** Blocks moved within or across files show as moves, and
  a view per file compares by syntax instead of by line.
- **Links and keys.** `lo://` links, a command-line tool, link routers such as Finicky, Velja and
  Choosy, Vim-style keys, a keymap you can change and a `?` sheet of every key.
- **A release.** A signed, notarized download and a Homebrew cask.

## Status

Lo is in early development and isn't released yet. There's no download or Homebrew cask: to try
it, build it from source. It runs on macOS only. Expect missing pieces, and things that change.

The [roadmap](docs/plan.md#roadmap) lists the milestones to v1, and the
[design](docs/design.md) describes v1 in full.

## Build from source

You need:

- **macOS with Xcode**, the app and not just the Command Line Tools: Zed's UI framework compiles
  Metal shaders.
- **git 2.41 or later.** Lo runs git to keep its local copies. Check with `git --version`.
- **[rustup](https://rustup.rs).** `rust-toolchain.toml` pins Zed's Rust toolchain, and cargo
  installs it on first use.
- **[mise](https://mise.jdx.dev),** which installs the cmake, Node and pnpm pinned in
  [`mise.toml`](mise.toml). cmake builds wasmtime for Zed's tree-sitter. Node and pnpm run the git
  hooks.

Then:

```sh
git clone https://github.com/scornz/lo.git
cd lo
git submodule update --init --depth 1  # Zed, one commit deep; always pass --depth 1
mise install                           # cmake, Node and pnpm
pnpm install                           # the git hooks, which you need to commit
mise exec -- cargo build -p lo         # or plain cargo, if your shell activates mise
```

The first build compiles Zed's crates, so it takes a while. "Set up" in
[`CLAUDE.md`](CLAUDE.md#set-up) has the details.

## Usage

```sh
lo <pull-request-link>
```

From a source build, the app is `target/debug/lo`, or run it through cargo:

```sh
cargo run -p lo -- https://github.com/sharkdp/fd/pull/2212
```

- Each pull request opens in its own tab, and `⌘O` opens another from its link. Started without a
  link, Lo opens on the pull requests you opened recently.
- To open pull requests in private repositories, sign in: "Accounts…" in the app menu signs in
  through GitHub's device flow, or imports your accounts from `gh`.
- `--trust` also checks the pull request out and runs rust-analyzer on it. rust-analyzer runs cargo
  and rustc in the checkout, which a repository can point at its own programs, so only pass it for
  code you'd run. Without it, no language server starts.
- Lo keeps its files in `~/.config/lo`, `~/Library/Application Support/Lo`, `~/Library/Caches/Lo`
  and `~/Library/Logs/Lo`.

Every action has a key. "Keys" in [`CLAUDE.md`](CLAUDE.md#keys) lists each key Lo binds today and
where it works. [Design §10](docs/design.md#10-keyboard-windows-and-opening-links) has the full
plan.

## Contributing

Open an [issue](https://github.com/scornz/lo/issues) for a bug or an idea. Changes land like this:

1. Branch from `main`, and run `pnpm install` once so the git hooks run.
2. Commit with [Conventional Commits](https://www.conventionalcommits.org), such as
   `feat(review_ui): mark files viewed`. commitlint checks every message, in the `commit-msg` hook
   and in CI.
3. Run the checks in "Verify" in [`CLAUDE.md`](CLAUDE.md#verify). CI runs the same on macOS.
4. Open a pull request with a Conventional Commits title. It's squash-merged, and its title becomes
   the commit on `main`. To update your branch, merge `origin/main` into it; don't force-push.

The docs:

- [`CLAUDE.md`](CLAUDE.md) (also `AGENTS.md`): the full guide to working on Lo, for people and
  coding agents alike. It covers the layout, setup, keys, the Zed submodule and its patches, and
  the checks.
- [`docs/design.md`](docs/design.md): the approved design.
- [`docs/plan.md`](docs/plan.md): the milestones.
- [`docs/decisions.md`](docs/decisions.md): calls made where the design is silent.
- [`docs/spike.md`](docs/spike.md): what the first build proved, the Zed crates compiled in, and
  the risks of trusting a repository.
- [`brand/`](brand/README.md): the mark, colors and type.

## Security

Please don't report a security problem in a public issue. Report it privately, through
[GitHub's private vulnerability reporting](https://github.com/scornz/lo/security/advisories/new).

## License

Lo is free software, under the [GNU General Public License, version 3 or later](LICENSE). Building
on Zed's crates, several of which are GPL-3.0-or-later, requires it.
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md) lists the code Lo includes from other projects.

## Acknowledgements

Lo stands on [Zed](https://github.com/zed-industries/zed). Its editor, side-by-side diff, UI
framework and language support are Zed's, used as libraries from a pinned release with a few small
patches ([`scornz/zed`](https://github.com/scornz/zed)). Thank you to the Zed team for building
them in the open. Lo isn't affiliated with or endorsed by Zed Industries.

Lo's syntax-aware diff ports [difftastic](https://github.com/Wilfred/difftastic)'s structural diff,
by Wilfred Hughes, and its symbol index uses the tags queries from the
[tree-sitter](https://tree-sitter.github.io) grammars Zed ships.
