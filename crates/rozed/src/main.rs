//! rozed, a read-only GitHub pull request reviewer for macOS, built on Zed's crates.
//!
//! Startup follows Zed's own (`third_party/zed/crates/zed/src/main.rs`) without the parts rozed
//! leaves out: no Zed account, telemetry, auto-update, extensions, AI or collaboration.

mod forced_settings;
mod http;
mod keymap;
mod trust;

use std::io::IsTerminal as _;
use std::sync::Arc;

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
use settings::Settings as _;
use theme::ActiveTheme as _;
use util::ResultExt as _;
use workspace::{AppState, MultiWorkspace, Workspace, WorkspaceStore};

use crate::http::{ZED_SERVICES_PLACEHOLDER, ZedHostGuard};

/// Set while rozed runs, so a rozed started by its own shell-environment capture can tell.
const SHELL_ENV_GUARD: &str = "ROZED_PARENT_PID";

fn main() {
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
        // Zed's askpass crate makes this executable the program git and ssh run to ask for a
        // password, through a script that runs `rozed --askpass=<socket>` with the prompt on
        // stdin. rozed passes the prompt to the running rozed over the socket and prints the
        // answer, as Zed does.
        Some(flag) if flag == "--askpass" || flag.starts_with("--askpass=") => {
            let socket = flag
                .strip_prefix("--askpass=")
                .or(args.get(1).map(String::as_str))
                .filter(|socket| !socket.is_empty());
            let Some(socket) = socket else {
                eprintln!("rozed: --askpass needs a socket: --askpass=<socket>");
                std::process::exit(2);
            };
            askpass::main(socket);
            return;
        }
        Some(flag) if flag.starts_with("--") => {
            eprintln!("rozed: unknown option `{flag}`");
            std::process::exit(2);
        }
        _ => {}
    }
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
        // Zed's client prefers this to the `server_url` setting, which rozed forces.
        std::env::remove_var("ZED_SERVER_URL");
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
        // Before any project exists, so every project tracks trust.
        trust::init(cx);
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
        keymap::load(cx);
        // Closing the last window quits.
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        match open_window(app_state, cx) {
            Ok(()) => cx.activate(true),
            Err(error) => {
                log::error!("{error:#}");
                cx.quit();
            }
        }
    });
}

/// rozed's version, from its Cargo manifest.
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
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
/// `RUST_LOG` sets the levels, e.g. `RUST_LOG=rozed=debug` to log every HTTP request.
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

/// The one HTTP client for everything, which refuses Zed's hosts, redirects included.
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

fn window_options(_display: Option<uuid::Uuid>, cx: &mut App) -> WindowOptions {
    WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("rozed".into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1200.), px(800.)),
            cx,
        ))),
        ..Default::default()
    }
}

/// Opens rozed's window: a workspace showing this file, read-only.
fn open_window(app_state: Arc<AppState>, cx: &mut App) -> anyhow::Result<()> {
    let options = (app_state.build_window_options)(None, cx);
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
                    // Zed's worktree trust: no language server or `.zed/settings.json` until
                    // rozed trusts the worktree (see `trust`).
                    init_worktree_trust: true,
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
        multi_workspace
            .workspace()
            .clone()
            .update(cx, |workspace, cx| show_source(workspace, window, cx));
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
}
