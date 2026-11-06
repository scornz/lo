//! Rule 3: a PR sits on the open PR whose head commit it contains, the nearest one if several.

use stacks::{Parent, Rule};

use crate::support::{With, assert_forest, pr};

#[test]
fn a_pr_containing_another_prs_head_sits_on_it() {
    let stacks = assert_forest(
        &[
            pr(1, "main", "a").commits(&["a.1", "a.2"]),
            pr(2, "main", "b").commits(&["a.1", "a.2", "b.1"]),
        ],
        &["main", "  #1", "    #2 shared-commits"],
    );
    let layer = stacks.layer(2).unwrap();
    assert_eq!(layer.parent, Parent::Pr(1));
    assert_eq!(layer.rule, Some(Rule::SharedCommits));
}

#[test]
fn the_nearest_pr_below_is_the_parent() {
    // #3 contains both heads; #4's is closer to #3's head, though #5 is newer.
    assert_forest(
        &[
            pr(5, "main", "x").commits(&["x.1"]),
            pr(4, "main", "y").commits(&["x.1", "y.1"]),
            pr(3, "main", "z").commits(&["x.1", "y.1", "z.1"]),
        ],
        &[
            "main",
            "  #5",
            "    #4 shared-commits",
            "      #3 shared-commits",
        ],
    );
}

#[test]
fn a_head_before_the_commits_listed_links_nothing() {
    // Only #2's recent commits are known, and #1's head is older than all of them.
    assert_forest(
        &[
            pr(1, "main", "a").commits(&["a.1"]),
            pr(2, "main", "b").commits(&["b.1", "b.2"]),
        ],
        &["main", "  #1", "  #2"],
    );
}

#[test]
fn a_pr_with_the_same_head_commit_is_not_below() {
    // #2's branch has no commits of its own yet.
    assert_forest(
        &[
            pr(1, "main", "a").commits(&["a.1"]),
            pr(2, "release", "b").commits(&["a.1"]),
        ],
        &["main", "  #1", "release", "  #2"],
    );
}

#[test]
fn of_two_prs_with_the_nearest_head_the_newest_is_below() {
    assert_forest(
        &[
            pr(1, "main", "a").commits(&["a.1"]),
            pr(2, "release", "a-backport").commits(&["a.1"]),
            pr(3, "main", "c").commits(&["a.1", "c.1"]),
        ],
        &["main", "  #1", "release", "  #2", "    #3 shared-commits"],
    );
}

#[test]
fn links_to_a_fork_head() {
    assert_forest(
        &[
            pr(1, "main", "feature").fork().commits(&["f.1"]),
            pr(2, "main", "more").fork().commits(&["f.1", "m.1"]),
        ],
        &["main", "  #1", "    #2 shared-commits"],
    );
}

#[test]
fn base_branch_wins_over_shared_commits() {
    // #3 targets #1's branch but also contains #2's head.
    assert_forest(
        &[
            pr(1, "main", "a").commits(&["a.1"]),
            pr(2, "main", "b").commits(&["b.1"]),
            pr(3, "a", "c").commits(&["b.1", "c.1"]),
        ],
        &["main", "  #1", "    #3 base-branch", "  #2"],
    );
}
