# Zed v1.27.0: what the Lo spike needs

Research for the Lo spike: a bare app that opens one GitHub PR in Zed's side-by-side diff
(`SplittableEditor`), with rust-analyzer running against a checkout of the PR head.

- Zed tree: `~/src/zed` at tag `v1.27.0` (`c84af5b99cbc366d675064e52b23664a015b6e35`), unchanged.
  All paths below are relative to the Zed root unless they start with `~`.
- "Read" means I read the code at that line. **Inferred** marks anything I reasoned out but didn't
  run or read end to end.
- Two scratch artifacts back some claims:
  - `zed_deps.py`, a feature-aware resolver over Zed's `Cargo.toml` files (macOS, normal deps).
  - `cargo-mock/`, a two-workspace Cargo experiment run with the pinned 1.98.1 toolchain.

  Both are in the scratchpad next to this file.

## Key findings

1. **A no-sign-in `AppState` works.** `Client::new(clock, http, cx)` (`crates/client/src/client.rs:558`)
   never connects on its own. Every connect path is a sign-in, a room join, or a reconnect after a
   previous connection. `crates/component_preview/examples/component_preview.rs` already builds an
   `AppState` and a `Workspace` outside the `zed` crate. Use it as the template.
2. **Two things contact Zed with no user action:**
   - **Telemetry** (`api.zed.dev/telemetry/events`). It fires once any editor opens, because
     `Editor Opened` is reported at `crates/editor/src/editor.rs:2758`. The flush runs after 1 s in
     debug and 5 min in release.
   - **`extension_host::init`**, which auto-installs the `html` extension and checks
     `api.zed.dev/extensions/updates` at startup.

   Block both with settings, and add an HTTP client that refuses Zed hosts.
3. **Dev builds read assets from the repo root, not the crate.** `util::fs_embed!` resolves assets
   against the nearest `.git` ancestor of the executable. For Lo that's Lo's root, so debug
   builds panic with `missing config for language`. Enable `util`'s `debug-embed` feature.
4. **Required features:**
   - `gpui_platform/font-kit`. Without it, macOS renders **no text** (`crates/gpui_macos/src/platform.rs:220-228`).
   - `languages/load-grammars`. Without it there are no tree-sitter grammars.
5. **Right-side buffers must be real project buffers** (`worktree::File`, local, not gitignored) for
   LSP to attach. The commit view's `GitBlob` buffers never get LSP. `git_ui_core/src/file_diff_view.rs`
   shows the pattern: a project buffer on the right, base text from anywhere.
6. **`paths`: one line.** v1.27 already has `pub const APP_NAME: &str = "Zed"` for forks
   (`crates/paths/src/paths.rs:18`). Setting it to `"Lo"` gives exactly the four folders asked for.
7. **Cargo:** `exclude = ["third_party/zed"]` is required. Without it, cargo makes Zed's crates
   members of Lo's workspace and inheritance fails (I reproduced this). With it, `workspace = true`
   resolves against Zed's root (confirmed). Zed's `[patch]`, `[profile]` and `.cargo/config.toml`
   are ignored (confirmed for profile and config). Seed Lo's `Cargo.lock` from Zed's.
8. **Weight:** about 104–116 Zed crates. `client`, cloud, telemetry, `agent_settings`/`language_model`,
   `dap`, `terminal`, `context_server` and `remote` are unavoidable. wasmtime is compiled even without
   `extension_host`, because `language` enables `tree-sitter/wasm`, so cmake is required. Don't depend
   on `git_ui` or `git_ui_core`: they pull `auto_update`.

---

## 1. Minimal startup

### What Zed's `main` does, and what Lo needs

Everything is in `crates/zed/src/main.rs` unless noted.

| Zed call (line) | Spike? | Notes |
|---|---|---|
| `init_paths()` (283, 1660-1678) | Yes, copy | `create_dir_all` for config, extensions, languages, debug_adapters, db, logs, temp, hang_traces. |
| `zlog::init()` + `init_output_file(paths::log_file(), …)` (289-299) | Yes | Writes to stdout when on a TTY. |
| `util::increase_open_file_limit()` (303) | Yes | File watchers need fds. |
| `build_application().with_assets(Assets)` (87-94, 342) | Yes | Use `gpui_platform::application()` (`crates/gpui_platform/src/gpui_platform.rs:13`) or `Application::with_platform(current_platform(false))`. |
| `db::AppDatabase::new()` then `cx.set_global(app_db)` (346, 480) | **Required** | Without it, `WorkspaceDb::global` panics with "database not initialized" (`crates/db/src/db.rs:80-91`). The db opens at `database_dir()/0-<channel>/` (`db.rs:164-168`). |
| `system_id`, `installation_id` (347-350) | No | Only feed telemetry. |
| `Session::new` (352-355) | Yes | Needed for `AppState.session`. Local kvp only. |
| single-instance check, crash handler (360-421) | No | |
| `RealFs::new(git_binary_path, executor)` (436) | Yes | Pass `None` for git (uses `git` on `PATH`). |
| `util::load_login_shell_environment()` when not a TTY (443-454) | Yes, for a Finder-launched `.app` | Gives `PATH` for git, node and rust-analyzer. |
| `trusted_worktrees::init(…)` (488) | **No, on purpose** | See section 3. Without the global, LSPs start ungated. |
| `menu::init()`, `zed_actions::init()` (489-490) | Optional | Both are empty functions. |
| `release_channel::init(app_version, cx)` (492) | **Required** | `ReleaseChannel::global` would panic, and `AppVersion` is read by telemetry and extension_host. |
| `gpui_tokio::init(cx)` (493) | **Required** | The reqwest client needs a tokio runtime. |
| `settings::init(cx)` (497) | **Required** | Creates `SettingsStore` from `assets/settings/default.json` (`crates/settings/src/settings.rs:146-150`). |
| `zed::watch_settings_files` (499) | Later | In the `zed` crate. `SettingsStore::watch_settings_files` (`crates/settings/src/settings_store.rs:362-410`) watches `settings.json` and `global_settings.json`. |
| `handle_keymap_file_changes` (500) | Replace | Lives in the `zed` crate. Write your own loader (section 7). |
| reqwest HTTP client and `cx.set_http_client` (502-515) | Yes | Wrap it in a Zed-host blocker (below). |
| `<dyn Fs>::set_global` (517) | **Required** | Used by `update_settings_file`, e.g. `DiffStyleControls` at `crates/editor/src/split.rs:434`. |
| `GitHostingProviderRegistry::set_global` + `git_hosting_providers::init` (519-520) | Optional | If you call `init`, set the registry first. `init` uses `::global` (`crates/git_hosting_providers/src/git_hosting_providers.rs:21`). |
| `extension::init(cx)` (524) | Comes via `Project::init` | Through `context_server_store::init`, `crates/project/src/context_server_store.rs:39-41`. |
| `Client::production(cx)` (527) | **Replace with `Client::new`** | `production` takes `server_url` = `https://zed.dev` (`client.rs:584-592`, `default.json:2888`). |
| `cx.set_http_client(client.http_client())` (528) | Yes, as your own wrapper | |
| `LanguageRegistry::new` + `set_language_server_download_dir(paths::languages_dir())` (529-531) | **Required** | |
| node options watch + `NodeRuntime::new(http, shell_env_rx, rx)` (532-559) | Yes | `NodeRuntime::unavailable()` is fine if only rust-analyzer is needed. |
| `languages::init(languages, fs, node, cx)` (562) | **Required** | `crates/languages/src/lib.rs:58`. Registers languages, grammars and LSP adapters. Note there's no `language::init` in 1.27. |
| `UserStore::new`, `WorkspaceStore::new` (563-564) | **Required** for `AppState` | Both are local. `UserStore` calls cloud only after the client authenticates (`crates/client/src/user.rs:215-235`). `WorkspaceStore::new` only registers RPC handlers (`crates/workspace/src/workspace.rs:10295-10304`). |
| `language_extension::init` (566-583) | Later, with extensions | |
| `Client::set_global` (585) | Optional | I found no `Client::global(cx)` in editor, workspace, project, vim, language or file_finder. |
| `project::Project::init(&client, cx)` (590) | **Yes** | Proto handler registration plus local globals: the connection `Manager`, `ExtensionHostProxy` and DAP locators (`crates/project/src/project.rs:1250-1284`). No network. |
| `client::init(&client, cx)` (593) | **No** | Only registers SignIn, SignOut and Reconnect action handlers (`client.rs:166-199`). |
| `FeatureFlagStore::init` (594) | Optional, harmless | Local (`crates/feature_flags/src/store.rs:79-91`). |
| `telemetry.start(…)`, telemetry events (600-642, 836-846) | **No** | |
| `AppState { … }` + `set_global` (645-655) | **Required** | Struct at `crates/workspace/src/workspace.rs:1332-1341`. |
| `auto_update::init`, `auto_update_ui`, `reliability`, `dap_adapters` (658-661) | **No** | |
| `extension_host::init` (662-668) | **No, for the spike** | Contacts api.zed.dev at startup. See the contact list below. |
| `theme_settings::init(LoadThemes::All(Box::new(Assets)), cx)` (670) | **Required** | `crates/theme_settings/src/theme_settings.rs:71`. |
| `eager_load_active_theme_and_icon_theme` (671) | Optional | `zed` crate. |
| `command_palette::init` (677) | Yes | |
| copilot, language_model(s), agent, web_search, edit prediction, repl, `AgentRegistryStore` (678-725) | **No** | `AgentRegistryStore::init_global` fetches `cdn.agentclientprotocol.com`. `AgentServerStore::local` only uses `try_global` (`crates/project/src/agent_server_store.rs:508`). |
| `load_embedded_fonts(cx)` (727, 1826-1848) | **Required** | `.ZedMono` and `.ZedSans` map to Lilex and IBM Plex Sans (`crates/gpui/src/text_system.rs:1268-1269`). Call `assets::Assets.load_fonts(cx)` (`crates/assets/src/assets.rs:46`). |
| `editor::init(cx)` (731) | **Required** | `crates/editor/src/editor.rs:364`. |
| `workspace::init(app_state, cx)` (737) | **Required** | `workspace.rs:992-1036`. Sets up HistoryManager, toasts and actions. |
| `file_finder::init`, `search::init` (741, 751) | Yes | |
| `PaneSearchBarCallbacks` global (753-761) | Yes, if you want buffer search or vim `/` | |
| `vim::init(cx)` (762) | Yes | |
| `call`, `notifications`, `collab_ui`, `channel`, `git_ui`, `onboarding`, `feedback`, `extensions_ui`, … (750, 773-789) | **No** | |
| Observer on `SettingsStore` that resets `http.base_url` from `server_url` (793-827) | **No** | |
| `languages.set_theme(cx.theme())` + observe `GlobalTheme` (828-835) | **Required** | Without it, syntax highlights have no colours. |
| `initialize_workspace` (875) | No | Title bar, panels and so on, all in the `zed` crate. |
| `authenticate(client)` (879-883, 1371-1383) | **Never** | Signs in when credentials exist. |

