//! What a commit's `.gitattributes` files say about its paths.
//!
//! Lo applies no attributes when it diffs or checks out (see [`crate::Git`]), but some of a
//! repository's attributes are worth reading: `linguist-generated` says a file is generated, so
//! its diff starts collapsed, as on GitHub.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use crate::git::{EMPTY_TREE, GitVersion, failure_message};
use crate::repo::NO_LAZY_FETCH_VAR;
use crate::{Error, Oid, Repo};

/// The most `.gitattributes` files Lo reads for one question: each is a download in a partial
/// clone.
pub const MAX_ATTRIBUTE_FILES: usize = 200;

/// How many paths go on one command line.
const PATHS_PER_COMMAND: usize = 1000;

/// One attribute's state for one path, as `git check-attr` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attribute {
    /// No `.gitattributes` line mentions it.
    Unspecified,
    /// Set: `name`.
    Set,
    /// Unset: `-name`.
    Unset,
    /// Set to a value: `name=value`.
    Value(String),
}

impl Attribute {
    /// The attribute read as GitHub's Linguist reads its own: unset or `false` is `Some(false)`,
    /// set or any other value is `Some(true)`, and no mention is `None`.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Unspecified => None,
            Self::Unset => Some(false),
            Self::Value(value) if value == "false" => Some(false),
            Self::Set | Self::Value(_) => Some(true),
        }
    }

    fn parse(value: &[u8]) -> Self {
        match value {
            b"unspecified" => Self::Unspecified,
            b"set" => Self::Set,
            b"unset" => Self::Unset,
            value => Self::Value(String::from_utf8_lossy(value).into_owned()),
        }
    }
}

impl Repo {
    /// What the `.gitattributes` files in commit `rev` say about `attribute` for each of `paths`,
    /// in the same order. Paths are bytes, as git stores them, and needn't exist in `rev`: a file
    /// the commit deletes still matches its patterns.
    ///
    /// git reads the files from the commit itself (`git check-attr --source`), so the answer is
    /// the commit's, never a checkout's or the user's. Patterns match case-sensitively, as on
    /// GitHub, whatever the disk. The `.gitattributes` files that apply, those in the paths'
    /// folders and the folders above, download first in one batch. When more than
    /// [`MAX_ATTRIBUTE_FILES`] apply, it reads none and fails with
    /// [`Error::TooManyAttributeFiles`].
    pub fn attribute(
        &self,
        rev: &Oid,
        attribute: &str,
        paths: &[&[u8]],
    ) -> Result<Vec<Attribute>, Error> {
        let valid = !attribute.is_empty()
            && !attribute.starts_with('-')
            && attribute
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte));
        if !valid {
            return Err(Error::Invalid {
                what: "attribute name",
                value: attribute.to_string(),
            });
        }
        if let Some(path) = paths
            .iter()
            .find(|path| path.is_empty() || path.contains(&0))
        {
            return Err(Error::Invalid {
                what: "path",
                value: String::from_utf8_lossy(path).into_owned(),
            });
        }
        if paths.is_empty() {
            return Ok(Vec::new());
        }

        let files = self.attribute_files(rev, paths)?;
        if files.len() > MAX_ATTRIBUTE_FILES {
            return Err(Error::TooManyAttributeFiles { found: files.len() });
        }
        self.download_attribute_files(rev, &files)?;

        let mut input = Vec::new();
        for path in paths {
            input.extend_from_slice(path);
            input.push(0);
        }
        let mut command = self.git().args([
            // A Mac's clone says its disk ignores case, which would make `Dist/**` match
            // `dist/app.js`. GitHub's patterns, and the commit's paths, are case-sensitive.
            "-c",
            "core.ignorecase=false",
            "check-attr",
            &format!("--source={rev}"),
            "-z",
            "--stdin",
            attribute,
        ]);
        // They're all here now: nothing more downloads, one at a time.
        if self.git_version() >= GitVersion::NO_LAZY_FETCH {
            command = command.env(NO_LAZY_FETCH_VAR, "1");
        }
        let display = command.display();
        let finished = command.finish(Some(&input))?;
        // When it can't download the commit or a `.gitattributes` file, git says so and answers
        // "unspecified" anyway, exiting 0.
        if !finished.status.success() || finished.stderr.contains("fatal:") {
            return Err(Error::from_git(display, failure_message(&finished), None));
        }
        parse(&finished.stdout, attribute, paths).ok_or_else(|| Error::Git {
            command: display,
            message: format!(
                "printed {:?}, not one answer for each of {} paths",
                String::from_utf8_lossy(&finished.stdout),
                paths.len()
            ),
        })
    }
}

impl Repo {
    /// The `.gitattributes` files in `rev` that apply to `paths`, in their folders and the
    /// folders above, by path. Reads only trees, which a partial clone has.
    fn attribute_files(&self, rev: &Oid, paths: &[&[u8]]) -> Result<Vec<(Vec<u8>, Oid)>, Error> {
        let mut candidates = BTreeSet::new();
        for path in paths {
            candidates.insert(b".gitattributes".to_vec());
            for (end, _) in path.iter().enumerate().filter(|(_, byte)| **byte == b'/') {
                candidates.insert([&path[..end], b"/.gitattributes"].concat());
            }
        }
        let candidates: Vec<_> = candidates.into_iter().collect();
        let mut files = Vec::new();
        for chunk in candidates.chunks(PATHS_PER_COMMAND) {
            let output = self.run(
                self.git()
                    .args(["ls-tree", "-z", "--full-tree", rev.as_str(), "--"])
                    .args(chunk.iter().map(|path| OsStr::from_bytes(path))),
            )?;
            for entry in output
                .split(|byte| *byte == 0)
                .filter(|entry| !entry.is_empty())
            {
                // "<mode> blob <oid>\t<path>"
                let parsed = entry
                    .iter()
                    .position(|byte| *byte == b'\t')
                    .and_then(|tab| {
                        let (info, path) = (&entry[..tab], &entry[tab + 1..]);
                        let mut fields = info.split(|byte| *byte == b' ');
                        let (_, kind, oid) = (fields.next()?, fields.next()?, fields.next()?);
                        let oid = std::str::from_utf8(oid).ok()?.parse().ok()?;
                        Some((kind == b"blob").then(|| (path.to_vec(), oid)))
                    });
                match parsed {
                    Some(Some(file)) => files.push(file),
                    Some(None) => {}
                    None => {
                        return Err(Error::Git {
                            command: "git ls-tree".into(),
                            message: format!(
                                "printed {:?}, not a tree entry",
                                String::from_utf8_lossy(entry)
                            ),
                        });
                    }
                }
            }
        }
        Ok(files)
    }

