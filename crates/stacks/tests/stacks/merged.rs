//! Merged PRs aren't layers, but they still link the layers above them.

use stacks::{NeedsRebase, Parent, Rule};

use crate::support::{With, assert_forest, pr};

#[test]
fn a_layer_on_a_squash_merged_pr_sits_on_its_root_and_needs_a_rebase() {
    let stacks = assert_forest(
        &[
            pr(1, "main", "a").commits(&["a.1"]).merged(false),
            pr(2, "a", "b").commits(&["b.1"]),
        ],
        &["main", "  #2 base-branch needs-rebase(#1 a.1)"],
    );
    let layer = stacks.layer(2).unwrap();
    assert_eq!(layer.parent, Parent::Branch("main".into()));
    assert_eq!(layer.rule, Some(Rule::BaseBranch));
    assert_eq!(
        layer.needs_rebase,
        Some(NeedsRebase {
            merged: 1,
            compare_against: "a.1".into(),
        })
    );
}

#[test]
fn a_layer_retargeted_after_a_squash_merge_still_needs_a_rebase() {
    // GitHub pointed #2 at main when #1's branch was deleted. It still has #1's commits.
    assert_forest(
        &[
            pr(1, "main", "a").commits(&["a.1", "a.2"]).merged(false),
            pr(2, "main", "b").commits(&["a.1", "a.2", "b.1"]),
        ],
        &["main", "  #2 shared-commits needs-rebase(#1 a.2)"],
    );
}

#[test]
fn a_layer_cut_before_a_squashed_layers_last_commit_still_links() {
    // #2 was cut from a.1, then #1 got a.2 and was squash-merged, and GitHub pointed #2 at main.
    assert_forest(
        &[
            pr(1, "main", "a").commits(&["a.1", "a.2"]).merged(false),
            pr(2, "main", "b").commits(&["a.1", "b.1"]),
        ],
        &["main", "  #2 shared-commits needs-rebase(#1 a.1)"],
    );
}

#[test]
fn a_layer_is_compared_against_the_newest_commit_it_has() {
    // As above, but #2 still targets `a`, which ends at a.2.
    assert_forest(
        &[
            pr(1, "main", "a").commits(&["a.1", "a.2"]).merged(false),
            pr(2, "a", "b").commits(&["a.1", "b.1"]).base_at("a.2"),
        ],
        &["main", "  #2 base-branch needs-rebase(#1 a.1)"],
    );
}

#[test]
fn a_rebase_merged_pr_matches_on_its_commits_too() {
    // Rebase-merging copies #1's commits, so its own aren't in main either.
    assert_forest(
        &[
            pr(1, "main", "a")
                .commits(&["a.1", "a.2", "a.3"])
                .merged(false),
            pr(2, "main", "b").commits(&["a.1", "a.2", "b.1"]),
        ],
        &["main", "  #2 shared-commits needs-rebase(#1 a.2)"],
    );
}

#[test]
fn a_commit_several_merged_prs_list_belongs_to_the_oldest() {
    // #2 was stacked on #1 by commits, so it lists a.1 too. #3 was cut from #1.
    assert_forest(
        &[
            pr(1, "main", "a").commits(&["a.1"]).merged(false),
            pr(2, "main", "b").commits(&["a.1", "b.1"]).merged(false),
            pr(3, "main", "c").commits(&["a.1", "c.1"]),
        ],
        &["main", "  #3 shared-commits needs-rebase(#1 a.1)"],
    );
}

#[test]
fn a_merged_layer_links_past_its_own_commits() {
    // #2, stacked on #1 by commits, was squash-merged; #3 was cut from #2.
    assert_forest(
        &[
            pr(1, "main", "a").commits(&["a.1"]),
            pr(2, "main", "m")
                .commits(&["a.1", "m.1", "m.2"])
                .merged(false),
            pr(3, "main", "c").commits(&["a.1", "m.1", "m.2", "c.1"]),
        ],
        &["main", "  #1", "    #3 shared-commits needs-rebase(#2 m.2)"],
    );
}

#[test]
fn layers_above_the_rebased_layer_stay_on_it() {
    assert_forest(
        &[
            pr(1, "main", "a").merged(false),
            pr(2, "a", "b"),
            pr(3, "b", "c"),
        ],
        &[
            "main",
            "  #2 base-branch needs-rebase(#1 a.1)",
            "    #3 base-branch",
        ],
    );
}

