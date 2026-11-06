//! Review order, path order and initial states for whole pull requests, as GitHub would list them.

use review::files::{
    ChangedFile, Generated, InitialState, initial_state, path_order, review_order,
};

fn file(path: &str, additions: u32, deletions: u32) -> ChangedFile {
    ChangedFile {
        path: path.to_string(),
        additions,
        deletions,
        ..Default::default()
    }
}

fn marked_generated(file: ChangedFile) -> ChangedFile {
    ChangedFile {
        linguist_generated: Some(true),
        ..file
    }
}

fn renamed(from: &str, file: ChangedFile) -> ChangedFile {
    ChangedFile {
        previous_path: Some(from.to_string()),
        ..file
    }
}

fn binary(path: &str) -> ChangedFile {
    ChangedFile {
        binary: true,
        ..file(path, 0, 0)
    }
}

/// The paths in `order`, checking it lists every file exactly once.
fn paths(files: &[ChangedFile], order: Vec<usize>) -> Vec<&str> {
    let mut sorted = order.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, (0..files.len()).collect::<Vec<_>>());
    order.into_iter().map(|i| files[i].path.as_str()).collect()
}

/// The files that start collapsed, with their states, in path order.
fn collapsed(files: &[ChangedFile]) -> Vec<(&str, InitialState)> {
    path_order(files)
        .into_iter()
        .map(|i| (files[i].path.as_str(), initial_state(&files[i])))
        .filter(|(_, state)| state.collapsed())
        .collect()
}

#[test]
fn a_typescript_monorepo_pull_request() {
    let files = vec![
        file("pnpm-lock.yaml", 4_210, 3_980),
        file("apps/web/src/components/Button.test.tsx", 64, 0),
        file("apps/web/src/components/Button.tsx", 41, 12),
        file(".github/workflows/ci.yml", 6, 2),
        file("apps/api/test/index.test.ts", 22, 3),
        file("apps/api/src/index.ts", 18, 4),
        file("apps/web/src/index.ts", 2, 1),
        marked_generated(file("apps/web/src/api/generated/schema.ts", 380, 120)),
        file("apps/web/src/api/client.ts", 33, 9),
        file("apps/web/src/api/client.spec.ts", 50, 2),
        file("apps/web/src/legacy/old.test.ts", 0, 88),
        file("apps/web/package.json", 3, 1),
        binary("apps/web/public/logo.png"),
        file("tsconfig.base.json", 1, 1),
        file(".gitattributes", 1, 0),
        file("README.md", 10, 2),
    ];

    assert_eq!(
        paths(&files, review_order(&files)),
        [
            "README.md",
            "apps/api/src/index.ts",
            "apps/api/test/index.test.ts",
            "apps/web/public/logo.png",
            "apps/web/src/api/client.ts",
            "apps/web/src/api/client.spec.ts",
            "apps/web/src/components/Button.tsx",
            "apps/web/src/components/Button.test.tsx",
            "apps/web/src/index.ts",
            "apps/web/src/legacy/old.test.ts",
            ".gitattributes",
            ".github/workflows/ci.yml",
            "apps/web/package.json",
            "tsconfig.base.json",
            "apps/web/src/api/generated/schema.ts",
            "pnpm-lock.yaml",
        ]
    );
    assert_eq!(
        collapsed(&files),
        [
            (
                "apps/web/src/api/generated/schema.ts",
                InitialState {
                    generated: Some(Generated::Attribute),
                    large: false,
                    binary: false,
                }
            ),
            (
                "pnpm-lock.yaml",
                InitialState {
                    generated: Some(Generated::Lockfile),
                    large: true,
                    binary: false,
                }
            ),
        ]
    );
    let logo = files
        .iter()
        .find(|file| file.path == "apps/web/public/logo.png")
        .unwrap();
    assert!(initial_state(logo).binary);
}

#[test]
fn a_go_service_pull_request() {
    let files = vec![
        file("go.sum", 40, 12),
        file("go.mod", 3, 1),
        file("cmd/server/main.go", 12, 3),
        file("internal/store/store_test.go", 140, 20),
        file("internal/store/store.go", 95, 30),
        file("internal/store/migrations.go", 24, 0),
        file("internal/store/export_test.go", 8, 8),
        file("internal/store/queries.sql", 30, 2),
        marked_generated(file("internal/store/queries.sql.go", 210, 14)),
        file("internal/store/testdata/fixture.json", 60, 0),
        file(".golangci.yml", 2, 0),
    ];

    assert_eq!(
        paths(&files, review_order(&files)),
        [
            "cmd/server/main.go",
            "go.mod",
            "internal/store/migrations.go",
            "internal/store/queries.sql",
            "internal/store/store.go",
            "internal/store/store_test.go",
            "internal/store/export_test.go",
            ".golangci.yml",
            "internal/store/testdata/fixture.json",
            "go.sum",
            "internal/store/queries.sql.go",
        ]
    );
    assert_eq!(
        paths(&files, path_order(&files)),
        [
            ".golangci.yml",
            "cmd/server/main.go",
            "go.mod",
            "go.sum",
            "internal/store/export_test.go",
            "internal/store/migrations.go",
            "internal/store/queries.sql",
            "internal/store/queries.sql.go",
            "internal/store/store.go",
            "internal/store/store_test.go",
            "internal/store/testdata/fixture.json",
        ]
    );
}