### Minimal init sketch

Inferred from the calls above and `component_preview.rs`, not compiled.

```rust
// lo/crates/lo/src/main.rs
fn main() {
    for dir in [paths::config_dir(), paths::data_dir(), paths::logs_dir(), paths::database_dir(),
                paths::languages_dir(), paths::temp_dir()] {
        std::fs::create_dir_all(dir).expect("create Lo dirs");
    }
    zlog::init();
    if std::io::stdout().is_terminal() { zlog::init_output_stdout(); }
    else { zlog::init_output_file(paths::log_file(), Some(paths::old_log_file())).ok(); }
    util::increase_open_file_limit().log_err();

    let app = gpui_platform::application().with_assets(assets::Assets);
    let app_db = db::AppDatabase::new();

    app.run(move |cx| {
        cx.set_global(app_db);
        release_channel::init(AppVersion::load(env!("CARGO_PKG_VERSION"), None, None), cx);
        gpui_tokio::init(cx);
        settings::init(cx);
        // Forced overlay at the top settings layer. Users can't override it. See below.
        SettingsStore::update_global(cx, |s, cx| s.set_server_settings(LO_FORCED, cx)).unwrap();

        // One HTTP client for everything, refusing Zed hosts.
        let inner = Arc::new(ReqwestClient::user_agent("lo/0.1").unwrap());
        let http: Arc<dyn HttpClient> = Arc::new(NoZedHosts(inner));
        cx.set_http_client(http.clone());
        let http_with_url = Arc::new(HttpClientWithUrl::new(http.clone(), "https://lo.invalid", None));

        let fs = fs::RealFs::new(None, cx.background_executor().clone());
        <dyn fs::Fs>::set_global(fs.clone(), cx);

        // Constructs Telemetry (sink only). No connection. Never call sign_in or connect.
        let client = Client::new(Arc::new(clock::RealSystemClock), http_with_url, cx);

        let mut languages = LanguageRegistry::new(cx.background_executor().clone());
        languages.set_language_server_download_dir(paths::languages_dir().clone());
        let languages = Arc::new(languages);
        let (node_tx, node_rx) = watch::channel(Some(NodeBinaryOptions {
            allow_path_lookup: true, allow_binary_download: true, use_paths: None }));
        std::mem::forget(node_tx); // inferred: keep the sender alive
        let node = NodeRuntime::new(client.http_client(), None, node_rx);
        languages::init(languages.clone(), fs.clone(), node.clone(), cx);

        let user_store = cx.new(|cx| UserStore::new(client.clone(), cx));
        let workspace_store = cx.new(|cx| WorkspaceStore::new(client.clone(), cx));
        project::Project::init(&client, cx);
        feature_flags::FeatureFlagStore::init(cx);

        let kvp = db::kvp::KeyValueStore::global(cx);
        let session = cx.foreground_executor().block_on(Session::new(uuid::Uuid::new_v4().to_string(), kvp));
        let app_state = Arc::new(AppState {
            languages, client, user_store, workspace_store, fs, node_runtime: node,
            build_window_options: lo_window_options,
            session: cx.new(|cx| AppSession::new(session, cx)),
        });
        AppState::set_global(app_state.clone(), cx);

        theme_settings::init(theme::LoadThemes::All(Box::new(assets::Assets)), cx);
        assets::Assets.load_fonts(cx).unwrap();
        app_state.languages.set_theme(cx.theme().clone());
        cx.observe_global::<theme::GlobalTheme>({ let l = app_state.languages.clone();
            move |cx| l.set_theme(cx.theme().clone()) }).detach();

        editor::init(cx);
        workspace::init(app_state.clone(), cx);
        command_palette::init(cx);
        file_finder::init(cx);
        search::init(cx);
        cx.set_global(workspace::PaneSearchBarCallbacks { /* copy main.rs:753-761 */ });
        vim::init(cx);
        load_keymaps(cx);              // section 7
        open_pr_window(app_state, cx); // below
    });
}

fn open_pr_window(app_state: Arc<AppState>, cx: &mut App) {
    let window = cx.open_window(lo_window_options(None, cx), {
        let app_state = app_state.clone();
        move |window, cx| {
            theme_settings::setup_ui_font(window, cx);
            let project = Project::local(
                app_state.client.clone(), app_state.node_runtime.clone(), app_state.user_store.clone(),
                app_state.languages.clone(), app_state.fs.clone(), None,
                project::LocalProjectFlags { init_worktree_trust: false, watch_global_configs: true }, cx);
            let workspace = cx.new(|cx| Workspace::new(None, project, app_state.clone(), window, cx));
            // Zed's own windows are rooted at MultiWorkspace (workspace.rs:2335-2358, 11328-11344).
            // workspace.rs has 16 `downcast::<MultiWorkspace>()` sites, so match it.
            cx.new(|cx| MultiWorkspace::new(workspace, window, cx))
        }
    }).unwrap();
    // then window.update(...) -> multi_workspace.workspace() -> spawn the PR diff (section 2)
}
```

`lo_window_options` can be plain `WindowOptions { titlebar: Some(TitlebarOptions { title: Some("Lo".into()), ..Default::default() }), ..Default::default() }`.
Zed's version (`crates/zed/src/zed.rs:360-410`) uses `appears_transparent: true` with a
traffic-light offset because it draws its own title bar from the `title_bar` crate. Lo has no
title bar, so keep the native one for the spike.

`MultiWorkspace::new` reads `AgentSettings` and `DisableAiSettings`
(`crates/workspace/src/multi_workspace.rs:341-357`). They're registered setting types, so this is
fine. Force `disable_ai: true` so the multi-workspace sidebar stays off.

### Is `AppState` constructible without signing in?

Yes. `AppState.client` is an `Arc<Client>` built by `Client::new`, which:

- creates `Peer`, `Telemetry::new`, `CloudApiClient::new` and the credentials provider wrapper,
  without touching the network or the Keychain (`client.rs:558-582`). Credentials are read lazily
  (`crates/zed_credentials_provider/src/zed_credentials_provider.rs:27-31`).
