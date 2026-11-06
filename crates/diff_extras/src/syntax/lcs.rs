// Ported from difftastic (https://github.com/Wilfred/difftastic), commit 87aad1a:
//   src/diff/lcs_diff.rs (its interface and tests; the algorithm is Myers's)
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

//! A diff of two sequences: the items both share, in order, and those only on one side.
//!
//! difftastic uses the imara-diff crate's histogram diff. This is Myers's algorithm, which finds
//! a shortest edit script, after skipping the items the sequences start and end with. Past
//! [`MAX_EDITS`] it gives up and calls everything in between changed, which is still a correct
//! diff, only a coarser one.

/// One step of a diff, as indices into the two sequences.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DiffResult {
    Both(usize, usize),
    /// Only in the left sequence.
    Left(usize),
    /// Only in the right sequence.
    Right(usize),
}

/// The most deletions and insertions the diff looks for. Its memory grows with this squared.
const MAX_EDITS: usize = 1000;

/// The diff of `lhs` and `rhs`. Within each run of changes, the left side's items come first.
pub(crate) fn slice<T: Eq>(lhs: &[T], rhs: &[T]) -> Vec<DiffResult> {
    let prefix = lhs.iter().zip(rhs).take_while(|(a, b)| a == b).count();
    let suffix = lhs[prefix..]
        .iter()
        .rev()
        .zip(rhs[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let left = &lhs[prefix..lhs.len() - suffix];
    let right = &rhs[prefix..rhs.len() - suffix];

    let mut steps = Vec::with_capacity(lhs.len().max(rhs.len()));
    steps.extend((0..prefix).map(|i| DiffResult::Both(i, i)));
    match edit_script(left, right) {
        Some(script) => {
            let (mut i, mut j) = (prefix, prefix);
            for edit in script {
                match edit {
                    Edit::Keep => {
                        steps.push(DiffResult::Both(i, j));
                        (i, j) = (i + 1, j + 1);
                    }
                    Edit::Delete => {
                        steps.push(DiffResult::Left(i));
                        i += 1;
                    }
                    Edit::Insert => {
                        steps.push(DiffResult::Right(j));
                        j += 1;
                    }
                }
            }
        }
        None => {
            steps.extend((prefix..prefix + left.len()).map(DiffResult::Left));
            steps.extend((prefix..prefix + right.len()).map(DiffResult::Right));
        }
    }
    let (lhs_end, rhs_end) = (lhs.len() - suffix, rhs.len() - suffix);
    steps.extend((0..suffix).map(|n| DiffResult::Both(lhs_end + n, rhs_end + n)));
    left_first(steps)
}

/// Reorders each run of changes so its left items come before its right ones.
fn left_first(steps: Vec<DiffResult>) -> Vec<DiffResult> {
    let mut out = Vec::with_capacity(steps.len());
    let mut rights = Vec::new();
    for step in steps {
        match step {
            DiffResult::Left(_) => out.push(step),
            DiffResult::Right(_) => rights.push(step),
            DiffResult::Both(..) => {
                out.append(&mut rights);
                out.push(step);
            }
        }
    }
    out.append(&mut rights);
    out
}

#[derive(Clone, Copy)]
enum Edit {
    /// Keep an item both have.
    Keep,
    /// Delete one of `a`'s items.
    Delete,
    /// Insert one of `b`'s items.
    Insert,
}

/// A shortest edit script from `a` to `b`, by Myers's algorithm. `None` past [`MAX_EDITS`].
fn edit_script<T: Eq>(a: &[T], b: &[T]) -> Option<Vec<Edit>> {
    let (n, m) = (a.len() as i64, b.len() as i64);
    let max = ((n + m) as usize).min(MAX_EDITS) as i64;
    // `v[k]`: the furthest x reached on diagonal k = x - y, offset to index from 0.
    let offset = max + 1;
    let mut v = vec![0i64; 2 * max as usize + 3];
    // The furthest x on diagonals -d..=d before each round d, for retracing the route.
    let mut trace: Vec<Vec<i64>> = Vec::new();
    for d in 0..=max {
        trace.push(v[(offset - d) as usize..=(offset + d) as usize].to_vec());
        for k in (-d..=d).step_by(2) {
            let i = (offset + k) as usize;
            // Step down (insert) from diagonal k + 1, or right (delete) from k - 1.
            let mut x = if k == -d || (k != d && v[i - 1] < v[i + 1]) {
                v[i + 1]
            } else {
                v[i - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[i] = x;
            if x >= n && y >= m {
                return Some(retrace(&trace, n, m));
            }
        }
    }
    None
}

/// The edit script that reached (`n`, `m`), retraced through `trace`.
fn retrace(trace: &[Vec<i64>], n: i64, m: i64) -> Vec<Edit> {
    let mut script = Vec::new();
    let (mut x, mut y) = (n, m);
    for (d, before) in trace.iter().enumerate().skip(1).rev() {
        let d = d as i64;
        let furthest = |k: i64| before[(k + d) as usize];
        let k = x - y;
        let previous_k = if k == -d || (k != d && furthest(k - 1) < furthest(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let previous_x = furthest(previous_k);
        let previous_y = previous_x - previous_k;
        while x > previous_x && y > previous_y {
            script.push(Edit::Keep);
            x -= 1;
            y -= 1;
        }
        script.push(if previous_k == k + 1 {
            Edit::Insert
        } else {
            Edit::Delete
        });
        (x, y) = (previous_x, previous_y);
    }
    script.extend((0..x).map(|_| Edit::Keep));
    script.reverse();
    script
}

#[cfg(test)]
mod tests {
    use super::DiffResult::*;
    use super::*;

    #[test]
    fn test_slice_same_items() {
        assert_eq!(slice(&["a", "b"], &["a", "b"]), [Both(0, 0), Both(1, 1)]);
    }

    #[test]
    fn test_slice_different_items() {
        assert_eq!(
            slice(&["a", "b"], &["c", "d"]),
            [Left(0), Left(1), Right(0), Right(1)]
        );
    }

    #[test]
    fn finds_the_longest_common_subsequence() {
        let lhs: Vec<char> = "ABCABBA".chars().collect();
        let rhs: Vec<char> = "CBABAC".chars().collect();

        let diff = slice(&lhs, &rhs);

        let common = diff.iter().filter(|d| matches!(d, Both(..))).count();
        assert_eq!(common, 4);
        assert_valid(&diff, &lhs, &rhs);
    }

    #[test]
    fn handles_empty_sides() {
        assert_eq!(slice::<u8>(&[], &[]), []);
        assert_eq!(slice(&[1], &[]), [Left(0)]);
        assert_eq!(slice(&[], &[1, 2]), [Right(0), Right(1)]);
    }

    #[test]
    fn keeps_a_common_start_and_end() {
        assert_eq!(
            slice(&[1, 2, 3, 9], &[1, 4, 3, 9]),
            [Both(0, 0), Left(1), Right(1), Both(2, 2), Both(3, 3)]
        );
    }

    #[test]
    fn calls_everything_changed_past_the_edit_limit() {
        let lhs: Vec<u32> = (0..2 * MAX_EDITS as u32).map(|n| n * 2).collect();
        let rhs: Vec<u32> = (0..2 * MAX_EDITS as u32).map(|n| n * 2 + 1).collect();

        let diff = slice(&lhs, &rhs);

        assert!(diff.iter().all(|d| !matches!(d, Both(..))));
        assert_valid(&diff, &lhs, &rhs);
    }

    /// Every item appears once, in order, and items called common are equal.
    fn assert_valid<T: Eq + std::fmt::Debug>(diff: &[DiffResult], lhs: &[T], rhs: &[T]) {
        let (mut i, mut j) = (0, 0);
        for step in diff {
            match *step {
                Both(a, b) => {
                    assert_eq!((a, b), (i, j));
                    assert_eq!(lhs[a], rhs[b]);
                    (i, j) = (i + 1, j + 1);
                }
                Left(a) => {
                    assert_eq!(a, i);
                    i += 1;
                }
                Right(b) => {
                    assert_eq!(b, j);
                    j += 1;
                }
            }
        }
        assert_eq!((i, j), (lhs.len(), rhs.len()));
    }
}
