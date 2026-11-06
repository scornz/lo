//! Trunk branches, such as the default branch and protected ones, are always roots: a PR from a
//! trunk is never below another PR, by any rule.

use stacks::Parent;

use crate::support::{With, assert_trunk_forest, pr};

#[test]
fn an_open_pr_from_a_trunk_is_never_below() {
    // A deploy PR from main into production.
    let stacks = assert_trunk_forest(
        &[
            pr(100, "production", "main").commits(&["m.1", "m.2"]),
            pr(1, "main", "f1"),
            pr(2, "main", "f2"),
            pr(3, "main", "f3"),
        ],
        &["main", "production"],
        &["main", "  #1", "  #2", "  #3", "production", "  #100"],
    );
    assert_eq!(
        stacks.layer(1).unwrap().parent,
        Parent::Branch("main".into())
    );
}

#[test]
fn a_merged_pr_from_a_trunk_is_never_below() {
    // #90 merged main into production, and main hasn't moved since.
    assert_trunk_forest(
        &[
            pr(90, "production", "main")
                .commits(&["x.1", "main.1"])
                .merged(true),
            pr(1, "main", "f1"),
        ],
        &["main", "production"],
        &["main", "  #1"],
    );
}

#[test]
fn a_trunk_head_among_a_prs_commits_links_nothing() {
    // #2 is aimed at release but built on main's tip, which #1 is deploying.
    assert_trunk_forest(
        &[
            pr(1, "production", "main").commits(&["m.1"]),
            pr(2, "release", "b").commits(&["m.1", "b.1"]),
        ],
        &["main"],
        &["production", "  #1", "release", "  #2"],
    );
}

#[test]
fn a_back_merge_pr_is_never_below() {
    // Gitflow: develop is the default branch, and main's hotfixes are merged back into it.
    assert_trunk_forest(
        &[
            pr(50, "develop", "main").commits(&["h.1"]),
            pr(51, "main", "hotfix-1"),
            pr(52, "main", "hotfix-2"),
            pr(60, "develop", "feat-1"),
        ],
        &["develop", "main"],
        &["develop", "  #50", "  #60", "main", "  #51", "  #52"],
    );
}

#[test]
fn a_release_and_its_back_merge_stay_on_their_trunks() {
    // Without trunks, #40 and #41 would target each other's head branch.
    let stacks = assert_trunk_forest(
        &[
            pr(40, "main", "develop").commits(&["d.1"]),
            pr(41, "develop", "main").commits(&["h.1"]),
            pr(50, "develop", "feat-1"),
            pr(51, "develop", "feat-2"),
            pr(60, "main", "hotfix-1"),
        ],
        &["main", "develop"],
        &[
            "main", "  #40", "  #60", "develop", "  #41", "  #50", "  #51",
        ],
    );
    assert!(stacks.cycles().is_empty());
}

#[test]
fn a_forks_branch_named_like_a_trunk_is_not_a_trunk() {
    // A contributor's PRs from their fork's main, the second built on the first.
    assert_trunk_forest(
        &[
            pr(1, "main", "main").fork().commits(&["f.1"]),
            pr(2, "main", "more").fork().commits(&["f.1", "f.2"]),
        ],
        &["main"],
        &["main", "  #1", "    #2 shared-commits"],
    );
}

#[test]
fn roots_list_trunks_first_in_the_order_given() {
    assert_trunk_forest(
        &[
            pr(1, "zeta", "a"),
            pr(2, "develop", "b"),
            pr(3, "alpha", "c"),
            pr(4, "main", "d"),
        ],
        &["main", "release", "develop"],
        &[
            "main", "  #4", "develop", "  #2", "alpha", "  #3", "zeta", "  #1",
        ],
    );
}
