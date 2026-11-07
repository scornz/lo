//! A repository's trunks: its long-lived branches, which [`stacks::Stacks::find`] never makes a
//! layer. They're its default branch, and the branches its branch protection rules and branch
//! rulesets name, matched the way GitHub matches them.
//!
//! - **Branch protection rules** come from each pull request's base and head branch
//!   (`refUpdateRule { pattern }`), which anyone who can read the repository sees. The
//!   repository's own list (`branchProtectionRules`) is empty for anyone who can't administer it.
//! - **Rulesets** count only when they're active and gate merging, with a rule that requires a
//!   pull request, a merge queue, status checks or deployments, or restricts updates: branches
//!   nobody pushes work to. A ruleset that only enforces a policy, such as signed commits, Copilot
//!   review or no force pushes, often covers work branches too. They're read on their own
//!   ([`crate::queries::trunk_rules`]), and without them the trunks are the default branch and the
//!   protected branches.
//! - **A rule that covers every branch** says nothing about which are long-lived, so it makes no
//!   branch a trunk: an include of `~ALL` or of wildcards alone, such as `*` or
//!   `refs/heads/*/**/*`, in a ruleset without excludes, or a protection rule's `*`. With excludes,
//!   such a ruleset protects everything but the excluded branches, and those it covers are trunks.
//!
//! A long-lived branch that nothing protects can't be told from a work branch.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The special name a ruleset includes or excludes the default branch by.
pub const DEFAULT_BRANCH: &str = "~DEFAULT_BRANCH";
/// The special name a ruleset includes every branch by.
pub const ALL: &str = "~ALL";
/// What a ruleset's ref name patterns match a branch's name after.
const HEADS: &str = "refs/heads/";

/// What decides a repository's trunks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Trunks {
    /// `defaultBranchRef { name }`, or `None` for an empty repository.
    pub default_branch: Option<String>,
    /// The patterns of the branch protection rules that cover its pull requests' branches,
    /// matched against branch names.
    pub protected: Vec<String>,
    /// The ref name conditions of the active branch rulesets that gate merging.
    pub rulesets: Vec<RefNames>,
}

/// A branch ruleset's ref name condition (`conditions { refName { include exclude } }`): it
/// covers a branch when one of `include` matches it and none of `exclude` does. Patterns match a
/// branch's full ref name, such as `refs/heads/main`, or are `~DEFAULT_BRANCH` or `~ALL`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RefNames {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl Trunks {
    /// Whether `branch` is a trunk: the default branch, or a branch a protection rule or ruleset
    /// names, other than by covering every branch.
    pub fn contains(&self, branch: &str) -> bool {
        let default_branch = self.default_branch.as_deref();
        default_branch == Some(branch)
            || (self.protected.iter())
                .any(|pattern| !covers_every_branch(pattern) && fnmatch(pattern, branch))
            || (self.rulesets.iter()).any(|ruleset| ruleset.names(branch, default_branch))
    }

    /// The trunks among the branches `prs` target or come from, for [`stacks::Stacks::find`]: the
    /// default branch first, then the others by name. A fork's branch is never one, whatever
    /// its name.
    pub fn among(&self, prs: &[stacks::PullRequest]) -> Vec<String> {
        let heads = (prs.iter())
            .filter(|pr| pr.head_in_base_repo)
            .map(|pr| pr.head_ref.as_str());
        let mut branches: Vec<&str> = (prs.iter().map(|pr| pr.base_ref.as_str()))
            .chain(heads)
            .filter(|branch| self.default_branch.as_deref() != Some(*branch))
            .collect();
        branches.sort_unstable();
        branches.dedup();
        branches.retain(|branch| self.contains(branch));
        (self.default_branch.iter().map(String::as_str))
            .chain(branches)
            .map(String::from)
            .collect()
    }
}

impl RefNames {
    /// Whether the condition covers `branch`, as GitHub decides.
    pub fn covers(&self, branch: &str, default_branch: Option<&str>) -> bool {
        let matches = |pattern: &String| ref_matches(pattern, branch, default_branch);
        self.include.iter().any(matches) && !self.exclude.iter().any(matches)
    }

    /// Whether the condition names `branch` as a trunk: it covers it, by an include that doesn't
    /// cover every branch, or does but comes with excludes.
    fn names(&self, branch: &str, default_branch: Option<&str>) -> bool {
        let matches = |pattern: &String| ref_matches(pattern, branch, default_branch);
        let everything_but = !self.exclude.is_empty();
        (self.include.iter())
            .filter(|pattern| everything_but || !covers_every_branch(pattern))
            .any(matches)
            && !self.exclude.iter().any(matches)
    }
}

