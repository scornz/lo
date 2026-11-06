//! Turning a pull request's images into small previews, safely.
//!
//! A pull request's images are untrusted: a few hundred bytes can declare a huge canvas, and an SVG
//! can point an `<image>` at any file on disk. So rozed checks an image's declared size before
//! decoding it, decodes with explicit limits and only a GIF's first frame, renders SVGs with
//! options that resolve no `<image>` at all, and keeps only a preview small enough to draw.

use std::io::Cursor;
use std::sync::{Arc, LazyLock};

use gpui::ImageFormat;
use image::{DynamicImage, ImageDecoder as _, ImageReader, Limits, RgbaImage};

/// The longest side rozed decodes, in pixels.
pub(crate) const MAX_SIDE: u32 = 8192;

/// The most pixels rozed decodes.
pub(crate) const MAX_PIXELS: u64 = 40_000_000;

/// The most memory a decoder may allocate.
const MAX_ALLOC: u64 = 256 * 1024 * 1024;

/// The longest side of a preview, in pixels: about twice a panel's height and width on a Retina
/// display.
pub(crate) const PREVIEW_SIDE: u32 = 1024;

/// A decoded preview, in BGRA as gpui draws it, no larger than [`PREVIEW_SIDE`] a side.
pub(crate) type Preview = image::RgbaImage;

/// Why an image is too large to decode, as its panel says it, from its declared size; `None`
/// when it's within the limits.
pub(crate) fn too_large(width: u32, height: u32) -> Option<String> {
    let over =
        width > MAX_SIDE || height > MAX_SIDE || u64::from(width) * u64::from(height) > MAX_PIXELS;
    over.then(|| format!("too large to preview: {width} × {height}"))
}

/// Decodes `bytes`, an image of `format`, to a preview. Raster images are checked against their
/// declared `dimensions` first.
pub(crate) fn preview(
    format: ImageFormat,
    bytes: &[u8],
    dimensions: Option<(u32, u32)>,
) -> Result<Preview, String> {
    let rgba = match format {
        ImageFormat::Svg => render_svg(bytes)?,
        _ => decode_raster(format, bytes, dimensions)?,
    };
    Ok(to_bgra(rgba))
}

fn decode_raster(
    format: ImageFormat,
    bytes: &[u8],
    dimensions: Option<(u32, u32)>,
) -> Result<RgbaImage, String> {
    let format = match format {
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Gif => image::ImageFormat::Gif,
        ImageFormat::Webp => image::ImageFormat::WebP,
        other => return Err(format!("{other:?} images aren't previewed")),
    };
    let (width, height) = dimensions.ok_or("its size couldn't be read")?;
    if let Some(why) = too_large(width, height) {
        return Err(why);
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_ALLOC);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|error| error.to_string())?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    // A GIF decodes as its first frame alone: an image decoder reads one image.
    let mut image = DynamicImage::from_decoder(decoder).map_err(|error| error.to_string())?;
    image.apply_orientation(orientation);
    Ok(downscale(image).into_rgba8())
}

/// `image` scaled down to fit [`PREVIEW_SIDE`] a side, keeping its shape.
fn downscale(image: DynamicImage) -> DynamicImage {
    if image.width() <= PREVIEW_SIDE && image.height() <= PREVIEW_SIDE {
        image
    } else {
        image.thumbnail(PREVIEW_SIDE, PREVIEW_SIDE)
    }
}

/// How many frames a GIF has, counted from its blocks without decoding any, or `None` when it
/// can't be read that far.
pub(crate) fn gif_frames(bytes: &[u8]) -> Option<usize> {
    let mut at = 13;
    let flags = *bytes.get(10)?;
    if flags & 0x80 != 0 {
        at += 3 << ((flags & 0x07) + 1);
    }
    let mut frames = 0;
    // Skips a run of sub-blocks, each a length byte then that many bytes, up to the empty one.
    let skip_sub_blocks = |at: &mut usize| -> Option<()> {
        loop {
            let len = usize::from(*bytes.get(*at)?);
            *at += 1 + len;
            if len == 0 {
                return Some(());
            }
        }
    };
    loop {
        match *bytes.get(at)? {
            // Trailer.
            0x3B => return Some(frames),
            // Extension: its label, then sub-blocks.
            0x21 => {
                at += 2;
                skip_sub_blocks(&mut at)?;
            }
            // Image descriptor, an optional local colour table, the LZW code size, sub-blocks.
            0x2C => {
                let flags = *bytes.get(at + 9)?;
                at += 10;
                if flags & 0x80 != 0 {
                    at += 3 << ((flags & 0x07) + 1);
                }
                at += 1;
                skip_sub_blocks(&mut at)?;
                frames += 1;
            }
            _ => return None,
        }
    }
}

