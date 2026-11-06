//! Every GraphQL query is valid against GitHub's vendored schema.
//!
//! `graphql_client` checks fields when it generates types, but not argument names or variable
//! types. `apollo-compiler` validates the whole query as the GraphQL spec does, so a query GitHub
//! would reject fails `cargo test` instead. `script/update-github-schema` runs these after a
//! schema refresh.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use apollo_compiler::validation::Valid;
use apollo_compiler::{ExecutableDocument, Schema};

fn graphql_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("graphql")
}

static SCHEMA: LazyLock<Valid<Schema>> = LazyLock::new(|| {
    let path = graphql_dir().join("schema.graphql");
    let source = std::fs::read_to_string(&path).expect("reading the schema");
    Schema::parse_and_validate(source, &path).unwrap_or_else(|errors| panic!("{errors}"))
});

fn validate(source: &str, path: &Path) -> Result<(), String> {
    ExecutableDocument::parse_and_validate(&SCHEMA, source, path)
        .map(|_| ())
        .map_err(|errors| errors.to_string())
}

#[test]
fn graphql_schema_validates_every_query() {
    let mut queries = 0;
    for entry in std::fs::read_dir(graphql_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "graphql")
            && path
                .file_name()
                .is_some_and(|name| name != "schema.graphql")
        {
            let source = std::fs::read_to_string(&path).unwrap();
            if let Err(errors) = validate(&source, &path) {
                panic!(
                    "{} isn't valid against GitHub's schema:\n{errors}",
                    path.display()
                );
            }
            queries += 1;
        }
    }
    assert!(queries >= 4, "found only {queries} queries");
}

#[test]
fn graphql_schema_check_catches_what_codegen_misses() {
    let path = Path::new("typo.graphql");
    let argument_typo = r#"query Q($owner: String!, $name: String!, $number: Int!) {
        repository(owner: $owner, name: $name) { pullRequest(numbr: $number) { title } }
    }"#;
    let error = validate(argument_typo, path).unwrap_err();
    assert!(error.contains("numbr"), "{error}");

    let wrong_variable_type = r#"query Q($owner: String!, $name: String!, $number: String!) {
        repository(owner: $owner, name: $name) { pullRequest(number: $number) { title } }
    }"#;
    let error = validate(wrong_variable_type, path).unwrap_err();
    assert!(error.contains("$number"), "{error}");
}

#[test]
fn graphql_schema_has_stacks() {
    let pull_request = SCHEMA.get_object("PullRequest").unwrap();
    assert!(pull_request.fields.contains_key("stackEntry"));
    assert!(SCHEMA.get_object("PullRequestStack").is_some());
}
