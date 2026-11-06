//! The files a pull request changes, as GitHub's REST API lists them
//! (`GET /repos/{owner}/{repo}/pulls/{number}/files`), each with its patch. The PR tab shows
//! these patches until the local copy is ready (design §11, "First open").

use http_client::Method;
use serde::Deserialize;

use crate::client::GithubClient;
use crate::error::GithubError;
use crate::patch::{self, Patch, PatchError};
use crate::pulls::PullRef;

/// GitHub lists at most 100 files a page.
pub const FILES_PER_PAGE: usize = 100;

/// GitHub lists at most 3,000 of a pull request's files; the rest only the local copy shows.
pub const MAX_FILES: usize = 3_000;

/// One changed file.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PullFile {
    /// The file's path in the head. For a deleted file, its path in the base.
    pub filename: String,
    pub status: FileStatus,
    pub additions: u32,
    pub deletions: u32,
    /// Additions plus deletions.
    pub changes: u32,
    /// The file's unified diff: its hunks, without the `diff --git`, `---` and `+++` lines.
    /// GitHub leaves it out for binary files, for diffs too large to show, and for changes with no
    /// changed lines, such as a pure rename or a change of mode.
    #[serde(default)]
    pub patch: Option<String>,
    /// The file's path in the base, when the pull request renames or copies it.
    #[serde(default)]
    pub previous_filename: Option<String>,
}

impl PullFile {
    /// The file's patch, read; `None` when GitHub left it out.
    pub fn parsed_patch(&self) -> Option<Result<Patch, PatchError>> {
        self.patch.as_deref().map(patch::parse_patch)
    }
}

/// How the pull request changes a file, in GitHub's words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileStatus {
    Added,
    Removed,
    Modified,
    Renamed,
    Copied,
    /// The file's type or mode changed.
    Changed,
    Unchanged,
    /// A status this version of rozed doesn't know.
    #[serde(other)]
    Other,
}

/// One page of a pull request's files, and where the next one is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilesPage {
    pub files: Vec<PullFile>,
    /// The next page's URL, from the `Link` header, or `None` on the last page.
    pub next: Option<String>,
}

impl PullRef {
    /// The REST path of the first page of the pull request's files, 100 a page. Later pages are
    /// at each page's [`FilesPage::next`].
    pub fn files_path(&self) -> String {
        format!("{}/files?per_page={FILES_PER_PAGE}", self.rest_path())
    }
}

impl GithubClient {
    /// One page of a pull request's files: `path` is [`PullRef::files_path`], or the previous
    /// page's [`FilesPage::next`].
    pub async fn pull_files_page(&self, path: &str) -> Result<FilesPage, GithubError> {
        let response = self.send(Method::GET, path, None, None).await?;
        Ok(FilesPage {
            files: response.json()?,
            next: response.header("link").and_then(next_link),
        })
    }

    /// Every file of the pull request GitHub lists, up to its 3,000, page by page.
    pub async fn pull_files(&self, pull: &PullRef) -> Result<Vec<PullFile>, GithubError> {
        let mut files = Vec::new();
        let mut pages = Pages::new(pull);
        while let Some(path) = pages.next_path() {
            let page = self.pull_files_page(&path).await?;
            files.extend(page.files);
            pages.read(page.next)?;
        }
        Ok(files)
    }
}

/// Where the next page of a pull request's files is, for a caller that reads them one page at a
/// time, such as through an account's request queue.
#[derive(Clone, Debug)]
pub struct Pages {
    next: Option<String>,
    read: usize,
}

impl Pages {
    pub fn new(pull: &PullRef) -> Self {
        Self {
            next: Some(pull.files_path()),
            read: 0,
        }
    }

    /// The path or URL of the next page to read, or `None` once every page is read.
    pub fn next_path(&self) -> Option<String> {
        self.next.clone()
    }

