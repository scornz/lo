//! Which threads the threads panel lists (design §8, "Threads panel").

use super::Thread;

/// Which threads the threads panel lists. Each thread there is labeled outdated when it is
/// ([`Thread::outdated`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PanelFilter {
    /// Unresolved threads, the default.
    #[default]
    Unresolved,
    /// Every thread.
    All,
    /// Threads you started or replied in, resolved or not.
    Yours,
}

impl PanelFilter {
    /// Whether the panel lists `thread` for the viewer with login `viewer`.
    pub fn shows(self, thread: &Thread, viewer: &str) -> bool {
        match self {
            Self::Unresolved => !thread.resolved,
            Self::All => true,
            Self::Yours => thread.involves(viewer),
        }
    }
}

/// The threads the panel lists, in the order given.
pub fn panel<'a>(
    threads: &'a [Thread],
    filter: PanelFilter,
    viewer: &'a str,
) -> impl Iterator<Item = &'a Thread> {
    threads
        .iter()
        .filter(move |thread| filter.shows(thread, viewer))
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::super::{CommentState, ThreadComment};
    use super::*;

    fn comment(author: Option<&str>) -> ThreadComment {
        ThreadComment {
            id: "PRRC_1".to_string(),
            author: author.map(str::to_string),
            body: "Why?".to_string(),
            state: CommentState::Submitted,
            created_at: SystemTime::UNIX_EPOCH,
            url: "https://github.com/octo/repo/pull/1#discussion_r1".to_string(),
            viewer_can_update: false,
            viewer_can_delete: false,
        }
    }

    fn thread(id: &str, resolved: bool, authors: &[Option<&str>]) -> Thread {
        Thread {
            id: id.to_string(),
            resolved,
            comments: authors.iter().map(|author| comment(*author)).collect(),
            ..Default::default()
        }
    }

    fn threads() -> Vec<Thread> {
        vec![
            thread("started", false, &[Some("alice"), Some("bob")]),
            thread("replied", true, &[Some("bob"), Some("Alice")]),
            thread("others", false, &[Some("bob"), None]),
            thread("resolved", true, &[Some("carol")]),
        ]
    }

    fn listed(filter: PanelFilter) -> Vec<String> {
        let threads = threads();
        panel(&threads, filter, "alice")
            .map(|thread| thread.id.clone())
            .collect()
    }

    #[test]
    fn it_lists_unresolved_threads_by_default() {
        assert_eq!(
            listed(PanelFilter::default()),
            ["started", "others"].map(String::from)
        );
    }

    #[test]
    fn it_can_list_every_thread() {
        assert_eq!(
            listed(PanelFilter::All),
            ["started", "replied", "others", "resolved"].map(String::from)
        );
    }

    #[test]
    fn yours_are_threads_you_started_or_replied_in() {
        // Logins ignore case, as GitHub's do.
        assert_eq!(
            listed(PanelFilter::Yours),
            ["started", "replied"].map(String::from)
        );
    }
}
