//! The helper git runs to ask for credentials (`GIT_ASKPASS`).
//!
//! git runs it with its prompt as the only argument and reads the answer from its output. It
//! answers the username prompt with `x-access-token` and the password prompt with the token in
//! [`TOKEN_VAR`], which rozed sets only on the git it runs for a repository that needs one. The
//! token never appears in a command line, a file or a log.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use crate::Error;
use crate::git::TOKEN_VAR;

const FILE_NAME: &str = "askpass.sh";

fn script() -> String {
    format!(
        r#"#!/bin/sh
# rozed's git askpass helper: answers git's prompts for a GitHub token. rozed writes this file and
# sets {TOKEN_VAR} only on the git it runs.
case "$1" in
Username*) printf '%s\n' x-access-token ;;
Password*) printf '%s\n' "${TOKEN_VAR}" ;;
*) exit 1 ;;
esac
"#
    )
}

/// Writes the helper into `dir` unless it's there already, and returns its path.
pub(crate) fn install(dir: &Path) -> Result<PathBuf, Error> {
    let path = dir.join(FILE_NAME);
    let script = script();
    let current = fs::read_to_string(&path).ok();
    let mode = fs::metadata(&path)
        .map(|metadata| metadata.permissions().mode() & 0o777)
        .ok();
    if current.as_deref() == Some(script.as_str()) && mode == Some(0o700) {
        return Ok(path);
    }
    fs::create_dir_all(dir).map_err(Error::io(format!("creating {}", dir.display())))?;
    // Written under another name and renamed, so a git running meanwhile never sees half of it.
    let temporary = dir.join(format!(".{FILE_NAME}.{}", std::process::id()));
    let write = || -> std::io::Result<()> {
        let _ = fs::remove_file(&temporary);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&temporary)?;
        file.write_all(script.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, &path)
    };
    write().map_err(Error::io(format!("writing {}", path.display())))?;
    Ok(path)
}
