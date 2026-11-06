//! The files a pull request changes, as GitHub's REST API lists them
//! (`GET /repos/{owner}/{repo}/pulls/{number}/files`), each with its patch. The PR tab shows
//! these patches until the local copy is ready (design §11, "First open").

use std::time::SystemTime;

use http_client::Method;
use serde::Deserialize;

use crate::client::{API_URL, GithubClient};
use crate::error::GithubError;
use crate::patch::{self, Patch, PatchError};
use crate::poll::RequestQueue;
use crate::pulls::PullRef;
use crate::rate_limit::RateLimits;

/// GitHub lists at most 100 files a page.
pub const FILES_PER_PAGE: usize = 100;

/// GitHub lists at most 3,000 of a pull request's files; the rest only the local copy shows.
pub const MAX_FILES: usize = 3_000;

/// The most pages GitHub lists: [`MAX_FILES`] at [`FILES_PER_PAGE`] a page.
pub const MAX_PAGES: usize = MAX_FILES / FILES_PER_PAGE;

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

/// A pull request's files, as far as they were read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PullFiles {
    pub files: Vec<PullFile>,
    /// Why reading stopped with pages left, if it did. GitHub's own limit of [`MAX_FILES`] can
    /// also end the pages without a link to more; the pull request's `changed_files` tells.
    pub cut_short: Option<CutShort>,
}

/// Why reading a pull request's files stopped with pages left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutShort {
    /// [`ReadFiles::max_pages`] were read.
    Pages,
    /// No more than [`ReadFiles::keep_requests`] of the rate limit's requests were left.
    RateLimit,
}

/// How much of a pull request's file list to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadFiles {
    /// Read no more pages than this.
    pub max_pages: usize,
    /// Read no page while no more than this many of the REST API's requests are left, as far as
    /// the client last heard.
    pub keep_requests: u32,
}

impl ReadFiles {
    /// Every page GitHub lists.
    pub const ALL: Self = Self {
        max_pages: MAX_PAGES,
        keep_requests: 0,
    };
}

impl GithubClient {
    /// One page of a pull request's files: `path` is [`PullRef::files_path`], or the previous
    /// page's [`FilesPage::next`].
    pub async fn pull_files_page(&self, path: &str) -> Result<FilesPage, GithubError> {
        let response = self.send(Method::GET, path, None, None).await?;
        // A response may carry several `Link` headers; together they're one list.
        let links: Vec<&str> = response
            .headers
            .get_all("link")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect();
        Ok(FilesPage {
            files: response.json()?,
            next: next_link(&links.join(", ")),
        })
    }

    /// Every file of the pull request GitHub lists, up to its 3,000, page by page.
    pub async fn pull_files(&self, pull: &PullRef) -> Result<Vec<PullFile>, GithubError> {
        Ok(self
            .read_pull_files(pull, None, ReadFiles::ALL)
            .await?
            .files)
    }

    /// The pull request's files, page by page, each through `queue` when there's one, as far as
    /// `limits` allow.
    pub async fn read_pull_files(
        &self,
        pull: &PullRef,
        queue: Option<&RequestQueue>,
        limits: ReadFiles,
    ) -> Result<PullFiles, GithubError> {
        let mut files = Vec::new();
        let mut pages = Pages::new(pull).with_max_pages(limits.max_pages);
        while let Some(path) = pages.next_path() {
            if few_left(&self.rate_limits(), limits.keep_requests, SystemTime::now()) {
                pages.stop(CutShort::RateLimit);
                break;
            }
            let page = match queue {
                Some(queue) => queue.read(|| self.pull_files_page(&path)).await?,
                None => self.pull_files_page(&path).await?,
            };
            files.extend(page.files);
            pages.read(page.next)?;
        }
        Ok(PullFiles {
            files,
            cut_short: pages.cut_short(),
        })
    }
}

/// Whether no more than `keep` of the REST API's requests are left, as `limits` last heard,
/// unless the window has reset since.
fn few_left(limits: &RateLimits, keep: u32, now: SystemTime) -> bool {
    limits
        .core()
        .is_some_and(|core| core.remaining <= keep && core.reset_time() > now)
}

/// Where the next page of a pull request's files is, for a caller that reads them one page at a
/// time.
#[derive(Clone, Debug)]
pub struct Pages {
    next: Option<String>,
    read: usize,
    max_pages: usize,
    cut_short: Option<CutShort>,
}

