//! rozed, a read-only GitHub pull request reviewer for macOS, built on Zed's crates.
//!
//! Startup follows Zed's own (`third_party/zed/crates/zed/src/main.rs`) without the parts rozed
//! leaves out: no Zed account, telemetry, auto-update, extensions, AI or collaboration.

mod forced_settings;
mod github_api;
mod http;
mod local_copy;
mod pr_diff;
mod pr_link;

use std::io::IsTerminal as _;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context as _;
use assets::Assets;
use client::{Client, UserStore};
use db::kvp::KeyValueStore;
use editor::{Editor, MultiBuffer};
use fs::RealFs;
use gpui::{
    App, AppContext as _, Bounds, Context, Menu, MenuItem, TaskExt as _, TitlebarOptions, Window,
    WindowBounds, WindowOptions, px, size,
};
use http_client::{HttpClient, HttpClientWithUrl};
use language::{Capability, LanguageRegistry};
use node_runtime::NodeRuntime;
use project::Project;
use release_channel::AppVersion;
use reqwest_client::ReqwestClient;
use session::{AppSession, Session};
use settings::{KeybindSource, KeymapFile, KeymapFileLoadResult, Settings as _, SettingsAssets};
use theme::ActiveTheme as _;
use util::ResultExt as _;
use workspace::notifications::NotifyTaskExt as _;
use workspace::{AppState, MultiWorkspace, Workspace, WorkspaceStore};

use crate::http::{ZED_SERVICES_PLACEHOLDER, ZedHostGuard};
use crate::pr_link::PrLink;

/// Set while rozed runs, so a rozed started by its own shell-environment capture can tell.
const SHELL_ENV_GUARD: &str = "ROZED_PARENT_PID";

const USAGE: &str = "usage: rozed [<pull request link>]
example: rozed https://github.com/sharkdp/fd/pull/2212";

