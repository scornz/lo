//! Parsing GitHub's enums, shared by the modules that take GitHub's data.

use std::fmt;

/// A value a GitHub enum doesn't have, such as one GitHub added after rozed was built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownValue(pub String);

impl fmt::Display for UnknownValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "GitHub value `{}` isn't one rozed knows", self.0)
    }
}

impl std::error::Error for UnknownValue {}

/// An enum named after one of GitHub's. It parses from GitHub's names for its values, ignoring
/// case, so GraphQL's `HAS_HOOKS` and REST's `has_hooks` both work, and `as_str` gives the GraphQL
/// name back, for a mutation's input.
macro_rules! github_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($(#[$variant_meta:meta])* $variant:ident = $github:literal,)*
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$variant_meta])* $variant,)*
        }

        impl $name {
            /// The value's name in GitHub's GraphQL schema.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $github,)*
                }
            }
        }

        impl std::str::FromStr for $name {
            type Err = $crate::UnknownValue;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                $(if value.eq_ignore_ascii_case($github) {
                    return Ok(Self::$variant);
                })*
                Err($crate::UnknownValue(value.to_string()))
            }
        }
    };
}

pub(crate) use github_enum;