- never connects. Connections happen only in:
  - `sign_in_with_optional_connect`, from the SignIn action or `authenticate`
  - `Project::in_room` (`crates/project/src/project.rs:1784`)
  - the reconnect loop, which runs only after `ConnectionLost` (`client.rs:694-705`)

`WorkspaceStore` and `UserStore` are local. `AppState::test` (`workspace.rs:1407-1442`) shows the
same construction with a fake HTTP client.

### Every Zed-host contact point in the spike's crate set

I grepped every crate in the resolved set for `build_zed_(api|cloud|llm)_url`, `cloud_client()`,
`zed.dev` URLs and websocket connects.

| Where | Host | Trigger | Without user action? | Mitigation |
|---|---|---|---|---|
| `crates/client/src/telemetry.rs:566-619` (`report_event`), `637-658` (`build_zed_api_url("/telemetry/events")`), `711-716` | `api.zed.dev` (derived from `server_url`, `http_client.rs:281-295`) | Any `telemetry::event!`. `Telemetry::new` (`telemetry.rs:186-268`, called from `Client::new`) installs the global sink (`:244`). The editor reports **"Editor Opened"** on creation (`editor.rs:2758`, `ReportEditorEvent` at `:324-336`). Workspace has 10 events and command_palette 1. The flush timer is 1 s in debug and 5 min in release (`telemetry.rs:78,81`). The queue flushes at 5 events in debug (`:72`). | **Yes** | `telemetry.metrics=false` (early return at `:571`). Also `diagnostics=false`, a non-Zed base URL and the host filter. |
| `crates/extension_host/src/extension_host.rs:464-470` → `auto_install_extensions` (`746-776`) → `install_latest_extension` (`975-990`, `/extensions/{id}/download`) | `api.zed.dev` | `ExtensionStore::new`, run by `extension_host::init` (`320-357`). Default `auto_install_extensions: {"html": true}` (`default.json:2358-2360`). | **Yes**, on first start | Don't call `extension_host::init` in the spike. Later, set `auto_install_extensions: {"html": false}` and rely on the host filter. |
| `extension_host.rs:778-782` `check_for_updates` → `687-735` (`/extensions/updates`) → `813-821` | `api.zed.dev` | Same startup task. It runs **even with zero extensions installed**: the `ids=` query is just empty. | **Yes** | Same. Only the host filter (or a patch) stops it, because there's no setting. |
| `extension_host.rs:661-684`, `1009-1030` | `api.zed.dev` | Extension store UI, upgrades | No | |
| `crates/client/src/client.rs:1042-1065` (`sign_in_with_optional_connect` → `connect_to_cloud`), `crates/cloud_api_client/src/websocket/native.rs:71` (`/client/users/connect`) | `cloud.zed.dev` (websocket, **not** through `HttpClient`) | SignIn action or `authenticate()` | No | Don't call `client::init` or `authenticate`. |
| `client.rs:1091` `connect` (collab RPC websocket), `1430-1460` browser auth, `1561-1580` admin impersonation | `zed.dev`, collab | Sign-in, joining a room | No | Same |
| `client/src/user.rs:215-235`, `721`, `943`; `cloud_api_client.rs:116-161, 244-305` | `cloud.zed.dev` | After authentication | No | |
| `context_server/src/oauth.rs:37` (`https://zed.dev/oauth/client-metadata.json`) | `zed.dev` | MCP OAuth for a configured context server | No | |
| `release_channel/src/lib.rs:10`, `workspace/src/notifications.rs:1389,1405`, `project/src/project.rs:4017`, `editor/src/edit_prediction.rs:2555` | `zed.dev/docs`, `/releases` | Links opened with `cx.open_url` when the user clicks | No | Cosmetic |
| Not linked by Lo: `auto_update`, `crashes`/minidumps, `copilot`, `language_models`, `web_search_providers`, `edit_prediction`, `AgentRegistryStore` | various | Started from `zed/src/main.rs` | n/a | Don't link or init them |

Other first-party downloads that aren't Zed hosts:

- rust-analyzer: `api.github.com` and `github.com` release assets (section 3).
- Node: `nodejs.org/dist/v24.22.1/...` (`crates/node_runtime/src/node_runtime.rs:609,703`).
- npm packages, for the npm-based LSPs.

### Blocking Zed hosts in one place

All HTTP traffic in this setup goes through one `Arc<dyn HttpClient>`:

- `cx.http_client()`, used for images and other non-client requests.
- `client.http_client()`, which `Project::local` hands to `LspStore::new_local`, `DapStore` and
  `AgentServerStore` (`project.rs:1351, 1425, 1436`), to `NodeRuntime`, and to `extension_host`
  (`extension_host.rs:329-337`).

Wrap the reqwest client in a filter. It must not be a blanket block: the same client downloads
rust-analyzer.

```rust
struct NoZedHosts(Arc<dyn HttpClient>);
impl HttpClient for NoZedHosts {
    fn send(&self, req: http::Request<AsyncBody>) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        let host = req.uri().host().unwrap_or_default().to_ascii_lowercase();
        if host == "zed.dev" || host.ends_with(".zed.dev") || host.ends_with(".invalid") {
            log::warn!("Lo: blocked request to {}", req.uri()); // the spike's check greps for this
            return Box::pin(async move { anyhow::bail!("blocked Zed host {host}") });
        }
        self.0.send(req)
    }
    fn user_agent(&self) -> Option<&HeaderValue> { self.0.user_agent() }
    fn proxy(&self) -> Option<&Url> { self.0.proxy() }
}
```

The websocket paths (`cloud.zed.dev`, collab) bypass `HttpClient`. They're unreachable because
nothing calls sign-in. To prove "nothing contacts Zed's servers", run the spike behind a logging
proxy (`HTTPS_PROXY`; `read_proxy_from_env`, `http_client.rs:369-383`) or with `nettop`. The filter
alone doesn't prove it.

### Forced settings overlay

Layer order is default → extension → global → user (plus release channel, OS and profile) →
**server** (`crates/settings/src/settings_store.rs:1379-1397`).

- `set_server_settings(&str)` (`:1020-1036`) is the top layer, so users can't override it. Use it
  for things Lo must force.
- `set_global_settings` (`:995`) sits below user settings, but `watch_settings_files` (`:362-410`)
  overwrites it from `~/.config/lo/global_settings.json`. Use it only if you don't call
  `watch_settings_files`.
- Don't use the extension layer. `languages::init` writes it (`crates/languages/src/lib.rs:314`).

```jsonc
// LO_FORCED (server layer)
{
  "telemetry": { "diagnostics": false, "metrics": false },
  "server_url": "https://lo.invalid",        // makes build_zed_*_url point nowhere
  "auto_update": false,
  "disable_ai": true,                            // also keeps MultiWorkspace's sidebar off
  "auto_install_extensions": { "html": false },
  "diff_view_style": "split",                    // default already "split" (default.json:419)
  "vim_mode": true                               // if Lo wants vim on by default; see section 7
}
```

---

## 2. The diff

### How `SplittableEditor` works

`crates/editor/src/split.rs`:

- **Struct** (`534-548`): an `rhs_multibuffer` and `rhs_editor` that are always present, an optional
  `lhs: LhsEditor`, a `WeakEntity<Workspace>`, and `diff_view_style`.
- **`new(style, rhs_multibuffer, project, workspace: Entity<Workspace>, window, cx)`** (`615-704`):
  - Builds `Editor::for_multibuffer(rhs_multibuffer, Some(project))` with expanded hunks, no
    runnables, no code lens, no inline diagnostics and no minimap.
  - Defers `split()` until after `added_to_workspace`.
  - **Needs a `Workspace` entity.** `split()` reads `workspace.project()` (`710-713`).
- **`split()`** (`706-924`):
  - Builds the left `MultiBuffer` (read-only) and an `Editor` over it.
  - The left editor gets **`disable_lsp_data()`** (`746`), `disable_diagnostics` and no runnables.
    The left side is a mirror built from each `BufferDiff`'s **base-text buffer**, via
    `add_inverted_diff` in `sync_lhs_for_paths` (`1301-1389`).
  - Scroll anchors are shared and cursors are synced.
- **`update_excerpts_for_path(path_key, buffer, ranges, context_lines, diff, cx)`** (`1192-1242`) is
  the only call needed per file. It adds excerpts and the diff to the right side, then mirrors them
  on the left.
