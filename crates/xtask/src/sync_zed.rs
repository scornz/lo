//! `cargo xtask sync-zed`: copies the Cargo settings rozed needs from Zed's tree.
//!
//! Cargo ignores a dependency's `[patch]`, `[profile]` and `.cargo/config.toml`, so rozed keeps
//! copies of Zed's in its own root, between generated-section markers:
//!
//! - `Cargo.toml`: Zed's `[patch.*]` sections, with `path` entries rewritten to point into
//!   `third_party/zed`, and its `[profile.*]` sections.
//! - `.cargo/config.toml`: the parts of Zed's that apply on macOS.
//! - `rust-toolchain.toml`: Zed's toolchain channel.
//!
//! Patches and per-package profile settings for crates that aren't in rozed's `Cargo.lock` are
//! left out, because cargo warns about each one on every build.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context as _, Result, bail};
use toml_edit::{DocumentMut, Item, Table, Value};

/// Zed's checkout, relative to rozed's root.
pub const ZED_DIR: &str = "third_party/zed";

/// Top-level keys of Zed's `Cargo.toml`: what's copied, and what's deliberately not.
const MANIFEST_COPIED: &[&str] = &["patch", "profile"];
const MANIFEST_SKIPPED: &[&str] = &["workspace"];

/// `[target.<key>]` sections of Zed's `.cargo/config.toml` that apply on macOS, and those that
/// don't.
const CONFIG_TARGETS_COPIED: &[&str] = &["cfg(all())"];
const CONFIG_TARGETS_SKIPPED: &[&str] =
    &["cfg(target_os = \"windows\")", "aarch64-unknown-linux-gnu"];
/// Other top-level keys of Zed's `.cargo/config.toml`. `alias` holds Zed's own commands.
const CONFIG_COPIED: &[&str] = &["env"];
const CONFIG_SKIPPED: &[&str] = &["alias"];

const USAGE: &str = "\
usage: cargo xtask sync-zed [--check | --seed-lock]

Copies the Cargo settings rozed needs from third_party/zed into rozed's root: Zed's [patch] and
[profile] sections into Cargo.toml, the macOS parts of its .cargo/config.toml, and its toolchain
channel into rust-toolchain.toml.

  --check      change nothing; exit 1, naming what differs, if the copies are stale
  --seed-lock  first replace Cargo.lock with Zed's, so dependencies resolve to the versions Zed
               tested; then sync. Run it after a Zed bump or after adding a Zed crate dependency.";

/// One file rozed keeps in sync with Zed: where it lives and what it should contain.
struct Synced {
    path: &'static str,
    current: String,
    expected: String,
}

pub fn run(args: &[String]) -> ExitCode {
    let mode = match args {
        [] => Mode::Write,
        [flag] if flag == "--check" => Mode::Check,
        [flag] if flag == "--seed-lock" => Mode::SeedLock,
        [flag] if flag == "--help" || flag == "-h" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run_mode(&repo_root(), mode) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("sync-zed: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Write,
    Check,
    SeedLock,
}

/// rozed's root: two levels above this crate's manifest.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/xtask has two ancestors")
        .to_path_buf()
}

/// Returns false if `--check` found stale copies.
fn run_mode(root: &Path, mode: Mode) -> Result<bool> {
    let zed = root.join(ZED_DIR);
    if !zed.join("Cargo.toml").exists() {
        bail!(
            "{} is empty. Run `git submodule update --init` first.",
            zed.display()
        );
    }

    match mode {
        Mode::Check => {
            let stale = check(&compute(root)?);
            if stale.is_empty() {
                println!("sync-zed: rozed's copies of Zed's Cargo settings are up to date.");
                Ok(true)
            } else {
                eprintln!("sync-zed: rozed's copies of Zed's Cargo settings are stale:");
                for line in stale {
                    eprintln!("  {line}");
                }
                eprintln!("Run `cargo xtask sync-zed` and commit the result.");
                Ok(false)
            }
        }
        Mode::Write => {
            write(root, &compute(root)?)?;
            Ok(true)
        }
        Mode::SeedLock => {
            std::fs::copy(zed.join("Cargo.lock"), root.join("Cargo.lock"))
                .context("copying Zed's Cargo.lock")?;
            println!("sync-zed: copied Zed's Cargo.lock.");
            // Against Zed's lockfile nothing is pruned yet. Let cargo resolve with every patch in
            // place, which drops what rozed doesn't use from the lockfile, then prune against it.
            // Pruning changes no package, so a last resolve only clears the pruned patches from
            // the lockfile's `[[patch.unused]]` list.
            write(root, &compute(root)?)?;
            resolve(root)?;
            write(root, &compute(root)?)?;
            resolve(root)?;
            Ok(true)
        }
    }
}

/// Runs `cargo metadata`, which updates `Cargo.lock` to rozed's graph.
fn resolve(root: &Path) -> Result<()> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--manifest-path"])
        .arg(root.join("Cargo.toml"))
        .stdout(Stdio::null())
        .status()
        .context("running cargo metadata")?;
    if !status.success() {
        bail!("cargo metadata failed");
    }
    Ok(())
}