#[test]
fn a_layer_on_a_merge_committed_pr_needs_no_rebase() {
    let stacks = assert_forest(
        &[pr(1, "main", "a").merged(true), pr(2, "a", "b")],
        &["main", "  #2 base-branch"],
    );
    assert_eq!(stacks.layer(2).unwrap().needs_rebase, None);
}

#[test]
fn a_branch_that_moved_on_after_its_pr_merged_is_a_root() {
    // `develop` was merged into main in #1 and has had more commits since.
    assert_forest(
        &[
            pr(1, "main", "develop").merged(true),
            pr(2, "develop", "b").base_at("develop.2"),
        ],
        &["develop", "  #2"],
    );
}

#[test]
fn a_deleted_base_branch_links_to_no_merged_pr() {
    assert_forest(
        &[
            pr(1, "main", "a").merged(false),
            pr(2, "a", "b").base_deleted(),
        ],
        &["a", "  #2"],
    );
}

#[test]
fn a_chain_of_merged_prs_links_whatever_their_tips() {
    // #2 was squash-merged into `a`, which moved `a` on, then #1 was squash-merged into main.
    assert_forest(
        &[
            pr(1, "main", "a").commits(&["a.1", "a.sq"]).merged(false),
            pr(2, "a", "b").base_at("a.1").merged(false),
            pr(3, "b", "c"),
        ],
        &["main", "  #3 base-branch needs-rebase(#2 b.1)"],
    );
}

#[test]
fn a_chain_of_merge_committed_prs_needs_no_rebase() {
    assert_forest(
        &[
            pr(1, "main", "a").commits(&["a.1", "a.mc"]).merged(true),
            pr(2, "a", "b").base_at("a.1").merged(true),
            pr(3, "b", "c"),
        ],
        &["main", "  #3 base-branch"],
    );
}

#[test]
fn a_merge_committed_head_is_just_history() {
    // #1's head is in main, so #2, built on main but aimed at release, isn't above it.
    assert_forest(
        &[
            pr(1, "main", "a").commits(&["a.1"]).merged(true),
            pr(2, "release", "b").commits(&["a.1", "b.1"]),
        ],
        &["release", "  #2"],
    );
}

#[test]
fn a_layer_on_a_pr_merged_into_another_layer_sits_on_that_layer() {
    // #2 was squash-merged into #1's branch.
    assert_forest(
        &[
            pr(1, "main", "a"),
            pr(2, "a", "m").merged(false),
            pr(3, "m", "b"),
        ],
        &["main", "  #1", "    #3 base-branch needs-rebase(#2 m.1)"],
    );
}

#[test]
fn after_two_squash_merges_the_nearest_merged_head_is_compared_against() {
    // #1 then #2 were squash-merged into main. #3 still has both PRs' commits.
    assert_forest(
        &[
            pr(1, "main", "a").commits(&["a.1"]).merged(false),
            pr(2, "main", "b").commits(&["a.1", "b.1"]).merged(false),
            pr(3, "b", "c").commits(&["a.1", "b.1", "c.1"]),
        ],
        &["main", "  #3 base-branch needs-rebase(#2 b.1)"],
    );
}

#[test]
fn an_open_pr_wins_over_a_merged_one_from_the_same_branch() {
    // Branch `a` was merged in #2, then reused for #1.
    assert_forest(
        &[
            pr(1, "main", "a").created(1),
            pr(2, "main", "a").created(2).merged(false),
            pr(3, "a", "b"),
        ],
        &["main", "  #1", "    #3 base-branch"],
    );
}

#[test]
fn a_github_stack_layer_on_a_merged_layer_needs_no_rebase() {
    // GitHub rebases a stack's layers itself when a lower one merges.
    let stacks = assert_forest(
        &[
            pr(1, "main", "a").native(100, 1).merged(false),
            pr(2, "main", "b").native(100, 2),
            pr(3, "b", "c").native(100, 3),
        ],
        &["main", "  #2 native-stack", "    #3 native-stack"],
    );
    assert_eq!(stacks.layer(2).unwrap().needs_rebase, None);
}
