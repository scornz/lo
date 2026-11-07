//! Links in a pull request's text, which Lo opens in the browser only when they're web pages.
//! A `file://` link, or one with an app's scheme, would hand a file or an app to macOS, which may
//! run it (design §1, "Never runs a repo's code").

use std::ops::Range;

use editor::Editor;
use gpui::App;
use http_client::Url;
use language::{BufferSnapshot, ToOffset as _};
use linkify::{LinkFinder, LinkKind};

/// How far either side of the cursor a link is looked for, as Zed's editor does.
const LIMIT: usize = 2048;

/// Whether `link` is a web page: an `http` or `https` URL with a host.
pub fn is_web_link(link: &str) -> bool {
    Url::parse(link).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some_and(|h| !h.is_empty())
    })
}

/// Opens `link` in the browser if it's a web page. Returns whether it did.
pub fn open_web_link(link: &str, cx: &mut App) -> bool {
    let web = is_web_link(link);
    if web {
        cx.open_url(link);
    } else {
        log::info!("not opening a link that isn't http or https");
    }
    web
}

/// The link at `editor`'s cursor, or the one its selection holds, found as Zed's `editor::OpenUrl`
/// finds it, whatever its scheme.
pub fn link_at_cursor(editor: &Editor, cx: &App) -> Option<String> {
    let selection = editor.selections.newest_anchor();
    let (head, tail) = (selection.head(), selection.tail());
    let multi_buffer = editor.buffer().read(cx);
    let (buffer, head) = multi_buffer.text_anchor_for_position(head, cx)?;
    let snapshot = buffer.read(cx).snapshot();
    let head = head.to_offset(&snapshot);
    if selection.start == selection.end {
        return link_around(&snapshot, head);
    }
    let (_, tail) = multi_buffer.text_anchor_for_position(tail, cx)?;
    let tail = tail.to_offset(&snapshot);
    link_in(&snapshot, head.min(tail)..head.max(tail))
}

/// The link whose text holds `offset`, in the run of text around it with no whitespace.
fn link_around(snapshot: &BufferSnapshot, offset: usize) -> Option<String> {
    let before: usize = snapshot
        .reversed_chars_at(offset)
        .take(LIMIT)
        .take_while(|c| !c.is_whitespace())
        .map(char::len_utf8)
        .sum();
    let after: usize = snapshot
        .chars_at(offset)
        .take(LIMIT)
        .take_while(|c| !c.is_whitespace())
        .map(char::len_utf8)
        .sum();
    let start = offset - before;
    let text: String = snapshot.text_for_range(start..offset + after).collect();
    links(&text)
        .find(|range| range.start <= before && before <= range.end)
        .map(|range| text[range].to_string())
}

/// The link that's the whole of the selected `range`, less the whitespace around it.
fn link_in(snapshot: &BufferSnapshot, range: Range<usize>) -> Option<String> {
    if range.len() >= LIMIT {
        return None;
    }
    let text: String = snapshot.text_for_range(range).collect();
    let text = text.trim();
    links(text)
        .next()
        .filter(|range| *range == (0..text.len()))
        .map(|range| text[range].to_string())
}

/// Where the URLs in `text` are, by byte.
fn links(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url]);
    finder
        .links(text)
        .map(|link| link.start()..link.end())
        .collect::<Vec<_>>()
        .into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_and_https_links_with_a_host_are_web_links() {
        for link in [
            "https://github.com/sharkdp/fd",
            "http://example.com",
            "HTTPS://EXAMPLE.COM/a?b#c",
        ] {
            assert!(is_web_link(link), "{link}");
        }
        for link in [
            "file:///Applications/Calculator.app",
            "file://localhost/etc/hosts",
            "javascript:alert(1)",
            "ssh://example.com",
            "vscode://file/etc/hosts",
            "zed://settings",
            "x-man-page://ls",
            "smb://server/share",
            "http://",
            "/etc/hosts",
            "example.com",
        ] {
            assert!(!is_web_link(link), "{link}");
        }
    }

    #[test]
    fn links_are_found_whatever_their_scheme() {
        let found = |text: &str| -> Vec<String> {
            links(text).map(|range| text[range].to_string()).collect()
        };
        assert_eq!(
            found("see https://example.com/a, and (http://example.com/b)."),
            ["https://example.com/a", "http://example.com/b"]
        );
        assert_eq!(
            found("file:///Applications/Calculator.app ssh://host/x"),
            ["file:///Applications/Calculator.app", "ssh://host/x"]
        );
    }
}
