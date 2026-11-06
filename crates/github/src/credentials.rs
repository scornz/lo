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
//! rozed has its own: mode 600 in a mode 700 folder, written atomically through a new temporary
//! file, under a lock that other rozed processes respect too.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// Serializes read-modify-write cycles within this process. A lock on a file beside the
    /// tokens serializes them between processes.
    lock: Mutex<()>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
struct StoredCredential {
    username: String,
    password: String,
}

impl fmt::Debug for StoredCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredCredential")
            .field("username", &self.username)
            .field("password", &"redacted")
            .finish()
    }
}

/// Why the token file couldn't be loaded.
enum LoadError {
    /// It isn't the JSON rozed writes.
    Corrupt,
    Other(anyhow::Error),
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

    fn dir(&self) -> Result<&Path> {
        self.path
            .parent()
            .context("the credentials file has no folder")
    }

    fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Creates the private folder, and takes the lock that every rozed process takes before
    /// changing the file. It's released when the returned file is dropped.
    fn lock_between_processes(&self) -> Result<File> {
        let dir = self.dir()?;
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("making {} private", dir.display()))?;
        let path = dir.join(format!(".{}.lock", self.file_name()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            // A link planted at the lock's path fails to open rather than being followed.
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        file.lock()
            .with_context(|| format!("locking {}", path.display()))?;
        Ok(file)
    }

    fn try_load(&self) -> Result<BTreeMap<String, StoredCredential>, LoadError> {
        let metadata = match fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(error) => {
                return Err(LoadError::Other(
                    anyhow::Error::from(error).context(format!("reading {}", self.path.display())),
                ));
            }
        };
        let mode = metadata.permissions().mode() & 0o777;
        if mode != 0o600 {
            log::warn!(
                "{} was mode {mode:o}; making it private (600)",
                self.path.display()
            );
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("making {} private", self.path.display()))
                .map_err(LoadError::Other)?;
        }
        let bytes = fs::read(&self.path)
            .with_context(|| format!("reading {}", self.path.display()))
            .map_err(LoadError::Other)?;
        serde_json::from_slice(&bytes).map_err(|_| LoadError::Corrupt)
    }

    fn load(&self) -> Result<BTreeMap<String, StoredCredential>> {
        self.try_load().map_err(|error| match error {
            // Never include the contents in the error: they're tokens.
            LoadError::Corrupt => anyhow::anyhow!("{} isn't valid JSON", self.path.display()),
            LoadError::Other(error) => error,
        })
    }

    /// Loads the tokens to change them. A file that isn't valid JSON can't be fixed by hand
    /// from rozed, and would block every change, so it's moved aside, still private, and rozed
    /// starts again from no tokens.
    fn load_for_change(&self) -> Result<BTreeMap<String, StoredCredential>> {
        match self.try_load() {
            Ok(credentials) => Ok(credentials),
            Err(LoadError::Other(error)) => Err(error),
            Err(LoadError::Corrupt) => {
                let seconds = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_secs())
                    .unwrap_or_default();
                let aside = self
                    .dir()?
                    .join(format!("{}.corrupt-{seconds}", self.file_name()));
                fs::rename(&self.path, &aside)
                    .with_context(|| format!("moving {} aside", self.path.display()))?;
                log::warn!(
                    "{} wasn't valid JSON; moved it to {} and started without tokens",
                    self.path.display(),
                    aside.display()
                );
                Ok(BTreeMap::new())
            }
        }
    }

    /// Replaces the file with `credentials`, through a new temporary file that's created
    /// exclusively, with a random name and mode 600, so nothing already at its path is followed
    /// or reused. Call it holding the lock.
    fn save(&self, credentials: &BTreeMap<String, StoredCredential>) -> Result<()> {
        let dir = self.dir()?;
        let result = (|| {
            let mut file = tempfile::Builder::new()
                .prefix(&format!(".{}.", self.file_name()))
                .suffix(".tmp")
                .permissions(fs::Permissions::from_mode(0o600))
                .tempfile_in(dir)?;
            file.write_all(&serde_json::to_vec_pretty(credentials)?)?;
            file.as_file().sync_all()?;
            file.persist(&self.path)?;
            // Make the rename itself durable.
            File::open(dir)?.sync_all()?;
            anyhow::Ok(())
        })();
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
        let _other_processes = self.lock_between_processes()?;
        let mut credentials = self.load_for_change()?;
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
        let _other_processes = self.lock_between_processes()?;
        let mut credentials = self.load_for_change()?;
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

        // No temporary files are left behind, only the lock other processes take.
        let mut names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, [".credentials.json.lock", "credentials.json"]);
        assert_eq!(mode(&path.with_file_name(".credentials.json.lock")), 0o600);
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

    #[test]
    fn a_link_planted_where_the_temporary_file_goes_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        // Where the temporary file used to go, a link to a file of the user's.
        let target = dir.path().join("elsewhere.txt");
        fs::write(&target, "untouched").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        let planted = dir
            .path()
            .join(format!(".credentials.json.{}.tmp", std::process::id()));
        std::os::unix::fs::symlink(&target, &planted).unwrap();

        let store = FileCredentialsProvider::new(path.clone());
        store
            .write("rozed://github.com/a", "a", b"gho_FAKE")
            .unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "untouched");
        assert_eq!(mode(&target), 0o644);
        assert!(store.read("rozed://github.com/a").unwrap().is_some());
    }

    #[test]
    fn two_stores_on_one_file_keep_each_others_tokens() {
        // As two rozed processes would: each has its own store, with its own in-process lock.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let writers: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|name| {
                let store = FileCredentialsProvider::new(path.clone());
                std::thread::spawn(move || {
                    for index in 0..40 {
                        store
                            .write(
                                &format!("rozed://github.com/{name}{index}"),
                                name,
                                b"gho_FAKE",
                            )
                            .unwrap();
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        let store = FileCredentialsProvider::new(path);
        let missing: Vec<String> = ["a", "b"]
            .into_iter()
            .flat_map(|name| (0..40).map(move |index| format!("rozed://github.com/{name}{index}")))
            .filter(|url| store.read(url).unwrap().is_none())
            .collect();
        assert_eq!(missing, Vec::<String>::new());
    }

    #[test]
    fn a_corrupt_file_is_set_aside_rather_than_blocking_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        fs::write(&path, "gho_FAKE_SECRET not json").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let store = FileCredentialsProvider::new(path.clone());
        store.delete("rozed://github.com/a").unwrap();
        store
            .write("rozed://github.com/a", "a", b"gho_FAKE")
            .unwrap();
        assert!(store.read("rozed://github.com/a").unwrap().is_some());
        // The unreadable file is kept, privately, for the user to look at.
        let aside: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("credentials.json.corrupt-")
            })
            .collect();
        assert_eq!(aside.len(), 1, "{aside:?}");
        assert_eq!(
            fs::read_to_string(&aside[0]).unwrap(),
            "gho_FAKE_SECRET not json"
        );
        assert_eq!(mode(&aside[0]), 0o600);
    }

    #[test]
    fn a_stored_credential_never_shows_its_token() {
        let stored = StoredCredential {
            username: "a".into(),
            password: "gho_SECRET".into(),
        };
        let debug = format!("{stored:?}");
        assert!(!debug.contains("SECRET"), "{debug}");
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
