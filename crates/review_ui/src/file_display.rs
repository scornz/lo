//! How each file shows in a pull request's diff (design §5, "The diff view"): whether it starts
//! collapsed and why, what a binary file and an image say, and a file's diff with changes to
//! whitespace alone hidden.

use std::fmt;
use std::io::Cursor;
use std::sync::Arc;

use gpui::ImageFormat;
use imara_diff::{Algorithm, Diff, InternedInput};
use review::files::{ChangedFile, Generated, LARGE_FILE_LINES, initial_state};

/// Why a file shows collapsed, which its header says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Collapsed {
    /// Generated: `.gitattributes` marks it `linguist-generated`, or it's a lockfile.
    Generated(Generated),
    /// Too large to load at once: its diff loads on a key. `why` says how large.
    Large {
        generated: Option<Generated>,
        why: String,
    },
    /// Every change in it is to whitespace, and whitespace changes are hidden.
    WhitespaceOnly,
    /// Its text didn't change: it was renamed, or only its mode or line endings changed.
    NoChanges,
}

impl Collapsed {
    /// What the file's header says, where `load_key` is the key that loads a large file.
    pub(crate) fn label(&self, load_key: &str) -> String {
        match self {
            Self::Generated(generated) => generated_label(*generated).to_string(),
            Self::Large {
                generated: None, ..
            } => format!("large: load with {load_key}"),
            Self::Large {
                generated: Some(generated),
                ..
            } => format!(
                "{}, large: load with {load_key}",
                generated_label(*generated)
            ),
            Self::WhitespaceOnly => "whitespace only".to_string(),
            Self::NoChanges => "no changes".to_string(),
        }
    }

    /// The label's tooltip.
    pub(crate) fn detail(&self, load_key: &str) -> String {
        match self {
            Self::Generated(Generated::Attribute) => {
                "The repository's .gitattributes marks this file linguist-generated, so it starts \
                 collapsed."
                    .to_string()
            }
            Self::Generated(Generated::Lockfile) => "Lockfiles start collapsed.".to_string(),
            Self::Large { why, .. } => {
                format!("This file isn't loaded: {why}. {load_key} loads it.")
            }
            Self::WhitespaceOnly => {
                "Every change to this file is to whitespace, and whitespace changes are hidden."
                    .to_string()
            }
            Self::NoChanges => "This file's text didn't change.".to_string(),
        }
    }

    /// Whether the file's diff isn't loaded until a key loads it.
    pub(crate) fn is_large(&self) -> bool {
        matches!(self, Self::Large { .. })
    }
}

fn generated_label(generated: Generated) -> &'static str {
    match generated {
        Generated::Attribute => "generated",
        Generated::Lockfile => "lockfile",
    }
}

/// Why a file starts collapsed, or `None` when it starts expanded, from
/// `review::files::initial_state`: generated files and lockfiles, and files too large to load
/// at once. Both diffs, GitHub's and the local copy's, decide with this. `loads` says whether
/// rozed would load anything for the file: a binary file that isn't an image shows only that it
/// changed, so its size doesn't matter.
pub(crate) fn initial_collapse(file: &ChangedFile, loads: bool) -> Option<Collapsed> {
    let state = initial_state(file);
    if state.large && loads {
        return Some(Collapsed::Large {
            generated: state.generated,
            why: large_why(file),
        });
    }
    state.generated.map(Collapsed::Generated)
}

/// How large a file is, for [`Collapsed::Large`]: its changed lines when there are too many,
/// otherwise its size.
fn large_why(file: &ChangedFile) -> String {
    let (additions, deletions) = (file.additions, file.deletions);
    if file.changed_lines() > LARGE_FILE_LINES {
        format!(
            "{additions} lines added and {deletions} deleted, more than the {LARGE_FILE_LINES} \
             rozed loads at once"
        )
    } else {
        format!(
            "{}, more than the 1 MiB rozed loads at once",
            size_text(file.size.unwrap_or_default())
        )
    }
}

/// A size in bytes, as "512 B", "2.1 KiB" or "3.4 MiB".
pub(crate) fn size_text(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.1} KiB", bytes as f64 / KIB),
        _ => format!("{:.1} MiB", bytes as f64 / (KIB * KIB)),
    }
}

/// What a binary file that isn't an image shows: that it changed, and its sizes.
pub(crate) fn binary_text(old: Option<u64>, new: Option<u64>) -> String {
    let size = |size: Option<u64>| size.map_or_else(|| "size unknown".to_string(), size_text);
    match (old, new) {
        (Some(_), Some(_)) | (None, None) => {
            format!("(binary file changed: {} → {})", size(old), size(new))
        }
        (None, Some(_)) => format!("(binary file added: {})", size(new)),
        (Some(_), None) => format!("(binary file deleted: {})", size(old)),
    }
}