- **Style:**
  - An observer flips split and unified whenever `EditorSettings.diff_view_style` changes
    (`660-666`). Set the style through settings, or the next settings change toggles it back.
  - Narrow windows unsplit automatically below `minimum_split_diff_width` (default 100 em,
    `default.json:426`; logic at `1391-1418`).
- `SplittableEditor` implements `workspace::Item` (`1943-2105`), so it can go straight into a pane
  for the spike.

### How the commit view builds it

`crates/git_ui/src/commit_view.rs`:

- **Multibuffer** (`305-309`): `MultiBuffer::new(Capability::ReadOnly)` with
  `set_all_diff_hunks_expanded`.
- **Editor** (`320-338`): `SplittableEditor::new(EditorSettings…diff_view_style, multibuffer, project, workspace_entity, …)`.
  Then `set_diff_hunk_renderer(Some(Arc::new(HiddenDiffHunkRenderer)))`, which hides
  stage and restore controls. It also turns off bookmarks and breakpoints on the right editor.
- **Per file** (`348-477`):
  - `build_buffer(new_text, GitBlob, …)` (`1114-1147`) creates an **in-memory** `Buffer` whose
    `File` is a `GitBlob` (`93-99`, `1070-1112`): `as_local() = None` and `DiskState::Historic`.
  - `build_buffer_diff(old_text, &buffer, …)` (`1149-1175`) does
    `BufferDiff::new(&snapshot.text, language, Some(registry), cx)`, then
    `set_base_text(Some(old_text), snapshot.text.clone(), cx).await`.
  - Excerpt ranges are `diff_snapshot.hunks(&snapshot).map(|h| h.buffer_range.to_point(&snapshot))`,
    or the whole file if there are no hunks (`428-448`).
  - `PathKey::with_sort_prefix(1, file.path())`.
  - `editor.update_excerpts_for_path(...)`, batched 10 excerpts at a time with `yield_now()`
    (`450-476`).
  - Calls `editor.split(window, cx)` after the first batch when the style is Split (`468-470`).
- **Afterwards:** registers a `CommitDiffAddon` that overrides per-buffer file status (`122-138`),
  and folds binary buffers (`479-496`).

The commit view's right side **never gets LSP**. `LspStore::register_buffer_with_language_servers`
returns early unless `File::from_dyn(buffer.file())` is a `worktree::File` with `is_local()`
(`crates/project/src/lsp_store.rs:3198-3216`).

### How Zed gets LSP on the right side

Two existing patterns use **real project buffers** on the right:

- **Branch and project diff:**
  - `DiffBufferList::load_buffer` (`crates/project/src/git_store/diff_buffer_list.rs:377-470`)
    calls `buffer_store.open_buffer(project_path)`.
  - For a merge base it calls `git_store.open_diff_since(old_oid, buffer, repo)`
    (`crates/project/src/git_store.rs:1654-1760`).
  - That loads the base blob, builds a `BufferDiff` over the live buffer, attaches the unstaged diff
    as a secondary diff, and recalculates when the buffer changes.
  - The file list comes from `repo.diff_tree(DiffTreeType::…)`. Plain two-commit
    `DiffTreeType::Since { base, head }` exists (`crates/git/src/status.rs:501-513`) and runs
    `git diff-tree -r -z --no-renames base head` (`crates/git/src/repository.rs:1915-1960`). Note
    `--no-renames`.
- **`zed --diff a b`:** `crates/git_ui_core/src/file_diff_view.rs`.
  - `open` (`45-103`) opens both files with `project.open_local_buffer`.
  - `build_buffer_diff` (`223-253`) sets the old buffer's text as the base of a `BufferDiff` over the
    **project** buffer.
  - `new` (`105-219`) wraps it in `MultiBuffer::singleton` + `add_diff` + `SplittableEditor`.
  - **Gotcha** (`150-166`): the project buffer's language may load after the diff is built, so it
    subscribes to `BufferEvent::LanguageChanged` and copies the language onto
    `diff.base_text_buffer()`. Without that, the left side has no highlighting.

Once a buffer is a local project buffer, the right `Editor` registers visible buffers with the
language servers by itself:

- `register_visible_buffers` and `register_buffer` (`crates/editor/src/editor.rs:11487-11520`).
- `is_lsp_relevant` (`crates/editor/src/completions.rs:139-157`). The file must be a worktree entry
  and **not gitignored**.

Hover, go-to-definition, inlay hints and diagnostics then work on the right side. The left side has
none, by design (`split.rs:746`).

### Sketch: Lo's PR diff over two commits

Adapted from `commit_view.rs` and `file_diff_view.rs`; not compiled. Lo's own code supplies:

- `base` and `head` SHAs
- a checkout of `head` at `checkout_dir`
- the changed files, from `git diff --name-status -z -M base head`, so renames are detected
- the old text, from `git cat-file --batch` or `git show base:path`

```rust
pub struct ChangedFile { pub old_path: Option<String>, pub new_path: Option<String>, pub old_text: Option<String>, pub binary: bool }

async fn open_pr_diff(workspace: WeakEntity<Workspace>, checkout_dir: PathBuf,
                      files: Vec<ChangedFile>, cx: &mut AsyncWindowContext) -> anyhow::Result<()> {
    let project = workspace.update(cx, |w, _| w.project().clone())?;
    // A visible local worktree at the checkout (the LSP root comes from Cargo.toml via ManifestTree).
    let (worktree, _) = project.update(cx, |p, cx| p.find_or_create_worktree(&checkout_dir, true, cx)).await?;
    let worktree_id = worktree.read_with(cx, |w, _| w.id());
    let languages = project.read_with(cx, |p, _| p.languages().clone());

    let multibuffer = cx.new(|cx| { let mut m = MultiBuffer::new(Capability::ReadOnly); m.set_all_diff_hunks_expanded(cx); m });
    let splittable = workspace.update_in(cx, |w, window, cx| {
        let ws = cx.entity();
        let e = cx.new(|cx| {
            let e = SplittableEditor::new(EditorSettings::get_global(cx).diff_view_style,
                                          multibuffer.clone(), project.clone(), ws, window, cx);
            e.set_diff_hunk_renderer(Some(Arc::new(editor::HiddenDiffHunkRenderer)), cx);
            e
        });
        w.add_item_to_active_pane(Box::new(e.clone()), None, true, window, cx);
        e
    })?;

    for (ix, f) in files.into_iter().enumerate() {
        // Right side: a real project buffer if the file exists at head, else an in-memory empty one.
        let buffer = match &f.new_path {
            Some(p) => {
                let rel = RelPath::from_unix_str(p)?.into_arc();
                project.update(cx, |p, cx| p.open_buffer((worktree_id, rel), cx)).await?
            }
            None => deleted_file_buffer(&f, &languages, cx).await?, // like commit_view::build_buffer + a custom File
        };
        let snapshot = buffer.read_with(cx, |b, _| b.snapshot());
        let diff = cx.new(|cx| BufferDiff::new(&snapshot.text, snapshot.language().cloned(), Some(languages.clone()), cx));
        let mut old = f.old_text.clone();
        if let Some(t) = &mut old { text::LineEnding::normalize(t); }
        diff.update(cx, |d, cx| d.set_base_text(old.map(Arc::from), snapshot.text.clone(), cx)).await;
        // Copy a late-loaded language onto the left side (file_diff_view.rs:150-166).
        cx.update(|_, cx| cx.subscribe(&buffer, { let base = diff.read(cx).base_text_buffer().downgrade();
            move |b, ev, cx| if let language::BufferEvent::LanguageChanged(_) = ev {
                let l = b.read(cx).language().cloned(); base.update(cx, |bb, cx| bb.set_language_async(l, cx)).ok(); }}).detach())?;

        let (ranges, key) = cx.update(|_, cx| {
            let ds = diff.read(cx).snapshot(cx);
            let mut r: Vec<_> = ds.hunks(&snapshot).map(|h| h.buffer_range.to_point(&snapshot)).collect();
            if r.is_empty() { r.push(Point::zero()..snapshot.max_point()); }
            (r, PathKey::with_sort_prefix(ix as u64, snapshot.file().unwrap().path().clone())) // sort prefix = review order
        })?;
        splittable.update_in(cx, |e, window, cx| {
            e.update_excerpts_for_path(key, buffer.clone(), ranges, editor::multibuffer_context_lines(cx), diff.clone(), cx);
            if ix == 0 && e.diff_view_style() == DiffViewStyle::Split { e.split(window, cx); }
        })?;
    }
    Ok(())
}
```

