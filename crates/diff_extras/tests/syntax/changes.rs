//! What the syntax-aware view shows for common kinds of change.

use crate::support::rows;

#[test]
fn a_renamed_variable_marks_only_the_name() {
    let old = "\
fn total(items: &[u32]) -> u32 {
    let sum = items.iter().sum();
    sum
}
";
    let new = "\
fn total(items: &[u32]) -> u32 {
    let subtotal = items.iter().sum();
    subtotal
}
";

    assert_eq!(
        rows("rust", old, new),
        [
            "1 fn total(items: &[u32]) -> u32 { | 1 fn total(items: &[u32]) -> u32 {",
            "2     let [sum] = items.iter().sum(); | 2     let [subtotal] = items.iter().sum();",
            "3     [sum] | 3     [subtotal]",
            "4 } | 4 }",
        ]
    );
}

#[test]
fn a_reordered_argument_list_marks_the_moved_argument() {
    let old = "draw(canvas, width, height);\n";
    let new = "draw(height, canvas, width);\n";

    assert_eq!(
        rows("typescript", old, new),
        ["1 draw(canvas, width[,] [height]); | 1 draw([height,] canvas, width);"]
    );
}

#[test]
fn a_reindented_block_has_no_changes() {
    let old = "\
fn f() {
    if ready {
        go();
    }
}
";
    let new = "\
fn f() {
  if ready {
      go();
  }
}
";

    assert_eq!(
        rows("rust", old, new),
        [
            "1 fn f() { | 1 fn f() {",
            "2     if ready { | 2   if ready {",
            "3         go(); | 3       go();",
            "4     } | 4   }",
            "5 } | 5 }",
        ]
    );
}

#[test]
fn a_moved_function_is_deleted_in_one_place_and_added_in_another() {
    let old = "\
fn first() {
    one();
}

fn second() {
    two();
}
";
    let new = "\
fn second() {
    two();
}

fn first() {
    one();
}
";

    assert_eq!(
        rows("rust", old, new),
        [
            "1 [fn] [first()] [{] | -",
            "2     [one();] | -",
            "3 [}] | -",
            "4 | -",
            "5 fn second() { | 1 fn second() {",
            "6     two(); | 2     two();",
            "7 } | 3 }",
            "- | 4",
            "- | 5 [fn] [first()] [{]",
            "- | 6     [one();]",
            "- | 7 [}]",
        ]
    );
}

#[test]
fn a_changed_json_value() {
    let old = "{\n  \"name\": \"rozed\",\n  \"version\": 1\n}\n";
    let new = "{\n  \"name\": \"rozed\",\n  \"version\": 2\n}\n";

    assert_eq!(
        rows("json", old, new),
        [
            "1 { | 1 {",
            "2   \"name\": \"rozed\", | 2   \"name\": \"rozed\",",
            "3   \"version\": [1] | 3   \"version\": [2]",
            "4 } | 4 }",
        ]
    );
}

#[test]
fn a_trailing_comma_alone_is_no_change() {
    let old = "const xs = [1, 2];\n";
    let new = "const xs = [1, 2,];\n";

    assert_eq!(
        rows("typescript", old, new),
        ["1 const xs = [1, 2]; | 1 const xs = [1, 2,];"]
    );
}

#[test]
fn an_added_argument_in_tsx() {
    let old = "render(<App />);\n";
    let new = "render(<App />, root);\n";

    assert_eq!(
        rows("tsx", old, new),
        ["1 render(<App />); | 1 render(<App />[,] [root]);"]
    );
}