fn rust_workspace_pull_request() -> Vec<ChangedFile> {
    vec![
        file("Cargo.lock", 18, 2),
        file("Cargo.toml", 1, 0),
        file("crates/review/Cargo.toml", 7, 0),
        file("crates/review/src/lib.rs", 6, 0),
        file("crates/review/src/files/order.rs", 412, 0),
        file("crates/review/tests/order.rs", 260, 0),
        file("crates/review/tests/common/mod.rs", 40, 0),
        file("docs/plan.md", 4, 1),
        file(".github/workflows/ci.yml", 3, 3),
    ]
}

#[test]
fn a_rust_workspace_pull_request() {
    let files = rust_workspace_pull_request();

    assert_eq!(
        paths(&files, review_order(&files)),
        [
            "crates/review/src/files/order.rs",
            "crates/review/tests/order.rs",
            "crates/review/src/lib.rs",
            "docs/plan.md",
            "crates/review/tests/common/mod.rs",
            ".github/workflows/ci.yml",
            "Cargo.toml",
            "crates/review/Cargo.toml",
            "Cargo.lock",
        ]
    );
    assert_eq!(
        paths(&files, path_order(&files)),
        [
            ".github/workflows/ci.yml",
            "Cargo.lock",
            "Cargo.toml",
            "crates/review/Cargo.toml",
            "crates/review/src/files/order.rs",
            "crates/review/src/lib.rs",
            "crates/review/tests/common/mod.rs",
            "crates/review/tests/order.rs",
            "docs/plan.md",
        ]
    );
    assert_eq!(
        collapsed(&files),
        [(
            "Cargo.lock",
            InitialState {
                generated: Some(Generated::Lockfile),
                large: false,
                binary: false,
            }
        )]
    );
}

#[test]
fn a_repository_that_unmarks_its_lockfile_reviews_it_with_config() {
    let mut files = rust_workspace_pull_request();
    files[0].linguist_generated = Some(false);

    assert_eq!(
        paths(&files, review_order(&files))[5..],
        [
            ".github/workflows/ci.yml",
            "Cargo.lock",
            "Cargo.toml",
            "crates/review/Cargo.toml",
        ]
    );
    assert!(collapsed(&files).is_empty());
}

#[test]
fn a_python_pull_request_with_renames_and_a_large_lockfile() {
    let files = vec![
        renamed(
            "src/srcs/billing/bill.py",
            file("src/srcs/billing/invoice.py", 20, 6),
        ),
        renamed(
            "tests/billing/test_bill.py",
            file("tests/billing/test_invoice.py", 4, 4),
        ),
        file("src/srcs/billing/tax.py", 31, 8),
        file("src/srcs/billing/tax_test.py", 45, 0),
        file("tests/billing/test_refunds.py", 70, 0),
        file("tests/conftest.py", 12, 0),
        file("src/srcs/billing/rates.proto", 9, 0),
        marked_generated(file("src/srcs/billing/rates_pb2.py", 140, 0)),
        file("pyproject.toml", 2, 1),
        file("uv.lock", 3_900, 2_300),
    ];

    assert_eq!(
        paths(&files, review_order(&files)),
        [
            "src/srcs/billing/invoice.py",
            "tests/billing/test_invoice.py",
            "src/srcs/billing/rates.proto",
            "src/srcs/billing/tax.py",
            "src/srcs/billing/tax_test.py",
            "tests/conftest.py",
            "tests/billing/test_refunds.py",
            "pyproject.toml",
            "src/srcs/billing/rates_pb2.py",
            "uv.lock",
        ]
    );
    assert_eq!(
        collapsed(&files)
            .into_iter()
            .map(|(path, state)| (path, state.generated, state.large))
            .collect::<Vec<_>>(),
        [
            (
                "src/srcs/billing/rates_pb2.py",
                Some(Generated::Attribute),
                false
            ),
            ("uv.lock", Some(Generated::Lockfile), true),
        ]
    );
}

#[test]
fn a_large_hand_written_file_starts_collapsed_but_keeps_its_place() {
    let files = vec![
        file("src/parser.rs", 4_000, 1_500),
        file("src/lexer.rs", 120, 40),
        file("tests/parser.rs", 300, 10),
    ];

    assert_eq!(
        paths(&files, review_order(&files)),
        ["src/lexer.rs", "src/parser.rs", "tests/parser.rs"]
    );
    assert_eq!(
        collapsed(&files),
        [(
            "src/parser.rs",
            InitialState {
                generated: None,
                large: true,
                binary: false,
            }
        )]
    );
}
