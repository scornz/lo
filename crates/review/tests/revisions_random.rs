//! Revisions built from random push histories, whatever the sources say: each head is one
//! revision, the pull request's head is the latest, and comparisons end at it.

use std::collections::HashSet;
use std::time::{Duration, SystemTime};

use review::revisions::{
    Comparison, FileComparison, PushRecord, ViewedState, build_revisions, default_comparison,
    viewed_file_comparison,
};

/// A linear congruential generator, so every run sees the same histories.
struct Random(u64);

impl Random {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 = (self.0)
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % bound
    }

    fn pick(&mut self, names: &[String]) -> String {
        names[self.below(names.len() as u64) as usize].clone()
    }

    fn time(&mut self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1 + self.below(100))
    }
}

#[test]
fn random_push_histories() {
    let mut random = Random(42);
    let names: Vec<String> = (0..12).map(|n| format!("h{n}")).collect();
    for _ in 0..20_000 {
        let mut records = Vec::new();
        // The heads the records name as heads, which are all revisions.
        let mut named = HashSet::new();
        for _ in 0..1 + random.below(12) {
            let head = random.pick(&names);
            let before = (random.below(3) != 0).then(|| random.pick(&names));
            let at = random.time();
            named.insert(head.clone());
            records.push(match random.below(5) {
                0 => PushRecord::Activity {
                    before,
                    after: head,
                    at,
                    forced: random.below(2) == 0,
                },
                1 => {
                    named.extend(before.clone());
                    PushRecord::ForcePush {
                        before,
                        after: head,
                        at,
                    }
                }
                2 => PushRecord::CheckSuite {
                    previous: before,
                    next: head,
                    at,
                },
                3 => PushRecord::Review { commit: head, at },
                _ => PushRecord::Polled { head, at },
            });
        }
        let mut commits: Vec<String> = Vec::new();
        for _ in 0..random.below(6) {
            let commit = random.pick(&names);
            if !commits.contains(&commit) {
                commits.push(commit);
            }
        }
        let head = random.pick(&names);
        named.insert(head.clone());
        // Half the time the pull request opened before every push; otherwise at a random time.
        let opened_before_every_push = random.below(2) == 0;
        let opened_at = match opened_before_every_push {
            true => SystemTime::UNIX_EPOCH,
            false => random.time(),
        };

        let revisions = build_revisions(&records, &commits, &head, opened_at);

        let heads: HashSet<&str> = revisions.iter().map(|r| r.head.as_str()).collect();
        assert_eq!(heads.len(), revisions.len(), "{records:?}");
        assert_eq!(revisions.latest().unwrap().head, head, "{records:?}");
        if opened_before_every_push {
            assert!(
                named.iter().all(|name| heads.contains(name.as_str())),
                "{records:?}"
            );
        }
        let (a, b) = (random.pick(&names), random.pick(&names));
        match default_comparison(&revisions, Some(&a), Some(&b)) {
            Comparison::Full { nothing_new_since } => {
                assert!(nothing_new_since.is_none_or(|since| since == head));
            }
            Comparison::Between { from, to } => {
                assert_eq!(to, head);
                assert!(revisions.number(&from) < revisions.number(&to));
            }
        }
        match viewed_file_comparison(&revisions, ViewedState::Dismissed, Some(&a), Some(&b)) {
            Some(FileComparison::SinceViewed { from, to })
            | Some(FileComparison::SinceLastReview { from, to }) => {
                assert_eq!(to, head);
                assert!(revisions.number(&from) < revisions.number(&to));
            }
            Some(FileComparison::Full) => {}
            None => panic!("a file changed since viewed always gets a comparison"),
        }
    }
}
