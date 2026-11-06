//! Where tokens are kept: the Keychain in release builds, a private file in debug builds.
//!
//! Both implement Zed's [`CredentialsProvider`], so tests can use its in-memory fake. Each token
//! is stored under its own server string, `rozed://github.com/<login>`, because the Keychain
//! looks items up by server alone: two accounts under one server would overwrite each other.
//! Debug builds use `rozed-dev://github.com/<login>`, so one that opts into the Keychain never
//! touches a release build's tokens, just as it never opens a release build's database.
//!
//! Debug builds use a file because macOS asks for Keychain access again after every unsigned
//! rebuild. Zed's own development file isn't private (`std::fs::write` leaves it at 0644), so
//! rozed has its own: mode 600 in a mode 700 folder, written atomically.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use credentials_provider::CredentialsProvider;
use gpui::AsyncApp;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// The host rozed signs in to. GitHub Enterprise hosts would get their own server strings.
pub const HOST: &str = "github.com";

/// Which kind of build is running. Debug and release builds keep separate accounts: separate
/// databases ([`crate::db::default_path`]) and separately named tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Build {
    Debug,
    Release,
}

impl Build {
    pub fn current() -> Self {
        if cfg!(debug_assertions) {
            Self::Debug
        } else {
            Self::Release
        }
    }
}

/// The server string an account's token is stored under in this build. Logins are lowercased:
/// GitHub's ignore case, and so does rozed's database, so a case-only rename keeps one item.
pub fn server_for(login: &str) -> String {
    server(Build::current(), &login.to_ascii_lowercase())
}

fn server(build: Build, login: &str) -> String {
    let scheme = match build {
        Build::Debug => "rozed-dev",
        Build::Release => "rozed",
    };
    format!("{scheme}://{HOST}/{login}")
}

/// Setting this to `1` makes a debug build use the Keychain, like a release build.
pub const USE_KEYCHAIN_ENV: &str = "ROZED_USE_KEYCHAIN";

/// Whether tokens go in the Keychain rather than the file.
pub fn uses_keychain() -> bool {
    !cfg!(debug_assertions) || std::env::var(USE_KEYCHAIN_ENV).as_deref() == Ok("1")
}

/// The token store for this build: the Keychain, or `~/.config/rozed/credentials.json`.
pub fn token_store() -> Arc<dyn CredentialsProvider> {
    if uses_keychain() {
        Arc::new(KeychainCredentialsProvider)
    } else {
        Arc::new(FileCredentialsProvider::new(
            paths::config_dir().join("credentials.json"),
        ))
    }
}

/// Tokens in the macOS Keychain, through GPUI. Each is an internet password item whose server is
/// [`server_for`] the login.
pub struct KeychainCredentialsProvider;

type BoxedFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + 'a>>;

impl CredentialsProvider for KeychainCredentialsProvider {
    fn read_credentials<'a>(
        &'a self,
        url: &'a str,
        cx: &'a AsyncApp,
    ) -> BoxedFuture<'a, Option<(String, Vec<u8>)>> {
        Box::pin(async move { cx.update(|cx| cx.read_credentials(url)).await })
    }

    fn write_credentials<'a>(
        &'a self,
        url: &'a str,
        username: &'a str,
        password: &'a [u8],
        cx: &'a AsyncApp,
    ) -> BoxedFuture<'a, ()> {
        Box::pin(async move {
            cx.update(|cx| cx.write_credentials(url, username, password))
                .await
        })
    }

    fn delete_credentials<'a>(&'a self, url: &'a str, cx: &'a AsyncApp) -> BoxedFuture<'a, ()> {
        Box::pin(async move { cx.update(|cx| cx.delete_credentials(url)).await })
    }
}

/// Tokens in a JSON file only its owner can read.
pub struct FileCredentialsProvider {
    path: PathBuf,
    /// Serializes read-modify-write cycles within this process.
    lock: Mutex<()>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
struct StoredCredential {
    username: String,
    password: String,
}

impl FileCredentialsProvider {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load(&self) -> Result<BTreeMap<String, StoredCredential>> {
        let metadata = match fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", self.path.display()));
            }
        };
        let mode = metadata.permissions().mode() & 0o777;
        if mode != 0o600 {
            log::warn!(
                "{} was mode {mode:o}; making it private (600)",
                self.path.display()
            );
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("making {} private", self.path.display()))?;
        }
        let bytes =
            fs::read(&self.path).with_context(|| format!("reading {}", self.path.display()))?;
        // Never include the contents in the error: they're tokens.
        serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("{} isn't valid JSON", self.path.display()))
    }

    fn save(&self, credentials: &BTreeMap<String, StoredCredential>) -> Result<()> {
        let dir = self
            .path
            .parent()
            .context("the credentials file has no folder")?;
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("making {} private", dir.display()))?;
        let temp = dir.join(format!(
            ".{}.{}.tmp",
            self.path
                .file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default(),
            std::process::id()
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&temp)?;
            // `mode` applies only when the file is created; make sure of it.
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            file.write_all(&serde_json::to_vec_pretty(credentials)?)?;
            file.sync_all()?;
            fs::rename(&temp, &self.path)?;
            anyhow::Ok(())
        })();
        if result.is_err() {
            fs::remove_file(&temp).ok();
        }
        result.with_context(|| format!("writing {}", self.path.display()))
    }

    pub fn read(&self, url: &str) -> Result<Option<(String, Vec<u8>)>> {
        let _lock = self.lock.lock();
        Ok(self
            .load()?
            .remove(url)
            .map(|stored| (stored.username, stored.password.into_bytes())))
    }

    pub fn write(&self, url: &str, username: &str, password: &[u8]) -> Result<()> {
        let password = String::from_utf8(password.to_vec())
            .map_err(|_| anyhow::anyhow!("a token must be text"))?;
        let _lock = self.lock.lock();
        let mut credentials = self.load()?;
        credentials.insert(
            url.to_string(),
            StoredCredential {
                username: username.to_string(),
                password,
            },
        );
        self.save(&credentials)
    }

    pub fn delete(&self, url: &str) -> Result<()> {
        let _lock = self.lock.lock();
        let mut credentials = self.load()?;
        if credentials.remove(url).is_some() {
            self.save(&credentials)?;
        }
        Ok(())
    }
}