Notes for the implementer:

- **Read-only.** `MultiBuffer::new(Capability::ReadOnly)` makes the editors read-only
  (`Editor::read_only` → `multi_buffer.read_only()`, `editor.rs:3439`; `multi_buffer.rs:1345`),
  even though project buffers are `ReadWrite`.
- **Manual `BufferDiff` versus `open_diff_since`.** A manual `BufferDiff` doesn't recompute if the
  file changes on disk. That's fine for a read-only checkout: rebuild when Lo checks out a new
  revision. `GitStore::open_diff_since(Some(old_blob_oid), buffer, repo, cx)` gives a live diff,
  but it needs the `Repository` entity and blob oids from a tree diff, and it attaches the
  unstaged diff.
- **Renames.** Key the `PathKey` by the new path and take the base text from the old path. Show
  "renamed from" with a custom header or addon later. Zed's `DiffTreeType` uses `--no-renames`.
- **Added files:** `set_base_text(None, …)`. **Binary files:** use
  `BufferDiff::new_unchanged` and fold the buffer, as `commit_view.rs:408-424` and `489-495` do.
- **Order.** `PathKey.sort_prefix` decides file order (`crates/multi_buffer/src/path_key.rs:19-44`).
  Use it for design §5's review order.
- **Per-buffer status.** To show added, deleted or modified badges in buffer headers, register an
  `editor::Addon` with `override_status_for_buffer_id` on the right editor, like `CommitDiffAddon`
  (`commit_view.rs:122-138`).
- **Avoid depending on `git_ui` or `git_ui_core`.** `git_ui` adds `auto_update`, `prompt_store`,
  `notifications`, `remote_connection`, `agent_skills` and `time_format`. `git_ui_core` also
  depends on `remote_connection`, which brings `auto_update`. Copy the ~150 lines you need.

---

## 3. Language server (rust-analyzer)

What has to exist:

1. **`languages::init(registry, fs, node, cx)`**, with the `languages` crate built with
   `features = ["load-grammars"]`. Zed sets this in `crates/zed/Cargo.toml:158`. Without it,
   `register_native_grammars` is compiled out (`crates/languages/src/lib.rs:59-60`) and there's no
   highlighting.
   - It registers `rust::RustLspAdapter` (`lib.rs:80`).
   - `LanguageRegistry::set_language_server_download_dir(paths::languages_dir())` is where binaries
     land: `~/Library/Application Support/Lo/languages/rust-analyzer/`.
2. **A `Project::local`** with a **visible local worktree at the checkout root**, created with
   `find_or_create_worktree(dir, true)`. Rust files must be opened **through the project**
   (`open_buffer` / `open_local_buffer`) and shown in an `Editor` with `lsp_data_enabled`. The
   right side of `SplittableEditor` qualifies.
3. **The HTTP client inside `Client`.**
   - `Project::local` passes `client.http_client()` to `LspStore::new_local` (`project.rs:1413-1428`).
     That becomes the adapter delegate's `http_client()` (`lsp_store.rs:17061`).
   - It must reach `api.github.com`, `github.com` and `objects.githubusercontent.com`.
   - The spike can use `NodeRuntime::unavailable()`: rust-analyzer doesn't need Node.
4. **No trust gate.**
   - If the `TrustedWorktrees` global exists and the worktree isn't trusted, server start waits
     (`lsp_store.rs:538-575`).
   - Without the global, `track_worktree_trust` just logs (`crates/project/src/trusted_worktrees.rs:66-106`)
     and servers start.
   - The spike should skip `trusted_worktrees::init` and pass `init_worktree_trust: false`.
   - **Design consequence:** with no gate, Lo must create a local worktree `Project` **only for
     repos it trusts** (design §7). A repo's `.zed/settings.json` can set `lsp.<server>.binary.path`.
     It's read by the project `SettingsObserver` (`BinarySettings` at
     `crates/settings_content/src/project.rs:311-317`).
   - Shell-env loading also runs the user's login shell in the worktree. Inferred: that triggers
     direnv and similar hooks.

Where rust-analyzer comes from (`crates/languages/src/rust.rs`):

- **`check_if_user_installed`** (`754-800`) runs first:
  1. If the worktree has a toolchain override, `rustup which rust-analyzer` runs in the worktree
     (`203-240`). Inferred: rustup may install the repo's pinned toolchain. Another trust concern.
  2. Otherwise `rust-analyzer` on the worktree's shell `PATH`, verified with `--help`.

  To force the download path in the spike, set
  `"lsp": { "rust-analyzer": { "binary": { "ignore_system_version": true } } }`
  (`lsp_store.rs:880-886`). This devbox has `~/.cargo/bin/rust-analyzer`, so without it the
  download path may not be exercised.
- **`fetch_latest_server_version`** (`811-835`) calls `latest_github_release("rust-lang/rust-analyzer", …)`.
  That's `https://api.github.com/repos/rust-lang/rust-analyzer/releases`
  (`crates/http_client/src/github.rs:9, 51-57`). It then picks the asset's `browser_download_url`
  (GitHub release assets).
  - **There's no Zed-hosted indirection.**
  - The request is unauthenticated unless `GITHUB_TOKEN` is set in the environment
    (`github.rs:143-152`). The 60/h rate limit applies.
- **`fetch_server_binary`** (`837-915`) downloads the `.gz`, writes metadata, and caches it under
  `languages_dir()/rust-analyzer/`.

---

## 4. The `paths` crate

How it works (`crates/paths/src/paths.rs`):

- `pub const APP_NAME: &str = "Zed"` (`18`). Its doc comment says "Forks should change this".
- `APP_NAME_LOWERCASE` is derived in a const block (`22-47`).
- Each directory is a `OnceLock` computed on first call. On macOS:

| Function | macOS path today | Line |
|---|---|---|
| `config_dir()` | `~/.config/zed` (`home_dir()/.config/APP_NAME_LOWERCASE`) | 122-141 |
| `data_dir()` | `~/Library/Application Support/Zed` | 144-167 |
| `state_dir()` | `~/.local/state/Zed` | 169-190 |
| `temp_dir()` | `dirs::cache_dir()/Zed` = `~/Library/Caches/Zed` | 193-219 |
| `logs_dir()` | `~/Library/Logs/Zed` | 228-237 |
| `log_file()` / `old_log_file()` | `logs_dir()/Zed.log`, `Zed.log.old` | 246-255 |
| everything else | under `config_dir()` or `data_dir()`, e.g. `database_dir` = data/db (258), `extensions_dir` (348), `languages_dir` (441), `themes_dir` = config/themes (372), `settings_file`, `keymap_file`, `global_settings_file` (278-299) | |

`set_custom_data_dir` (`103-119`) **doesn't** isolate Lo:

- `config_dir` becomes `custom/config` and `data_dir` becomes `custom`.
- On macOS, `logs_dir` (`231-232`), `temp_dir` (`196-199`) and `state_dir` (`172-173`) ignore it
  and stay in Zed's folders.

### Smallest patch (recommended)

One commit on the patch branch:

```diff
--- a/crates/paths/src/paths.rs
+++ b/crates/paths/src/paths.rs
@@ -15,7 +15,7 @@
 /// Forks should change this to avoid colliding with Zed's user data.
-pub const APP_NAME: &str = "Zed";
+pub const APP_NAME: &str = "Lo";
```

The resulting macOS paths are exactly the four requested:

- `~/.config/lo`
- `~/Library/Application Support/Lo`
- `~/Library/Caches/Lo`
- `~/Library/Logs/Lo`, with `Lo.log` inside

It also moves `~/.local/state/Lo`. Nothing else in Lo's crate set reads `APP_NAME`.
`crates/zed/src/main.rs:10-16` asserts that `APP_NAME_LOWERCASE` equals the binary name, but Lo
doesn't build the `zed` crate.

**Alternative, for upstreaming:** a settable name. Replace the const with
`static APP_NAME: OnceLock<&'static str>` plus `pub fn set_app_name(&'static str)`, panicking if any
directory `OnceLock` is already set, the same way `set_custom_data_dir` does at `104-106`. That also
means:

- turning `APP_NAME` and `APP_NAME_LOWERCASE` into functions
- rewriting `GLOBAL_AGENTS_FILE_DISPLAY`, which uses `const_format::concatcp!` (`338-343`)
- updating the const assert in `zed/src/main.rs:10-16`

It's a much bigger diff and more rebase churn. Use it only to send upstream.

### Call sites in the spike's crate set

