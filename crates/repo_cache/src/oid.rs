//! Object IDs.

use std::fmt;
use std::str::FromStr;

use crate::Error;

/// A git object ID: 40 hex digits (SHA-1), or 64 (SHA-256). Always lowercase, so two IDs of one
/// object compare equal.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Oid(String);

impl Oid {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first `len` digits, for showing.
    pub fn short(&self, len: usize) -> &str {
        &self.0[..len.min(self.0.len())]
    }
}

impl FromStr for Oid {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Error> {
        let valid = matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit());
        if !valid {
            return Err(Error::Invalid {
                what: "commit ID",
                value: value.to_string(),
            });
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Oid({})", self.0)
    }
}

/// Reads one object ID from git's output, such as `rev-parse`'s.
pub(crate) fn parse_output(output: &[u8]) -> Result<Oid, Error> {
    let output = String::from_utf8_lossy(output);
    output.trim().parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_sha1_and_sha256_ids_and_lowercases_them() {
        let sha1 = "22C70ACC6992C0C51CB68527B87ADF114D01ACBC";
        let oid: Oid = sha1.parse().unwrap();
        assert_eq!(oid.as_str(), sha1.to_ascii_lowercase());
        assert_eq!(oid.short(7), "22c70ac");
        assert!("a".repeat(64).parse::<Oid>().is_ok());
    }

    #[test]
    fn refuses_anything_else() {
        let not_ids = [
            "",
            "HEAD",
            "22c70ac",
            "refs/heads/main",
            "--upload-pack=evil-and-forty-characters-long",
            &"g".repeat(40),
            &"a".repeat(41),
        ];
        for value in not_ids {
            let error = value.parse::<Oid>().unwrap_err();
            assert!(
                error.to_string().contains("isn't a valid commit ID"),
                "{value}: {error}"
            );
        }
    }
}