fn main() {
    let launched = Instant::now();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") => {
            println!("rozed {}", version());
            return;
        }
        // Zed's crates learn a shell's environment by running this executable with `--printenv`
        // inside it: at startup (below) and for each project folder.
        Some("--printenv") => {
            util::shell_env::print_env();
            return;
        }
        _ => {}
    }
    let pull_request = match parse_args(&args) {
        Ok(pull_request) => pull_request,
        Err(error) => {
            eprintln!("rozed: {error}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    // If `--printenv` ever stops being handled above, every capture would start another rozed,
    // which would capture again. Refuse to start inside a running rozed instead.
    if let Some(parent) = std::env::var_os(SHELL_ENV_GUARD) {
        eprintln!(
            "rozed: not starting, because rozed (pid {}) started this process",
            parent.to_string_lossy()
        );
        std::process::exit(1);
    }

    // SAFETY: no other thread exists yet, so nothing reads the environment concurrently.
    unsafe {
        std::env::set_var(SHELL_ENV_GUARD, std::process::id().to_string());
        // Debug builds of Zed's crates turn on every staff-only feature flag unless this is set.
        // Set it so debug builds behave like release ones.
        std::env::set_var("ZED_DISABLE_STAFF", "1");
    }

    if let Err(error) = create_dirs() {
        eprintln!("rozed: {error:#}");
        std::process::exit(1);
    }
    init_logging();
    log::info!("starting rozed {}", version());
    util::increase_open_file_limit().log_err();

    let app = gpui_platform::application().with_assets(Assets);
    let app_db = db::AppDatabase::new();
    let session = app.background_executor().spawn(Session::new(
        uuid::Uuid::new_v4().to_string(),
        KeyValueStore::from_app_db(&app_db),
    ));
    let fs = RealFs::new(None, app.background_executor());
    if !std::io::stdout().is_terminal() {
        // Started from Finder rather than a shell: load the login shell's environment, so git
        // and language servers are on PATH.
        app.background_executor()
            .spawn(async { util::load_login_shell_environment().await.log_err() })
            .detach();
    }

    app.run(move |cx| {
        cx.set_global(app_db);
        release_channel::init(AppVersion::load(version(), None, None), cx);
        gpui_tokio::init(cx);
        settings::init(cx);
        forced_settings::apply(cx);

        let http = http_client(cx);
        cx.set_http_client(http.clone());
        <dyn fs::Fs>::set_global(fs.clone(), cx);

        // rozed never signs in to Zed, so this client never connects. It's here because Zed's
        // workspace and project take one.
        let client = Client::new(
            Arc::new(clock::RealSystemClock),
            Arc::new(HttpClientWithUrl::new_url(
                http.clone(),
                format!("https://{ZED_SERVICES_PLACEHOLDER}"),
                http.proxy().cloned(),
            )),
            cx,
        );

        let mut languages = LanguageRegistry::new(cx.background_executor().clone());
        languages.set_language_server_download_dir(paths::languages_dir().clone());
        let languages = Arc::new(languages);
        let node_runtime = NodeRuntime::unavailable();
        languages::init(languages.clone(), fs.clone(), node_runtime.clone(), cx);

        let user_store = cx.new(|cx| UserStore::new(client.clone(), cx));
        let workspace_store = cx.new(|cx| WorkspaceStore::new(client.clone(), cx));
        Project::init(&client, cx);
        feature_flags::FeatureFlagStore::init(cx);

        let session = cx.foreground_executor().block_on(session);
        let app_state = Arc::new(AppState {
            languages,
            client,
            user_store,
            workspace_store,
            fs,
            build_window_options: window_options,
            node_runtime,
            session: cx.new(|cx| AppSession::new(session, cx)),
        });
        AppState::set_global(app_state.clone(), cx);

        theme_settings::init(theme::LoadThemes::All(Box::new(Assets)), cx);
        Assets
            .load_fonts(cx)
            .expect("loading rozed's embedded fonts");
        app_state.languages.set_theme(cx.theme().clone());
        cx.observe_global::<theme::GlobalTheme>({
            let languages = app_state.languages.clone();
            move |cx| languages.set_theme(cx.theme().clone())
        })
        .detach();

        editor::init(cx);
        workspace::init(app_state.clone(), cx);

        cx.on_action(|_: &zed_actions::Quit, cx| cx.quit());
        cx.set_menus([
            Menu::new("rozed").items([MenuItem::action("Quit rozed", zed_actions::Quit)])
        ]);
        load_keymaps(cx);
        // Closing the last window quits.
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        if let Err(error) = open_window(app_state, pull_request, launched, cx) {
            log::error!("{error:#}");
            cx.quit();
        }
        cx.activate(true);
    });
}

/// rozed's version, from its Cargo manifest.
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Reads the command line after the program name: nothing, or one pull request link.
fn parse_args(args: &[String]) -> anyhow::Result<Option<PrLink>> {
    match args {
        [] => Ok(None),
        [link] => link
            .parse()
            .map(Some)
            .with_context(|| format!("can't open `{link}`")),
        _ => anyhow::bail!(
            "expected one pull request link, got {} arguments",
            args.len()
        ),
    }
}

/// Creates rozed's folders. Zed's `paths` crate, patched to use the name rozed, puts them in
/// `~/.config/rozed`, `~/Library/Application Support/rozed`, `~/Library/Caches/rozed` and
/// `~/Library/Logs/rozed`.
fn create_dirs() -> anyhow::Result<()> {
    for dir in [
        paths::config_dir(),
        paths::data_dir(),
        paths::database_dir(),
        paths::languages_dir(),
        paths::logs_dir(),
        paths::temp_dir(),
    ] {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    Ok(())
}

/// Logs to `~/Library/Logs/rozed/rozed.log`, and to stdout too when it's a terminal.
/// `RUST_LOG` sets the levels, e.g. `RUST_LOG=rozed=debug` to log every HTTP request's host.
fn init_logging() {
    zlog::init();
    if let Err(error) = zlog::init_output_file(paths::log_file(), Some(paths::old_log_file())) {
        eprintln!("rozed: can't open {}: {error}", paths::log_file().display());
        zlog::init_output_stderr();
    }
    if std::io::stdout().is_terminal() {
        zlog::init_output_stdout();
    }
}

/// The one HTTP client for everything, which refuses Zed's hosts.
fn http_client(cx: &mut App) -> Arc<dyn HttpClient> {
    let user_agent = format!(
        "rozed/{} ({}; {})",
        version(),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let proxy = client::ProxySettings::get_global(cx).proxy_url();
    let reqwest = {
        // Build it on gpui's Tokio runtime rather than letting it start its own.
        let _guard = gpui_tokio::Tokio::handle(cx).enter();
        ReqwestClient::proxy_and_user_agent(proxy, &user_agent).expect("starting the HTTP client")
    };
    Arc::new(ZedHostGuard::new(Arc::new(reqwest)))
}

/// Loads Zed's default macOS keymap. Bindings for actions from Zed crates rozed doesn't include
/// (the agent, terminal, debugger and so on) are skipped.
fn load_keymaps(cx: &mut App) {
    for path in [
        settings::DEFAULT_KEYMAP_PATH,
        settings::SPECIFIC_OVERRIDES_KEYMAP_PATH,
    ] {
        let content = util::asset_str::<SettingsAssets>(path);
        let mut bindings = match KeymapFile::load(&content, cx) {
            KeymapFileLoadResult::Success { key_bindings } => key_bindings,
            KeymapFileLoadResult::SomeFailedToLoad {
                key_bindings,
                error_message,
            } => {
                log::debug!(
                    "skipped bindings in {path} for actions rozed doesn't have: {error_message}"
                );
                key_bindings
            }
            KeymapFileLoadResult::JsonParseFailure { error } => {
                log::error!("can't parse {path}: {error:#}");
                continue;
            }
        };
        for binding in &mut bindings {
            binding.set_meta(KeybindSource::Default.meta());
        }
        cx.bind_keys(bindings);
    }
}

fn window_options(_display: Option<uuid::Uuid>, cx: &mut App) -> WindowOptions {
    // Wide enough for a side-by-side diff, which Zed's editor turns into one column below about
    // 100 characters a side, but never bigger than the screen.
    let screen = cx
        .primary_display()
        .map(|display| display.bounds().size)
        .unwrap_or(size(px(1600.), px(1000.)));
    let window_size = size(
        px(1600.).min(screen.width * 0.9),
        px(1000.).min(screen.height * 0.9),
    );
    WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("rozed".into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            window_size,
            cx,
        ))),
        ..Default::default()
    }
}

/// Opens rozed's window: a workspace showing the pull request, or without one this file,
/// read-only.
fn open_window(
    app_state: Arc<AppState>,
    pull_request: Option<PrLink>,
    launched: Instant,
    cx: &mut App,
) -> anyhow::Result<()> {
    let mut options = (app_state.build_window_options)(None, cx);
    if let Some(pull_request) = &pull_request {
        log::info!("opening {pull_request}");
        options.titlebar = Some(TitlebarOptions {
            title: Some(format!("{pull_request} - rozed").into()),
            ..Default::default()
        });
    }
    let window = cx
        .open_window(options, |window, cx| {
            theme_settings::setup_ui_font(window, cx);
            let project = Project::local(
                app_state.client.clone(),
                app_state.node_runtime.clone(),
                app_state.user_store.clone(),
                app_state.languages.clone(),
                app_state.fs.clone(),
                None,
                project::LocalProjectFlags {
                    init_worktree_trust: false,
                    watch_global_configs: true,
                },
                cx,
            );
            let workspace =
                cx.new(|cx| Workspace::new(None, project, app_state.clone(), window, cx));
            // Zed roots every window at a MultiWorkspace, and parts of its workspace code look
            // for one.
            cx.new(|cx| MultiWorkspace::new(workspace, window, cx))
        })
        .context("opening rozed's window")?;
    window.update(cx, |multi_workspace, window, cx| {
        let workspace = multi_workspace.workspace().clone();
        match pull_request {
            Some(pull_request) => {
                let weak_workspace = workspace.downgrade();
                window
                    .spawn(cx, async move |cx| {
                        pr_diff::open(weak_workspace, pull_request, launched, cx).await
                    })
                    .detach_and_notify_err(workspace.downgrade(), window, cx);
            }
            None => workspace.update(cx, |workspace, cx| show_source(workspace, window, cx)),
        }
    })?;
    Ok(())
}

/// Shows rozed's own `main.rs` in a read-only editor, highlighted as Rust. Built the way Zed shows
/// its bundled files: a project buffer created with its language, so nothing resets it to plain
/// text.
fn show_source(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let rust = workspace.app_state().languages.language_for_name("Rust");
    let project = workspace.project().clone();
    cx.spawn_in(window, async move |workspace, cx| {
        let rust = rust.await?;
        workspace.update_in(cx, |workspace, window, cx| {
            let buffer = project.update(cx, |project, cx| {
                project.create_local_buffer(include_str!("main.rs"), Some(rust), false, cx)
            });
            buffer.update(cx, |buffer, cx| {
                buffer.set_capability(Capability::ReadOnly, cx)
            });
            let multi_buffer =
                cx.new(|cx| MultiBuffer::singleton(buffer, cx).with_title("main.rs".into()));
            let editor = cx.new(|cx| {
                let mut editor =
                    Editor::for_multibuffer(multi_buffer, Some(project.clone()), window, cx);
                editor.set_read_only(true);
                editor.set_should_serialize(false, cx);
                editor
            });
            workspace.add_item_to_active_pane(Box::new(editor), None, true, window, cx);
        })
    })
    .detach_and_log_err(cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_semver() {
        let parts: Vec<&str> = version().split('.').collect();
        assert_eq!(parts.len(), 3, "{} isn't major.minor.patch", version());
        for part in parts {
            assert!(part.parse::<u64>().is_ok(), "{part} isn't a number");
        }
    }

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn no_arguments_open_no_pull_request() {
        assert_eq!(parse_args(&[]).unwrap(), None);
    }

    #[test]
    fn one_link_opens_that_pull_request() {
        let pull_request = parse_args(&args(&["https://github.com/sharkdp/fd/pull/2212"]))
            .unwrap()
            .unwrap();
        assert_eq!(pull_request.to_string(), "sharkdp/fd#2212");
    }

    #[test]
    fn a_bad_link_says_what_and_why() {
        let error = parse_args(&args(&["https://github.com/sharkdp/fd/issues/1"])).unwrap_err();
        assert_eq!(
            format!("{error:#}"),
            "can't open `https://github.com/sharkdp/fd/issues/1`: \
             it isn't a link to a pull request"
        );
    }

    #[test]
    fn more_than_one_argument_is_an_error() {
        let error = parse_args(&args(&["https://github.com/a/b/pull/1", "x"])).unwrap_err();
        assert!(error.to_string().contains("2 arguments"), "{error}");
    }
}