/// System fonts, for an SVG's text, loaded once.
static FONTS: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
    let mut fonts = usvg::fontdb::Database::new();
    fonts.load_system_fonts();
    Arc::new(fonts)
});

/// Options for an SVG from a pull request: no `<image>` resolves, neither a path nor a `data:`
/// URL, so rendering one reads nothing from disk and decodes nothing it embeds.
fn svg_options() -> usvg::Options<'static> {
    usvg::Options {
        resources_dir: None,
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        fontdb: FONTS.clone(),
        ..Default::default()
    }
}

/// Renders an SVG to fit [`PREVIEW_SIDE`] a side, at up to twice its own size.
fn render_svg(bytes: &[u8]) -> Result<RgbaImage, String> {
    let tree = usvg::Tree::from_data(bytes, &svg_options()).map_err(|error| error.to_string())?;
    let size = tree.size();
    let longest = size.width().max(size.height());
    if !(longest.is_finite() && longest > 0.) {
        return Err("it has no size".to_string());
    }
    let scale = (PREVIEW_SIDE as f32 / longest).min(2.);
    let width = (size.width() * scale).ceil().clamp(1., PREVIEW_SIDE as f32) as u32;
    let height = (size.height() * scale)
        .ceil()
        .clamp(1., PREVIEW_SIDE as f32) as u32;
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(width, height).ok_or("it couldn't be drawn at that size")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // Premultiplied, as tiny-skia draws; gpui wants straight alpha.
    let mut rgba = pixmap.take();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        if alpha > 0 && alpha < 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    RgbaImage::from_raw(width, height, rgba).ok_or_else(|| "it drew no pixels".to_string())
}

