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
  executable with `--printenv` inside that shell, once at startup when there's no terminal and once
  per project folder. Before rozed handled the flag, each capture started another full rozed, which
  captured again: a runaway chain of processes. rozed now answers it as Zed does, and a rozed started
  by a running rozed refuses to start.
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
2. The local copy (`crates/rozed/src/local_copy.rs`, a stand-in for milestone 4's `repo_cache`): a
   bare `blob:none` partial clone at `~/Library/Caches/rozed/repos/github.com/<owner>/<repo>.git`, a
   fetch of `refs/pull/<n>/head` and the base branch into `refs/rozed/pull/<n>/head` and
   `refs/rozed/base/<branch>`, their merge base, the changed files (`git diff -M` with
   `--name-status` and `--numstat`), and a detached `git worktree` of the head at
   `~/Library/Caches/rozed/checkouts/github.com/<owner>/<repo>/pull-<n>`.
3. rozed trusts that checkout, and only it, then opens it as a local project. Every changed file
   goes into one `SplittableEditor`, in git's order: the right side is the project buffer, the left
   side the merge base's text from `git cat-file`, as in Zed's `file_diff_view.rs`.

On `sharkdp/fd#2212` (7 files: YAML, Markdown, TOML, a Makefile, an added shell script and Rust),
every file shows side by side, highlighted on both sides, with the added script's left side empty.
rust-analyzer's hovers work on the right side (`fn run() -> Result<ExitCode>`,
`const DEFAULT_LS_COLORS: &str`). Screenshots of both stay on the machine that took them (see
`decisions.md`). The pull request has no deleted, renamed or binary file: the local copy's tests
cover those, but how the diff shows them wasn't checked on screen.

What it took:

- **Right-side buffers must be project buffers** opened from the checkout, or no language server
  attaches. The left side never gets one: `SplittableEditor` turns it off.
- **The left side's language** has to be copied from the right buffer once it loads, as
  `file_diff_view.rs` does, or the left side stays plain text.
- **Deleted and binary files** get in-memory buffers with a `File` of rozed's own; binary ones show
  a folded placeholder, as Zed's commit view does. An editor `Addon` makes each file's header show
  the pull request's change (added, deleted, renamed or modified), not the checkout's status, which
  is always clean.
- **git without the user's config.** rozed runs git with `GIT_CONFIG_GLOBAL=/dev/null`,
  `GIT_CONFIG_NOSYSTEM=1` and `GIT_TERMINAL_PROMPT=0`, so URL rewrites, credential helpers, LFS
  filters and hooks configured there don't apply, and git never prompts. `--numstat` reads both
  sides of every changed file, so git downloads them in one batch and the later `git cat-file`
  calls are local. Symlinks are checked out as plain files.
- **rust-analyzer**, through settings in Zed's top layer, comes from its GitHub releases rather
  than `PATH`, and runs without build scripts, proc macros, `cargo check` or dependencies
  (`cargo.noDeps`). Hovers on the repository's own code work; anything typed by a dependency shows
  `{unknown}`, which matches design §7 ("a symbol from one shows its import and nothing more").
- **rust-analyzer still needs a Rust toolchain** to load the workspace. With rustup and no default
  toolchain, as on the machine used here, it reports "Failed to load workspaces" unless the
  repository names a toolchain, or rozed was started by `cargo run`, which passes `RUSTUP_TOOLCHAIN`
  on.

## Network check

Method: three runs of the debug build on `sharkdp/fd#2212`, each from a clean state.

- **Clean state.** rozed ran with `HOME` set to a new, empty folder, so its four folders, the clone
  cache and the rust-analyzer download all started empty, without touching the machine's own
  (`CARGO_HOME` and `RUSTUP_HOME` still pointed at the real toolchain).
- **Run A**, 3 minutes with scrolling and hovering: `RUST_LOG=info,rozed=debug` logged every
  request and redirect through rozed's HTTP client, and any refused Zed host at `warn`. A script
  sampled `lsof -i -n -P -a -p <pids>` every second for rozed and every process it had started, and
  each remote address was matched against GitHub's published ranges (`api.github.com/meta`).
- **Runs B and C**, 2.5 minutes each, the same but through a local proxy (`HTTPS_PROXY`) that
  logged the host of every `CONNECT`. The proxy names the hosts of short-lived processes that
  one-second sampling misses, such as git and the cargo and rustc calls rust-analyzer makes. Run C
  also selected a Rust toolchain (`RUSTUP_TOOLCHAIN`) so rust-analyzer could load the workspace,
  with scrolling and hovering.

Every host, in all three runs:

| Host | Contacted by | For |
|---|---|---|
| `api.github.com` | rozed's HTTP client | The pull request, and rust-analyzer's latest release |
| `github.com` | git | The partial clone's fetch and the batched downloads of file contents |
| `github.com` | rozed's HTTP client | rust-analyzer's release asset, which redirects to the next host |
| `release-assets.githubusercontent.com` | rozed's HTTP client | GitHub's release CDN, for `rust-analyzer-aarch64-apple-darwin.gz` |

- No Zed host appeared, and the refused-host warning never fired.
- In run A, `lsof` saw three remote addresses, all in GitHub's published ranges: `172.182.252.137`
  (api), `172.182.252.133` (git and web) and `185.199.110.133` (GitHub's CDN).
- In runs B and C, every socket rozed and its children held went to the proxy, so nothing bypassed
  it, and the proxy saw no plain HTTP. rust-analyzer's cargo and rustc calls made no connections.
- The language servers that need Node (for YAML and Bash) and Zed's default prettier failed to
  start without connecting anywhere, because rozed gives Zed no Node runtime.

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
| `target/` at the end | 4.6 GB, with 91 GB of disk still free |
| Cache saved | 1.3 GB |

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

There's no trust prompt yet (milestone 10): rozed trusts the checkout of whatever pull request it's
given, so language servers start there. Only open pull requests you'd trust. What a pull request
could do to the machine today, which milestone 10 has to close:

- **A `.zed/settings.json` in the checkout overrides rozed's forced settings.** Zed applies a
  trusted worktree's local settings over every global layer, including the top one rozed forces. A
  pull request that adds one can set `lsp.rust-analyzer.binary.path` to a script of its own, or turn
  build scripts and proc macros back on, and rozed would run them. rozed has to ignore a
  repository's `.zed` settings before it trusts anyone's code.
- **rustup follows the repository's toolchain file.** rust-analyzer runs cargo and rustc in the
  checkout through rustup's proxies, which use, and may first download and install, the toolchain a
  `rust-toolchain.toml` names. A repository's `.cargo/config.toml` can also point cargo at another
  `rustc` or a wrapper.
- **Zed captures the login shell's environment in the checkout**, so shell hooks that react to the
  directory (direnv, mise and the like) run there, behind their own safeguards.
- **Already closed:** git ignores the user's and the system's git config (no filters, hooks or
  credential helpers), symlinks are checked out as plain files, and rust-analyzer runs no build
  scripts, proc macros or `cargo check` and loads no dependencies.

## Follow-ups

- **Quitting logs `timed out waiting on app_will_quit`**, in debug and release builds. gpui gives
  the handlers Zed's crates register for quitting (workspace and session state, project shutdown)
  200 ms, and one of them takes longer. Nothing rozed relies on is saved yet; find which before
  rozed keeps state there.
- **The window title reads "empty project"**, Zed workspace's title for a project with no folders.
  With a pull request open it reads `pull-2212 — main.rs`: the checkout folder's name and the
  active file.
- **Zed's welcome page** ("Welcome back to Zed", with clone and extension entries) fills the pane
  until the diff tab opens. Milestone 5's PR tab and recent-PRs screen replace it and the title.
- **Quitting hangs with a pull request open, on the desktop used here.** Cmd-Q reaches rozed's quit
  handler, but `-[NSApplication terminate:]` then waits on `NSDocumentController`'s pending
  recent-documents update, which never completed in that desktop session. Zed's worktree store adds
  every visible local worktree to macOS's recent documents (`project/src/worktree_store.rs`,
  `add_recent_document`), and the checkout is one; without a pull request, rozed quits normally.
  A cached checkout doesn't belong in Recent Items anyway: keep it out (a Zed patch, or an
  invisible worktree if language servers work on one), then check quitting again.
- **Language servers start for every changed file's language.** The YAML and Bash servers and
  Zed's default prettier need Node, which rozed doesn't provide, so they fail at startup and log
  errors. rozed never formats, so prettier can go; milestone 10 decides which servers run.
- **rust-analyzer always comes from GitHub,** where design §2 says rozed uses an installed copy
  when it finds one. The copy on `PATH` is usually rustup's proxy (see "Trust risks"); an installed
  binary that isn't a proxy would be safe to use. Milestone 10 decides.
- **A force-pushed head replaces the old one** in `refs/rozed/pull/<n>/head`, so older revisions
  don't survive yet; milestone 4 keeps every fetched revision.
- **Big pull requests:** rozed runs one `git cat-file` per file and adds each file's hunks at once.
  `git cat-file --batch` and adding hunks in batches, as Zed's commit view does, would keep large
  diffs responsive.
- **GitHub's limit without signing in,** 60 requests an hour per address, is shared by the pull
  request lookup and rust-analyzer's release check until milestone 3 signs in.