Every one follows the constant automatically.

| Function | Call sites |
|---|---|
| `config_dir` | `crates/zed_credentials_provider/src/zed_credentials_provider.rs:131` (dev credentials file); `crates/project/src/git_store.rs:62` (import) |
| `data_dir` | `crates/node_runtime/src/node_runtime.rs:639, 1033` (Node install); `crates/project/src/debugger/session.rs:3292` |
| `database_dir` | `crates/db/src/db.rs:11, 64` (`AppDatabase::new`) |
| `languages_dir` | set by Lo via `set_language_server_download_dir` (Zed does it at `main.rs:530`) |
| `extensions_dir` | `crates/extension_host/src/extension_host.rs:329` |
| `default_prettier_dir` | `crates/prettier/src/prettier.rs:9`; `crates/project/src/prettier_store.rs:23` |
| `debug_adapters_dir` | `crates/dap/src/adapters.rs:280`; `crates/project/src/debugger/dap_store.rs:203`; `session.rs:3332` |
| `external_agents_dir` | `crates/project/src/agent_server_store.rs:403, 1387`; `agent_registry_store.rs:627` |
| `logs_dir` | `crates/client/src/telemetry.rs:285` (`telemetry.log`, created by `Telemetry::new` at `:214-224`); Lo's own `zlog` setup |
| `temp_dir` | `crates/fs/src/fs.rs:988, 1013` |
| `settings_file` | `crates/settings/src/settings_store.rs:371, 555, 581, 1766, 1770`; `agent_settings.rs:2118-2227`; `json_schema_store.rs:432` |
| `global_settings_file` | `settings_store.rs:376` |
| `keymap_file` | `crates/settings/src/keymap_file.rs:878`; `json_schema_store.rs:442` |
| `tasks_file` / `debug_scenarios_file` | `crates/project/src/project_settings.rs:953-1020`; `json_schema_store.rs:447, 454` |
| `snippets_dir` | `crates/snippet_provider/src/lib.rs:155`; `json_schema_store.rs:462` |
| `agents_file` | `crates/agent_settings/src/user_agents_md.rs:114-240` (unused by Lo) |
| per-project names (not app-named) | `.zed` via `local_settings_folder_name` (`486-489`) in `crates/worktree/src/worktree.rs:42` and `settings_store.rs:13`. PR checkouts' `.zed/settings.json` is still read. See section 3 on trust. |

Other "Zed" identities that don't affect folders:

- `ReleaseChannel::app_id()` = `dev.zed.Zed` (`crates/release_channel/src/lib.rs:228-235`). Only
  the `zed` crate and unlinked UI crates use it.
- The Keychain key is `server_url` (`client.rs:366-374`). It's only used when signing in.
- The db scope directory is `0-stable` (`db.rs:164-168`).

---

## 5. Cargo

### Workspace inheritance: confirmed, with a required `exclude`

Experiment in `cargo-mock/`: a Lo-like root with `crates/app` depending by path on
`third_party/zed/crates/{a,b}`. `a` uses `edition.workspace`, `publish.workspace` and
`b.workspace = true` (path + `features=["fancy"]`). Toolchain 1.98.1, offline.

- **Without `exclude`:** `error inheriting 'edition' from workspace root manifest's 'workspace.package.edition'`.
  Cargo makes path dependencies inside the workspace directory **members of Lo's workspace**, so
  they inherit from Lo's root.
- **With `exclude = ["third_party/zed"]`:** it builds. `b` came through with the `fancy` feature,
  and `cargo metadata` lists only `app` as a member. Inheritance resolves against **Zed's root**,
  the nearest ancestor `[workspace]`.
- In the same run, Zed's `[profile.dev] opt-level = 1` and `.cargo/config.toml` `[env]` were
  **ignored silently**. I didn't test `[patch]`; it's documented and the handoff confirms it.

### What Lo's root must copy

From `Cargo.toml` and `.cargo/config.toml` at the Zed root:

```toml
# lo/Cargo.toml
[workspace]
resolver = "2"                      # match Zed (Cargo.toml:2)
members = ["crates/*"]
exclude = ["third_party/zed"]       # required, see above

[workspace.package]
edition = "2024"                    # Zed crates are edition 2024 (Cargo.toml:277)

[patch.crates-io]                   # copy of Cargo.toml:1033-1064, with scratch's path rewritten
cpal = { git = "https://github.com/RustAudio/cpal", rev = "e1612d5d98152f8dc2a62e1b51ef7cbf4f7f26b7" }
tree-sitter-language = { git = "https://github.com/tree-sitter/tree-sitter", rev = "803c6ebaac27d69e1808b45058482e868d073ceb" }
async-process = { git = "https://github.com/zed-industries/async-process.git", rev = "0b6d6713570af61806e1e5cb40e0f757cb93fd9d" }
async-task = { git = "https://github.com/zed-industries/async-task.git", rev = "015845a19fc31752c02367567d3b8900a406f9da" }
windows-capture = { git = "https://github.com/zed-industries/windows-capture.git", rev = "f0d6c1b6691db75461b732f6d5ff56eed002eeb9" }
calloop = { git = "https://github.com/zed-industries/calloop", rev = "3759371fee14c40066d64777e2a36b6ffcc22590" }
livekit = { git = "https://github.com/zed-industries/livekit-rust-sdks", rev = "0a1c519cfce9b365229026b55de9b9dbdb6fed3c" }
libwebrtc = { git = "https://github.com/zed-industries/livekit-rust-sdks", rev = "0a1c519cfce9b365229026b55de9b9dbdb6fed3c" }
notify = { git = "https://github.com/zed-industries/notify", rev = "d842f16b2716bd60f09caf3ae3a894237ab38f54" }
notify-types = { git = "https://github.com/zed-industries/notify", rev = "d842f16b2716bd60f09caf3ae3a894237ab38f54" }
webrtc-sys = { git = "https://github.com/zed-industries/livekit-rust-sdks", rev = "0a1c519cfce9b365229026b55de9b9dbdb6fed3c" }
khronos-egl = { git = "https://github.com/zed-industries/khronos-egl", rev = "42214b36347b108fd9630ca6254b96dde59e04fe" }
crash-context = { git = "https://github.com/rust-minidump/minidump-writer", rev = "911b04e09b79b736cc15cbe712e83e2d3f348bb9" }
scratch = { path = "third_party/zed/tooling/corgi/patches/scratch" }
```

Which patches matter, from reverse dependencies in `Cargo.lock` cross-checked against normal deps:

- **Correctness on macOS:**
  - `tree-sitter-language`: unifies `LanguageFn` with the git tree-sitter. Without it, grammars
    are type-incompatible (comment at `Cargo.toml:1035-1037`).
  - `async-task` and `async-process`: gpui and smol.
  - `notify` and `notify-types`: `fs`.
- **Resolution only:** `calloop`, `khronos-egl` and `windows-capture` are Linux and Windows deps of
  `gpui_platform`. Cargo resolves every target into the lockfile.
- **Unused by the spike:** `cpal`, `livekit`, `libwebrtc`, `webrtc-sys`, `scratch` and
  `crash-context` only arrive through dev-deps such as vim's `settings_ui` dev-dep or
  file_finder's `remote_server` dev-dep. Cargo will warn "patch … was not used".

A sync script that copies the whole block verbatim, rewriting `path =` entries to
`third_party/zed/…`, is simplest. Inferred: unused git patches may still be fetched.

Profiles are performance-only, not needed for correctness, but worth copying (`Cargo.toml:1066-1159`):

- `[profile.dev]`: `split-debuginfo = "unpacked"`, `incremental`, `codegen-units = 16`,
  `debug = "limited"`, plus a matching `build-override`.
- `[profile.dev.package]`: `opt-level = 3` for proc macros (`gpui_macros`, `syn`, `quote`, …),
  `tree-sitter`, `taffy`, `resvg`, `wasmtime`, `cranelift-codegen` and `serde_json`. Debug
  tree-sitter and wasmtime are slow.
- `[profile.release]`: `lto = "thin"`, `codegen-units = 1`, `debug = "limited"`.
- `[profile.dbg]` and `[profile.release-fast]`.
- Entries for packages that aren't in the graph produce warnings, so prune them.

```toml
# lo/.cargo/config.toml (copy of Zed's .cargo/config.toml; the Windows and Linux sections are optional)
[target.'cfg(all())']
rustflags = ["-C", "symbol-mangling-version=v0", "--cfg", "tokio_unstable"]
[env]
MACOSX_DEPLOYMENT_TARGET = "12.0"
```