/// `image`'s pixels swapped from RGBA to BGRA, as gpui draws them.
fn to_bgra(mut image: RgbaImage) -> Preview {
    for pixel in image.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    image
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use std::time::{Duration, Instant};

    /// A `width` × `height` PNG of one colour.
    pub(crate) fn png_of(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbaImage::from_pixel(width, height, image::Rgba([255, 0, 0, 255]))
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    /// A GIF with a `width` × `height` screen and `frames` frames of 1 × 1 pixel: a few hundred
    /// bytes that declare a large canvas.
    pub(crate) fn gif_of(width: u16, height: u16, frames: usize) -> Vec<u8> {
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0]);
        for _ in 0..frames {
            // A graphic control extension, then a 1 × 1 frame with a two-colour local table.
            bytes.extend_from_slice(&[0x21, 0xF9, 0x04, 0x00, 0x0A, 0x00, 0x00, 0x00]);
            bytes.extend_from_slice(&[0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0x80]);
            bytes.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
            bytes.extend_from_slice(&[0x02, 0x02, 0x44, 0x01, 0x00]);
        }
        bytes.push(0x3B);
        bytes
    }

    fn opaque_pixels(preview: &Preview) -> usize {
        preview
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[3] != 0)
            .count()
    }

    #[test]
    fn an_svg_image_element_reads_nothing_from_disk() {
        let dir = tempfile::TempDir::new().unwrap();
        let png = dir.path().join("secret.png");
        std::fs::write(&png, png_of(20, 20)).unwrap();
        for href in [png.to_str().unwrap(), "/dev/zero", "secret.png"] {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="20" height="20"><image xlink:href="{href}" width="20" height="20"/></svg>"#
            );
            let started = Instant::now();
            let drawn = preview(ImageFormat::Svg, svg.as_bytes(), None).unwrap();
            assert!(started.elapsed() < Duration::from_secs(5), "{href}");
            // An image from the file would have drawn opaque red.
            assert_eq!(opaque_pixels(&drawn), 0, "{href}");
        }
    }

    #[test]
    fn an_svg_data_image_is_refused_too() {
        let png = png_of(4, 4);
        let data = base64_of(&png);
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><image href="data:image/png;base64,{data}" width="4" height="4"/></svg>"#
        );
        let drawn = preview(ImageFormat::Svg, svg.as_bytes(), None).unwrap();
        assert_eq!(opaque_pixels(&drawn), 0);
    }

    fn base64_of(bytes: &[u8]) -> String {
        const DIGITS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk.iter().fold(0u32, |n, byte| n << 8 | u32::from(*byte))
                << (8 * (3 - chunk.len()));
            for i in 0..=chunk.len() {
                out.push(DIGITS[(n >> (18 - 6 * i) & 63) as usize] as char);
            }
            out.push_str(&"=".repeat(3 - chunk.len()));
        }
        out
    }

    #[test]
    fn an_svg_draws_its_own_shapes_at_up_to_twice_its_size() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="5"><rect width="10" height="5" fill="blue"/></svg>"#;
        let drawn = preview(ImageFormat::Svg, svg, None).unwrap();
        assert_eq!(drawn.dimensions(), (20, 10));
        assert_eq!(opaque_pixels(&drawn), 200);
        // BGRA: blue comes first.
        assert_eq!(&drawn.as_raw()[..4], &[255, 0, 0, 255]);

        let huge = br#"<svg xmlns="http://www.w3.org/2000/svg" width="40000" height="20000"/>"#;
        assert_eq!(
            preview(ImageFormat::Svg, huge, None).unwrap().dimensions(),
            (PREVIEW_SIDE, PREVIEW_SIDE / 2)
        );
    }

    #[test]
    fn an_image_too_large_for_its_header_is_never_decoded() {
        assert_eq!(too_large(8192, 4882), None);
        assert_eq!(
            too_large(8192, 4883).as_deref(),
            Some("too large to preview: 8192 × 4883")
        );
        assert_eq!(
            too_large(8193, 1).as_deref(),
            Some("too large to preview: 8193 × 1")
        );
        assert_eq!(
            too_large(1, 16385).as_deref(),
            Some("too large to preview: 1 × 16385")
        );
        // 11000 × 11000 pixels in a 37 KB PNG, or a GIF of a few hundred bytes.
        let started = Instant::now();
        assert_eq!(
            preview(ImageFormat::Png, &png_of(1, 1), Some((11000, 11000))),
            Err("too large to preview: 11000 × 11000".to_string())
        );
        let gif = gif_of(65535, 65535, 2);
        assert_eq!(
            preview(ImageFormat::Gif, &gif, Some((65535, 65535))),
            Err("too large to preview: 65535 × 65535".to_string())
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_header_that_lies_is_caught_by_the_decoders_limits() {
        // The header says 2 × 2; the decoder reads the real 8193 × 1 and refuses it.
        let real = png_of(8193, 1);
        assert!(preview(ImageFormat::Png, &real, Some((2, 2))).is_err());
    }

    #[test]
    fn a_large_image_keeps_only_a_small_preview() {
        let drawn = preview(ImageFormat::Png, &png_of(3000, 1500), Some((3000, 1500))).unwrap();
        assert_eq!(drawn.dimensions(), (PREVIEW_SIDE, PREVIEW_SIDE / 2));
        // BGRA: red comes third.
        assert_eq!(&drawn.as_raw()[..4], &[0, 0, 255, 255]);

        let small = preview(ImageFormat::Png, &png_of(3, 2), Some((3, 2))).unwrap();
        assert_eq!(small.dimensions(), (3, 2));
    }

    #[test]
    fn a_gif_decodes_its_first_frame_only_and_counts_the_rest() {
        let gif = gif_of(4096, 4096, 8);
        assert_eq!(gif_frames(&gif), Some(8));
        let drawn = preview(ImageFormat::Gif, &gif, Some((4096, 4096))).unwrap();
        // One frame, scaled down: 4 MiB, not eight frames of 64 MiB.
        assert_eq!(drawn.dimensions(), (PREVIEW_SIDE, PREVIEW_SIDE));

        assert_eq!(gif_frames(&gif_of(3, 3, 1)), Some(1));
        assert_eq!(gif_frames(b"GIF89a\x01\x00"), None);
        assert_eq!(gif_frames(&gif[..gif.len() - 3]), None);
    }
}
