//! Zed settings rozed forces, whatever the user's settings say.
//!
//! They sit in Zed's top settings layer (the "server" layer, merged after the user's), so a
//! user's `settings.json` can't override them. They keep Zed's account, telemetry, update and AI
//! code from starting, point every Zed service URL at a placeholder that rozed's HTTP client
//! refuses ([`crate::http`]), and keep rust-analyzer from running a repository's code.

use gpui::{App, UpdateGlobal as _};
use settings::SettingsStore;

/// The forced settings, in the format of Zed's `settings.json`.
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
  // Only rozed decides which worktrees are trusted to run language servers (`crate::trust`).
  "session": { "trust_all_worktrees": false },
  "lsp": {
    "rust-analyzer": {
      // Use the rust-analyzer rozed downloads from its GitHub releases, never one found on PATH.
      // With rustup, that one is a proxy that runs, and may first install, whatever toolchain the
      // repository's rust-toolchain.toml names.
      "binary": { "ignore_system_version": true },
      // Never run the repository's code or download its dependencies (design §1): no build
      // scripts, no proc macros, no `cargo check`, and no `cargo metadata` for dependencies.
      "initialization_options": {
        "cargo": { "buildScripts": { "enable": false }, "noDeps": true },
        "procMacro": { "enable": false },
        "checkOnSave": false
      }
    }
  }
}"#;

pub fn apply(cx: &mut App) {
    SettingsStore::update_global(cx, |store, cx| {
        store
            .set_server_settings(FORCED_SETTINGS, cx)
            .expect("rozed's forced settings are valid");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    use client::{ClientSettings, TelemetrySettings};
    use project::DisableAiSettings;
    use project::project_settings::ProjectSettings;
    use settings::{
        ParseStatus, RootUserSettings as _, SaturatingBool, Settings as _, SettingsContent,
    };

    #[test]
    fn forced_settings_set_the_fields_they_mean_to() {
        let content = SettingsContent::parse_json_with_comments(FORCED_SETTINGS).unwrap();
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
    }

    /// User settings that ask for everything the forced settings turn off.
    const CONTRARY_USER_SETTINGS: &str = r#"{
      "telemetry": { "diagnostics": true, "metrics": true },
      "server_url": "https://zed.dev",
      "disable_ai": false,
      "session": { "trust_all_worktrees": true }
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
    }

    #[gpui::test]
    fn forced_settings_win_over_earlier_user_settings(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            settings::init(cx);
            set_user_settings(CONTRARY_USER_SETTINGS, cx);
            // The user's settings did change the defaults, so the next check means something.
            assert!(TelemetrySettings::get_global(cx).metrics);
            apply(cx);
            assert_forced(cx);
        });
    }

    #[gpui::test]
    fn forced_settings_win_over_later_user_settings(cx: &mut gpui::TestAppContext) {
        // rozed applies them at startup, before reading the user's settings file.
        cx.update(|cx| {
            settings::init(cx);
            apply(cx);
            set_user_settings(CONTRARY_USER_SETTINGS, cx);
            assert_forced(cx);
        });
    }

    #[test]
    fn rust_analyzer_is_downloaded_and_runs_nothing_from_the_repository() {
        let content = SettingsContent::parse_json_with_comments(FORCED_SETTINGS).unwrap();
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
        let content = SettingsContent::parse_json_with_comments(FORCED_SETTINGS).unwrap();
        let url: http_client::Url = content.server_url.unwrap().parse().unwrap();
        assert!(crate::http::is_refused_host(url.host_str().unwrap()));
    }
}