- `tokio_unstable`: no Zed source references it. Keep it so tokio's API matches Zed's builds.
- `symbol-mangling-version=v0`: only for better backtraces.
- The deployment target matches gpui's Metal build. `-mmacosx-version-min=12.0` is hardcoded at
  `crates/gpui_apple/build.rs:142`, and `crates/zed/build.rs:27` sets the same value.

### Also needed

- **Toolchain.** `rust-toolchain.toml` with `channel = "1.98.1"`, matching Zed's file. The
  `wasm32-wasip2` target is only for building extensions.
- **`Cargo.lock`.** Lo's lockfile replaces Zed's, so registry deps would resolve to the newest
  semver-compatible versions, which Zed never tested. **Seed it**: copy
  `third_party/zed/Cargo.lock` to Lo's root before the first build and let cargo prune it.
  Inferred: cargo keeps the locked versions for packages still in the graph. Make the sync script
  re-seed it on every Zed bump.
- **Xcode, not just the Command Line Tools.** `gpui_apple/build.rs:128-200` runs
  `xcrun -sdk macosx metal` and `metallib`. This machine has
  `xcode-select -p` = `/Applications/Xcode.app/...` and `metal` 32023.922. The
  `gpui_platform/runtime_shaders` feature skips build-time Metal compilation (`gpui_apple/Cargo.toml:19`).
  Zed's docs require the Xcode 26 SDK for bundling (`docs/src/development/macos.md`).
- **cmake: required.** `language` enables `tree-sitter = { features = ["wasm"] }`
  (`crates/language/Cargo.toml:76`; also multi_buffer, languages and migrator). Tree-sitter's wasm
  feature depends on `wasmtime-c-api-impl` 48.0.7, whose `build.rs` runs `cmake` (I read the cached
  registry source). This happens **with or without `extension_host`**. cmake is on this machine
  through mise.
- **protoc: not needed.** `crates/proto/build.rs` uses `protox`, a pure-Rust compiler.
- **Required features:**
  - `gpui_platform = { …, features = ["font-kit"] }`. Without it gpui_macos logs "no text will be
    rendered" (`crates/gpui_macos/src/platform.rs:220-228`). Only dev-deps and examples enable it in
    Zed's graph, so Lo must.
  - `languages = { …, features = ["load-grammars"] }`.
  - `util = { …, features = ["debug-embed"] }`, for dev builds (section 7).
- **Binary `build.rs`.** Inferred minimum: `cargo:rustc-env=MACOSX_DEPLOYMENT_TARGET=12.0`.
  Zed's binary also links with `-Wl,-ObjC`, `-rpath /usr/lib/swift` and weak `ReplayKit` and
  `ScreenCaptureKit` (`crates/zed/build.rs:26-37`). The frameworks are only for `screen-capture`.
  `-ObjC` is harmless to copy. No gpui crate emits link args.
- **Env vars.** `ZED_RELEASE_CHANNEL`, if set at build time, overrides
  `crates/zed/RELEASE_CHANNEL` (`stable` at this tag; `release_channel/build.rs`,
  `release_channel/src/lib.rs:13-33`). Debug builds also read it at runtime. Leave it unset.
- **Name collisions.** None between Lo's planned crate names and Zed's. Never add the crates.io
  `gpui` alongside the path one.

---

## 6. Crate list

### Direct dependencies for the spike

All by path into `third_party/zed/crates/...`:

- **UI and app:** `gpui`, `gpui_platform` (`font-kit`), `gpui_tokio`, `ui`, `theme`,
  `theme_settings`, `settings`, `assets`
- **Editor:** `editor`, `multi_buffer`, `buffer_diff`, `language`, `languages` (`load-grammars`),
  `text` and `rope` (or their re-exports through `language`)
- **Project and workspace:** `project`, `workspace`, `client`, `clock`, `node_runtime`,
  `http_client`, `reqwest_client`, `fs`, `db`, `session`, `release_channel`, `paths`
- **Support:** `util` (`debug-embed`), `zlog`, `menu`, `zed_actions`, `feature_flags`
- **Optional for the spike:** `git_hosting_providers`, `vim`, `command_palette`, `file_finder`,
  `search`
- **Later:** `markdown`, `extension_host`, `language_extension`, `theme_extension`

### Transitive Zed crates

Computed with `zed_deps.py`, a feature-aware resolver over the manifests: macOS target, normal deps
only, with features from the dependency specs above. The lockfile closure, which includes
dev-deps and other platforms, is a superset. For example the lockfile shows `editor` alone at 112
crates; the resolver says 96.

- **Minimal set (no vim, palette, finder or markdown): 104 Zed crates.**
  agent_settings askpass assets breadcrumbs buffer_diff client clock cloud_api_client cloud_api_types
  cloud_llm_client collections component context_server credentials_provider dap db derive_refineable
  edit_prediction_types editor env_var extension feature_flags feature_flags_macros file_content
  file_icons fs fuzzy fuzzy_nucleo git git_hosting_providers gpui gpui_apple gpui_macos gpui_macros
  gpui_platform gpui_shared_string gpui_tokio gpui_util grammars http_client http_client_tls icons
  json_schema_store language language_core language_detection language_model language_model_core
  languages lsp markdown menu mermaid_render migrator multi_buffer net node_runtime
  oauth_callback_server path paths prettier project proto proxy_handshake refineable release_channel
  remote reqwest_client rope rpc scheduler session settings settings_content settings_json
  settings_macros snippet snippet_provider sqlez sqlez_macros sum_tree syntax_theme task telemetry
  telemetry_events terminal text theme theme_settings ui ui_input ui_macros util vim_mode_setting
  watch workspace worktree zed_actions zed_credentials_provider zed_env_vars zeta_prompt zlog
  ztracing ztracing_macro
- **Adding vim, command_palette, file_finder and markdown: +9** = `channel` (from file_finder),
  `command_palette`, `command_palette_hooks`, `file_finder`, `open_path_prompt`, `picker`,
  `picker_preview`, `search` (from vim), `vim`.
- **Adding extension_host, language_extension and theme_extension: +3.** wasmtime is already in
  through tree-sitter's wasm feature.
- **Adding `git_ui`: +8,** including **`auto_update`** (through `remote_connection`), `prompt_store`,
  `notifications` and `agent_skills`. Avoid it.

### What comes along that design §2 wants left out

Chains are from the resolver's `--why`.

| Crate(s) | Pulled by | Avoidable? |
|---|---|---|
| `client`, `cloud_api_client`, `cloud_api_types`, `cloud_llm_client`, `rpc`, `proto`, `telemetry`, `telemetry_events`, `zed_credentials_provider`, `zeta_prompt` | `editor`, `workspace` and `project` all depend on `client`; `zeta_prompt` comes via `cloud_api_types`. `AppState` and `Project::local` take an `Arc<Client>`. | **No.** Keep it inert as in section 1. |
| `agent_settings`, `language_model`, `language_model_core` | `workspace` → `agent_settings`. `MultiWorkspace::new` reads `AgentSettings`. | **No.** Settings types only. |
| `edit_prediction_types` | `editor` | No. Types only. |
| `dap` (debug adapter protocol), plus the `DapStore` inside `Project` | `editor`; `project` | No. No `debugger_ui`, nothing starts. |
| `terminal` (alacritty_terminal backend) | `languages` → `terminal` | No. No `terminal_view`. |
| `context_server`, `oauth_callback_server` (MCP) | `project` | No. Nothing starts without MCP settings. |
| `remote` (SSH and WSL remoting), `prettier`, `extension` | `project` | No |
| `channel` (collab channels store) | `file_finder` (uses `ChannelStore::try_global`, `crates/file_finder/src/file_finder.rs:1032`) | Only by dropping file_finder. Not initialized, so inert. |
| **wasmtime** + cranelift (registry) | `language` → `tree-sitter/wasm` → `wasmtime-c-api-impl` | **No** without a patch. Large compile cost and needs cmake. |
| **Not in the graph:** `auto_update`, `call`, `collab_ui`, `livekit*`, `audio`, `agent`, `agent_ui`, `copilot`, `edit_prediction`, `terminal_view`, `debugger_ui`, `extensions_ui`, `title_bar`, `crashes`, `onboarding`, `git_ui`, `git_ui_core` | n/a | |

---

## 7. Anything else that trips an implementer

### Dev builds read assets from the repo checkout

`util::fs_embed!` (`crates/util/src/util.rs:780-886`) embeds files in release builds. In **debug
builds** it reads them at runtime from `dev_repo_root()/root_relative`.