/// One side of a changed image, read from git. It's decoded only when it shows, and then only to a
/// small preview (`image_preview`).
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ImageSide {
    pub(crate) format: ImageFormat,
    pub(crate) bytes: Arc<[u8]>,
    /// Width and height in pixels, from the image's header. `None` for SVG.
    pub(crate) dimensions: Option<(u32, u32)>,
    /// How many frames a GIF has, counted from its blocks, when it has more than one. Only the
    /// first shows.
    pub(crate) frames: Option<usize>,
}

impl fmt::Debug for ImageSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageSide")
            .field("format", &self.format)
            .field("bytes", &self.bytes.len())
            .field("dimensions", &self.dimensions)
            .field("frames", &self.frames)
            .finish()
    }
}

impl ImageSide {
    /// `bytes` as an image, when `path` names one of the formats rozed shows (PNG, JPEG, GIF,
    /// WebP and SVG) and the bytes are one of them. `None` otherwise, such as for a Git LFS
    /// pointer, which is text. Reads only the image's header.
    pub(crate) fn new(path: &str, bytes: Vec<u8>) -> Option<Self> {
        let extension = path.rsplit_once('.')?.1.to_ascii_lowercase();
        if extension == "svg" {
            let text = std::str::from_utf8(&bytes).ok()?;
            return text.contains("<svg").then(|| Self {
                format: ImageFormat::Svg,
                bytes: bytes.into(),
                dimensions: None,
                frames: None,
            });
        }
        if !["png", "jpg", "jpeg", "gif", "webp"].contains(&extension.as_str()) {
            return None;
        }
        // By its bytes, not its name: a `.png` holding a JPEG still shows.
        let found = image::guess_format(&bytes).ok()?;
        let format = match found {
            image::ImageFormat::Png => ImageFormat::Png,
            image::ImageFormat::Jpeg => ImageFormat::Jpeg,
            image::ImageFormat::Gif => ImageFormat::Gif,
            image::ImageFormat::WebP => ImageFormat::Webp,
            _ => return None,
        };
        let dimensions = image::ImageReader::with_format(Cursor::new(&bytes), found)
            .into_dimensions()
            .ok()?;
        let frames = (format == ImageFormat::Gif)
            .then(|| crate::image_preview::gif_frames(&bytes))
            .flatten()
            .filter(|frames| *frames > 1);
        Some(Self {
            format,
            bytes: bytes.into(),
            dimensions: Some(dimensions),
            frames,
        })
    }

    /// The image's line in the diff: its format, dimensions, frames and size.
    pub(crate) fn summary(&self) -> String {
        let format = match self.format {
            ImageFormat::Png => "PNG",
            ImageFormat::Jpeg => "JPEG",
            ImageFormat::Gif => "GIF",
            ImageFormat::Webp => "WebP",
            ImageFormat::Svg => "SVG",
            _ => "image",
        };
        let mut parts = vec![format!("{format} image")];
        if let Some((width, height)) = self.dimensions {
            parts.push(format!("{width} × {height}"));
        }
        if let Some(frames) = self.frames {
            parts.push(format!("animated, {frames} frames"));
        }
        parts.push(size_text(self.bytes.len() as u64));
        parts.join(", ")
    }
}

/// The two sides of a changed image. A side is `None` when the pull request adds or deletes the
/// image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Images {
    pub(crate) old: Option<ImageSide>,
    pub(crate) new: Option<ImageSide>,
}

