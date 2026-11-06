//! Reading attributes from a commit's `.gitattributes` files.

use repo_cache::{Attribute, Error, Oid, PullRequestRefs, Repo};
use tempfile::TempDir;

use crate::support::{Server, blob_id, missing_objects};

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