impl CredentialsProvider for FileCredentialsProvider {
    fn read_credentials<'a>(
        &'a self,
        url: &'a str,
        _cx: &'a AsyncApp,
    ) -> BoxedFuture<'a, Option<(String, Vec<u8>)>> {
        Box::pin(async move { self.read(url) })
    }

    fn write_credentials<'a>(
        &'a self,
        url: &'a str,
        username: &'a str,
        password: &'a [u8],
        _cx: &'a AsyncApp,
    ) -> BoxedFuture<'a, ()> {
        Box::pin(async move { self.write(url, username, password) })
    }

    fn delete_credentials<'a>(&'a self, url: &'a str, _cx: &'a AsyncApp) -> BoxedFuture<'a, ()> {
        Box::pin(async move { self.delete(url) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn server_strings_are_per_account_and_per_build() {
        assert_ne!(server_for("a"), server_for("b"));
        // A debug build's items never share a name with a release build's, even when it opts
        // into the Keychain.
        assert_eq!(
            server(Build::Release, "rozed-bot"),
            "rozed://github.com/rozed-bot"
        );
        assert_eq!(
            server(Build::Debug, "rozed-bot"),
            "rozed-dev://github.com/rozed-bot"
        );
        // Tests are debug builds.
        assert_eq!(server_for("rozed-bot"), "rozed-dev://github.com/rozed-bot");
        // GitHub's logins ignore case, so a case-only rename keeps the same item.
        assert_eq!(server_for("Rozed-Bot"), server_for("rozed-bot"));
    }

    #[test]
    fn the_file_is_private_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rozed/credentials.json");
        let store = FileCredentialsProvider::new(path.clone());
        assert_eq!(store.read("rozed://github.com/a").unwrap(), None);

        store
            .write("rozed://github.com/a", "a", b"gho_FAKE_A")
            .unwrap();
        store
            .write("rozed://github.com/b", "b", b"gho_FAKE_B")
            .unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        assert_eq!(
            store.read("rozed://github.com/a").unwrap(),
            Some(("a".to_string(), b"gho_FAKE_A".to_vec()))
        );

        // Writing the same server replaces its token and leaves the other alone.
        store
            .write("rozed://github.com/a", "a", b"gho_FAKE_A2")
            .unwrap();
        store.delete("rozed://github.com/b").unwrap();
        let reopened = FileCredentialsProvider::new(path.clone());
        assert_eq!(
            reopened.read("rozed://github.com/a").unwrap().unwrap().1,
            b"gho_FAKE_A2"
        );
        assert_eq!(reopened.read("rozed://github.com/b").unwrap(), None);

        // No temporary files are left behind.
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["credentials.json"]);
    }

    #[test]
    fn a_file_others_can_read_is_made_private_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let store = FileCredentialsProvider::new(path.clone());
        store
            .write("rozed://github.com/a", "a", b"gho_FAKE")
            .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.read("rozed://github.com/a").unwrap().is_some());
        assert_eq!(mode(&path), 0o600);
    }

    #[test]
    fn a_corrupt_file_is_an_error_that_doesnt_echo_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        fs::write(&path, "gho_FAKE_SECRET not json").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let error = FileCredentialsProvider::new(path).read("x").unwrap_err();
        assert!(!format!("{error:#}").contains("SECRET"), "{error:#}");
    }

    #[gpui::test]
    async fn works_as_a_credentials_provider(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn CredentialsProvider> = Arc::new(FileCredentialsProvider::new(
            dir.path().join("credentials.json"),
        ));
        let cx = cx.to_async();
        store
            .write_credentials("rozed://github.com/a", "a", b"gho_FAKE", &cx)
            .await
            .unwrap();
        assert_eq!(
            store
                .read_credentials("rozed://github.com/a", &cx)
                .await
                .unwrap(),
            Some(("a".to_string(), b"gho_FAKE".to_vec()))
        );
        store
            .delete_credentials("rozed://github.com/a", &cx)
            .await
            .unwrap();
        assert_eq!(
            store
                .read_credentials("rozed://github.com/a", &cx)
                .await
                .unwrap(),
            None
        );
    }
}
