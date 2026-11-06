//! Comparing stays fast on a large file.

use std::time::{Duration, Instant};

use diff_extras::syntax::{Fallback, Limits};

use crate::support::{assert_every_line_once, compare_with};

/// How many functions the large file has, each 5 lines long.
const FUNCTIONS: usize = 2_000;

/// Function `n` of the large file, with `edited` changes: a renamed variable, a changed number
/// and an added statement.
fn function(n: usize, edited: bool) -> String {
    let (name, limit, extra) = if edited {
        ("subtotal", n + 1, format!("    log(\"function {n}\");\n"))
    } else {
        ("total", n, String::new())
    };
    format!(
        "fn function_{n}(items: &[Item]) -> u64 {{\n    \
             let {name}: u64 = items.iter().map(|item| item.price * {limit}).sum();\n\
         {extra}    \
             {name} + tax({name})\n\
         }}\n\n"
    )
}

/// A 10,000-line file, and the same with every tenth function edited, one deleted and one added.
fn large_file() -> (String, String) {
    let old: String = (0..FUNCTIONS).map(|n| function(n, false)).collect();
    let new: String = (0..FUNCTIONS)
        .filter(|n| *n != 500)
        .map(|n| function(n, n % 10 == 3))
        .chain([function(FUNCTIONS, false)])
        .collect();
    (old, new)
}

#[test]
fn a_large_file_with_scattered_changes() {
    let (old, new) = large_file();

    let start = Instant::now();
    let diff = compare_with("rust", &old, &new, &Limits::default()).unwrap();
    let elapsed = start.elapsed();

    assert_every_line_once(&diff, &old, &new);
    let changed = diff
        .rows
        .iter()
        .filter(|row| {
            row.old.as_ref().is_some_and(|line| !line.novel.is_empty())
                || row.new.as_ref().is_some_and(|line| !line.novel.is_empty())
        })
        .count();
    // Each edited function changes 3 lines; a deleted or added one, its 4 lines with code.
    assert_eq!(changed, (FUNCTIONS / 10) * 3 + 4 + 4);

    // Debug builds are much slower; CI runs the tests in one.
    let limit = if cfg!(debug_assertions) {
        Duration::from_secs(5)
    } else {
        Duration::from_millis(500)
    };
    eprintln!(
        "compared {} and {} lines in {elapsed:?}",
        old.lines().count(),
        new.lines().count()
    );
    assert!(elapsed < limit, "took {elapsed:?}");
}

#[test]
fn a_rewritten_file_gives_up_at_the_graph_limit() {
    // Every function changed in every token but its punctuation: past difftastic's graph limit.
    let old: String = (0..400)
        .map(|n| format!("fn old_{n}(a: A{n}) -> B{n} {{ call_{n}(a, {n}) }}\n"))
        .collect();
    let new: String = (0..400)
        .map(|n| {
            format!(
                "fn new_{n}(x: X{n}) -> Y{n} {{ other_{n}(x, {}) }}\n",
                n + 1
            )
        })
        .collect();

    let start = Instant::now();
    let result = compare_with("rust", &old, &new, &Limits::default());
    let elapsed = start.elapsed();

    eprintln!("gave up on {} lines in {elapsed:?}", old.lines().count());
    assert_eq!(result, Err(Fallback::TooLarge));
    // Reaching difftastic's limit takes a while, as in difftastic.
    let limit = if cfg!(debug_assertions) {
        Duration::from_secs(30)
    } else {
        Duration::from_secs(3)
    };
    assert!(elapsed < limit, "took {elapsed:?}");
}
