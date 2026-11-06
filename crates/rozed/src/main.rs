//! rozed, a read-only GitHub pull request reviewer for macOS. A stub until the spike
//! (`docs/plan.md`, milestone 2) builds the app.

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("rozed {}", version());
    }
}

/// rozed's version, from its Cargo manifest.
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_semver() {
        let parts: Vec<&str> = version().split('.').collect();
        assert_eq!(parts.len(), 3, "{} isn't major.minor.patch", version());
        for part in parts {
            assert!(part.parse::<u64>().is_ok(), "{part} isn't a number");
        }
    }
}
