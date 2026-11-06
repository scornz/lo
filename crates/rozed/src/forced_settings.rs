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
  // If Zed's extension host is ever started, it mustn't install anything from Zed's servers.
  "auto_install_extensions": { "html": false },
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

    use settings::{RootUserSettings as _, SaturatingBool, SettingsContent};

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
