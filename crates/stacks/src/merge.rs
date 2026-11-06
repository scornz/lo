//! Merges a chain that can't become a GitHub stack one layer at a time (design §9, "Stacks"): its
//! bottom layer merges, and the layers on it move to the root branch and need a rebase.

use crate::{Retarget, Rule, Stacks};

/// Merging a stack's bottom layer. Run it in field order: move the bottom layer onto the root branch
/// if it needs to, merge it, then move the layers on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeBottom {
    /// The stack's bottom layer: the only one that merges now.
    pub pr: u64,
    /// The root branch it merges into.
    pub into: String,
    /// Moves it onto the root branch before it merges, when its base branch is still a merged pull
    /// request's.
    pub retarget_before: Option<Retarget>,
    /// Once it has merged, moves each layer on it onto the root branch, oldest first. A layer
    /// already based there needs no change.
    pub retarget_after: Vec<Retarget>,
    /// The layers on it, oldest first, to mark "needs rebase" once it has merged: they still carry
    /// its commits, which a squash or rebase merge leaves out of the root branch.
    pub needs_rebase: Vec<u64>,
}

/// Plans merging the stack under `layer` one layer at a time, for a chain that can't become a
/// GitHub stack: its bottom layer merges now, and every layer on it moves to the root branch, the
/// one on the way to `layer` and any others. `layer` can be any layer of the stack. `None` when it
/// isn't a layer.
///
/// A layer on it that's in the same GitHub stack is left out: GitHub moves and rebases a stack's
/// own layers itself when a lower one merges. Whether the bottom layer can merge, including
/// whether it needs a rebase itself ([`Layer::needs_rebase`](crate::Layer)), isn't checked here.
pub fn merge_one_layer_at_a_time(stacks: &Stacks, layer: u64) -> Option<MergeBottom> {
    let stack = stacks.stack_of(layer)?;
    // The walk starts at the bottom layer.
    let bottom = stacks.layer(*stack.layers.first()?)?;
    let into = stack.root.clone();
    let retarget_before = bottom.retarget_to.clone().map(|to| Retarget {
        pr: bottom.number,
        from: stacks.prs[&bottom.number].base_ref.clone(),
        to,
    });
    let mut retarget_after = Vec::new();
    let mut needs_rebase = Vec::new();
    for &above in &bottom.children {
        if stacks.layer(above)?.rule == Some(Rule::NativeStack) {
            continue;
        }
        let from = &stacks.prs[&above].base_ref;
        if *from != into {
            retarget_after.push(Retarget {
                pr: above,
                from: from.clone(),
                to: into.clone(),
            });
        }
        needs_rebase.push(above);
    }
    Some(MergeBottom {
        pr: bottom.number,
        into,
        retarget_before,
        retarget_after,
        needs_rebase,
    })
}