    /// Downloads those of `files`, `.gitattributes` files in `rev`, that the clone lacks, in one
    /// batch. A clone opened to read offline downloads nothing: it fails unless it has them all.
    fn download_attribute_files(&self, rev: &Oid, files: &[(Vec<u8>, Oid)]) -> Result<(), Error> {
        if files.is_empty() {
            return Ok(());
        }
        if self.is_offline() {
            // An older git can't be told not to download what it reads.
            if self.git_version() < GitVersion::NO_LAZY_FETCH {
                return Err(Error::ReadOnly {
                    name: self.git_dir().display().to_string(),
                    what: "read .gitattributes without downloading them on this git",
                });
            }
        } else {
            // Counting a diff's lines needs its files, and git downloads the ones it lacks
            // together first.
            self.run(
                self.git()
                    .args(["diff", "--no-ext-diff", "--no-textconv", "--numstat", "-z"])
                    .args([EMPTY_TREE, rev.as_str(), "--"])
                    .args(files.iter().map(|(path, _)| OsStr::from_bytes(path))),
            )?;
            // An older git downloads anything still missing as it reads it; a newer one is told
            // not to, so it mustn't be missing anything.
            if self.git_version() < GitVersion::NO_LAZY_FETCH {
                return Ok(());
            }
        }
        let input: String = files.iter().map(|(_, oid)| format!("{oid}\n")).collect();
        let output = self
            .git()
            .env(NO_LAZY_FETCH_VAR, "1")
            .args(["cat-file", "--batch-check=%(objectname)"])
            .run_with_input(Some(input.as_bytes()))?;
        let output = String::from_utf8_lossy(&output);
        let missing: Vec<_> = output
            .lines()
            .filter_map(|line| line.strip_suffix(" missing"))
            .collect();
        match missing.first() {
            None => Ok(()),
            Some(oid) => Err(Error::Git {
                command: "git cat-file --batch-check".into(),
                message: format!(
                    "the clone lacks {} of the .gitattributes files, such as {oid}",
                    missing.len()
                ),
            }),
        }
    }
}

/// Reads `git check-attr -z` output: `<path> NUL <attribute> NUL <value> NUL` for each path, in
/// the order asked. `None` when it doesn't answer exactly `paths`.
fn parse(output: &[u8], attribute: &str, paths: &[&[u8]]) -> Option<Vec<Attribute>> {
    let output = output.strip_suffix(b"\0")?;
    let mut fields = output.split(|byte| *byte == 0);
    let mut values = Vec::with_capacity(paths.len());
    for path in paths {
        let (Some(found), Some(name), Some(value)) = (fields.next(), fields.next(), fields.next())
        else {
            return None;
        };
        if found != *path || name != attribute.as_bytes() {
            return None;
        }
        values.push(Attribute::parse(value));
    }
    fields.next().is_none().then_some(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_read_as_linguist_reads_them() {
        assert_eq!(Attribute::Set.as_bool(), Some(true));
        assert_eq!(Attribute::Value("true".into()).as_bool(), Some(true));
        assert_eq!(Attribute::Unset.as_bool(), Some(false));
        assert_eq!(Attribute::Value("false".into()).as_bool(), Some(false));
        assert_eq!(Attribute::Value("1".into()).as_bool(), Some(true));
        assert_eq!(Attribute::Value("maybe".into()).as_bool(), Some(true));
        assert_eq!(Attribute::Unspecified.as_bool(), None);
    }

    #[test]
    fn output_is_read_in_the_order_asked() {
        let paths: [&[u8]; 3] = [b"a.pb.go", b"Cargo.lock", b"odd\nname"];
        let output = b"a.pb.go\0gen\0set\0Cargo.lock\0gen\0unset\0odd\nname\0gen\0maybe\0";
        assert_eq!(
            parse(output, "gen", &paths),
            Some(vec![
                Attribute::Set,
                Attribute::Unset,
                Attribute::Value("maybe".into())
            ])
        );
    }

    #[test]
    fn output_that_doesnt_answer_each_path_is_refused() {
        let paths: [&[u8]; 2] = [b"a", b"b"];
        // Too few, too many, another path, another attribute, cut short.
        for output in [
            &b"a\0gen\0set\0"[..],
            b"a\0gen\0set\0b\0gen\0set\0c\0gen\0set\0",
            b"a\0gen\0set\0c\0gen\0set\0",
            b"a\0gen\0set\0b\0other\0set\0",
            b"a\0gen\0set\0b\0gen",
        ] {
            assert_eq!(
                parse(output, "gen", &paths),
                None,
                "{:?}",
                String::from_utf8_lossy(output)
            );
        }
    }
}
