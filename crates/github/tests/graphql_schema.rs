//! Every GraphQL query is valid against GitHub's vendored schema.
//!
//! `graphql_client` checks fields when it generates types, but not argument names or variable
//! types. `apollo-compiler` validates the whole query as the GraphQL spec does, so a query GitHub
//! would reject fails `cargo test` instead. `script/update-github-schema` runs these after a
//! schema refresh.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use std::collections::BTreeSet;

use apollo_compiler::ast::Value;
use apollo_compiler::executable::{Field, Selection, SelectionSet};
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
    assert!(queries >= 6, "found only {queries} queries");
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

fn parse(file: &str) -> Valid<ExecutableDocument> {
    let path = graphql_dir().join(file);
    let source = std::fs::read_to_string(&path).unwrap();
    ExecutableDocument::parse_and_validate(&SCHEMA, source, &path).unwrap()
}

/// The field whose response key is `key`, anywhere in `set`.
fn find<'a>(set: &'a SelectionSet, key: &str) -> Option<&'a Field> {
    set.selections.iter().find_map(|selection| match selection {
        Selection::Field(field) if field.response_key() == key => Some(&**field),
        Selection::Field(field) => find(&field.selection_set, key),
        Selection::InlineFragment(fragment) => find(&fragment.selection_set, key),
        Selection::FragmentSpread(_) => None,
    })
}

/// The `itemTypes` the field `key` of `file`'s one operation asks for.
fn item_types(file: &str, key: &str) -> BTreeSet<String> {
    let document = parse(file);
    let operation = document.operations.iter().next().unwrap();
    let field = find(&operation.selection_set, key).unwrap();
    let argument = (field.arguments.iter())
        .find(|argument| argument.name == "itemTypes")
        .unwrap();
    let Value::List(types) = &*argument.value else {
        panic!("{file}'s itemTypes isn't a list");
    };
    (types.iter())
        .map(|value| match &**value {
            Value::Enum(name) => name.to_string(),
            other => panic!("{other:?} isn't an item type"),
        })
        .collect()
}

/// `HeadRefForcePushedEvent`'s item type, `HEAD_REF_FORCE_PUSHED_EVENT`.
fn item_type(typename: &str) -> String {
    let mut name = String::new();
    for (index, character) in typename.chars().enumerate() {
        if character.is_ascii_uppercase() && index > 0 {
            name.push('_');
        }
        name.push(character.to_ascii_uppercase());
    }
    name
}

#[test]
fn the_check_watches_the_item_types_the_timeline_reads() {
    let document = parse("timeline.graphql");
    let operation = document.operations.iter().next().unwrap();
    let node = find(&operation.selection_set, "node").unwrap();
    let read: BTreeSet<String> = (node.selection_set.selections.iter())
        .filter_map(|selection| match selection {
            Selection::InlineFragment(fragment) => fragment.type_condition.as_ref(),
            _ => None,
        })
        .map(|typename| item_type(typename))
        .collect();
    assert_eq!(
        item_type("HeadRefForcePushedEvent"),
        "HEAD_REF_FORCE_PUSHED_EVENT"
    );
    assert!(read.len() > 20, "{read:?}");
    for item_type in &read {
        let value = SCHEMA.get_enum("PullRequestTimelineItemsItemType").unwrap();
        assert!(value.values.contains_key(item_type.as_str()), "{item_type}");
    }
    assert_eq!(
        item_types("pull_request_check.graphql", "timelineEvents"),
        read
    );
    assert_eq!(item_types("timeline.graphql", "timelineEvents"), read);
}
