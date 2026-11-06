//! Generated files: those the repository marks `linguist-generated`, and lockfiles.

use super::ChangedFile;

/// Why a file counts as generated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Generated {
    /// The repository's `.gitattributes` marks it `linguist-generated`.
    Attribute,
    /// It's a package manager's lockfile ([`is_lockfile`]).
    Lockfile,
}

/// Lockfile names, matched against a path's last component.
const LOCKFILES: &[&str] = &[
    // JavaScript: pnpm, npm, Yarn, Bun.
    "pnpm-lock.yaml",
    "package-lock.json",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
    // Rust, Go.
    "Cargo.lock",
    "go.sum",
    // Python: uv, Poetry.
    "uv.lock",
    "poetry.lock",
    // Ruby, PHP.
    "Gemfile.lock",
    "composer.lock",
    // Apple: CocoaPods, Swift Package Manager.
    "Podfile.lock",
    "Package.resolved",
    // Dart, Elixir, Nix, .NET (NuGet).
    "pubspec.lock",
    "mix.lock",
    "flake.lock",
    "packages.lock.json",
];

/// Whether `path` names a package manager's lockfile, in any directory.
///
/// Matches the file name exactly, case included: `Cargo.lock` is a lockfile, `cargo.lock` isn't.
pub fn is_lockfile(path: &str) -> bool {
    LOCKFILES.contains(&file_name(path))
}

/// Whether the file is generated, and why.
///
/// The `linguist-generated` attribute decides when `.gitattributes` sets or unsets it. Otherwise,
/// lockfiles are generated. So `Cargo.lock -linguist-generated` keeps `Cargo.lock` expanded, as
/// GitHub does.
pub fn generated(file: &ChangedFile) -> Option<Generated> {
    match file.linguist_generated {
        Some(true) => Some(Generated::Attribute),
        Some(false) => None,
        None => is_lockfile(&file.path).then_some(Generated::Lockfile),
    }
}

/// A path's last component.
pub(super) fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_LOCKFILES: [&str; 17] = [
        "pnpm-lock.yaml",
        "package-lock.json",
        "yarn.lock",
        "Cargo.lock",
        "go.sum",
        "uv.lock",
        "poetry.lock",
        "Gemfile.lock",
        "composer.lock",
        "Podfile.lock",
        "Package.resolved",
        "pubspec.lock",
        "mix.lock",
        "flake.lock",
        "bun.lock",
        "bun.lockb",
        "packages.lock.json",
    ];

    fn file(path: &str, linguist_generated: Option<bool>) -> ChangedFile {
        ChangedFile {
            path: path.to_string(),
            linguist_generated,
            ..Default::default()
        }
    }

    #[test]
    fn every_listed_lockfile_is_a_lockfile() {
        for name in ALL_LOCKFILES {
            assert!(is_lockfile(name), "{name}");
        }
    }

    #[test]
    fn lockfiles_count_in_any_directory() {
        assert!(is_lockfile("apps/web/pnpm-lock.yaml"));
        assert!(is_lockfile("crates/xtask/Cargo.lock"));
        assert!(is_lockfile(
            "App.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved"
        ));
    }

    #[test]
    fn other_files_are_not_lockfiles() {
        for path in [
            "package.json",
            "Cargo.toml",
            "go.mod",
            "Gemfile",
            "cargo.lock",
            "yarn.lock.orig",
            "old-yarn.lock",
            "Cargo.lock/notes.md",
            "docs/lockfiles.md",
            "",
        ] {
            assert!(!is_lockfile(path), "{path}");
        }
    }

    #[test]
    fn the_attribute_marks_a_file_generated() {
        assert_eq!(
            generated(&file("src/schema.ts", Some(true))),
            Some(Generated::Attribute)
        );
    }

    #[test]
    fn lockfiles_are_generated_without_the_attribute() {
        assert_eq!(
            generated(&file("Cargo.lock", None)),
            Some(Generated::Lockfile)
        );
    }

    #[test]
    fn unsetting_the_attribute_keeps_a_lockfile_expanded() {
        assert_eq!(generated(&file("Cargo.lock", Some(false))), None);
    }

    #[test]
    fn a_lockfile_marked_generated_cites_the_attribute() {
        assert_eq!(
            generated(&file("yarn.lock", Some(true))),
            Some(Generated::Attribute)
        );
    }

    #[test]
    fn other_files_are_not_generated() {
        assert_eq!(generated(&file("src/main.rs", None)), None);
        assert_eq!(generated(&file("src/main.rs", Some(false))), None);
    }
}
