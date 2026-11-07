//! The Rust toolchain rust-analyzer runs cargo and rustc from, chosen by Lo rather than the
//! repository.
//!
//! rust-analyzer runs `cargo` and `rustc` in the checkout. With rustup, those are proxies that
//! follow the repository's `rust-toolchain.toml`, and may first download and install the toolchain
//! it names. Lo's forced settings ([`crate::forced_settings`]) give rust-analyzer
//! `RUSTUP_TOOLCHAIN`, which rustup prefers to any toolchain file, set to the toolchain [`find`]
//! returns, and `RUSTUP_AUTO_INSTALL=0` in any case, so rustup never installs one.

use std::process::{Command, Stdio};

/// The toolchain rust-analyzer should use: the one Lo itself was started with, as `cargo run`
/// does, or else rustup's default. `None` when there's neither, or no rustup.
pub fn find() -> Option<String> {
    if let Some(toolchain) = std::env::var("RUSTUP_TOOLCHAIN")
        .ok()
        .filter(|toolchain| !toolchain.is_empty())
    {
        return Some(toolchain);
    }
    // `rustup default` reports the configured default, whatever folder it runs in.
    let output = Command::new("rustup")
        .arg("default")
        .current_dir("/")
        .env("RUSTUP_AUTO_INSTALL", "0")
        .env_remove("RUSTUP_TOOLCHAIN")
        .stdin(Stdio::null())
        .output()
        .inspect_err(|error| log::info!("no rustup to ask for a default toolchain: {error}"))
        .ok()?;
    if !output.status.success() {
        log::info!(
            "rustup has no default toolchain: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return None;
    }
    parse_rustup_default(&String::from_utf8_lossy(&output.stdout))
}

/// The toolchain `rustup default` names, e.g. `stable-aarch64-apple-darwin (default)`.
fn parse_rustup_default(output: &str) -> Option<String> {
    let name = output.split_whitespace().next()?;
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rustup_default() {
        assert_eq!(
            parse_rustup_default("stable-aarch64-apple-darwin (default)\n").as_deref(),
            Some("stable-aarch64-apple-darwin")
        );
        assert_eq!(
            parse_rustup_default("1.98.1-aarch64-apple-darwin\n").as_deref(),
            Some("1.98.1-aarch64-apple-darwin")
        );
        assert_eq!(parse_rustup_default(""), None);
        assert_eq!(parse_rustup_default("\n"), None);
    }
}