fn read(root: &Path, path: &str) -> Result<String> {
    let full = root.join(path);
    match std::fs::read_to_string(&full) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error).with_context(|| format!("reading {}", full.display())),
    }
}

fn compute(root: &Path) -> Result<Vec<Synced>> {
    let zed_file = |name: &str| read(root, &format!("{ZED_DIR}/{name}"));
    let packages = lock_packages(&read(root, "Cargo.lock")?)?;

    let manifest = read(root, "Cargo.toml")?;
    let manifest_block = manifest_block(&zed_file("Cargo.toml")?, &packages)?;
    let config = read(root, ".cargo/config.toml")?;
    let config_block = config_block(&zed_file(".cargo/config.toml")?)?;
    let toolchain = read(root, "rust-toolchain.toml")?;

    Ok(vec![
        Synced {
            path: "Cargo.toml",
            expected: splice(&manifest, &markers("Cargo.toml"), &manifest_block)?,
            current: manifest,
        },
        Synced {
            path: ".cargo/config.toml",
            expected: splice(&config, &markers(".cargo/config.toml"), &config_block)?,
            current: config,
        },
        Synced {
            path: "rust-toolchain.toml",
            expected: sync_toolchain(&zed_file("rust-toolchain.toml")?, &toolchain)?,
            current: toolchain,
        },
    ])
}

fn write(root: &Path, files: &[Synced]) -> Result<()> {
    for file in files {
        if file.current == file.expected {
            println!("sync-zed: {} is up to date.", file.path);
            continue;
        }
        let full = root.join(file.path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&full, &file.expected)
            .with_context(|| format!("writing {}", full.display()))?;
        println!("sync-zed: updated {}.", file.path);
    }
    Ok(())
}

/// Names what differs in each stale file. Empty when everything is in sync.
fn check(files: &[Synced]) -> Vec<String> {
    let mut stale = Vec::new();
    for file in files {
        if file.current == file.expected {
            continue;
        }
        let differences = section_differences(&file.current, &file.expected);
        if differences.is_empty() {
            stale.push(format!("{}: differs outside any section", file.path));
        }
        for difference in differences {
            stale.push(format!("{}: {difference}", file.path));
        }
    }
    stale
}

/// Compares two TOML texts section by section (`[header]` to the next header), naming each
/// section that's missing, extra or different. Blank lines around a section don't count.
fn section_differences(current: &str, expected: &str) -> Vec<String> {
    let current = sections(current);
    let expected = sections(expected);
    let mut differences = Vec::new();
    for (header, text) in &expected {
        match current.get(header) {
            None => differences.push(format!("{header} is missing")),
            Some(current_text) if current_text.trim() != text.trim() => {
                differences.push(format!("{header} differs"))
            }
            Some(_) => {}
        }
    }
    for header in current.keys() {
        if !expected.contains_key(header) {
            differences.push(format!("{header} should not be there"));
        }
    }
    differences
}

