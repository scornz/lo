// Ported from difftastic (https://github.com/Wilfred/difftastic), commit 87aad1a:
//   src/words.rs
//   src/parse/syntax.rs (split_atom_words)
//
// MIT License
//
// Copyright (c) 2021-2025 Wilfred Hughes
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

//! Compares a replaced comment or string word by word. When enough of its words are on the other
//! side, only the words that aren't are novel: in `"hello brave world"` against `"hello bold
//! world"`, just `brave`.

use std::ops::Range;

use super::lcs::{self, DiffResult};

/// A word of a comment or string, as a byte range within it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Word {
    /// Only on this side.
    Novel(Range<usize>),
    /// On both sides, at `opposite` in the other side's text.
    Unchanged {
        range: Range<usize>,
        opposite: Range<usize>,
    },
}

/// The words of `text` compared with `opposite`'s, or `None` if they share too few to show word
/// by word. Whitespace is never novel.
pub(crate) fn split_atom_words(text: &str, opposite: &str) -> Option<Vec<Word>> {
    let words = split_words_and_numbers(text);
    let opposite_words = split_words_and_numbers(opposite);
    let diff = lcs::slice(&words, &opposite_words);
    if !has_common_words(&diff, &words) {
        return None;
    }
    let ranges = byte_ranges(&words);
    let opposite_ranges = byte_ranges(&opposite_words);
    let mut result = Vec::new();
    for step in diff {
        match step {
            DiffResult::Left(i) if !words[i].trim().is_empty() => {
                result.push(Word::Novel(ranges[i].clone()));
            }
            DiffResult::Both(i, j) => result.push(Word::Unchanged {
                range: ranges[i].clone(),
                opposite: opposite_ranges[j].clone(),
            }),
            DiffResult::Left(_) | DiffResult::Right(_) => {}
        }
    }
    Some(result)
}

/// Each word's byte range, the words being consecutive.
fn byte_ranges(words: &[&str]) -> Vec<Range<usize>> {
    let mut start = 0;
    words
        .iter()
        .map(|word| {
            let range = start..start + word.len();
            start = range.end;
            range
        })
        .collect()
}

/// Whether enough words are on both sides to show the changed ones alone: more than two that
/// aren't a space, since the delimiters of a comment or string are words too, and at least half
/// as many as there are novel words on either side. Each changed word makes a novel word on each
/// side, but an unchanged one counts once.
fn has_common_words(diff: &[DiffResult], words: &[&str]) -> bool {
    let mut novel = 0;
    let mut unchanged = 0;
    for step in diff {
        match step {
            DiffResult::Both(i, _) => {
                if words[*i] != " " {
                    unchanged += 1;
                }
            }
            DiffResult::Left(_) | DiffResult::Right(_) => novel += 1,
        }
    }
    unchanged > 2 && unchanged * 2 >= novel
}

/// `text` split into words and numbers, and each other character on its own: `"foo..bar23"` is
/// `["foo", ".", ".", "bar", "23"]`.
pub(crate) fn split_words_and_numbers(text: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let mut word_start: Option<(usize, char)> = None;
    for (at, c) in text.char_indices() {
        let in_word = c.is_alphanumeric() || c == '_';
        match word_start {
            Some((start, start_char)) if in_word => {
                // A word and a number next to each other are two words.
                if c.is_ascii_digit() != start_char.is_ascii_digit() {
                    words.push(&text[start..at]);
                    word_start = Some((at, c));
                }
            }
            Some((start, _)) => {
                words.push(&text[start..at]);
                words.push(&text[at..at + c.len_utf8()]);
                word_start = None;
            }
            None if in_word => word_start = Some((at, c)),
            None => words.push(&text[at..at + c.len_utf8()]),
        }
    }
    if let Some((start, _)) = word_start {
        words.push(&text[start..]);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_words_and_numbers() {
        assert_eq!(split_words_and_numbers("a123b"), ["a", "123", "b"]);
    }

    #[test]
    fn test_split_words_and_numbers_spaces() {
        assert_eq!(split_words_and_numbers("foo bar"), ["foo", " ", "bar"]);
    }

    #[test]
    fn splits_punctuation_and_unicode() {
        assert_eq!(
            split_words_and_numbers("example..com"),
            ["example", ".", ".", "com"]
        );
        assert_eq!(
            split_words_and_numbers("a xöy 💝\nb"),
            ["a", " ", "xöy", " ", "💝", "\n", "b"]
        );
    }

    #[test]
    fn test_split_atom_words() {
        let words = split_atom_words("abc def ghi novel", "abc def ghi").unwrap();

        let unchanged = |range: Range<usize>| Word::Unchanged {
            range: range.clone(),
            opposite: range,
        };
        assert_eq!(
            words,
            [
                unchanged(0..3),
                unchanged(3..4),
                unchanged(4..7),
                unchanged(7..8),
                unchanged(8..11),
                Word::Novel(12..17),
            ]
        );
    }

    #[test]
    fn too_few_common_words_is_none() {
        assert_eq!(split_atom_words("\"abc\"", "\"xyz\""), None);
        assert_eq!(
            split_atom_words("// one two three", "// four five six"),
            None
        );
    }
}
