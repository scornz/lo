//! What changed between two commits, from `git diff --raw --numstat -z`.

use crate::{Error, Oid};

/// A file that changed between two commits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    pub change: Change,
    /// The file before. `None` when it was added.
    pub old: Option<FileVersion>,
    /// The file after. `None` when it was deleted.
    pub new: Option<FileVersion>,
    /// Whether git considers either side binary, from its content or `.gitattributes`.
    pub binary: bool,
    /// Lines added and deleted. `None` for a binary file.
    pub lines: Option<LineCounts>,
}

/// How a file changed, as git reports it. Renames are detected as GitHub does, at 50% similarity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Added,
    Deleted,
    Modified,
    /// Renamed, and maybe edited. `similarity` is a percentage.
    Renamed {
        similarity: u8,
    },
    /// The same path changed type, e.g. a symlink became a file.
    TypeChanged,
}

/// One side of a changed file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileVersion {
    pub path: String,
    /// The blob, or for a submodule the commit it points at.
    pub oid: Oid,
    pub mode: FileMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileMode {
    Regular,
    Executable,
    Symlink,
    Submodule,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineCounts {
    pub added: u32,
    pub deleted: u32,
}

impl ChangedFile {
    /// The path to show: the new path, or the old one for a deleted file.
    pub fn path(&self) -> &str {
        self.new
            .as_ref()
            .or(self.old.as_ref())
            .map(|version| version.path.as_str())
            .unwrap_or_default()
    }

    pub fn old_path(&self) -> Option<&str> {
        self.old.as_ref().map(|version| version.path.as_str())
    }

    pub fn new_path(&self) -> Option<&str> {
        self.new.as_ref().map(|version| version.path.as_str())
    }
}

/// Parses `git diff -z --raw --numstat --no-abbrev`: a raw record for each file, then a numstat
/// record for each, in the same order.
pub(crate) fn parse(output: &[u8]) -> Result<Vec<ChangedFile>, Error> {
    let fields = nul_fields(output)?;
    let mut fields = fields.iter().map(String::as_str).peekable();
    let mut files = Vec::new();
    while let Some(header) = fields.next_if(|field| field.starts_with(':')) {
        let mut path = || {
            fields
                .next()
                .filter(|path| !path.is_empty())
                .ok_or_else(|| unexpected(&format!("{header} without a path")))
        };
        let raw = parse_raw_header(header)?;
        let (old_path, new_path) = match raw.status {
            'R' | 'C' => (Some(path()?), Some(path()?)),
            _ => {
                let path = path()?;
                (Some(path), Some(path))
            }
        };
        let version = |path: Option<&str>, mode: Option<FileMode>, oid: &Oid| -> Option<_> {
            Some(FileVersion {
                path: path?.to_string(),
                oid: oid.clone(),
                mode: mode?,
            })
        };
        let change = match raw.status {
            'A' => Change::Added,
            'D' => Change::Deleted,
            'M' => Change::Modified,
            'T' => Change::TypeChanged,
            // Copies are off (no -C), but a copy reads best as an added file.
            'C' => Change::Added,
            'R' => Change::Renamed {
                similarity: raw.score.unwrap_or(100),
            },
            status => return Err(unexpected(&format!("file status {status:?}"))),
        };
        let old = match change {
            Change::Added => None,
            _ => version(old_path, raw.old_mode, &raw.old_oid),
        };
        let new = match change {
            Change::Deleted => None,
            _ => version(new_path, raw.new_mode, &raw.new_oid),
        };
        if old.is_none() && new.is_none() {
            return Err(unexpected(header));
        }
        files.push(ChangedFile {
            change,
            old,
            new,
            binary: false,
            lines: None,
        });
    }

    let mut index = 0;
    while let Some(field) = fields.next() {
        let mut parts = field.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(unexpected(&format!("--numstat line {field:?}")));
        };
        let path = if path.is_empty() {
            // A rename: the old and new paths follow as fields of their own.
            let _old = fields.next();
            fields
                .next()
                .ok_or_else(|| unexpected(&format!("--numstat rename {field:?}")))?
        } else {
            path
        };
        let file = files
            .get_mut(index)
            .ok_or_else(|| unexpected(&format!("--numstat line for {path} without a raw line")))?;
        if file.path() != path {
            return Err(unexpected(&format!(
                "--numstat line for {path} where {} was expected",
                file.path()
            )));
        }
        if added == "-" && deleted == "-" {
            file.binary = true;
        } else {
            let count = |count: &str| {
                count
                    .parse()
                    .map_err(|_| unexpected(&format!("--numstat line {field:?}")))
            };
            file.lines = Some(LineCounts {
                added: count(added)?,
                deleted: count(deleted)?,
            });
        }
        index += 1;
    }
    if index != files.len() {
        return Err(unexpected(&format!(
            "{} --raw lines and {index} --numstat lines",
            files.len()
        )));
    }
    Ok(files)
}

struct RawHeader {
    old_mode: Option<FileMode>,
    new_mode: Option<FileMode>,
    old_oid: Oid,
    new_oid: Oid,
    status: char,
    score: Option<u8>,
}

