//! Zed settings rozed forces, whatever the user's settings say.
//!
//! They sit in Zed's top settings layer (the "server" layer, merged after the user's), so a
//! user's `settings.json` can't override them. They keep Zed's account, telemetry, update and AI
//! code from starting, and point every Zed service URL at a placeholder that rozed's HTTP client
//! refuses ([`crate::http`]).

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
  // Only rozed decides which worktrees are trusted to run language servers (`crate::trust`).
  "session": { "trust_all_worktrees": false }
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
    fn the_server_url_is_refused_by_the_http_client() {
        let content = SettingsContent::parse_json_with_comments(FORCED_SETTINGS).unwrap();
        let url: http_client::Url = content.server_url.unwrap().parse().unwrap();
        assert!(crate::http::is_refused_host(url.host_str().unwrap()));
    }
}