/// Whether a ruleset's ref name pattern matches `branch`.
fn ref_matches(pattern: &str, branch: &str, default_branch: Option<&str>) -> bool {
    match pattern {
        ALL => true,
        DEFAULT_BRANCH => default_branch == Some(branch),
        pattern => fnmatch(pattern, &format!("{HEADS}{branch}")),
    }
}

/// Whether a protection rule's or ruleset's pattern covers every branch, or nearly: `~ALL`, or
/// a pattern of `*`, `?` and `/` alone, after a ruleset's `refs/heads/`.
fn covers_every_branch(pattern: &str) -> bool {
    let wildcards = pattern.strip_prefix(HEADS).unwrap_or(pattern);
    pattern == ALL
        || (!wildcards.is_empty() && wildcards.chars().all(|c| matches!(c, '*' | '?' | '/')))
}

/// Whether `name` matches `pattern` as Ruby's `File.fnmatch` with `File::FNM_PATHNAME` decides,
/// which is how GitHub matches branch protection and ruleset patterns:
///
/// - `*` matches any run of characters but `/`, and `?` any one character but `/`;
/// - `**/` matches any number of directories, none included; `**` anywhere else is `*`;
/// - `[set]` matches one character in the set, which can hold ranges such as `a-z`, and `[!set]`
///   one character outside it;
/// - `\` makes the next character plain, as in Ruby.
///
/// GitHub doesn't support `{a,b}` (`File::FNM_EXTGLOB`), so braces are plain characters. It says
/// a set can't be complemented with `^` and `\` can't quote, without saying what they do instead;
/// here `^` is plain and `\` escapes. A branch's name can hold none of `\ ^ [ * ?`, so a pattern
/// that relies on either matches nothing real. Matching is case-sensitive, as git's names are.
pub fn fnmatch(pattern: &str, name: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').collect();
    let name: Vec<&str> = name.split('/').collect();
    match_segments(&pattern, &name)
}

/// Matches a pattern's segments, split at `/`, against a name's.
fn match_segments(pattern: &[&str], name: &[&str]) -> bool {
    match (pattern.split_first(), name.split_first()) {
        (None, _) => name.is_empty(),
        // `**/`: any number of directories, none included.
        (Some((&"**", rest)), _) if !rest.is_empty() => {
            (0..=name.len()).any(|skip| match_segments(rest, &name[skip..]))
        }
        (Some((first, rest)), Some((segment, name_rest))) => {
            match_segment(first, segment) && match_segments(rest, name_rest)
        }
        (Some(_), None) => false,
    }
}