/// `old` with each line whose change was only to whitespace replaced by `new`'s line, as `git diff
/// -w` shows it: compared with `new`, only changes to more than whitespace remain. Whitespace is
/// what git and GitHub ignore: spaces, tabs, carriage returns and line feeds, never other Unicode
/// spaces, which can change what code means. Lines keep their places and their own line endings,
/// so line numbers stay `old`'s, and lines that changed in more than whitespace keep `old`'s text.
pub(crate) fn without_whitespace_changes(old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.split_inclusive('\n').collect();
    let new_lines: Vec<&str> = new.split_inclusive('\n').collect();
    let key = |line: &&str| -> String {
        line.chars()
            .filter(|c| !matches!(c, ' ' | '\t' | '\r' | '\n'))
            .collect()
    };
    let mut input = InternedInput::default();
    input.update_before(old_lines.iter().map(key));
    input.update_after(new_lines.iter().map(key));
    let diff = Diff::compute(Algorithm::Histogram, &input);

    let mut text = String::with_capacity(old.len());
    let (mut at_old, mut at_new) = (0, 0);
    // A matched line is the head's, ended as the base's was, so a head without a final newline
    // never joins two base lines, nor a base without one gains a row.
    let matched = |text: &mut String, to: usize, at_old: &mut usize, at_new: &mut usize| {
        while *at_old < to {
            text.push_str(new_lines[*at_new].trim_end_matches('\n'));
            if old_lines[*at_old].ends_with('\n') {
                text.push('\n');
            }
            *at_old += 1;
            *at_new += 1;
        }
    };
    for hunk in diff.hunks() {
        matched(
            &mut text,
            hunk.before.start as usize,
            &mut at_old,
            &mut at_new,
        );
        for line in &old_lines[hunk.before.start as usize..hunk.before.end as usize] {
            text.push_str(line);
        }
        at_old = hunk.before.end as usize;
        at_new = hunk.after.end as usize;
    }
    matched(&mut text, old_lines.len(), &mut at_old, &mut at_new);
    text
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn file(path: &str, changed_lines: u32, size: Option<u64>) -> ChangedFile {
        ChangedFile {
            path: path.into(),
            additions: changed_lines,
            size,
            ..Default::default()
        }
    }

    #[test]
    fn generated_files_and_lockfiles_start_collapsed_saying_which() {
        let attribute = initial_collapse(
            &ChangedFile {
                linguist_generated: Some(true),
                ..file("api.pb.go", 10, None)
            },
            true,
        );
        assert_eq!(attribute, Some(Collapsed::Generated(Generated::Attribute)));
        assert_eq!(attribute.unwrap().label("⌥A"), "generated");

        let lockfile = initial_collapse(&file("Cargo.lock", 10, None), true);
        assert_eq!(lockfile.unwrap().label("⌥A"), "lockfile");

        assert_eq!(initial_collapse(&file("src/lib.rs", 10, None), true), None);
    }

    #[test]
    fn large_files_start_collapsed_and_say_which_key_loads_them() {
        let large = initial_collapse(&file("data.txt", 9_000, None), true).unwrap();
        assert!(large.is_large());
        assert_eq!(large.label("⌥A"), "large: load with ⌥A");
        assert_eq!(
            large.detail("⌥A"),
            "This file isn't loaded: 9000 lines added and 0 deleted, more than the 5000 rozed \
             loads at once. ⌥A loads it."
        );

        let large_lockfile = initial_collapse(&file("Cargo.lock", 9_000, None), true).unwrap();
        assert_eq!(large_lockfile.label("⌥A"), "lockfile, large: load with ⌥A");

        let heavy = initial_collapse(&file("app.min.js", 1, Some(2 * 1024 * 1024)), true);
        assert_eq!(
            heavy,
            Some(Collapsed::Large {
                generated: None,
                why: "2.0 MiB, more than the 1 MiB rozed loads at once".into()
            })
        );
    }

    #[test]
    fn a_large_binary_file_that_loads_nothing_isnt_collapsed() {
        let binary = |path| ChangedFile {
            binary: true,
            ..file(path, 0, Some(9 * 1024 * 1024))
        };
        assert_eq!(initial_collapse(&binary("data.bin"), false), None);
        assert_eq!(
            initial_collapse(&binary("bun.lockb"), false),
            Some(Collapsed::Generated(Generated::Lockfile))
        );
    }

    #[test]
    fn sizes_read_in_the_largest_whole_unit() {
        assert_eq!(size_text(0), "0 B");
        assert_eq!(size_text(1023), "1023 B");
        assert_eq!(size_text(1024), "1.0 KiB");
        assert_eq!(size_text(2150), "2.1 KiB");
        assert_eq!(size_text(3 * 1024 * 1024 + 400_000), "3.4 MiB");
    }

    #[test]
    fn binary_files_say_they_changed_and_their_sizes() {
        assert_eq!(
            binary_text(Some(2150), Some(4096)),
            "(binary file changed: 2.1 KiB → 4.0 KiB)"
        );
        assert_eq!(binary_text(None, Some(10)), "(binary file added: 10 B)");
        assert_eq!(binary_text(Some(10), None), "(binary file deleted: 10 B)");
    }

    /// A 3 × 2 PNG.
    pub(crate) fn png() -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbaImage::from_pixel(3, 2, image::Rgba([255, 0, 0, 255]))
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    #[test]
    fn images_are_read_by_their_bytes_when_their_name_says_image() {
        let logo = ImageSide::new("assets/logo.png", png()).unwrap();
        assert_eq!(logo.format, ImageFormat::Png);
        assert_eq!(logo.dimensions, Some((3, 2)));
        assert!(logo.summary().starts_with("PNG image, 3 × 2, "), "{logo:?}");

        // The name decides whether it's an image, case aside, and the bytes which format.
        assert!(ImageSide::new("LOGO.JPG", png()).is_some());
        assert_eq!(
            ImageSide::new("logo.jpg", png()).unwrap().format,
            ImageFormat::Png
        );
        assert_eq!(ImageSide::new("logo.bin", png()), None);
        assert_eq!(ImageSide::new("logo", png()), None);
        // A Git LFS pointer isn't an image.
        let pointer = b"version https://git-lfs.github.com/spec/v1\noid sha256:ab\nsize 12\n";
        assert_eq!(ImageSide::new("logo.png", pointer.to_vec()), None);
    }

    #[test]
    fn an_animated_gif_says_how_many_frames_it_has() {
        let gif =
            ImageSide::new("spin.gif", crate::image_preview::tests::gif_of(30, 20, 3)).unwrap();
        assert_eq!(gif.frames, Some(3));
        assert!(
            gif.summary()
                .starts_with("GIF image, 30 × 20, animated, 3 frames, "),
            "{gif:?}"
        );
        let still =
            ImageSide::new("dot.gif", crate::image_preview::tests::gif_of(1, 1, 1)).unwrap();
        assert_eq!(still.frames, None);
        assert!(still.summary().starts_with("GIF image, 1 × 1, "));
    }

    #[test]
    fn svg_files_are_images_when_they_hold_svg() {
        let svg = ImageSide::new(
            "icon.svg",
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"/>"#.to_vec(),
        )
        .unwrap();
        assert_eq!(svg.format, ImageFormat::Svg);
        assert_eq!(svg.summary(), "SVG image, 62 B");
        assert_eq!(ImageSide::new("icon.svg", b"not svg".to_vec()), None);
    }

    #[test]
    fn indentation_alone_leaves_no_change() {
        let old = "fn main() {\nprintln!(\"hi\");\n}\n";
        let new = "fn main() {\n    println!(\"hi\");\n}\n";
        assert_eq!(without_whitespace_changes(old, new), new);
    }

    #[test]
    fn changes_to_more_than_whitespace_stay_with_the_bases_text() {
        let old = "a\n  b\nc\nd  \n";
        let new = "a\n\tb\nC\nd\n";
        // b's indentation and d's trailing spaces read as the head's; c's change stays.
        assert_eq!(without_whitespace_changes(old, new), "a\n\tb\nc\nd\n");
    }

    #[test]
    fn added_and_deleted_lines_keep_their_places() {
        let old = "keep\ngone\n  moved in\nend\n";
        let new = "keep\nmoved in\nnew line\nend\n";
        // `gone` is deleted and `new line` added; `moved in` only lost its indentation.
        assert_eq!(
            without_whitespace_changes(old, new),
            "keep\ngone\nmoved in\nend\n"
        );
    }

    #[test]
    fn a_blank_line_added_is_still_a_change() {
        let old = "a\nb\n";
        let new = "a\n\nb\n";
        assert_eq!(without_whitespace_changes(old, new), old);
    }

    #[test]
    fn only_spaces_tabs_and_line_breaks_count_as_whitespace() {
        // In JavaScript a line separator ends the line, so cleanup() becomes code.
        let old = "// TODO: call cleanup() later\n";
        let new = "// TODO: call\u{2028}cleanup() later\n";
        assert_eq!(without_whitespace_changes(old, new), old);
        // A no-break space in a string, an ideographic space, a form feed.
        for (old, new) in [
            ("s = \"a b\";\n", "s = \"a\u{a0}b\";\n"),
            ("x = 1\n", "x\u{3000}= 1\n"),
            ("a b\n", "a\u{c}b\n"),
        ] {
            assert_eq!(without_whitespace_changes(old, new), old, "{new:?}");
        }
        // Tabs and carriage returns are whitespace, as for `git diff -w`.
        assert_eq!(without_whitespace_changes("a b\n", "a\tb\r\n"), "a\tb\r\n");
    }

    #[test]
    fn a_missing_final_newline_on_either_side_keeps_the_bases_lines() {
        // The head drops the base's last line and its own final newline.
        assert_eq!(without_whitespace_changes("a\nb\nc\n", "a\nb"), "a\nb\nc\n");
        assert_eq!(
            without_whitespace_changes("one\ntwo\nthree\nfour\n", "one\n  two\nthree"),
            "one\n  two\nthree\nfour\n"
        );
        // The base has no final newline, and the head adds a line after it.
        assert_eq!(without_whitespace_changes("a\n b", "a\nb\nc\n"), "a\nb");
        for (old, new) in [("a\nb\nc\n", "a\nb"), ("a\n b", "a\nb\nc\n")] {
            assert_eq!(
                without_whitespace_changes(old, new).matches('\n').count(),
                old.matches('\n').count(),
                "{old:?} against {new:?}"
            );
        }
    }

    #[test]
    fn whitespace_inside_a_line_is_ignored_too() {
        let old = "let x = a+b;\n";
        let new = "let x = a + b;\n";
        assert_eq!(without_whitespace_changes(old, new), new);
    }
}
