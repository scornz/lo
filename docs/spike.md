# Spike: rozed on Zed's crates

What building rozed on Zed v1.27.0's crates, and opening a pull request in Zed's side-by-side diff,
showed (design §2, "First milestone").

## Building

rozed depends by path on 30 Zed crates (listed in the root `Cargo.toml`'s `[workspace.dependencies]`).
With what they pull in, `Cargo.lock` holds 1,120 packages, 111 of them Zed crates (all targets,
so including Linux and Windows platform crates that macOS builds skip). The binary compiles in 104 of
them; see "Zed crates compiled in".

What the build needs beyond Rust:

- **Xcode**, not just the Command Line Tools. gpui compiles its Metal shaders at build time.
- **cmake.** Zed's `language` crate turns on tree-sitter's `wasm` feature, which builds wasmtime's
  C API with cmake, whether or not rozed uses extensions. It's pinned in `mise.toml`. mise's shim
  fails when a build script calls cmake from outside the repo, so cargo has to run with mise
  activated or under `mise exec --`.
- **Three crate features** that only Zed's own binary or dev-dependencies turn on:
  `gpui_platform/font-kit` (without it macOS renders no text), `languages/load-grammars` (no
  grammars, so no highlighting) and `util/debug-embed` (debug builds otherwise look for Zed's assets
  under rozed's root and panic).
- **Zed's `[patch]` sections, profiles and `.cargo/config.toml`**, which cargo ignores in a
  dependency. `cargo xtask sync-zed` copies them, and seeds `Cargo.lock` with Zed's versions.

Things that surprised:

- **`--printenv`.** Zed's crates learn the login shell's environment by running the current
  executable with `--printenv` inside that shell: at startup when there's no terminal, and, unless
  the project is given an environment, once per project folder. Before rozed handled the flag, each
  capture started another full rozed, which captured again: a runaway chain of processes. rozed now
  answers it as Zed does, a rozed started by a running rozed refuses to start, and rozed gives its
  project its own environment, so only the startup capture happens (see "Trust risks").
- **Debug links warn about unwind info.** Unoptimized code gives a debug rozed 34 MB of
  `__eh_frame`, more than the 16 MB ld's compact unwind table can index, so ld warns on every debug
  link. It's harmless, and Zed's own binary gets the same warning. Linking with `-no_compact_unwind`
  silences it but leaves out `__unwind_info`, which the unwinder needs: every panic then aborts with
  "failed to initiate panic", and one failing test kills the whole test binary. Optimized builds stay
  under the limit (`release` 7 MB, `release-fast` 11 MB) and don't warn.
- **`shallow = true` doesn't keep the submodule shallow.** With it, `git submodule update --init`
  still fetched all of Zed's history (400 MB, 55 s), because the pinned commit, on rozed's patch
  branch, isn't on the fork's default branch. `git submodule update --init --depth 1` fetches the
  pinned commit alone (32 MB, 7 s), as CI's checkout does.
- **Zed-owned files.** A run creates only rozed's folders. It also creates
  `~/Library/Logs/rozed/telemetry.log`, empty, because Zed's `Client` sets up its telemetry sink;
  nothing is sent.

## Zed crates compiled in

rozed compiles in 104 Zed crates: everything reachable from the `rozed` package through normal
dependencies on macOS, which is what gets linked into the binary. `script/zed-crates.txt` lists
them, and `script/check-zed-crates.sh`, which CI runs, fails when the set changes, so a new Zed
crate shows up in review. It lists them with `cargo tree -p rozed -e normal` for the host platform.
`cargo metadata` would list two more, `util_macros` and `perf`: its dependency graph merges in the
`test-support` features rozed's dev-dependencies turn on, which a build of the binary doesn't.

What rozed uses:

- **UI (25):** `gpui`, `gpui_apple`, `gpui_macos`, `gpui_macros`, `gpui_platform`,
  `gpui_shared_string`, `gpui_tokio`, `gpui_util`, `ui`, `ui_input`, `ui_macros`, `component`,
  `icons`, `file_icons`, `assets`, `theme`, `theme_settings`, `syntax_theme`, `refineable`,
  `derive_refineable`, `menu`, `zed_actions`, `breadcrumbs`, `markdown`, `mermaid_render`.
- **Editor and languages (17):** `editor`, `multi_buffer`, `buffer_diff`, `text`, `rope`,
  `sum_tree`, `language`, `language_core`, `language_detection`, `languages`, `grammars`, `lsp`,
  `snippet`, `snippet_provider`, `fuzzy`, `fuzzy_nucleo`, `vim_mode_setting`.
- **Project and workspace (15):** `project`, `workspace`, `worktree`, `fs`, `git`,
  `git_hosting_providers`, `task`, `file_content`, `json_schema_store`, `askpass`, `node_runtime`,
  `session`, `db`, `sqlez`, `sqlez_macros`.
- **Settings (11):** `settings`, `settings_content`, `settings_json`, `settings_macros`,
  `migrator`, `feature_flags`, `feature_flags_macros`, `release_channel`, `paths`, `env_var`,
  `zed_env_vars`.
- **HTTP (4):** `http_client`, `http_client_tls`, `reqwest_client`, `net`.
- **Support (9):** `util`, `collections`, `clock`, `path`, `scheduler`, `watch`, `zlog`,
  `ztracing`, `ztracing_macro`.

What comes along for features rozed leaves out (design §2) or doesn't have (23). None of it starts:
rozed never signs in, sends telemetry, configures an AI provider, a debugger, a remote host, a
context server or an extension, and never formats. "Pulled in by" is each crate's direct dependents
(`cargo tree -i <crate> -e normal --depth 1`):

| Crate | Pulled in by |
|---|---|
| **Zed account and collaboration** | |
| `client` | `editor`, `workspace`, `project`, `dap`, `edit_prediction_types`, and rozed itself: `Workspace` and `Project::local` take a `Client` |
| `cloud_api_client` | `client` |
| `cloud_api_types` | `client`, `cloud_api_client`, `extension` |
| `rpc` | `client`, `editor`, `language`, `project`, `remote`, `worktree` |
| `proto` | `rpc`, `fs`, `task`, `dap` |
| `credentials_provider` | `client`, `project`, `language_model`, `zed_credentials_provider` |
| `zed_credentials_provider` | `client`, `project` |
| `proxy_handshake` | `client` |
| **Telemetry** | |
| `telemetry` | `client`, `editor`, `workspace`, `fs`, `remote`, `dap` |
| `telemetry_events` | `client`, `telemetry` |
| **AI** | |
| `agent_settings` | `workspace` (`MultiWorkspace` reads `AgentSettings`) |
| `language_model` | `agent_settings` |
| `language_model_core` | `language_model`, `settings_content` |
| `cloud_llm_client` | `client`, `cloud_api_types`, `language_model_core` |
| `edit_prediction_types` | `editor` |
| `zeta_prompt` | `cloud_api_types` |
| **Terminal** | |
| `terminal` | `languages`, `project` |
| **Debugger** | |
| `dap` | `editor`, `project`, `extension`, `json_schema_store` |
| **Remote development** | |
| `remote` | `project`, `workspace` |
| **Context servers (MCP)** | |
| `context_server` | `project` |
| `oauth_callback_server` | `context_server` |
| **Extensions** | |
| `extension` | `project`, `json_schema_store`, `snippet_provider` |
| **Formatter** | |
| `prettier` | `project` |

Not compiled in: `auto_update`, `extension_host`, `git_ui`, `git_ui_core`, `call`, `collab_ui`,
`agent`, `agent_ui`, `copilot`, `edit_prediction`, `terminal_view`, `debugger_ui`, `title_bar`,
`crashes` and `onboarding`. Only patching Zed would drop the 23 above, since they all hang off
`editor`, `project` and `workspace`.

## The pull request diff

`cargo run -p rozed -- <pull request link>` opens a public pull request in Zed's side-by-side diff:

1. `GET api.github.com/repos/<owner>/<repo>/pulls/<n>`, without signing in, for the base branch and
   the clone URL.
2. The local copy (`crates/rozed/src/local_copy.rs`, a stand-in for milestone 4's `repo_cache`,
   which has since replaced it): a bare `blob:none` partial clone at
   `~/Library/Caches/rozed/repos/github.com/<owner>/<repo>.git`, a fetch of `refs/pull/<n>/head`
   and the base branch into `refs/rozed/pull/<n>/head` and
   `refs/rozed/base/<branch>`, their merge base, and the changed files with each side's mode and
   blob ID (`git diff -M` with `--raw` and `--numstat`).
3. Every changed file goes into one `SplittableEditor`, in git's order, both sides read from git
   with `git cat-file` into in-memory buffers, as in Zed's `commit_view.rs`. There's no checkout, no
   project folder and no language server.

With `--trust`, rozed also checks the head out, detached, as a `git worktree` at
`~/Library/Caches/rozed/checkouts/github.com/<owner>/<repo>/pull-<n>/<head>`, trusts that folder and
nothing else, and opens it as the project's only folder. A file whose bytes there hash to the
head's blob (`git hash-object --no-filters`, against the blob ID from `--raw`) shows the checkout's
project buffer on the right, so rust-analyzer attaches to it, as in Zed's `file_diff_view.rs`. A
file that doesn't, because two paths collide on a case-insensitive disk or `.gitattributes`
converted it, shows git's text, with a warning in its header.

On `sharkdp/fd#2212` (7 files: YAML, Markdown, TOML, a Makefile, an added shell script and Rust),
every file shows side by side, highlighted on both sides, with the added script's left side empty,
with or without `--trust`. Without it, rozed made no checkout and started no language server.
With it, all seven files hashed to their blobs in the checkout, so all are project buffers, and
rust-analyzer's hovers work on the right side (`&'static str`, "value of literal", on the string
`DEFAULT_LS_COLORS` holds). Cmd-Q quits with the pull request open either way, rust-analyzer with
it. Screenshots of both stay on the machine that took them (see `decisions.md`). The pull request
has no deleted, renamed, binary or submodule file and no case collision: tests cover those, but how
the diff shows them wasn't checked on screen.

What it took:

- **Language servers need project buffers** opened from a project folder; an in-memory buffer
  never reaches one. The left side never gets one either: `SplittableEditor` turns it off.
- **The left side's language** has to be copied from the right buffer once it loads, as
  `file_diff_view.rs` does, or the left side stays plain text.
- **Files rozed doesn't compare get placeholders,** in-memory buffers with a `File` of rozed's own:
  a binary file a folded "(binary file not shown)", a submodule its old and new commit, and a file
  rozed couldn't read the reason, which its header repeats, with a notice counting them. A
  submodule is recognized by its mode in `--raw` before anything reads a blob: its commit is in
  another repository, and reading it as a blob would ask the remote for it. An editor `Addon`
  makes each file's header show the pull request's change (added, deleted, renamed or modified),
  not the checkout's status, which is always clean, and any warning.
- **git without the user's config.** rozed runs git with `GIT_CONFIG_GLOBAL=/dev/null`,
  `GIT_CONFIG_NOSYSTEM=1` and `GIT_TERMINAL_PROMPT=0`, so URL rewrites, credential helpers, LFS
  filters and hooks configured there don't apply, and git never prompts. `--numstat` reads both
  sides of every changed file, so git downloads them in one batch and the later `git cat-file`
  calls are local. Symlinks are checked out as plain files.
- **rust-analyzer**, through settings in Zed's top layer, comes from its GitHub releases rather
  than `PATH`, and runs without build scripts, proc macros, `cargo check` or dependencies
  (`cargo.noDeps`). Hovers on the repository's own code work; anything typed by a dependency shows
  `{unknown}`, which matches design §7 ("a symbol from one shows its import and nothing more").
- **rust-analyzer still needs a Rust toolchain** to load the workspace. rozed pins the one it was
  started with (`cargo run` passes `RUSTUP_TOOLCHAIN` on), else rustup's default. With neither, as
  on the machine used here when rozed isn't started by cargo, rust-analyzer reports "Failed to load
  workspaces" unless the repository's `rust-toolchain.toml` names a toolchain that's installed:
  rustup won't install one for it.
- **Checkouts stay out of Recent Items** (Zed patch 3). Zed adds every visible project folder to
  macOS's Recent Items, and on the desktop used here that update never finished, so cmd-Q hung
  with a pull request open.

## Network check

Method: runs of the debug build on `sharkdp/fd#2212`, each from a clean state.

- **Clean state.** rozed ran with `HOME` set to a new, empty folder, so its four folders, the clone
  cache and the rust-analyzer download all started empty, without touching the machine's own
  (`CARGO_HOME` and `RUSTUP_HOME` still pointed at the real toolchain).
- **Logging.** `RUST_LOG=info,rozed=debug` logged every request and redirect through rozed's HTTP
  client, and any refused Zed host at `warn`. A script sampled `lsof -i -n -P -a -p <pids>` and the
  command line of rozed and every process it had started, every second.
- **Runs A, B and C** checked the first version, which trusted every pull request. Run A, 3
  minutes with scrolling and hovering, matched each remote address against GitHub's published
  ranges (`api.github.com/meta`). Runs B and C, 2.5 minutes each, went through a local proxy
  (`HTTPS_PROXY`) that logged the host of every `CONNECT`, which also names the hosts of
  short-lived processes that one-second sampling misses, such as git and the cargo and rustc calls
  rust-analyzer makes. Run C also selected a Rust toolchain (`RUSTUP_TOOLCHAIN`) so rust-analyzer
  could load the workspace.
- **Runs D and E** checked `--trust` as it is now, through the same proxy, with `RUSTUP_TOOLCHAIN`
  set as `cargo run` sets it, so rust-analyzer loaded the workspace, and with `SHELL` pointed at a
  wrapper that logged every shell started and its arguments. Run D (2.5 minutes, scrolling and
  hovering) had Zed's inline git blame on, run E (2 minutes) the forced settings that turn it off.

Every host, in every run:

| Host | Contacted by | For |
|---|---|---|
| `api.github.com` | rozed's HTTP client | The pull request, and rust-analyzer's latest release |
| `github.com` | git | The partial clone's fetch and the batched downloads of file contents |
| `github.com` | rozed's HTTP client | rust-analyzer's release asset, which redirects to the next host |
| `release-assets.githubusercontent.com` | rozed's HTTP client | GitHub's release CDN, for `rust-analyzer-aarch64-apple-darwin.gz` |

- No Zed host appeared, and the refused-host warning never fired.
- In run A, `lsof` saw three remote addresses, all in GitHub's published ranges: `172.182.252.137`
  (api), `172.182.252.133` (git and web) and `185.199.110.133` (GitHub's CDN).
- In runs B to E, every socket rozed and its children held went to the proxy, so nothing bypassed
  it, and the proxy saw no plain HTTP. rust-analyzer's cargo and rustc calls made no connections.
- In run D, Zed's inline git blame ran `git blame` on each open file, and in the partial clone
  each blame downloaded the file's earlier versions one at a time: 899 connections to `github.com`
  in 2.5 minutes. The forced settings now turn inline blame off, and run E made 4.
- In runs D and E the only shell was the one rozed starts at startup when it has no terminal, in
  the home folder; none ran in the checkout. rust-analyzer's environment held `RUSTC_WRAPPER=`,
  `RUSTC_WORKSPACE_WRAPPER=`, `RUSTUP_AUTO_INSTALL=0` and the pinned `RUSTUP_TOOLCHAIN`.
- Runs A to C started the language servers that need Node (for YAML and Bash) and Zed's default
  Prettier, which failed without connecting anywhere, because rozed gives Zed no Node runtime. In
  runs D and E only rust-analyzer started, and Prettier never did.

**Verdict: pass.** rozed contacted only GitHub, for the API, git and rust-analyzer's download.

## GitHub findings

Checked live on 2026-11-06 against the private test repository `rozed-dev/playground`; the full log
is `docs/research/github-checks.md`.

- **Attachments: rozed's own OAuth token works.** A device-flow token for rozed's OAuth app
  (`gho_…`, scopes `repo read:org workflow`) uploaded an image with the request `gh` uses
  (`POST uploads.github.com/user-attachments/assets?name=…&content_type=…&repository_id=…`): `201`
  with a `github.com/user-attachments/assets/<id>` URL. rozed doesn't need to borrow a `gh` token.
- **Images in private repositories need the token.** The asset URL answers `404` without it and,
  with it, redirects to a signed S3 URL valid for 5 minutes. rozed has to fetch images itself, with
  the token, and must not send the token on to S3.
- **Organizations that restrict OAuth apps,** GitHub's default for new ones, need an owner to grant
  rozed access; a member who isn't an owner can only request it.
- **Stacks need no opt-in.** `POST /repos/{owner}/{repo}/stacks` with two chained pull requests made
  a stack on a brand-new private repository (`201`). rozed's token reads it over REST
  (`/stacks?pull_request=<n>`, a pull request's `stack` field) and GraphQL (`stack`, `stackEntry`).
  Stack numbers share the issue and pull request sequence. Creating a stack with rozed's token
  wasn't tried; it has the same `repo` scope.
- **Async merge errors** for an unknown, expired or malformed merge ID are all `404` with GitHub's
  usual error body, whose `status` is the string `"404"`.

## Numbers

Local, on an 18-core Apple silicon Mac with 64 GB:

| | |
|---|---|
| Clean debug build of `rozed` | 1 min 33 s (770 s of CPU) |
| Rebuild after changing `crates/rozed` | about 2 s |
| `target/` after a clean debug build | 11 GB (14 GB with clippy and test builds) |
| Debug binary | 518 MB |
| Release build (thin LTO, one codegen unit) | 6 min |
| Release binary | 139 MB, 106 MB stripped |

With the pull request diff, measured while another Zed build ran on the same machine:

| | |
|---|---|
| Clean debug build of `rozed` | 1 min 44 s (786 s of CPU) |
| Rebuild after changing `crates/rozed` | 2 to 4 s |
| `target/` after a clean debug build | 11 GB (14 GB with clippy and test builds) |
| Debug binary | 527 MB |

Launch to the diff on `sharkdp/fd#2212`, debug build, from rozed's own log:

| | Cold: nothing cached | Warm: clone and checkout cached |
|---|---|---|
| GitHub's answer | 0.8 s after launch | 0.7 s after launch |
| Local copy | 1.3 to 1.4 s (fetch 0.6 s, changed files 0.3 s, checkout 0.35 s) | 0.4 s |
| First file shown | 2.1 to 2.3 s after launch | 1.1 to 1.2 s after launch |
| All 7 files shown | 2.2 to 2.5 s after launch | 1.2 to 1.3 s after launch |
| rust-analyzer, first run | downloaded and started 2 s after the diff | already downloaded |

CI, `rust` job on `macos-26` (arm64):

| | |
|---|---|
| No cache | 10 min (fetching and building xtask 3 min, clippy 2 min, tests 3 min) |
| Cached | 8.5 min (restoring the cache 40 s, clippy 2 min, tests 4 min) |
| With `test-support` features, cache partly matching (after a `Cargo.lock` change) | 13 min (restoring the cache 45 s, clippy 3.5 min, tests 7 min) |
| With `test-support` features, cache fully matching | 5.7 min (restoring the cache 35 s, clippy 1.3 min, tests 3 min) |
| `target/` at the end | 4.6 GB, 5.3 GB with `test-support`, with 90 GB of disk still free |
| Cache saved | 1.3 GB |

rozed's gpui tests need `test-support` features on `gpui`, `project`, `settings` and `workspace`.
Features differ between test and normal builds, so tests compile Zed's crates a second time locally
too: the first `cargo test` after `cargo build` takes about a minute and adds to `target/`.

With the pull request diff, the `rust` job took 21 min with no cache (the Cargo settings check,
which fetches crates and builds xtask, 4 min; clippy 7 min; tests 7.5 min; the Zed crate check
15 s) and 11 min with the cache (restoring it 25 s, clippy 2.5 min, tests 6 min).

CI turns off debug info and incremental compilation, which only cost time and disk in a fresh
checkout. Nothing else was needed for disk, memory or time.

The cache holds registry and git crates only. `Swatinem/rust-cache` keeps only packages whose
manifests sit outside the repository, and Zed's crates sit inside it, in `third_party/zed`, so they
build on every run. Caching them would also need their files' modification times fixed, because
cargo decides whether a path dependency changed by modification time, and a checkout makes every
file new.

## Trust risks

Language servers run code from the repository they open, so rozed starts them only with `--trust`,
until milestone 10's trust prompt, and then only rust-analyzer. Without `--trust`, rozed makes no
checkout, opens no project folder and starts no language server: a pull request's files are only
read by git and shown.

Closed, with or without `--trust`:

- **A repository's own Zed settings never apply** (Zed patch 2): `.zed/settings.json`,
  `.zed/tasks.json`, `.zed/debug.json`, `.vscode/tasks.json` and `.vscode/launch.json`, in any
  folder. Zed merges a trusted folder's `.zed/settings.json` over every global layer, including the
  one rozed forces, so a pull request could otherwise name its own language server binary.
  `.editorconfig` still applies: it sets only indentation, line endings and line length.
- **Only rust-analyzer can start.** The forced settings give every language rozed knows an empty
  `language_servers` list, and Rust `["rust-analyzer"]`, so servers on `PATH` (vtsls, gopls, YAML
  and JSON servers) and `package-version-server` never start, whatever the user's settings say.
  They also keep Prettier off for every language: rozed never formats, and Prettier loads a
  repository's plugins.
- **No login shell runs in the checkout.** Given no environment, Zed's project runs the user's
  login shell, interactively, `cd`'d into each project folder to capture one, so directory hooks
  (direnv, mise, asdf, nvm, zsh's `chpwd`) run on the pull request's files. rozed hands the project
  its own environment instead. Started from Finder, rozed captures the login shell's environment
  once, in the home folder, before it opens the project.
- **The right side shows the commit.** Its text comes from git. With a checkout, a file's project
  buffer is used only when its bytes there hash to the head's blob, so two paths that collide on a
  case-insensitive disk (`Lib.rs` and `lib.rs`) can't hide one file's text behind the other's.
- **No file is dropped silently.** A file rozed can't show gets a placeholder with the reason, and
  a submodule bump shows both commits.
- **Trust stays on the checkout.** Siblings (`pull-12` beside `pull-1`) and the folder above stay
  untrusted. Zed trusts every single-file project folder once any folder is trusted, so after
  trusting a checkout rozed restricts any single-file folder the project gets, such as a file
  dropped on the window.
- **Trust doesn't outlive the run.** Zed's workspace saves every trusted path to its
  `trusted_worktrees` table, and Zed reads it back at startup. rozed never reads it, and empties it
  at startup.
- **git ignores the user's and the system's git config** when rozed fetches and checks out (no
  filters, hooks or credential helpers), and symlinks are checked out as plain files.

With `--trust`, also closed:

- **rustup doesn't follow the repository.** rust-analyzer's environment pins `RUSTUP_TOOLCHAIN` to
  the toolchain rozed finds, which rustup prefers to a `rust-toolchain.toml`, and sets
  `RUSTUP_AUTO_INSTALL=0`, so rustup never downloads or installs a toolchain, even when rozed finds
  none. Checked with rustup 1.29.3 in a folder whose `rust-toolchain.toml` names 1.70.0, which isn't
  installed: with `RUSTUP_AUTO_INSTALL=0` cargo fails with "toolchain '1.70.0-aarch64-apple-darwin'
  is not installed"; with `RUSTUP_TOOLCHAIN` it runs the pinned toolchain; with neither, rustup
  starts downloading from `static.rust-lang.org`.
- **No rustc wrapper.** `RUSTC_WRAPPER` and `RUSTC_WORKSPACE_WRAPPER` are set empty for
  rust-analyzer, which overrides a wrapper `.cargo/config.toml` names (`build.rustc-wrapper`,
  `build.rustc-workspace-wrapper`).
- **rust-analyzer runs no build scripts, proc macros or `cargo check`**, and loads no dependencies.

Still open with `--trust`, so only trust a pull request whose code you'd run:

- **cargo runs what `.cargo/config.toml` says.** rust-analyzer still runs `cargo metadata`,
  `cargo rustc -- --print cfg`, `--print target-spec-json` and `rustc -vV` in the checkout, and
  cargo reads the repository's `.cargo/config.toml`: `build.rustc` names another program to run as
  rustc, `[env]` sets variables for it, and `build.rustflags` adds flags to it. Pinning more
  variables (`RUSTC`, `CARGO`) narrows this but can't close it. Closing it takes rust-analyzer not
  running cargo in the checkout (a `rust-project.json` rozed writes) or a sandbox.
- **rust-analyzer reads the repository's `rust-analyzer.toml`,** which may turn back on options
  the forced settings turn off. Not checked.
- **Zed's own git runs with the user's git config** in the checkout, so filters or an fsmonitor
  hook configured there run on the pull request's files.

## Follow-ups

- **Quitting logs `timed out waiting on app_will_quit`**, in debug and release builds. gpui gives
  the handlers Zed's crates register for quitting (workspace and session state, project shutdown)
  200 ms, and one of them takes longer. Nothing rozed relies on is saved yet; find which before
  rozed keeps state there.
- **The window title reads "empty project"**, Zed workspace's title for a project with no folders,
  as it does with an untrusted pull request open. With `--trust` it reads `pull-2212 — main.rs`:
  the checkout folder's name and the active file.
- **A trusted file's header offers Open File,** which opens the checkout's file in a plain Zed
  editor tab. rozed binds no save key and Zed's autosave is off, but that tab should be read-only
  too.
- **Zed's welcome page** ("Welcome back to Zed", with clone and extension entries) fills the pane
  until the diff tab opens. Milestone 5's PR tab and recent-PRs screen replace it and the title.
- **rust-analyzer always comes from GitHub,** where design §2 says rozed uses an installed copy
  when it finds one. The copy on `PATH` is usually rustup's proxy (see "Trust risks"); an installed
  binary that isn't a proxy would be safe to use. Milestone 10 decides.
- **A force-pushed head replaced the old one** in `refs/rozed/pull/<n>/head`, so older revisions
  didn't survive. Milestone 4's `repo_cache` pins every fetched head under
  `refs/rozed/pull/<n>/revs/<sha>`.
- **Big pull requests:** rozed reads both sides through one long-lived `git cat-file --batch` per
  repository, with `--trust` runs a `git hash-object` per file, and adds each file's hunks at once.
  Adding hunks in batches, as Zed's commit view does, would keep large diffs responsive.
- **GitHub's limit without signing in,** 60 requests an hour per address, is shared by the pull
  request lookup and rust-analyzer's release check until milestone 3 signs in.