/// Matches one segment of a pattern, which holds no `/`, against one segment of a name.
fn match_segment(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut p, mut n) = (0, 0);
    // Where to go on from after the last `*`: the pattern after it, and the name it has taken so
    // far.
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        let step = match pattern.get(p).copied() {
            Some('*') => {
                star = Some((p + 1, n));
                p += 1;
                continue;
            }
            Some('?') => Some(1),
            Some('[') => bracket(&pattern[p + 1..], name[n]).map(|used| used + 1),
            Some('\\') if p + 1 < pattern.len() => (pattern[p + 1] == name[n]).then_some(2),
            Some(c) => (c == name[n]).then_some(1),
            None => None,
        };
        match (step, star) {
            (Some(step), _) => {
                p += step;
                n += 1;
            }
            // Let the last `*` take one more character, and try again from after it.
            (None, Some((after, taken))) => {
                star = Some((after, taken + 1));
                p = after;
                n = taken + 1;
            }
            (None, None) => return false,
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

/// Matches `c` against the set that starts just after a `[`, returning how many characters of
/// `pattern` the set takes up to and including its `]`, or `None` when `c` isn't in the set or
/// the set never ends. A `]` straight after the `[` or `[!` ends an empty set, as in Ruby.
fn bracket(pattern: &[char], c: char) -> Option<usize> {
    let complement = pattern.first() == Some(&'!');
    let mut at = usize::from(complement);
    // A character of the set, with a `\` before it taken as an escape.
    let plain = |at: usize| -> Option<(char, usize)> {
        match *pattern.get(at)? {
            '\\' => Some((*pattern.get(at + 1)?, at + 2)),
            c => Some((c, at + 1)),
        }
    };
    let mut found = false;
    loop {
        if *pattern.get(at)? == ']' {
            at += 1;
            break;
        }
        let (low, next) = plain(at)?;
        at = next;
        let range =
            pattern.get(at) == Some(&'-') && pattern.get(at + 1).is_some_and(|&next| next != ']');
        let high = if range {
            let (high, next) = plain(at + 1)?;
            at = next;
            high
        } else {
            low
        };
        found |= (low..=high).contains(&c);
    }
    (found != complement).then_some(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_star_stops_at_slashes() {
        // GitHub's own examples (`about-fnmatch` in github/docs).
        assert!(fnmatch("qa/*", "qa/feature"));
        assert!(!fnmatch("qa/*", "qa/foo/bar"));
        assert!(fnmatch("qa/**/*", "qa/foo/bar/foobar/hello-world"));
        assert!(fnmatch("qa/**/*", "qa/foo"));
        assert!(fnmatch("qa**/**/*", "qa/foo/bar"));
        assert!(fnmatch("qa**/**/*", "qa-team/foo"));
        assert!(!fnmatch("qa**/**/*", "qa"));
        assert!(fnmatch("*release*", "pre-release-2"));
        assert!(!fnmatch("*release*", "releases/2.0"));
        assert!(fnmatch("*", "main"));
        assert!(!fnmatch("*", "user/topic"));
    }

    #[test]
    fn double_stars_match_directories_only_before_a_slash() {
        assert!(fnmatch("**/main", "main"));
        assert!(fnmatch("**/main", "a/b/main"));
        assert!(!fnmatch("**/main", "a/b/not-main"));
        // Anywhere else, `**` is `*`.
        assert!(fnmatch("**", "main"));
        assert!(!fnmatch("**", "a/b"));
        assert!(!fnmatch("a/**", "a/b/c"));
        assert!(fnmatch("a**", "abc"));
        assert!(fnmatch("refs/heads/**/*", "refs/heads/a/b/c"));
    }

    #[test]
    fn question_marks_sets_and_escapes() {
        assert!(fnmatch("v?", "v1"));
        assert!(!fnmatch("v?", "v10"));
        assert!(!fnmatch("a?b", "a/b"));
        // zed-industries/zed's release branches.
        let releases = "refs/heads/v[0-9].[0-9]*.x";
        assert!(fnmatch(releases, "refs/heads/v1.27.x"));
        assert!(fnmatch(releases, "refs/heads/v0.203.x"));
        assert!(!fnmatch(releases, "refs/heads/v1.27.0"));
        assert!(!fnmatch(releases, "refs/heads/main"));
        assert!(fnmatch("[!a-c]x", "dx"));
        assert!(!fnmatch("[!a-c]x", "bx"));
        assert!(fnmatch("[a-]x", "-x"));
        assert!(!fnmatch("[]]x", "]x"), "`[]` is an empty set");
        assert!(
            !fnmatch("[abc", "a"),
            "a set that never ends matches nothing"
        );
        assert!(fnmatch(r"a\*", "a*"));
        assert!(!fnmatch(r"a\*", "ab"));
        assert!(fnmatch(r"[\]]", "]"));
        // GitHub doesn't complement with `^`, nor expand braces.
        assert!(fnmatch("[^a]", "^"));
        assert!(fnmatch("[^a]", "a"));
        assert!(!fnmatch("[^a]", "b"));
        assert!(!fnmatch("{main,dev}", "main"));
        assert!(!fnmatch("Main", "main"));
    }

    #[test]
    fn stars_backtrack() {
        assert!(fnmatch("*-*-x", "a-b-c-x"));
        assert!(fnmatch("*a*b", "xaxxab"));
        assert!(!fnmatch("*a*b", "xaxxa"));
        assert!(fnmatch(
            "enterprise-*.*.*-release",
            "enterprise-3.18.1-release"
        ));
        assert!(!fnmatch(
            "enterprise-*.*.*-release",
            "enterprise-3.18-release"
        ));
    }

    fn refs(include: &[&str], exclude: &[&str]) -> RefNames {
        RefNames {
            include: include.iter().map(|s| s.to_string()).collect(),
            exclude: exclude.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn a_ruleset_matches_full_ref_names_and_its_special_names() {
        let main = Some("main");
        let release = refs(&["refs/heads/release/*"], &[]);
        assert!(release.covers("release/1.0", main));
        // Patterns match `refs/heads/…`, so one without it names no branch.
        assert!(!refs(&["release/*"], &[]).covers("release/1.0", main));
        assert!(refs(&[DEFAULT_BRANCH], &[]).covers("main", main));
        assert!(!refs(&[DEFAULT_BRANCH], &[]).covers("develop", main));
        assert!(!refs(&[DEFAULT_BRANCH], &[]).covers("main", None));
        assert!(refs(&[ALL], &[]).covers("anything/at/all", main));
        // An exclude wins over an include.
        let not_dev = refs(
            &["refs/heads/main", "refs/heads/master"],
            &["refs/heads/dev*"],
        );
        assert!(not_dev.covers("master", main));
        assert!(!refs(&[ALL], &["refs/heads/dev*"]).covers("develop", main));
        assert!(!refs(&[ALL], &[DEFAULT_BRANCH]).covers("main", main));
    }

    fn trunks(default: &str, protected: &[&str], rulesets: Vec<RefNames>) -> Trunks {
        Trunks {
            default_branch: Some(default.into()),
            protected: protected.iter().map(|s| s.to_string()).collect(),
            rulesets,
        }
    }

    #[test]
    fn rules_for_every_branch_make_no_trunks() {
        // rust-lang/rust's rulesets as a reader sees them, cut down.
        let rust = trunks(
            "main",
            &[],
            vec![
                refs(&["refs/heads/*"], &[]),
                refs(&["refs/heads/*/**/*"], &[]),
                refs(&["refs/heads/beta"], &[]),
                refs(&["refs/heads/automation/bors/try"], &[]),
            ],
        );
        assert!(rust.contains("main"));
        assert!(rust.contains("beta"));
        assert!(rust.contains("automation/bors/try"));
        assert!(!rust.contains("topic"));
        assert!(!rust.contains("user/topic"));
        // Copilot reviews on every branch, as in cli/cli.
        let cli = trunks("trunk", &[], vec![refs(&[ALL], &[])]);
        assert!(cli.contains("trunk"));
        assert!(!cli.contains("stack-a"));
        // A catch-all include is skipped, but the ruleset's other includes still count.
        let mixed = trunks(
            "main",
            &["*", "**/*"],
            vec![refs(&[ALL, "refs/heads/next"], &[])],
        );
        assert!(mixed.contains("next"));
        assert!(!mixed.contains("feature"));
    }

    #[test]
    fn a_rule_for_every_branch_but_some_names_the_rest() {
        // Pull requests on every branch but work branches: develop and release branches are
        // trunks, and so is anything else no rule excludes.
        let flow = trunks(
            "main",
            &[],
            vec![refs(
                &[ALL],
                &["refs/heads/feature/**/*", "refs/heads/users/**/*"],
            )],
        );
        assert!(flow.contains("develop"));
        assert!(flow.contains("release/2.0"));
        assert!(!flow.contains("feature/a"));
        assert!(!flow.contains("feature/a/b"));
        assert!(!flow.contains("users/me/topic"));
        // The same with wildcards for every branch.
        let wildcards = trunks(
            "main",
            &[],
            vec![refs(&["refs/heads/**/*"], &["refs/heads/feature/*"])],
        );
        assert!(wildcards.contains("develop"));
        assert!(!wildcards.contains("feature/a"));
    }

    #[test]
    fn protection_patterns_match_branch_names() {
        let repo = trunks(
            "main",
            &["develop", "release/*"],
            vec![refs(&["refs/heads/production"], &[])],
        );
        assert!(repo.contains("develop"));
        assert!(repo.contains("release/2.0"));
        assert!(!repo.contains("release/2.0/hotfix"));
        assert!(repo.contains("production"));
        assert!(!repo.contains("feature"));
        // Without a default branch, only the rules name trunks.
        let empty = Trunks::default();
        assert!(!empty.contains("main"));
    }

    fn pr(base: &str, head: &str, from_fork: bool) -> stacks::PullRequest {
        stacks::PullRequest {
            number: 1,
            state: stacks::State::Open,
            base_ref: base.into(),
            base_tip: None,
            head_ref: head.into(),
            head_in_base_repo: !from_fork,
            head_oid: String::new(),
            commits: Vec::new(),
            created_at: std::time::SystemTime::UNIX_EPOCH,
            native_stack: None,
        }
    }

    #[test]
    fn trunks_among_pull_requests_list_the_default_branch_first() {
        let repo = trunks("main", &["develop", "release/*"], Vec::new());
        let prs = [
            pr("main", "develop", false),
            pr("release/2.0", "fix", false),
            pr("develop", "topic", false),
            // A fork's `release/1.0` isn't the repository's.
            pr("main", "release/1.0", true),
            pr("main", "develop", false),
        ];
        assert_eq!(repo.among(&prs), ["main", "develop", "release/2.0"]);
        assert_eq!(Trunks::default().among(&prs), Vec::<String>::new());
    }
}
