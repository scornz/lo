//! The forest doesn't depend on the order PRs are passed in.

use stacks::{PullRequest, Stacks};

use crate::support::{With, pr};

/// PRs that exercise every rule, the tie-breaks between equally good matches, and cycles.
fn tangled_repo() -> Vec<PullRequest> {
    vec![
        // A deploy PR from a trunk, which nothing sits on.
        pr(1, "production", "main"),
        // A base-branch chain with two children created at the same time.
        pr(2, "main", "a"),
        pr(3, "a", "b").created(50),
        pr(4, "a", "c").created(50),
        // Two open PRs from one branch, with the same head commit, and a layer on each tie.
        pr(5, "main", "d").created(60),
        pr(6, "release", "d").created(60),
        pr(7, "d", "e"),
        pr(8, "main", "f").commits(&["d.1", "f.1"]),
        // Squash-merged layers that list each other's commits, and a layer cut from the lower.
        pr(9, "main", "g").commits(&["g.1", "g.2"]).merged(false),
        pr(10, "main", "h")
            .commits(&["g.1", "g.2", "h.1"])
            .merged(false),
        pr(11, "main", "i").commits(&["g.1", "i.1"]),
        // A GitHub stack with a closed middle layer.
        pr(12, "main", "j").native(500, 1),
        pr(13, "j", "k").native(500, 2).closed(),
        pr(14, "k", "l").native(500, 3),
        // Cycles: two open PRs, three open PRs, and one through a merged PR.
        pr(15, "n", "m").created(90),
        pr(16, "m", "n").created(80),
        pr(17, "q", "o"),
        pr(18, "o", "p"),
        pr(19, "p", "q"),
        pr(20, "s", "r").merged(false),
        pr(21, "r", "s"),
        // A fork PR, and a layer whose base is a fork branch's name.
        pr(22, "main", "t").fork().commits(&["t.1"]),
        pr(23, "t", "u").commits(&["t.1", "u.1"]),
        // A duplicate number: the open copy wins wherever it's passed.
        pr(24, "main", "v").closed(),
        pr(24, "main", "v"),
    ]
}

/// Shuffles `prs` with a seeded linear congruential generator, so runs repeat.
fn shuffled(prs: &[PullRequest], seed: u64) -> Vec<PullRequest> {
    let mut prs = prs.to_vec();
    let mut state = seed;
    for i in (1..prs.len()).rev() {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        prs.swap(i, (state >> 33) as usize % (i + 1));
    }
    prs
}

#[test]
fn the_forest_is_the_same_in_any_input_order() {
    let prs = tangled_repo();
    let trunks = ["main", "production"];
    let expected = Stacks::find(&prs, &trunks);
    assert_eq!(expected.cycles().len(), 3);
    assert_eq!(expected.layer(4).unwrap().depth, 1);
    assert!(expected.layer(24).is_some());

    for seed in 0..200 {
        let prs = shuffled(&prs, seed);
        assert_eq!(Stacks::find(&prs, &trunks), expected, "seed {seed}");
    }
}