/// Splits TOML text into sections keyed by their header line, or by a generated-section marker.
/// Text before the first header is keyed as "the top of the file".
fn sections(text: &str) -> BTreeMap<String, String> {
    let mut sections = BTreeMap::new();
    let mut header = "the top of the file".to_string();
    let mut body = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let is_table = trimmed.starts_with('[') && !trimmed.starts_with("[[");
        let is_marker = trimmed.starts_with(MARKER_BEGIN) || trimmed.starts_with(MARKER_END);
        if is_table || is_marker {
            let name = if is_marker {
                format!("the marker `{trimmed}`")
            } else {
                trimmed.to_string()
            };
            sections.insert(std::mem::replace(&mut header, name), body);
            body = String::new();
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    sections.insert(header, body);
    sections
}

const MARKER_BEGIN: &str = "# BEGIN generated from Zed's";
const MARKER_END: &str = "# END generated from Zed's";

/// The lines that open and close the generated section copied from Zed's `file`.
fn markers(file: &str) -> (String, String) {
    (
        format!("{MARKER_BEGIN} {file}. Run `cargo xtask sync-zed`; don't edit by hand."),
        format!("{MARKER_END} {file}."),
    )
}

/// Replaces the text between the markers with `block`, or appends the markers and `block` when
/// the file has none.
fn splice(text: &str, (begin, end): &(String, String), block: &str) -> Result<String> {
    let lines: Vec<&str> = text.lines().collect();
    let find = |marker: &str| -> Result<Option<usize>> {
        let mut found = lines.iter().enumerate().filter(|(_, l)| l.trim() == marker);
        let first = found.next().map(|(ix, _)| ix);
        if found.next().is_some() {
            bail!("`{marker}` appears more than once");
        }
        Ok(first)
    };
    let block = block.trim();
    match (find(begin)?, find(end)?) {
        (Some(start), Some(stop)) if start < stop => {
            let mut out: Vec<&str> = lines[..=start].to_vec();
            out.extend(block.lines());
            out.extend(&lines[stop..]);
            Ok(out.join("\n") + "\n")
        }
        (None, None) => {
            let head = text.trim_end();
            let separator = if head.is_empty() { "" } else { "\n\n" };
            Ok(format!("{head}{separator}{begin}\n{block}\n{end}\n"))
        }
        _ => bail!("the generated-section markers are broken; expected `{begin}` then `{end}`"),
    }
}

/// The names of every package in a `Cargo.lock`.
fn lock_packages(lock: &str) -> Result<BTreeSet<String>> {
    let lock: DocumentMut = lock.parse().context("parsing Cargo.lock")?;
    let mut names = BTreeSet::new();
    if let Some(packages) = lock.get("package").and_then(Item::as_array_of_tables) {
        for package in packages {
            if let Some(name) = package.get("name").and_then(Item::as_str) {
                names.insert(name.to_string());
            }
        }
    }
    Ok(names)
}

/// Fails on a top-level key that's neither copied nor skipped, so a new section in Zed's file
/// gets a decision instead of being dropped silently.
fn check_known_keys(table: &Table, file: &str, copied: &[&str], skipped: &[&str]) -> Result<()> {
    for (key, _) in table.iter() {
        if !copied.contains(&key) && !skipped.contains(&key) {
            bail!(
                "{ZED_DIR}/{file} has a section `{key}` that sync-zed doesn't know. Decide whether \
                 rozed needs it and add it to the copied or skipped list in \
                 crates/xtask/src/sync_zed.rs."
            );
        }
    }
    Ok(())
}

/// Zed's `[patch.*]` and `[profile.*]` sections as rozed's root needs them.
fn manifest_block(zed_manifest: &str, packages: &BTreeSet<String>) -> Result<String> {
    let zed: DocumentMut = zed_manifest.parse().context("parsing Zed's Cargo.toml")?;
    check_known_keys(
        zed.as_table(),
        "Cargo.toml",
        MANIFEST_COPIED,
        MANIFEST_SKIPPED,
    )?;

    let mut out = DocumentMut::new();
    if let Some(patch) = zed.get("patch").and_then(Item::as_table) {
        let mut patch = patch.clone();
        for (_, registry) in patch.iter_mut() {
            let Some(registry) = registry.as_table_mut() else {
                continue;
            };
            registry.retain(|name, item| packages.contains(package_name(name, item)));
            for (_, item) in registry.iter_mut() {
                rewrite_path(item)?;
            }
        }
        patch.retain(|_, registry| !registry.as_table().is_some_and(Table::is_empty));
        if !patch.is_empty() {
            out.insert("patch", Item::Table(patch));
        }
    }
    if let Some(profile) = zed.get("profile").and_then(Item::as_table) {
        let mut profile = profile.clone();
        for (_, settings) in profile.iter_mut() {
            let Some(settings) = settings.as_table_mut() else {
                continue;
            };
            if let Some(per_package) = settings.get_mut("package").and_then(Item::as_table_mut) {
                per_package.retain(|spec, _| packages.contains(spec_name(spec)));
                if per_package.is_empty() {
                    settings.remove("package");
                }
            }
        }
        out.insert("profile", Item::Table(profile));
    }
    Ok(out.to_string())
}

/// The crate a `[patch]` entry replaces: its key, or its `package` if it renames one.
fn package_name<'a>(key: &'a str, item: &'a Item) -> &'a str {
    item.get("package").and_then(Item::as_str).unwrap_or(key)
}

/// The crate name in a profile package spec such as `syn` or `syn@2.0.0`.
fn spec_name(spec: &str) -> &str {
    spec.split('@').next().unwrap_or(spec)
}

/// Points a `[patch]` entry's `path` into Zed's checkout, keeping its formatting.
fn rewrite_path(item: &mut Item) -> Result<()> {
    // `TableLike::get_mut`, not `Item::get_mut`, which inserts the key when it's missing.
    let Some(path) = item
        .as_table_like_mut()
        .and_then(|entry| entry.get_mut("path"))
    else {
        return Ok(());
    };
    let Some(relative) = path.as_str() else {
        bail!("a [patch] entry in Zed's Cargo.toml has a `path` that isn't a string");
    };
    let mut rewritten = Value::from(format!("{ZED_DIR}/{relative}"));
    if let Some(old) = path.as_value() {
        *rewritten.decor_mut() = old.decor().clone();
    }
    *path = Item::Value(rewritten);
    Ok(())
}

/// The parts of Zed's `.cargo/config.toml` that apply on macOS.
fn config_block(zed_config: &str) -> Result<String> {
    let zed: DocumentMut = zed_config
        .parse()
        .context("parsing Zed's .cargo/config.toml")?;
    let mut known = CONFIG_COPIED.to_vec();
    known.push("target");
    check_known_keys(zed.as_table(), ".cargo/config.toml", &known, CONFIG_SKIPPED)?;

    let mut out = DocumentMut::new();
    if let Some(targets) = zed.get("target").and_then(Item::as_table) {
        check_known_keys(
            targets,
            ".cargo/config.toml [target]",
            CONFIG_TARGETS_COPIED,
            CONFIG_TARGETS_SKIPPED,
        )?;
        let mut targets = targets.clone();
        targets.retain(|key, _| CONFIG_TARGETS_COPIED.contains(&key));
        if !targets.is_empty() {
            targets.set_implicit(true);
            out.insert("target", Item::Table(targets));
        }
    }
    for key in CONFIG_COPIED {
        if let Some(item) = zed.get(key) {
            out.insert(key, item.clone());
        }
    }
    Ok(out.to_string())
}

/// rozed's `rust-toolchain.toml` with Zed's channel. Everything else in it is rozed's.
fn sync_toolchain(zed_toolchain: &str, toolchain: &str) -> Result<String> {
    let zed: DocumentMut = zed_toolchain
        .parse()
        .context("parsing Zed's rust-toolchain.toml")?;
    let Some(channel) = zed
        .get("toolchain")
        .and_then(|t| t.get("channel"))
        .and_then(Item::as_str)
    else {
        bail!("Zed's rust-toolchain.toml has no toolchain.channel");
    };
    let mut doc: DocumentMut = toolchain.parse().context("parsing rust-toolchain.toml")?;
    let table = doc
        .entry("toolchain")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .context("rust-toolchain.toml's `toolchain` isn't a table")?;
    if table.get("channel").and_then(Item::as_str) != Some(channel) {
        let mut value = Value::from(channel);
        if let Some(old) = table.get("channel").and_then(Item::as_value) {
            *value.decor_mut() = old.decor().clone();
        }
        table.insert("channel", Item::Value(value));
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packages(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    const ZED_MANIFEST: &str = r#"[workspace]
members = ["crates/zed"]

[patch.crates-io]
# Kept: rozed uses it.
notify = { git = "https://github.com/zed-industries/notify", rev = "abc" }
# Pruned: nothing in rozed uses it.
cpal = { git = "https://github.com/RustAudio/cpal", rev = "def" }
# Kept, with its path rewritten.
scratch = { path = "tooling/patches/scratch" }

[profile.dev]
incremental = true

[profile.dev.package]
syn = { opt-level = 3 }
livekit = { opt-level = 3 }

[profile.release]
lto = "thin"

[profile.release.package]
zed = { codegen-units = 16 }
"#;

    #[test]
    fn manifest_block_copies_patches_and_profiles() {
        let block = manifest_block(ZED_MANIFEST, &packages(&["notify", "scratch", "syn"])).unwrap();
        assert_eq!(
            block,
            r#"
[patch.crates-io]
# Kept: rozed uses it.
notify = { git = "https://github.com/zed-industries/notify", rev = "abc" }
# Kept, with its path rewritten.
scratch = { path = "third_party/zed/tooling/patches/scratch" }

[profile.dev]
incremental = true

[profile.dev.package]
syn = { opt-level = 3 }

[profile.release]
lto = "thin"
"#
        );
    }

    #[test]
    fn manifest_block_drops_a_patch_section_left_empty() {
        let block = manifest_block(ZED_MANIFEST, &packages(&[])).unwrap();
        assert!(!block.contains("[patch"), "{block}");
        assert!(block.contains("[profile.dev]"), "{block}");
    }

    #[test]
    fn manifest_block_prunes_by_the_renamed_package() {
        let zed = r#"[patch.crates-io]
renamed = { git = "https://example.com/x", package = "real" }
"#;
        assert!(
            manifest_block(zed, &packages(&["real"]))
                .unwrap()
                .contains("renamed")
        );
        assert!(
            !manifest_block(zed, &packages(&["renamed"]))
                .unwrap()
                .contains("renamed")
        );
    }

    #[test]
    fn manifest_block_rejects_an_unknown_section() {
        let error = manifest_block("[package]\nname = \"x\"\n", &packages(&[])).unwrap_err();
        assert!(error.to_string().contains("`package`"), "{error}");
    }

    #[test]
    fn spec_name_strips_a_version() {
        assert_eq!(spec_name("syn"), "syn");
        assert_eq!(spec_name("syn@2.0.1"), "syn");
    }

    const ZED_CONFIG: &str = r#"[target.'cfg(all())']
# v0 mangling
rustflags = ["-C", "symbol-mangling-version=v0", "--cfg", "tokio_unstable"]

[alias]
xtask = "run --package xtask --"

[target.'cfg(target_os = "windows")']
rustflags = ["--cfg", "windows_slim_errors"]

[target.aarch64-unknown-linux-gnu]
rustflags = ["-C", "link-arg=-fuse-ld=lld"]

[env]
MACOSX_DEPLOYMENT_TARGET = "12.0"
"#;

    #[test]
    fn config_block_keeps_only_the_macos_parts() {
        assert_eq!(
            config_block(ZED_CONFIG).unwrap(),
            r#"[target.'cfg(all())']
# v0 mangling
rustflags = ["-C", "symbol-mangling-version=v0", "--cfg", "tokio_unstable"]

[env]
MACOSX_DEPLOYMENT_TARGET = "12.0"
"#
        );
    }

    #[test]
    fn config_block_rejects_an_unknown_target() {
        let zed = "[target.'cfg(target_os = \"macos\")']\nrustflags = []\n";
        let error = config_block(zed).unwrap_err();
        assert!(error.to_string().contains("cfg(target_os"), "{error}");
    }

    fn test_markers() -> (String, String) {
        ("# BEGIN".to_string(), "# END".to_string())
    }

    #[test]
    fn splice_appends_markers_to_a_file_without_them() {
        let out = splice(
            "[workspace]\nmembers = []\n",
            &test_markers(),
            "[a]\nb = 1\n",
        )
        .unwrap();
        assert_eq!(
            out,
            "[workspace]\nmembers = []\n\n# BEGIN\n[a]\nb = 1\n# END\n"
        );
        assert_eq!(
            splice("", &test_markers(), "[a]").unwrap(),
            "# BEGIN\n[a]\n# END\n"
        );
    }

    #[test]
    fn splice_replaces_between_markers_and_keeps_the_rest() {
        let text = "[workspace]\n\n# BEGIN\n[old]\n# END\n\n[after]\nx = 1\n";
        let out = splice(text, &test_markers(), "\n[new]\n").unwrap();
        assert_eq!(
            out,
            "[workspace]\n\n# BEGIN\n[new]\n# END\n\n[after]\nx = 1\n"
        );
        assert_eq!(splice(&out, &test_markers(), "[new]").unwrap(), out);
    }

    #[test]
    fn splice_rejects_broken_markers() {
        assert!(splice("# BEGIN\n", &test_markers(), "").is_err());
        assert!(splice("# END\n# BEGIN\n", &test_markers(), "").is_err());
        assert!(splice("# BEGIN\n# BEGIN\n# END\n", &test_markers(), "").is_err());
    }

    #[test]
    fn sync_toolchain_sets_zeds_channel_and_keeps_comments() {
        let zed = "[toolchain]\nchannel = \"1.99.0\"\ntargets = [\"wasm32-wasip2\"]\n";
        let rozed = "# Zed's toolchain.\n[toolchain]\nchannel = \"1.98.1\" # pinned\nprofile = \"minimal\"\n";
        assert_eq!(
            sync_toolchain(zed, rozed).unwrap(),
            "# Zed's toolchain.\n[toolchain]\nchannel = \"1.99.0\" # pinned\nprofile = \"minimal\"\n"
        );
        assert_eq!(
            sync_toolchain(zed, &sync_toolchain(zed, rozed).unwrap()).unwrap(),
            sync_toolchain(zed, rozed).unwrap()
        );
    }

    #[test]
    fn lock_packages_lists_names() {
        let lock = "version = 4\n\n[[package]]\nname = \"a\"\nversion = \"1.0.0\"\n\n[[package]]\nname = \"b\"\nversion = \"2.0.0\"\n";
        assert_eq!(lock_packages(lock).unwrap(), packages(&["a", "b"]));
    }

    #[test]
    fn check_names_missing_markers() {
        let (begin, end) = markers("Cargo.toml");
        let current = "[workspace]\nmembers = []\n".to_string();
        let expected = format!("{current}\n{begin}\n[profile.dev]\nx = 1\n{end}\n");
        let files = [Synced {
            path: "Cargo.toml",
            current,
            expected,
        }];
        assert_eq!(
            check(&files),
            [
                "Cargo.toml: [profile.dev] is missing".to_string(),
                format!("Cargo.toml: the marker `{begin}` is missing"),
                format!("Cargo.toml: the marker `{end}` is missing"),
            ]
        );
    }

    #[test]
    fn check_names_the_sections_that_differ() {
        let files = [
            Synced {
                path: "Cargo.toml",
                current: "[workspace]\n[patch.crates-io]\na = 1\n[profile.dev]\nx = 1\n".into(),
                expected: "[workspace]\n[patch.crates-io]\na = 2\n[profile.release]\nx = 1\n"
                    .into(),
            },
            Synced {
                path: "rust-toolchain.toml",
                current: "same\n".into(),
                expected: "same\n".into(),
            },
        ];
        assert_eq!(
            check(&files),
            [
                "Cargo.toml: [patch.crates-io] differs",
                "Cargo.toml: [profile.release] is missing",
                "Cargo.toml: [profile.dev] should not be there",
            ]
        );
    }
}