impl Pages {
    pub fn new(pull: &PullRef) -> Self {
        Self {
            next: Some(pull.files_path()),
            read: 0,
            max_pages: MAX_PAGES,
            cut_short: None,
        }
    }

    /// Reads no more than `max_pages`, and never more than GitHub lists.
    pub fn with_max_pages(self, max_pages: usize) -> Self {
        Self {
            max_pages: max_pages.min(MAX_PAGES),
            ..self
        }
    }

    /// The path or URL of the next page to read, or `None` once every page is read.
    pub fn next_path(&self) -> Option<String> {
        self.next.clone()
    }

    /// Takes the `next` of the page just read. A link past the last page to read is ignored, and
    /// one back to the same page is an error rather than a loop.
    pub fn read(&mut self, next: Option<String>) -> Result<(), GithubError> {
        self.read += 1;
        if let (Some(next), Some(this)) = (&next, &self.next)
            && on_the_api(next) == on_the_api(this)
        {
            return Err(GithubError::Inconsistent(format!(
                "page {} of the files links to itself",
                self.read
            )));
        }
        self.next = next;
        if self.read >= self.max_pages {
            // GitHub lists no more than its 30 pages, whatever a link says.
            let why = (self.max_pages < MAX_PAGES).then_some(CutShort::Pages);
            self.stop_with(why);
        }
        Ok(())
    }

    /// Stops reading, with pages left if there are any.
    pub fn stop(&mut self, why: CutShort) {
        self.stop_with(Some(why));
    }

    fn stop_with(&mut self, why: Option<CutShort>) {
        if self.next.take().is_some() {
            self.cut_short = self.cut_short.or(why);
        }
    }

    /// Why reading stopped with pages left, if it did.
    pub fn cut_short(&self) -> Option<CutShort> {
        self.cut_short
    }
}

/// `path` as a path on GitHub's API: a full URL there and the path alone name the same page.
fn on_the_api(path: &str) -> &str {
    path.strip_prefix(API_URL).unwrap_or(path)
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
        let client = GithubClient::new(http.clone(), Token::new("gho_FAKE"));
        let error = block_on(client.pull_files(&PullRef::new("o", "r", 7))).unwrap_err();
        assert!(matches!(error, GithubError::Inconsistent(_)), "{error:?}");
        // The first page is read once: its link, a full URL, names the path asked for.
        assert_eq!(http.seen().len(), 1);
    }

    #[test]
    fn reading_stops_after_the_pages_asked_for() {
        let http = two_pages();
        let client = GithubClient::anonymous(http.clone());
        let limits = ReadFiles {
            max_pages: 1,
            keep_requests: 0,
        };
        let read = block_on(client.read_pull_files(&PullRef::new("o", "r", 7), None, limits));
        let read = read.unwrap();
        assert_eq!(read.files.len(), 1);
        assert_eq!(read.cut_short, Some(CutShort::Pages));
        assert_eq!(http.seen().len(), 1);

        // Reading every page isn't cut short.
        let client = GithubClient::anonymous(two_pages());
        let read =
            block_on(client.read_pull_files(&PullRef::new("o", "r", 7), None, ReadFiles::ALL));
        assert_eq!(read.unwrap().cut_short, None);
    }

    #[test]
    fn reading_stops_while_few_requests_are_left() {
        let reset = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3_600;
        let next = format!("<{SECOND}>; rel=\"next\"");
        let mut first = page(FIRST, Some(&next), serde_json::json!([]));
        for (name, value) in [
            ("x-ratelimit-limit", "60".to_string()),
            ("x-ratelimit-remaining", "10".to_string()),
            ("x-ratelimit-reset", reset.to_string()),
            ("x-ratelimit-resource", "core".to_string()),
        ] {
            first.response.headers.insert(name.to_string(), value);
        }
        let http = Arc::new(ReplayHttpClient::from_interactions("files", [first]).unwrap());
        let client = GithubClient::anonymous(http.clone());
        let limits = ReadFiles {
            max_pages: 3,
            keep_requests: 10,
        };

        let read = block_on(client.read_pull_files(&PullRef::new("o", "r", 7), None, limits));

        assert_eq!(read.unwrap().cut_short, Some(CutShort::RateLimit));
        assert_eq!(http.seen().len(), 1);
        // With so few left, not even a first page is read.
        let read = block_on(client.read_pull_files(&PullRef::new("o", "r", 7), None, limits));
        assert_eq!(read.unwrap().cut_short, Some(CutShort::RateLimit));
        assert_eq!(http.seen().len(), 1);
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
