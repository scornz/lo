//! How lines are aligned into rows.

use crate::support::rows;

#[test]
fn identical_files_pair_every_line() {
    let text = "fn main() {\n    println!(\"hi\");\n}\n";

    assert_eq!(
        rows("rust", text, text),
        [
            "1 fn main() { | 1 fn main() {",
            "2     println!(\"hi\"); | 2     println!(\"hi\");",
            "3 } | 3 }",
        ]
    );
}

#[test]
fn an_added_line_has_no_old_line() {
    let old = "fn f() {\n    a();\n    c();\n}\n";
    let new = "fn f() {\n    a();\n    b();\n    c();\n}\n";

    assert_eq!(
        rows("rust", old, new),
        [
            "1 fn f() { | 1 fn f() {",
            "2     a(); | 2     a();",
            "- | 3     [b();]",
            "3     c(); | 4     c();",
            "4 } | 5 }",
        ]
    );
}

#[test]
fn a_new_file_has_only_new_lines() {
    let new = "fn f() {}\n";

    assert_eq!(rows("rust", "", new), ["- | 1 [fn] [f()] [{}]"]);
}

#[test]
fn a_deleted_file_has_only_old_lines() {
    let old = "fn f() {}\n// done\n";

    assert_eq!(
        rows("rust", old, ""),
        ["1 [fn] [f()] [{}] | -", "2 [// done] | -"]
    );
}

#[test]
fn blank_lines_before_a_matched_line_pair_up() {
    let old = "fn a() {}\n\n\nfn b() {}\n";
    let new = "fn a() {}\nfn x() {}\n\n\nfn b() {}\n";

    assert_eq!(
        rows("rust", old, new),
        [
            "1 fn a() {} | 1 fn a() {}",
            "- | 2 [fn] [x()] [{}]",
            "2 | 3",
            "3 | 4",
            "4 fn b() {} | 5 fn b() {}",
        ]
    );
}

#[test]
fn a_token_split_over_lines_pairs_its_first_line() {
    let old = "f(a, b);\n";
    let new = "f(\n    a,\n    b,\n);\n";

    assert_eq!(
        rows("rust", old, new),
        [
            "1 f(a, b); | 1 f(",
            "- | 2     a,",
            "- | 3     b,",
            "- | 4 );"
        ]
    );
}

#[test]
fn novel_ranges_are_byte_ranges_within_the_line() {
    let old = "let a = 0;\nlet t = (\"é\", 1);\n";
    let new = "let a = 0;\nlet t = (\"é\", 2);\n";

    let diff = crate::support::compare_with("rust", old, new, &Default::default()).unwrap();

    let line = diff.rows[1].new.as_ref().unwrap();
    assert_eq!(line.number, 2);
    // `é` takes two bytes, so the `2` is at byte 15, character 14.
    assert_eq!(line.novel.len(), 1);
    assert_eq!(line.novel[0], 15..16);
}

#[test]
fn crlf_line_endings_are_not_part_of_a_line() {
    let old = "let x = 1;\r\nlet y = 2;\r\n";
    let new = "let x = 1;\r\nlet y = 3;\r\n";

    assert_eq!(
        rows("rust", old, new),
        [
            "1 let x = 1; | 1 let x = 1;",
            "2 let y = [2]; | 2 let y = [3];"
        ]
    );
}
