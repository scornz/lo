//! Reading attributes from a commit's `.gitattributes` files.

use repo_cache::{Attribute, Error, Oid, PullRequestRefs, Repo};
use tempfile::TempDir;

use crate::support::{Server, blob_id, missing_objects, packs};

/// A pull request whose head changes what `.gitattributes` says, fetched into a clone. The head
/// has a top-level `.gitattributes`, with a macro, and another in `sub/`.
struct Fixture {
    base: Oid,
    head: Oid,
    _server: Server,
    _cache_dir: TempDir,
    repo: Repo,
    head_attributes: String,
}

fn fixture() -> Fixture {
    let server = Server::new();
    let remote = server.repo("owner", "repo");
    remote
        .write(".gitattributes", "*.pb.go -linguist-generated\n")
        .write("api.pb.go", "package api\n")
        .write("src/main.rs", "fn main() {}\n");
    let base = remote.commit("base");

    remote
        .switch("feature")
        .write(
            ".gitattributes",
            "[attr]generated linguist-generated\n\
             *.pb.go linguist-generated\n\
             Cargo.lock -linguist-generated\n\
             dist/** linguist-generated=true\n\
             keep.ts linguist-generated=false\n\
             odd.ts linguist-generated=maybe\n\
             *.gen generated\n",
        )
        .write("sub/.gitattributes", "*.rs linguist-generated\n")
        .write("api.pb.go", "package api // changed\n")
        .write("Cargo.lock", "# lock\n")
        .write("dist/app.js", "app()\n")
        .write("keep.ts", "keep\n")
        .write("odd.ts", "odd\n")
        .write("schema.gen", "schema\n")
        .write("sub/lib.rs", "pub fn lib() {}\n")
        .remove("src/main.rs");
    let head = remote.commit("head");
    remote.publish("refs/pull/1/head");
    remote.switch("trunk");
    remote.publish("refs/heads/trunk");
    let head_attributes = blob_id(&remote.work, &head, ".gitattributes");

    let (cache_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    repo.fetch_pull(&PullRequestRefs {
        number: 1,
        head: Some(head.clone()),
        base_branch: "trunk".into(),
        base: None,
    })
    .unwrap();
    Fixture {
        base,
        head,
        _server: server,
        _cache_dir: cache_dir,
        repo,
        head_attributes,
    }
}

/// A pull request whose head adds `files` to a base with a README, fetched into a clone.
struct Pull {
    head: Oid,
    work: std::path::PathBuf,
    _server: Server,
    _cache_dir: TempDir,
    repo: Repo,
}

fn pull(files: &[(String, &str)]) -> Pull {
    let server = Server::new();
    let remote = server.repo("owner", "repo");
    remote.write("README", "readme\n");
    remote.commit("base");
    remote.switch("feature");
    for (path, contents) in files {
        remote.write(path, contents);
    }
    let head = remote.commit("head");
    remote.publish("refs/pull/1/head");
    remote.switch("trunk");
    remote.publish("refs/heads/trunk");
    let (cache_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    repo.fetch_pull(&PullRequestRefs {
        number: 1,
        head: Some(head.clone()),
        base_branch: "trunk".into(),
        base: None,
    })
    .unwrap();
    Pull {
        head,
        work: remote.work.clone(),
        _server: server,
        _cache_dir: cache_dir,
        repo,
    }
}

fn files<const N: usize>(files: [(&str, &'static str); N]) -> Vec<(String, &'static str)> {
    files
        .into_iter()
        .map(|(path, contents)| (path.to_string(), contents))
        .collect()
}

fn linguist_generated(repo: &Repo, rev: &Oid, paths: &[&str]) -> Vec<Attribute> {
    let paths: Vec<&[u8]> = paths.iter().map(|path| path.as_bytes()).collect();
    repo.attribute(rev, "linguist-generated", &paths).unwrap()
}

#[test]
fn reads_an_attribute_from_a_commits_gitattributes() {
    let f = fixture();
    // The partial clone hasn't downloaded the head's `.gitattributes` yet.
    assert!(missing_objects(f.repo.git_dir(), f.head.as_str()).contains(&f.head_attributes));

    let attributes = linguist_generated(
        &f.repo,
        &f.head,
        &[
            "api.pb.go",
            "Cargo.lock",
            "dist/app.js",
            "keep.ts",
            "odd.ts",
            "schema.gen",
            "sub/lib.rs",
            // Deleted by the head, and never in the repository: patterns still match names.
            "src/main.rs",
            "elsewhere/other.pb.go",
        ],
    );

    use Attribute::*;
    assert_eq!(
        attributes,
        [
            Set,
            Unset,
            Value("true".into()),
            Value("false".into()),
            Value("maybe".into()),
            // Through the `generated` macro.
            Set,
            // From `sub/.gitattributes`.
            Set,
            Unspecified,
            Set,
        ]
    );
    // Downloaded to read it.
    assert!(!missing_objects(f.repo.git_dir(), f.head.as_str()).contains(&f.head_attributes));
}

#[test]
fn each_commit_answers_with_its_own_gitattributes() {
    let f = fixture();

    assert_eq!(
        linguist_generated(&f.repo, &f.base, &["api.pb.go", "Cargo.lock"]),
        [Attribute::Unset, Attribute::Unspecified]
    );
    assert_eq!(
        linguist_generated(&f.repo, &f.head, &["api.pb.go", "Cargo.lock"]),
        [Attribute::Set, Attribute::Unset]
    );
}

#[test]
fn names_that_arent_utf8_or_hold_a_line_break_are_answered() {
    let f = fixture();
    let paths: [&[u8]; 2] = [b"caf\xe9.pb.go", b"line\nbreak.pb.go"];

    let attributes = f
        .repo
        .attribute(&f.head, "linguist-generated", &paths)
        .unwrap();

    assert_eq!(attributes, [Attribute::Set, Attribute::Set]);
}

#[test]
fn no_paths_ask_git_nothing() {
    let f = fixture();
    assert_eq!(
        f.repo
            .attribute(&f.head, "linguist-generated", &[])
            .unwrap(),
        []
    );
}

#[test]
fn refuses_attribute_names_and_paths_git_would_misread() {
    let f = fixture();
    for name in ["", "-linguist-generated", "two words", "a\0b"] {
        assert!(
            matches!(
                f.repo.attribute(&f.head, name, &[b"a"]),
                Err(Error::Invalid { .. })
            ),
            "{name:?}"
        );
    }
    for path in [&b""[..], b"a\0b"] {
        assert!(matches!(
            f.repo.attribute(&f.head, "linguist-generated", &[path]),
            Err(Error::Invalid { .. })
        ));
    }
}

#[test]
fn a_commit_the_clone_lacks_is_an_error() {
    let f = fixture();
    let unknown: Oid = "3333333333333333333333333333333333333333".parse().unwrap();
    assert!(
        f.repo
            .attribute(&unknown, "linguist-generated", &[b"a"])
            .is_err()
    );
}

#[test]
fn patterns_match_case_sensitively_whatever_the_disk() {
    let p = pull(&files([
        (
            ".gitattributes",
            "Dist/** linguist-generated\n*.JS linguist-generated\n",
        ),
        ("dist/app.js", "app()\n"),
    ]));
    // On a Mac's disk, the clone's config says case doesn't matter, which mustn't change the
    // answer.
    assert_eq!(
        linguist_generated(&p.repo, &p.head, &["dist/app.js", "Dist/App.JS"]),
        [Attribute::Unspecified, Attribute::Set]
    );
}

#[test]
fn a_nested_gitattributes_overrides_its_parents() {
    let p = pull(&files([
        (".gitattributes", "*.js linguist-generated\n"),
        ("vendor/.gitattributes", "*.js -linguist-generated\n"),
        ("app.js", "app()\n"),
        ("vendor/lib.js", "lib()\n"),
        ("vendor/deep/x.js", "x()\n"),
    ]));
    assert_eq!(
        linguist_generated(
            &p.repo,
            &p.head,
            &["app.js", "vendor/lib.js", "vendor/deep/x.js"]
        ),
        [Attribute::Set, Attribute::Unset, Attribute::Unset]
    );
}

#[test]
fn the_gitattributes_that_apply_download_together_and_no_others() {
    let p = pull(&files([
        (".gitattributes", "*.txt linguist-generated\n"),
        ("a/.gitattributes", "*.txt -linguist-generated\n"),
        ("a/b/.gitattributes", "*.txt linguist-generated=true\n"),
        ("c/.gitattributes", "*.txt linguist-generated=false\n"),
        ("a/b/f.txt", "f\n"),
        ("c/g.txt", "g\n"),
    ]));
    let blob = |path| blob_id(&p.work, &p.head, path);
    let applying = [
        blob(".gitattributes"),
        blob("a/.gitattributes"),
        blob("a/b/.gitattributes"),
    ];
    let missing = missing_objects(p.repo.git_dir(), p.head.as_str());
    assert!(applying.iter().all(|blob| missing.contains(blob)));
    let before = packs(p.repo.git_dir());

    assert_eq!(
        linguist_generated(&p.repo, &p.head, &["a/b/f.txt"]),
        [Attribute::Value("true".into())]
    );

    assert_eq!(packs(p.repo.git_dir()), before + 1, "one download");
    let missing = missing_objects(p.repo.git_dir(), p.head.as_str());
    assert!(applying.iter().all(|blob| !missing.contains(blob)));
    assert!(missing.contains(&blob("c/.gitattributes")));
}

#[test]
fn past_the_limit_no_gitattributes_are_read() {
    let mut head = vec![(".gitattributes".to_string(), "*.txt linguist-generated\n")];
    let mut paths = Vec::new();
    for dir in 0..repo_cache::MAX_ATTRIBUTE_FILES {
        head.push((
            format!("d{dir}/.gitattributes"),
            "*.txt -linguist-generated\n",
        ));
        head.push((format!("d{dir}/f.txt"), "f\n"));
        paths.push(format!("d{dir}/f.txt"));
    }
    let p = pull(&head);
    let paths: Vec<&[u8]> = paths.iter().map(|path| path.as_bytes()).collect();
    let before = packs(p.repo.git_dir());

    let error = p
        .repo
        .attribute(&p.head, "linguist-generated", &paths)
        .unwrap_err();

    assert!(
        matches!(error, Error::TooManyAttributeFiles { found } if found == paths.len() + 1),
        "{error:?}"
    );
    assert_eq!(packs(p.repo.git_dir()), before, "nothing downloaded");
    // One fewer is read.
    assert!(
        p.repo
            .attribute(&p.head, "linguist-generated", &paths[1..])
            .is_ok()
    );
}