- `dev_repo_root()` (`util.rs:681-701`) takes the first ancestor of `current_exe()` (then
  `current_dir()`) that contains `.git`. For `lo/target/debug/lo`, that's **Lo's root**.
- These lookups would miss:
  - `SettingsAssets` (`assets/settings/*`, `assets/keymaps/*`; `crates/settings/src/settings.rs:138-144`)
  - `assets::Assets` (fonts, icons, themes, images, sounds; `crates/assets/src/assets.rs:10-24`)
  - `grammars::GrammarDir` (`crates/grammars/src`; `grammars.rs:8-13`)
- `grammars::load_config` then panics with `missing config for language` (`grammars.rs:49-60`).
  `settings::default_settings()` fails too.

**Fix:** depend on `util` with `features = ["debug-embed"]` (`crates/util/Cargo.toml:19-20`). It
switches every `fs_embed!` to compile-time `RustEmbed` (`util.rs:759-778`). Folders resolve relative
to each Zed crate's manifest, so `third_party/zed/assets` is found. The alternative, symlinking
`assets/` and `crates/grammars/src` into Lo's root, is fragile.

### Required global state

Set before the first window opens. The ones that panic when missing are marked "panic".

- `db::AppDatabase` (panic in `WorkspaceDb::global`)
- `SettingsStore` from `settings::init` (panic)
- `GlobalAppVersion` and `GlobalReleaseChannel` from `release_channel::init` (panic in `ReleaseChannel::global`)
- Tokio from `gpui_tokio::init`
- the gpui HTTP client from `cx.set_http_client`
- `dyn Fs` (panic in `<dyn Fs>::global`)
- theme globals (`ThemeRegistry`, `GlobalTheme`, `SystemAppearance`, `FontFamilyCache`) from `theme_settings::init` (panic)
- `AppState` (panic in `AppState::global`, e.g. on the `NewFile` action)
- `ExtensionHostProxy` and the connection `Manager` from `Project::init`
- `HistoryManager` from `workspace::init`
- `VimGlobals` from `vim::init`
- `CommandPaletteFilter` from `command_palette::init`
- `PaneSearchBarCallbacks` (optional)
- `GitHostingProviderRegistry` (only if you call `git_hosting_providers::init`)
- `LanguageRegistry::set_theme`, which isn't a global but is needed for highlight colours
- `TrustedWorktrees`: **leave it unset** (section 3)

### Assets: themes, fonts, icons

- **Embedding.** `assets::Assets` embeds `assets/{fonts,icons,images,themes,sounds,prompts}` and
  `*.md`. Settings and keymaps are a separate embed, `settings::SettingsAssets`. The whole `assets/`
  directory is about 4.6 MB: fonts 1.6 MB, icons 1.6 MB, sounds 680 KB.
- **Registering.** `Application::with_assets(assets::Assets)` (`crates/gpui/src/app.rs:233`)
  registers it as the `AssetSource`. Icons (`ui::IconName` → `icons/*.svg`) and file icons go
  through it.
- **Fonts** need an explicit `Assets.load_fonts(cx)`. Zed calls `load_embedded_fonts`
  (`main.rs:727, 1826-1848`).
- **Themes:** `theme_settings::init(LoadThemes::All(Box::new(Assets)))` loads the bundled themes
  (`theme_settings.rs:71-80`). Defaults are the `One` themes and the "Zed (Default)" icon theme
  (`default.json:9-14`).
- **Lo's own assets.** `with_assets` takes one source, so write a small `AssetSource` that checks
  Lo's embedded files first and falls back to `assets::Assets`. That's inferred, but it's the
  usual gpui pattern.

### Keymaps

The `zed` crate loads keymaps (`crates/zed/src/zed.rs:2348-2381`, `load_default_keymap`), in this order:

1. `keymaps/default-macos.json`
2. the base keymap asset (none for `BaseKeymap::Zed`; `crates/settings/src/base_keymap_setting.rs:98-110`)
3. `keymaps/vim.json` if `vim_mode` is on
4. `keymaps/specific-overrides-macos.json`

**Gotcha:**

- `KeymapFile::load_asset` **returns `Err` if any binding names an unregistered action**
  (`crates/settings/src/keymap_file.rs:180-202`; `NotFound` at `565-569`). Zed `.unwrap()`s it.
- Lo doesn't link agent, terminal, collab, debugger and similar crates, so their actions in the
  default keymap aren't registered and the load fails.
- Use **`KeymapFile::load_asset_allow_partial_failure`** (`204-222`). It drops unknown actions and
  returns the rest. Log its error once.
- Later, Lo should ship its own keymap.

Reloading keymaps when `vim_mode` or `base_keymap` changes is done by `handle_keymap_file_changes`
in the `zed` crate (`zed.rs:2132-2160`). Copy that observer if Lo lets users toggle vim at
runtime.

### Turning on vim mode

- `vim::init(cx)` (`crates/vim/src/vim.rs:286-320`) registers `VimGlobals` and a
  `cx.observe_new(Vim::register)` hook on every new `Editor` (`vim.rs:620-660`).
- Vim activates when `VimModeSetting` is true. That's the `"vim_mode"` setting
  (`default.json:144`, from the `vim_mode_setting` crate), and the editor must allow modal editing.
- The bindings come from `keymaps/vim.json`, so load it as above.
- `ToggleVimMode` writes `vim_mode` to the user settings file (`vim.rs:292-301`).
- Vim's `/` search needs the buffer search bar, so set `PaneSearchBarCallbacks` and call
  `search::init`.
- Both sides of `SplittableEditor` are full `Editor`s, so both get vim. The buffers are read-only,
  so insert-mode edits are refused.

### `release_channel`

- The channel name comes from `include_str!("../../zed/RELEASE_CHANNEL")`. That's `stable` at this
  tag, so Lo builds are "stable".
- `ZED_RELEASE_CHANNEL` overrides it at build time, and in debug builds also at runtime
  (`crates/release_channel/src/lib.rs:13-33`; `build.rs`).
- Effects:
  - db scope `0-stable`
  - `AppVersion` build metadata
  - settings `"stable": {...}` overrides
  - `zed_credentials_provider` uses a dev file instead of the Keychain only on `Dev`
- `release_channel::init` must run, because `ReleaseChannel::global` panics otherwise.
- `feature_flags` turns on **all staff-only flags in debug builds** unless `ZED_DISABLE_STAFF=1`
  (`crates/feature_flags/src/store.rs:106, 180, 252`; `feature_flags.rs:20-22`). Debug and release
  behaviour can differ. Set it in the dev environment, or call `std::env::set_var` at the top of
  `main` (it's `unsafe` in edition 2024).

### Smaller items

- **Late language load.** Copy the project buffer's language onto the left side, as
  `file_diff_view.rs:150-166` does.
- **Split style** follows the global `diff_view_style` setting (`split.rs:660-666`), and windows
  narrower than 100 columns unsplit (`default.json:426`).
- **`Client::production` resets the base URL.** In Zed, an observer resets `http.base_url` from
  `server_url` on every settings change (`main.rs:818-824`). Lo doesn't copy that observer.
- **Not a TTY.** When launched as a `.app`, call `util::load_login_shell_environment()`
  (`main.rs:443-454`). Otherwise `git`, `rustup`, `rust-analyzer` and `node` aren't on `PATH`.
  Language-server binary lookup also uses the worktree's shell env.
- **Shell environment.** `ProjectEnvironment` captures the user's login-shell environment for each
  worktree directory. Inferred: that runs shell hooks such as direnv in the PR checkout. One more
  reason to keep local projects to trusted repos.
- **Telemetry log file.** `Telemetry::new` creates `~/Library/Logs/Lo/telemetry.log` locally
  (`telemetry.rs:214-224`). It's harmless, but it means "telemetry" shows up on disk.
- **Disk state.** `Workspace::new(None, …)`, with no `WorkspaceId`, keeps the workspace out of the
  db. `Workspace::new_local` would restore Zed-style session state.
- **Reference implementation.** `crates/component_preview/examples/component_preview.rs` is a
  complete minimal non-`zed` startup that already compiles in Zed's tree: `AppDatabase`, fonts,
  reqwest, `RealFs`, settings, theme, `Client::production`, stores, `Session`, `AppState`,
  `workspace::init`, `editor::init`, a window with `Project::local(init_worktree_trust: false)` and
  `Workspace::new`. Start from it, swapping `Client::production` for `Client::new` and the filtered
  client.
