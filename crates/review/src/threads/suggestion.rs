//! Suggested changes (design §8, "Suggestions"): a comment whose `suggestion` block holds the lines
//! as they should read, which the pull request's author can apply on GitHub.

use super::{DiffSide, Lines};

impl Lines {
    /// Whether a comment on these lines can suggest a change: GitHub applies suggestions to the
    /// head's lines only, so every line must be on the right side.
    pub fn takes_suggestion(&self) -> bool {
        self.first().side == DiffSide::Right && self.end.side == DiffSide::Right
    }
}

/// A `suggestion` block holding `lines`, the selected lines' text without line endings, for the
/// comment box to open with. A `\r` left at the end of a line, from a file with Windows line
/// endings, is dropped. Its fence is longer than any run of backticks that starts a line, so lines
/// with fences of their own, such as Markdown's, stay inside it. No lines suggests deleting the
/// selected ones.
pub fn suggestion_block<S: AsRef<str>>(lines: &[S]) -> String {
    let longest = (lines.iter())
        .map(|line| {
            let line = line.as_ref().trim_start();
            line.len() - line.trim_start_matches('`').len()
        })
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    let mut block = format!("{fence}suggestion\n");
    for line in lines {
        let line = line.as_ref();
        block.push_str(line.strip_suffix('\r').unwrap_or(line));
        block.push('\n');
    }
    block.push_str(&fence);
    block
}

#[cfg(test)]
mod tests {
    use super::super::Line;
    use super::*;

    #[test]
    fn the_block_holds_the_selected_lines() {
        assert_eq!(
            suggestion_block(&["    let total = 1;", "    total + 1"]),
            "```suggestion\n    let total = 1;\n    total + 1\n```"
        );
    }

    #[test]
    fn a_blank_selected_line_stays_in_the_block() {
        assert_eq!(suggestion_block(&[""]), "```suggestion\n\n```");
    }

    #[test]
    fn no_lines_suggest_deleting_the_selection() {
        assert_eq!(suggestion_block::<&str>(&[]), "```suggestion\n```");
    }

    #[test]
    fn lines_with_their_own_fences_get_a_longer_one() {
        assert_eq!(
            suggestion_block(&["```rust", "fn main() {}", "```"]),
            "````suggestion\n```rust\nfn main() {}\n```\n````"
        );
        assert_eq!(
            suggestion_block(&["  `````", "x"]),
            "``````suggestion\n  `````\nx\n``````"
        );
    }

    #[test]
    fn lines_keep_no_carriage_return() {
        // Lines of a file with Windows line endings, split on `\n` only.
        assert_eq!(
            suggestion_block(&["a\r", "b\r"]),
            "```suggestion\na\nb\n```"
        );
    }

    #[test]
    fn a_fence_after_a_tab_counts_and_a_tilde_fence_does_not() {
        assert_eq!(
            suggestion_block(&["\t```", "x"]),
            "````suggestion\n\t```\nx\n````"
        );
        assert_eq!(
            suggestion_block(&["~~~", "x"]),
            "```suggestion\n~~~\nx\n```"
        );
    }

    #[test]
    fn backticks_inside_a_line_keep_the_usual_fence() {
        assert_eq!(
            suggestion_block(&["let fence = \"```\";"]),
            "```suggestion\nlet fence = \"```\";\n```"
        );
    }

    #[test]
    fn only_the_heads_lines_take_a_suggestion() {
        assert!(Lines::one(Line::right(4)).takes_suggestion());
        assert!(Lines::range(Line::right(2), Line::right(4)).takes_suggestion());
        assert!(!Lines::one(Line::left(4)).takes_suggestion());
        assert!(!Lines::range(Line::left(3), Line::right(4)).takes_suggestion());
    }
}
