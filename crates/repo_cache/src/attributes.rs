//! What a commit's `.gitattributes` files say about its paths.
//!
//! rozed applies no attributes when it diffs or checks out (see [`crate::Git`]), but some of a
//! repository's attributes are worth reading: `linguist-generated` says a file is generated, so
//! its diff starts collapsed, as on GitHub.

use crate::git::failure_message;
use crate::{Error, Oid, Repo};

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
    /// The attribute read as GitHub's Linguist reads its own: set or `true` is `Some(true)`, unset
    /// or `false` is `Some(false)`, and anything else, including no mention, is `None`.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Set => Some(true),
            Self::Unset => Some(false),
            Self::Value(value) if value == "true" => Some(true),
            Self::Value(value) if value == "false" => Some(false),
            Self::Value(_) | Self::Unspecified => None,
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
    /// git reads the files from the commit itself (`git check-attr --source`), downloading any
    /// the clone lacks, so the answer is the commit's, never a checkout's or the user's.
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

        let mut input = Vec::new();
        for path in paths {
            input.extend_from_slice(path);
            input.push(0);
        }
        let command = self.git().args([
            "check-attr",
            &format!("--source={rev}"),
            "-z",
            "--stdin",
            attribute,
        ]);
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
        assert_eq!(Attribute::Value("1".into()).as_bool(), None);
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
