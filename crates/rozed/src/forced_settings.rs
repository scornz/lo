//! Zed settings rozed forces, whatever the user's settings say.
//!
//! They sit in Zed's top settings layer (the "server" layer, merged after the user's), so a
//! user's `settings.json` can't override them, and a repository's `.zed/settings.json` never
//! applies at all ([`review_ui::local_project`]). They keep Zed's account, telemetry, update and AI
//! code from starting, point every Zed service URL at a placeholder that rozed's HTTP client
//! refuses ([`crate::http`]), let only rust-analyzer start, and only with `--trust`, keep Prettier
//! off, and narrow what rust-analyzer does.
//!
//! They don't keep a repository's code from running: rust-analyzer still runs cargo and rustc in
//! the checkout, and a repository's `.cargo/config.toml` can point those at its own programs. So
//! only a pull request the user trusts gets a checkout ([`review_ui::trust`]). Without `--trust`,
//! no language server can start at all, as a second line: Zed counts a worktree that holds a
//! single file, such as one dropped on the window, as trusted. `docs/spike.md`, "Trust risks",
//! lists what the settings stop and what they don't.

use gpui::{App, UpdateGlobal as _};
use language::LanguageName;
use settings::SettingsStore;

/// The forced settings, in the format of Zed's `settings.json`. [`forced_settings`] adds what
/// depends on the machine.
pub const FORCED_SETTINGS: &str = r#"{
  // No telemetry: no usage events and no crash or diagnostics reports.
  "telemetry": { "diagnostics": false, "metrics": false },
  // Every URL Zed's crates build for a Zed service points at a host that doesn't exist.
  "server_url": "https://rozed.invalid",
  "auto_update": false,
  // No AI features. This also keeps the multi-workspace sidebar off.
  "disable_ai": true,
  // Zed's defaults have its extension host install the HTML extension from Zed's servers. rozed
  // doesn't start the extension host; if it ever does, this turns off that one default. Entries
  // in the user's settings still apply.
  "auto_install_extensions": { "html": false },
  // Only rozed decides which worktrees are trusted to run language servers (`review_ui::trust`).
  "session": { "trust_all_worktrees": false },
  // rozed titles its window after the active pull request (`review_ui::window_title`). Zed's
  // title names the project's folders, "empty project" in rozed; this one never changes, so Zed
  // never sets it again over rozed's.
  "window_title_format": "rozed",
  // No inline git blame. In rozed's partial clone, blaming a file downloads each earlier version
  // of it, one request at a time: about 900 for sharkdp/fd#2212.
  "git": { "inline_blame": { "enabled": false } },
  // Only rust-analyzer can start, and only for Rust; no other language gets a language server.
  // Without `--trust`, rozed turns Rust's off too. Prettier never runs: rozed never formats, and
  // Prettier loads a repository's plugins. A language's own entry wins over these defaults, so
  // rozed adds them for every language it knows. Later milestones add servers one at a time.
  "language_servers": [],
  "prettier": { "allowed": false },
  "languages": {
    "Rust": { "language_servers": ["rust-analyzer"], "prettier": { "allowed": false } }
  },
  "lsp": {
    "rust-analyzer": {
      "binary": {
        // Use the rust-analyzer rozed downloads from its GitHub releases, never one found on
        // PATH. With rustup, that one is a proxy that runs, and may first install, whatever
        // toolchain the repository's rust-toolchain.toml names.
        "ignore_system_version": true,
        // The environment of rust-analyzer and the cargo and rustc it runs. No rustc wrapper,
        // whether the repository's .cargo/config.toml or rozed's environment names one, and
        // rustup never installs a toolchain. rozed adds RUSTUP_TOOLCHAIN when it finds a
        // toolchain (`crate::rust_toolchain`); rustup prefers it to a rust-toolchain.toml.
        "env": {
          "RUSTC_WRAPPER": "",
          "RUSTC_WORKSPACE_WRAPPER": "",
          "RUSTUP_AUTO_INSTALL": "0"
        }
      },
      // No build scripts, no proc macros, no `cargo check`, and no `cargo metadata` for
      // dependencies, which would download them.
      "initialization_options": {
        "cargo": { "buildScripts": { "enable": false }, "noDeps": true },
        "procMacro": { "enable": false },
        "checkOnSave": false
      }
    }
  }
}"#;

/// Which language servers can start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Servers<'a> {
    /// None, for any language: rozed wasn't asked to trust a pull request.
    None,
    /// rust-analyzer, for Rust, with rustup pinned to `toolchain` when rozed found one
    /// ([`crate::rust_toolchain`]). Only the trusted pull request's checkout runs it.
    RustAnalyzer { toolchain: Option<&'a str> },
}