/// Parses `:100644 100644 <old oid> <new oid> R087`.
fn parse_raw_header(header: &str) -> Result<RawHeader, Error> {
    let parts: Vec<&str> = header.trim_start_matches(':').split(' ').collect();
    let [old_mode, new_mode, old_oid, new_oid, status] = parts[..] else {
        return Err(unexpected(header));
    };
    let mut status_chars = status.chars();
    let status_char = status_chars.next().ok_or_else(|| unexpected(header))?;
    let score = status_chars.as_str();
    Ok(RawHeader {
        old_mode: parse_mode(old_mode)?,
        new_mode: parse_mode(new_mode)?,
        old_oid: old_oid.parse()?,
        new_oid: new_oid.parse()?,
        status: status_char,
        score: if score.is_empty() {
            None
        } else {
            Some(score.parse().map_err(|_| unexpected(header))?)
        },
    })
}

fn parse_mode(mode: &str) -> Result<Option<FileMode>, Error> {
    Ok(Some(match mode {
        "000000" => return Ok(None),
        "100644" => FileMode::Regular,
        "100755" => FileMode::Executable,
        "120000" => FileMode::Symlink,
        "160000" => FileMode::Submodule,
        _ => return Err(unexpected(&format!("file mode {mode}"))),
    }))
}

/// Splits `-z` output into its NUL-terminated fields.
fn nul_fields(output: &[u8]) -> Result<Vec<String>, Error> {
    let output = output.strip_suffix(b"\0").unwrap_or(output);
    if output.is_empty() {
        return Ok(Vec::new());
    }
    output
        .split(|byte| *byte == 0)
        .map(|field| {
            String::from_utf8(field.to_vec()).map_err(|_| unexpected("a path that isn't UTF-8"))
        })
        .collect()
}

fn unexpected(what: &str) -> Error {
    Error::Git {
        command: "git diff --raw --numstat".into(),
        message: format!("printed an unexpected {what}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";
    const ZERO: &str = "0000000000000000000000000000000000000000";

    fn version(path: &str, oid: &str, mode: FileMode) -> Option<FileVersion> {
        Some(FileVersion {
            path: path.into(),
            oid: oid.parse().unwrap(),
            mode,
        })
    }

    #[test]
    fn parses_raw_and_numstat_records() {
        let output = format!(
            ":000000 100644 {ZERO} {A} A\0added.txt\0\
             :100644 100644 {A} {B} M\0data.bin\0\
             :100644 000000 {A} {ZERO} D\0gone.txt\0\
             :100644 100755 {A} {B} M\0run.sh\0\
             :120000 100644 {A} {B} T\0link\0\
             :100644 100644 {A} {B} R087\0old.rs\0src/new.rs\0\
             :160000 160000 {A} {B} M\0vendor/lib\0\
             1\t0\tadded.txt\0-\t-\tdata.bin\x000\t1\tgone.txt\x000\t0\trun.sh\0\
             1\t1\tlink\x003\t1\t\0old.rs\0src/new.rs\x001\t1\tvendor/lib\0"
        );
        let files = parse(output.as_bytes()).unwrap();
        let lines = |added, deleted| Some(LineCounts { added, deleted });
        let expected = [
            (
                Change::Added,
                None,
                version("added.txt", A, FileMode::Regular),
                false,
                lines(1, 0),
            ),
            (
                Change::Modified,
                version("data.bin", A, FileMode::Regular),
                version("data.bin", B, FileMode::Regular),
                true,
                None,
            ),
            (
                Change::Deleted,
                version("gone.txt", A, FileMode::Regular),
                None,
                false,
                lines(0, 1),
            ),
            (
                Change::Modified,
                version("run.sh", A, FileMode::Regular),
                version("run.sh", B, FileMode::Executable),
                false,
                lines(0, 0),
            ),
            (
                Change::TypeChanged,
                version("link", A, FileMode::Symlink),
                version("link", B, FileMode::Regular),
                false,
                lines(1, 1),
            ),
            (
                Change::Renamed { similarity: 87 },
                version("old.rs", A, FileMode::Regular),
                version("src/new.rs", B, FileMode::Regular),
                false,
                lines(3, 1),
            ),
            (
                Change::Modified,
                version("vendor/lib", A, FileMode::Submodule),
                version("vendor/lib", B, FileMode::Submodule),
                false,
                lines(1, 1),
            ),
        ];
        assert_eq!(files.len(), expected.len());
        for (file, (change, old, new, binary, lines)) in files.iter().zip(expected) {
            assert_eq!(
                (file.change, &file.old, &file.new, file.binary, file.lines),
                (change, &old, &new, binary, lines)
            );
        }
        assert_eq!(files[2].path(), "gone.txt");
        assert_eq!(files[5].path(), "src/new.rs");
        assert_eq!(files[5].old_path(), Some("old.rs"));
        assert_eq!(files[0].old_path(), None);
    }

    #[test]
    fn nothing_changed_is_an_empty_list() {
        assert_eq!(parse(b"").unwrap(), vec![]);
    }

    #[test]
    fn refuses_output_it_doesnt_understand() {
        let bad = [
            format!(":100644 100644 {A} {B} R100\0only-one-path\0"),
            format!(":100644 100644 {A} {B} X\0what\0"),
            format!(":100644 100644 {A} {B} M\0a\0"),
            format!(":100644 100644 {A} {B} M\0a\x001\t1\tb\0"),
            format!(":100644 100644 {A} {B} M\0a\x001\t1\ta\x001\t1\ta\0"),
            format!(":040000 040000 {A} {B} M\0dir\x001\t1\tdir\0"),
            "1\t1\ta\0".to_string(),
        ];
        for output in bad {
            assert!(parse(output.as_bytes()).is_err(), "{output:?}");
        }
    }
}