    /// Takes the `next` of the page just read. GitHub lists no more than 30 pages, so a link past
    /// them is ignored, and one back to the same page is an error rather than a loop.
    pub fn read(&mut self, next: Option<String>) -> Result<(), GithubError> {
        self.read += 1;
        if next.is_some() && next == self.next {
            return Err(GithubError::Inconsistent(format!(
                "page {} of the files links to itself",
                self.read
            )));
        }
        self.next = next.filter(|_| self.read < MAX_FILES / FILES_PER_PAGE);
        Ok(())
    }
}

/// The `rel="next"` URL in a `Link` header, such as
/// `<https://api.github.com/repositories/1/pulls/2/files?page=2>; rel="next", <…>; rel="last"`.
pub fn next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|link| {
        let (url, params) = link.trim().split_once(';')?;
        let url = url.trim().strip_prefix('<')?.strip_suffix('>')?;
        params
            .split(';')
            .filter_map(|param| param.trim().split_once('='))
            .any(|(name, value)| {
                name.trim() == "rel"
                    && value
                        .trim()
                        .trim_matches('"')
                        .split(' ')
                        .any(|rel| rel == "next")
            })
            .then(|| url.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use futures::executor::block_on;

    use crate::cassette::{Interaction, RecordedRequest, RecordedResponse};
    use crate::replay::ReplayHttpClient;
    use crate::token::Token;

    #[test]
    fn the_next_link_is_the_one_marked_next() {
        let header = "<https://api.github.com/repositories/1/pulls/2/files?per_page=100&page=2>; \
                      rel=\"next\", <https://api.github.com/repositories/1/pulls/2/files?per_page=100&page=4>; rel=\"last\"";
        assert_eq!(
            next_link(header).as_deref(),
            Some("https://api.github.com/repositories/1/pulls/2/files?per_page=100&page=2")
        );
        let last_page = "<https://api.github.com/repositories/1/pulls/2/files?page=1>; rel=\"prev\", \
                         <https://api.github.com/repositories/1/pulls/2/files?page=1>; rel=\"first\"";
        assert_eq!(next_link(last_page), None);
        assert_eq!(next_link(""), None);
        assert_eq!(next_link("garbage"), None);
        assert_eq!(
            next_link("<https://api.github.com/x?page=3>; rel=\"prev next\"").as_deref(),
            Some("https://api.github.com/x?page=3")
        );
    }

    fn page(url: &str, link: Option<&str>, files: serde_json::Value) -> Interaction {
        Interaction {
            request: RecordedRequest {
                method: "GET".into(),
                url: url.into(),
                ..Default::default()
            },
            response: RecordedResponse {
                status: 200,
                headers: link
                    .map(|link| ("link".to_string(), link.to_string()))
                    .into_iter()
                    .collect(),
                body: Some(files),
                body_text: None,
            },
        }
    }

    const FIRST: &str = "https://api.github.com/repos/o/r/pulls/7/files?per_page=100";
    const SECOND: &str = "https://api.github.com/repositories/9/pulls/7/files?per_page=100&page=2";

    fn two_pages() -> Arc<ReplayHttpClient> {
        let next = format!("<{SECOND}>; rel=\"next\", <{SECOND}>; rel=\"last\"");
        Arc::new(
            ReplayHttpClient::from_interactions(
                "files",
                [
                    page(
                        FIRST,
                        Some(&next),
                        serde_json::json!([{
                            "sha": "1111111111111111111111111111111111111111",
                            "filename": "src/main.rs",
                            "status": "modified",
                            "additions": 1,
                            "deletions": 1,
                            "changes": 2,
                            "patch": "@@ -1 +1 @@\n-old\n+new"
                        }]),
                    ),
                    page(
                        SECOND,
                        None,
                        serde_json::json!([
                            {
                                "sha": "2222222222222222222222222222222222222222",
                                "filename": "logo.png",
                                "status": "added",
                                "additions": 0,
                                "deletions": 0,
                                "changes": 0
                            },
                            {
                                "sha": "3333333333333333333333333333333333333333",
                                "filename": "docs/new.md",
                                "previous_filename": "docs/old.md",
                                "status": "renamed",
                                "additions": 0,
                                "deletions": 0,
                                "changes": 0
                            },
                            {
                                "filename": "weird",
                                "status": "something-new",
                                "additions": 0,
                                "deletions": 0,
                                "changes": 0
                            }
                        ]),
                    ),
                ],
            )
            .unwrap(),
        )
    }

    #[test]
    fn files_are_read_page_by_page() {
        let http = two_pages();
        let client = GithubClient::new(http.clone(), Token::new("gho_FAKE"));
        let files = block_on(client.pull_files(&PullRef::new("o", "r", 7))).unwrap();

        let summary: Vec<_> = files
            .iter()
            .map(|file| {
                (
                    file.filename.as_str(),
                    file.status,
                    file.patch.is_some(),
                    file.previous_filename.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("src/main.rs", FileStatus::Modified, true, None),
                ("logo.png", FileStatus::Added, false, None),
                (
                    "docs/new.md",
                    FileStatus::Renamed,
                    false,
                    Some("docs/old.md")
                ),
                ("weird", FileStatus::Other, false, None),
            ]
        );
        let patch = files[0].parsed_patch().unwrap().unwrap();
        assert_eq!(patch.hunks[0].new, 0..1);
        assert!(files[1].parsed_patch().is_none());

        let seen: Vec<_> = http.seen().iter().map(|seen| seen.url.clone()).collect();
        assert_eq!(seen, [FIRST, SECOND]);
        assert!(
            http.seen()
                .iter()
                .all(|seen| seen.header("authorization") == Some("token gho_FAKE"))
        );
    }

    #[test]
    fn without_an_account_no_token_is_sent() {
        let http = two_pages();
        let client = GithubClient::anonymous(http.clone());
        let files = block_on(client.pull_files(&PullRef::new("o", "r", 7))).unwrap();
        assert_eq!(files.len(), 4);
        for seen in http.seen() {
            assert_eq!(seen.header("authorization"), None);
            assert_eq!(seen.header("accept"), Some("application/vnd.github+json"));
        }
    }

    #[test]
    fn a_next_page_off_githubs_api_is_refused() {
        let next = "<https://evil.example/files?page=2>; rel=\"next\"";
        let http = Arc::new(
            ReplayHttpClient::from_interactions(
                "files",
                [page(FIRST, Some(next), serde_json::json!([]))],
            )
            .unwrap(),
        );
        let client = GithubClient::new(http.clone(), Token::new("gho_FAKE"));
        let error = block_on(client.pull_files(&PullRef::new("o", "r", 7))).unwrap_err();
        assert!(matches!(error, GithubError::RefusedUrl { .. }), "{error:?}");
        assert_eq!(http.seen().len(), 1);
    }

    #[test]
    fn a_page_that_links_to_itself_is_an_error_not_a_loop() {
        let next = format!("<{FIRST}>; rel=\"next\"");
        let http = Arc::new(
            ReplayHttpClient::from_interactions(
                "files",
                [page(FIRST, Some(&next), serde_json::json!([]))],
            )
            .unwrap(),
        );
        let client = GithubClient::new(http, Token::new("gho_FAKE"));
        let error = block_on(client.pull_files(&PullRef::new("o", "r", 7))).unwrap_err();
        assert!(matches!(error, GithubError::Inconsistent(_)), "{error:?}");
    }

    #[test]
    fn no_more_than_30_pages_are_read() {
        let mut pages = Pages::new(&PullRef::new("o", "r", 7));
        let mut read = 0;
        while let Some(path) = pages.next_path() {
            read += 1;
            pages
                .read(Some(format!("{path}&page={}", read + 1)))
                .unwrap();
        }
        assert_eq!(read, 30);
    }
}