/// The forced settings, with no language server or Prettier for any of `languages`, except
/// what `servers` allows for Rust.
pub fn forced_settings(languages: &[LanguageName], servers: Servers) -> String {
    let mut settings: serde_json::Value = settings::parse_json_with_comments(FORCED_SETTINGS)
        .expect("rozed's forced settings are valid");
    let forced_languages = settings["languages"]
        .as_object_mut()
        .expect("the forced settings have a languages object");
    for language in languages {
        forced_languages.entry(language.as_ref()).or_insert_with(
            || serde_json::json!({ "language_servers": [], "prettier": { "allowed": false } }),
        );
    }
    match servers {
        Servers::None => {
            settings["languages"]["Rust"]["language_servers"] = serde_json::json!([]);
        }
        Servers::RustAnalyzer {
            toolchain: Some(toolchain),
        } => {
            settings["lsp"]["rust-analyzer"]["binary"]["env"]["RUSTUP_TOOLCHAIN"] =
                toolchain.into();
        }
        Servers::RustAnalyzer { toolchain: None } => {}
    }
    settings.to_string()
}

/// Sets the forced settings. Call it at startup, before anything reads settings, and again once
/// every language is registered.
pub fn apply(languages: &[LanguageName], servers: Servers, cx: &mut App) {
    let settings = forced_settings(languages, servers);
    SettingsStore::update_global(cx, |store, cx| {
        store
            .set_server_settings(&settings, cx)
            .expect("rozed's forced settings are valid");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;
    use std::sync::Arc;

    use client::{ClientSettings, TelemetrySettings};
    use language::language_settings::AllLanguageSettings;
    use language::{LanguageRegistry, LanguageServerName};
    use node_runtime::NodeRuntime;
    use project::DisableAiSettings;
    use project::FakeFs;
    use project::project_settings::ProjectSettings;
    use settings::{
        ParseStatus, RootUserSettings as _, SaturatingBool, Settings as _, SettingsContent,
    };

    fn content(languages: &[LanguageName], servers: Servers) -> SettingsContent {
        SettingsContent::parse_json_with_comments(&forced_settings(languages, servers)).unwrap()
    }

    const TRUSTED: Servers<'static> = Servers::RustAnalyzer { toolchain: None };

    #[test]
    fn forced_settings_set_the_fields_they_mean_to() {
        let content = content(&[], TRUSTED);
        let telemetry = content.telemetry.unwrap();
        assert_eq!(telemetry.diagnostics, Some(false));
        assert_eq!(telemetry.metrics, Some(false));
        assert_eq!(content.server_url.as_deref(), Some("https://rozed.invalid"));
        assert_eq!(content.auto_update, Some(false));
        assert_eq!(content.project.disable_ai, Some(SaturatingBool(true)));
        assert_eq!(
            content.extension.auto_install_extensions.get("html"),
            Some(&false)
        );
        assert_eq!(content.session.unwrap().trust_all_worktrees, Some(false));
        assert_eq!(
            content.workspace.window_title_format.as_deref(),
            Some("rozed")
        );
    }

    /// User settings that ask for everything the forced settings turn off.
    const CONTRARY_USER_SETTINGS: &str = r#"{
      "telemetry": { "diagnostics": true, "metrics": true },
      "server_url": "https://zed.dev",
      "disable_ai": false,
      "session": { "trust_all_worktrees": true },
      "git": { "inline_blame": { "enabled": true } },
      "language_servers": ["..."],
      "prettier": { "allowed": true },
      "languages": {
        "JavaScript": { "prettier": { "allowed": true } },
        "Python": { "language_servers": ["pyright", "ruff", "..."], "enable_language_server": true },
        "Rust": { "language_servers": ["rust-analyzer", "..."] },
        "TypeScript": { "language_servers": ["vtsls", "eslint", "..."] }
      },
      "lsp": {
        "rust-analyzer": {
          "binary": { "ignore_system_version": false, "env": { "RUSTC_WRAPPER": "sccache" } }
        }
      }
    }"#;

    fn set_user_settings(json: &str, cx: &mut App) {
        SettingsStore::update_global(cx, |store, cx| {
            let result = store.set_user_settings(json, cx);
            assert_eq!(result.parse_status, ParseStatus::Success);
        });
    }

    fn assert_forced(cx: &App) {
        let telemetry = TelemetrySettings::get_global(cx);
        assert!(!telemetry.metrics, "telemetry metrics are on");
        assert!(!telemetry.diagnostics, "telemetry diagnostics are on");
        assert_eq!(
            ClientSettings::get_global(cx).server_url,
            "https://rozed.invalid"
        );
        assert!(DisableAiSettings::get_global(cx).disable_ai, "AI is on");
        assert!(
            !ProjectSettings::get_global(cx).session.trust_all_worktrees,
            "every worktree is trusted"
        );
        assert!(
            !ProjectSettings::get_global(cx).git.inline_blame.enabled,
            "inline git blame is on"
        );
        let binary = rust_analyzer_binary(cx);
        assert_eq!(binary.ignore_system_version, Some(true));
        assert_eq!(binary.env.unwrap()["RUSTC_WRAPPER"], "");
    }

    fn rust_analyzer_binary(cx: &App) -> settings::BinarySettings {
        ProjectSettings::get_global(cx).lsp[&LanguageServerName::new_static("rust-analyzer")]
            .binary
            .clone()
            .unwrap()
    }

    #[gpui::test]
    fn forced_settings_win_over_earlier_user_settings(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            settings::init(cx);
            set_user_settings(CONTRARY_USER_SETTINGS, cx);
            // The user's settings did change the defaults, so the next check means something.
            assert!(TelemetrySettings::get_global(cx).metrics);
            apply(&[], TRUSTED, cx);
            assert_forced(cx);
        });
    }

    #[gpui::test]
    fn forced_settings_win_over_later_user_settings(cx: &mut gpui::TestAppContext) {
        // rozed applies them at startup, before reading the user's settings file.
        cx.update(|cx| {
            settings::init(cx);
            apply(&[], TRUSTED, cx);
            set_user_settings(CONTRARY_USER_SETTINGS, cx);
            assert_forced(cx);
        });
    }

    #[gpui::test]
    fn trusted_only_rust_analyzer_can_start_and_prettier_never_does(cx: &mut gpui::TestAppContext) {
        for (name, started) in started_servers(TRUSTED, cx) {
            let expected = if name.as_ref() == "Rust" {
                vec![LanguageServerName::new_static("rust-analyzer")]
            } else {
                Vec::new()
            };
            assert_eq!(started, expected, "{name:?}");
        }
    }

    /// The language servers each of the languages rozed knows would start under `servers`, and
    /// whether any would run Prettier.
    fn started_servers(
        servers: Servers,
        cx: &mut gpui::TestAppContext,
    ) -> Vec<(LanguageName, Vec<LanguageServerName>)> {
        let fs = FakeFs::new(cx.executor());
        cx.update(|cx| {
            settings::init(cx);
            let languages = Arc::new(LanguageRegistry::test(cx.background_executor().clone()));
            languages::init(languages.clone(), fs, NodeRuntime::unavailable(), cx);
            let names = languages.language_names();
            let python = LanguageName::new_static("Python");
            assert!(
                !languages.lsp_adapters(&python).is_empty(),
                "Python has language servers to turn off"
            );
            set_user_settings(CONTRARY_USER_SETTINGS, cx);
            apply(&names, servers, cx);
            let available: Vec<LanguageServerName> = languages.available_lsp_adapter_names();
            names
                .iter()
                .map(|name| {
                    let mut adapters: Vec<LanguageServerName> = languages
                        .lsp_adapters(name)
                        .iter()
                        .map(|adapter| adapter.name.clone())
                        .collect();
                    adapters.extend(available.iter().cloned());
                    let settings =
                        AllLanguageSettings::get_global(cx).language(None, Some(name), cx);
                    assert!(!settings.prettier.allowed, "Prettier runs for {name:?}");
                    (
                        name.clone(),
                        settings.customized_language_servers(&adapters),
                    )
                })
                .collect()
        })
    }

    #[gpui::test]
    fn untrusted_no_language_server_can_start(cx: &mut gpui::TestAppContext) {
        let started = started_servers(Servers::None, cx);
        assert!(
            started.iter().any(|(name, _)| name.as_ref() == "Rust"),
            "Rust is among the languages"
        );
        for (name, servers) in started {
            assert_eq!(servers, Vec::<LanguageServerName>::new(), "{name:?}");
        }
    }

    /// Opens `/elsewhere/src/main.rs` the way Zed opens a file dropped on a pane, in a worktree of
    /// its own that Zed counts as trusted, with a fake rust-analyzer for Rust, and says whether it
    /// started.
    async fn rust_analyzer_starts_for_a_dropped_file(
        servers: Servers<'static>,
        cx: &mut gpui::TestAppContext,
    ) -> bool {
        cx.update(|cx| {
            settings::init(cx);
            release_channel::init(semver::Version::new(0, 0, 0), cx);
            project::trusted_worktrees::init(Default::default(), cx);
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/elsewhere",
            serde_json::json!({
                "Cargo.toml": "[package]\nname = \"elsewhere\"\n",
                "src": { "main.rs": "fn main() {}\n" },
            }),
        )
        .await;
        let project = project::Project::test_with_worktree_trust(fs, [], cx).await;
        let languages = project.read_with(cx, |project, _| project.languages().clone());
        languages.add(language::rust_lang());
        let mut fake_servers = languages.register_fake_lsp(
            "Rust",
            language::FakeLspAdapter {
                name: "rust-analyzer",
                ..Default::default()
            },
        );
        cx.update(|cx| apply(&[LanguageName::new_static("Rust")], servers, cx));

        // What a pane does with a dropped file: a worktree of its own, not visible, which the
        // project keeps only while something holds it.
        let (_worktree, _) = project
            .update(cx, |project, cx| {
                project.find_or_create_worktree("/elsewhere/src/main.rs", false, cx)
            })
            .await
            .unwrap();
        cx.run_until_parked();
        let (_buffer, _handle) = project
            .update(cx, |project, cx| {
                project.open_local_buffer_with_lsp("/elsewhere/src/main.rs", cx)
            })
            .await
            .unwrap();
        cx.run_until_parked();
        let worktree_visible = project.read_with(cx, |project, cx| {
            project.worktrees(cx).next().unwrap().read(cx).is_visible()
        });
        assert!(
            !worktree_visible,
            "the file opened in a worktree of its own"
        );
        fake_servers.try_recv().is_ok()
    }

    /// Why rozed turns every language server off without `--trust`: Zed trusts the worktree a
    /// dropped file opens in, so rust-analyzer would start there.
    #[gpui::test]
    async fn trusted_rust_analyzer_starts_for_a_file_zed_trusts(cx: &mut gpui::TestAppContext) {
        assert!(rust_analyzer_starts_for_a_dropped_file(TRUSTED, cx).await);
    }

    #[gpui::test]
    async fn untrusted_no_language_server_starts_even_where_zed_trusts(
        cx: &mut gpui::TestAppContext,
    ) {
        assert!(!rust_analyzer_starts_for_a_dropped_file(Servers::None, cx).await);
    }

    #[gpui::test]
    fn rust_analyzer_gets_no_wrappers_and_the_toolchain_rozed_found(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            settings::init(cx);
            set_user_settings(CONTRARY_USER_SETTINGS, cx);

            apply(&[], TRUSTED, cx);
            let env = rust_analyzer_binary(cx).env.unwrap();
            assert_eq!(
                env,
                BTreeMap::from(
                    [
                        ("RUSTC_WRAPPER", ""),
                        ("RUSTC_WORKSPACE_WRAPPER", ""),
                        ("RUSTUP_AUTO_INSTALL", "0"),
                    ]
                    .map(|(name, value)| (name.to_string(), value.to_string()))
                )
            );

            apply(
                &[],
                Servers::RustAnalyzer {
                    toolchain: Some("1.98.1-aarch64-apple-darwin"),
                },
                cx,
            );
            let env = rust_analyzer_binary(cx).env.unwrap();
            assert_eq!(env["RUSTUP_TOOLCHAIN"], "1.98.1-aarch64-apple-darwin");
            assert_eq!(env["RUSTUP_AUTO_INSTALL"], "0");
        });
    }

    #[test]
    fn rust_analyzer_is_downloaded_with_build_scripts_proc_macros_and_checks_off() {
        let content = content(&[], TRUSTED);
        let rust_analyzer = &content.project.lsp.0["rust-analyzer"];
        assert_eq!(
            rust_analyzer.binary.as_ref().unwrap().ignore_system_version,
            Some(true)
        );
        let options = rust_analyzer.initialization_options.as_ref().unwrap();
        assert_eq!(options["cargo"]["buildScripts"]["enable"], false);
        assert_eq!(options["cargo"]["noDeps"], true);
        assert_eq!(options["procMacro"]["enable"], false);
        assert_eq!(options["checkOnSave"], false);
    }

    #[test]
    fn the_server_url_is_refused_by_the_http_client() {
        let content = content(&[], TRUSTED);
        let url: http_client::Url = content.server_url.unwrap().parse().unwrap();
        assert!(crate::http::is_refused_host(url.host_str().unwrap()));
    }
}
