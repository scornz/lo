//! Turning a pull request's images into small previews, in a helper process.
//!
//! A pull request's images are untrusted: a few hundred bytes can declare a huge canvas, an SVG can
//! point an `<image>` at any file on disk, and an SVG's nesting, patterns, layers and filters can
//! take gigabytes, overflow a stack or run for hours however small its canvas. So rozed decodes
//! every preview in a helper process, `rozed --render-preview`, which [`crate::preview_pool`]
//! starts, watches and kills when it runs too long or grows too large. The helper reads the
//! image's format, the size it should fit and its bytes on stdin, and writes the preview, or why
//! there's none, on stdout.
//!
//! Inside the helper, decoding is still careful: a raster image's declared size is checked first,
//! decoders run with explicit limits and only a GIF's first frame decodes, and an SVG renders with
//! options that resolve no `<image>` at all. The decode runs on a thread with a large stack, and a
//! panic becomes an error.

use std::io::{Cursor, Read as _, Write as _};
use std::sync::{Arc, LazyLock};

use gpui::ImageFormat;
use image::{DynamicImage, ImageDecoder as _, ImageReader, Limits, RgbaImage};

/// The longest side rozed decodes, in pixels.
pub(crate) const MAX_SIDE: u32 = 8192;

/// The most pixels rozed decodes.
pub(crate) const MAX_PIXELS: u64 = 40_000_000;

/// The most a decoded image may take before it's scaled down: refuses, say, 40 megapixels at 16
/// bits a channel.
const DECODED_BUDGET: u64 = 192 * 1024 * 1024;

/// The most a decoder may allocate for itself besides the decoded image, such as an embedded
/// colour profile.
const DECODER_BUDGET: u64 = 64 * 1024 * 1024;

/// The longest side of a preview, in pixels.
pub(crate) const PREVIEW_SIDE: u32 = 1024;

/// The most bytes the helper reads as an image.
pub(crate) const MAX_INPUT: usize = 64 * 1024 * 1024;

/// The flag that runs rozed as the helper.
pub const HELPER_FLAG: &str = "--render-preview";

/// What starts the helper's reply on stdout, so anything printed before it is skipped.
pub(crate) const REPLY_MAGIC: &[u8] = b"\nROZED-PREVIEW-REPLY 1\n";

/// The stack the helper decodes on: usvg and resvg recurse once a nesting level.
const DECODE_STACK: usize = 64 * 1024 * 1024;

/// A decoded preview, in BGRA as gpui draws it, no larger than the size asked for.
pub(crate) type Preview = image::RgbaImage;

/// Why an image is too large to decode, as its panel says it, from its declared size; `None`
/// when it's within the limits.
pub(crate) fn too_large(width: u32, height: u32) -> Option<String> {
    let over =
        width > MAX_SIDE || height > MAX_SIDE || u64::from(width) * u64::from(height) > MAX_PIXELS;
    over.then(|| format!("too large to preview: {width} × {height}"))
}

/// The name of `format` in the helper's request.
pub(crate) fn format_name(format: ImageFormat) -> Option<&'static str> {
    Some(match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpeg",
        ImageFormat::Gif => "gif",
        ImageFormat::Webp => "webp",
        ImageFormat::Svg => "svg",
        _ => return None,
    })
}

fn parse_format(name: &str) -> Option<ImageFormat> {
    Some(match name {
        "png" => ImageFormat::Png,
        "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::Webp,
        "svg" => ImageFormat::Svg,
        _ => return None,
    })
}

/// The helper's request: the first line names the format and the size to fit, and the image's
/// bytes follow.
pub(crate) fn request_header(format: ImageFormat, fit: (u32, u32)) -> Option<String> {
    Some(format!(
        "{} {} {}\n",
        format_name(format)?,
        fit.0.max(1),
        fit.1.max(1)
    ))
}

/// Decodes `bytes`, an image of `format`, to a preview that fits `fit`, in pixels. This is what
/// the helper runs; rozed itself never calls it on a pull request's image.
pub(crate) fn render(
    format: ImageFormat,
    bytes: &[u8],
    fit: (u32, u32),
) -> Result<Preview, String> {
    let fit = (fit.0.clamp(1, PREVIEW_SIDE), fit.1.clamp(1, PREVIEW_SIDE));
    let rgba = match format {
        ImageFormat::Svg => render_svg(bytes, fit)?,
        _ => decode_raster(format, bytes, fit)?,
    };
    Ok(to_bgra(rgba))
}

fn decode_raster(format: ImageFormat, bytes: &[u8], fit: (u32, u32)) -> Result<RgbaImage, String> {
    let format = match format {
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Gif => image::ImageFormat::Gif,
        ImageFormat::Webp => image::ImageFormat::WebP,
        other => return Err(format!("{other:?} images aren't previewed")),
    };
    let mut limits = Limits::default();
    limits.max_alloc = Some(DECODER_BUDGET);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits.clone());
    // Reads the header only.
    let mut decoder = reader.into_decoder().map_err(|error| error.to_string())?;
    let (width, height) = decoder.dimensions();
    if let Some(why) = too_large(width, height) {
        return Err(why);
    }
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    // `from_decoder` doesn't count the decoded image against the limits, so this does, from its
    // own budget, before any of it is allocated.
    let mut decoded = Limits::default();
    decoded.max_alloc = Some(DECODED_BUDGET);
    if decoded.reserve(decoder.total_bytes()).is_err() {
        return Err(format!("too large to preview: {width} × {height}"));
    }
    decoder
        .set_limits(limits)
        .map_err(|error| error.to_string())?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    // A GIF decodes as its first frame alone: an image decoder reads one image.
    let image = DynamicImage::from_decoder(decoder).map_err(|error| error.to_string())?;
    // Turned after it's scaled down, so turning copies only the preview.
    let turns = matches!(
        orientation,
        image::metadata::Orientation::Rotate90
            | image::metadata::Orientation::Rotate270
            | image::metadata::Orientation::Rotate90FlipH
            | image::metadata::Orientation::Rotate270FlipH
    );
    let mut image = downscale(image, if turns { (fit.1, fit.0) } else { fit });
    image.apply_orientation(orientation);
    Ok(image.into_rgba8())
}

/// `image` scaled down to fit `fit`, keeping its shape.
fn downscale(image: DynamicImage, fit: (u32, u32)) -> DynamicImage {
    if image.width() <= fit.0 && image.height() <= fit.1 {
        image
    } else {
        image.thumbnail(fit.0, fit.1)
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

/// System fonts, for an SVG's text, loaded once, and only for an SVG that may have text.
static FONTS: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
    let mut fonts = usvg::fontdb::Database::new();
    fonts.load_system_fonts();
    Arc::new(fonts)
});

/// Whether an SVG may hold text, which needs the system's fonts.
fn may_have_text(bytes: &[u8]) -> bool {
    bytes.windows(4).any(|window| window == b"text")
}

/// Options for an SVG from a pull request: no `<image>` resolves, neither a path nor a `data:`
/// URL, so rendering one reads nothing from disk and decodes nothing it embeds.
fn svg_options(bytes: &[u8]) -> usvg::Options<'static> {
    usvg::Options {
        resources_dir: None,
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        fontdb: if may_have_text(bytes) {
            FONTS.clone()
        } else {
            Arc::default()
        },
        ..Default::default()
    }
}

/// Renders an SVG to fit `fit`, at up to twice its own size.
fn render_svg(bytes: &[u8], fit: (u32, u32)) -> Result<RgbaImage, String> {
    let tree =
        usvg::Tree::from_data(bytes, &svg_options(bytes)).map_err(|error| error.to_string())?;
    let size = tree.size();
    if !(size.width() > 0. && size.height() > 0. && size.width().is_finite()) {
        return Err("it has no size".to_string());
    }
    let scale = (fit.0 as f32 / size.width())
        .min(fit.1 as f32 / size.height())
        .min(2.);
    let width = (size.width() * scale).ceil().clamp(1., fit.0 as f32) as u32;
    let height = (size.height() * scale).ceil().clamp(1., fit.1 as f32) as u32;
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

/// Runs rozed as the preview helper, [`HELPER_FLAG`]: reads a request on stdin, writes the
/// preview or why there's none on stdout, and exits. It never starts the app.
pub fn serve_helper() -> ! {
    let reply = match read_request() {
        Ok(Request { format, fit, bytes }) => {
            // Fonts load before reading anything else is refused.
            if format == ImageFormat::Svg && may_have_text(&bytes) {
                LazyLock::force(&FONTS);
            }
            sandbox::enter();
            decode_on_big_stack(format, bytes, fit)
        }
        Err(error) => Err(error),
    };
    let mut out = std::io::stdout().lock();
    let written = out.write_all(REPLY_MAGIC).and_then(|()| match &reply {
        Ok(preview) => {
            writeln!(out, "ok {} {}", preview.width(), preview.height())?;
            out.write_all(preview.as_raw())
        }
        Err(error) => writeln!(out, "err {}", error.replace('\n', " ")),
    });
    let flushed = written.and_then(|()| out.flush());
    std::process::exit(if flushed.is_ok() { 0 } else { 1 })
}

/// What the helper is asked: the image's format, the size to fit and its bytes.
struct Request {
    format: ImageFormat,
    fit: (u32, u32),
    bytes: Vec<u8>,
}

fn read_request() -> Result<Request, String> {
    let mut input = Vec::new();
    std::io::stdin()
        .lock()
        .take(MAX_INPUT as u64 + 4096)
        .read_to_end(&mut input)
        .map_err(|error| format!("couldn't read the image: {error}"))?;
    let end = input
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or("the request has no header")?;
    let header =
        std::str::from_utf8(&input[..end]).map_err(|_| "the request's header isn't text")?;
    let mut fields = header.split(' ');
    let (Some(format), Some(width), Some(height), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return Err(format!("the request's header is {header:?}"));
    };
    let format = parse_format(format).ok_or_else(|| format!("no format {format:?}"))?;
    let fit = (
        width
            .parse()
            .map_err(|_| "the request's width isn't a number")?,
        height
            .parse()
            .map_err(|_| "the request's height isn't a number")?,
    );
    input.drain(..=end);
    if input.len() > MAX_INPUT {
        return Err("the image is too large to read".to_string());
    }
    Ok(Request {
        format,
        fit,
        bytes: input,
    })
}

/// [`render`] on a thread with [`DECODE_STACK`] of stack, with a panic as an error.
fn decode_on_big_stack(
    format: ImageFormat,
    bytes: Vec<u8>,
    fit: (u32, u32),
) -> Result<Preview, String> {
    let thread = std::thread::Builder::new()
        .name("preview".into())
        .stack_size(DECODE_STACK)
        .spawn(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render(format, &bytes, fit)))
        })
        .map_err(|error| format!("couldn't start decoding: {error}"))?;
    match thread.join() {
        Ok(Ok(result)) => result,
        Ok(Err(_)) | Err(_) => Err("the decoder failed".to_string()),
    }
}

/// Keeps the helper, once it has read its image, from reading files or using the network.
pub(crate) mod sandbox {
    use std::ffi::{CString, c_char, c_int};

    unsafe extern "C" {
        fn sandbox_init(profile: *const c_char, flags: u64, errorbuf: *mut *mut c_char) -> c_int;
        fn sandbox_free_error(errorbuf: *mut c_char);
    }

    /// Everything the helper still does is allowed: computing, allocating and writing to the
    /// pipe it was given.
    const PROFILE: &str =
        "(version 1) (allow default) (deny network*) (deny file-read*) (deny file-write*)";

    /// Applies [`PROFILE`] to the helper for the rest of its life. A sandbox that can't be
    /// applied leaves the helper as it is: its limits still hold.
    pub(crate) fn enter() {
        let profile = CString::new(PROFILE).expect("no NUL in the profile");
        let mut error: *mut c_char = std::ptr::null_mut();
        // SAFETY: the profile is a NUL-terminated string, and an error string sandbox_init
        // returns is freed with sandbox_free_error.
        unsafe {
            if sandbox_init(profile.as_ptr(), 0, &mut error) != 0 && !error.is_null() {
                sandbox_free_error(error);
            }
        }
    }
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

    /// A zlib stream of `count` zero bytes, a few bytes per 258 of them: deflate's fixed codes
    /// for one literal zero, then copies of the byte before.
    pub(crate) fn zlib_zeros(count: usize) -> Vec<u8> {
        struct Bits {
            out: Vec<u8>,
            bit: u32,
            used: u32,
        }
        impl Bits {
            // `count` bits of `value`, least significant first.
            fn put(&mut self, value: u32, count: u32) {
                for i in 0..count {
                    self.bit |= ((value >> i) & 1) << self.used;
                    self.used += 1;
                    if self.used == 8 {
                        self.out.push(self.bit as u8);
                        (self.bit, self.used) = (0, 0);
                    }
                }
            }
            // A Huffman code, most significant bit first.
            fn code(&mut self, code: u32, count: u32) {
                for i in (0..count).rev() {
                    self.put((code >> i) & 1, 1);
                }
            }
        }
        let mut bits = Bits {
            out: vec![0x78, 0x01],
            bit: 0,
            used: 0,
        };
        // The final block, with fixed codes.
        bits.put(1, 1);
        bits.put(1, 2);
        let mut left = count;
        if left > 0 {
            // A literal zero.
            bits.code(0x30, 8);
            left -= 1;
        }
        while left >= 3 {
            let length = left.min(258);
            if length == 258 {
                // Length code 285, then distance code 0: one byte back.
                bits.code(0xC5, 8);
            } else {
                // Lengths 3 to 257: codes 257 to 284, some with extra bits.
                let (symbol, base, extra) = length_code(length as u32);
                if symbol < 280 {
                    bits.code(symbol - 256, 7);
                } else {
                    bits.code(0xC0 + symbol - 280, 8);
                }
                bits.put(length as u32 - base, extra);
            }
            bits.code(0, 5);
            left -= length;
        }
        for _ in 0..left {
            bits.code(0x30, 8);
        }
        // End of block.
        bits.code(0, 7);
        if bits.used > 0 {
            bits.out.push(bits.bit as u8);
        }
        // Adler-32 of zeros: the first sum stays 1, the second counts them.
        let adler = ((count % 65521) as u32) << 16 | 1;
        bits.out.extend_from_slice(&adler.to_be_bytes());
        bits.out
    }

    /// Deflate's length symbol, its base length and its number of extra bits, for 3 to 257.
    fn length_code(length: u32) -> (u32, u32, u32) {
        const BASES: [u32; 28] = [
            3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99,
            115, 131, 163, 195, 227,
        ];
        let index = BASES.iter().rposition(|base| *base <= length).unwrap();
        let extra = if index < 8 { 0 } else { (index as u32 - 4) / 4 };
        (257 + index as u32, BASES[index], extra)
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    /// A PNG of `width` × `height` pixels at `depth` bits a channel, RGBA, every pixel zero, with
    /// an `iCCP` colour profile that inflates to `profile` zero bytes when it's given.
    pub(crate) fn png_of_zeros(
        width: u32,
        height: u32,
        depth: u8,
        profile: Option<usize>,
    ) -> Vec<u8> {
        let row = 1 + width as usize * 4 * usize::from(depth / 8);
        png_with(width, height, depth, profile, row * height as usize)
    }

    /// A PNG whose header says `width` × `height` and whose pixels are `zeros` zero bytes, which
    /// needn't be as many as it says.
    pub(crate) fn png_with(
        width: u32,
        height: u32,
        depth: u8,
        profile: Option<usize>,
        zeros: usize,
    ) -> Vec<u8> {
        let chunk = |png: &mut Vec<u8>, kind: &[u8], body: &[u8]| {
            png.extend_from_slice(&(body.len() as u32).to_be_bytes());
            let start = png.len();
            png.extend_from_slice(kind);
            png.extend_from_slice(body);
            let crc = crc32(&png[start..]);
            png.extend_from_slice(&crc.to_be_bytes());
        };
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut header = width.to_be_bytes().to_vec();
        header.extend_from_slice(&height.to_be_bytes());
        header.extend_from_slice(&[depth, 6, 0, 0, 0]);
        chunk(&mut png, b"IHDR", &header);
        if let Some(profile) = profile {
            let mut body = b"x\0\0".to_vec();
            body.extend_from_slice(&zlib_zeros(profile));
            chunk(&mut png, b"iCCP", &body);
        }
        // Each row: filter type 0, then its pixels, all zero.
        chunk(&mut png, b"IDAT", &zlib_zeros(zeros));
        chunk(&mut png, b"IEND", b"");
        png
    }

    fn opaque_pixels(preview: &Preview) -> usize {
        preview
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[3] != 0)
            .count()
    }

    const FIT: (u32, u32) = (PREVIEW_SIDE, PREVIEW_SIDE);

    #[test]
    fn the_zlib_stream_of_zeros_inflates_to_them() {
        let png = png_of_zeros(3, 2, 8, Some(1000));
        let image = image::load_from_memory(&png).unwrap().into_rgba8();
        assert_eq!(image.dimensions(), (3, 2));
        assert!(image.as_raw().iter().all(|byte| *byte == 0));
        for count in [0, 1, 2, 3, 4, 258, 259, 260, 262, 1000, 70000] {
            let png = png_of_zeros(1, 1, 8, Some(count));
            assert!(image::load_from_memory(&png).is_ok(), "{count}");
        }
    }

    #[test]
    fn an_svg_image_element_reads_nothing_from_disk() {
        let dir = tempfile::TempDir::new().unwrap();
        let png = dir.path().join("secret.png");
        std::fs::write(&png, png_of(20, 20)).unwrap();
        // Opening a FIFO to read it blocks until something writes, so a read would hang, and the
        // timeout below fails the test rather than hanging it.
        let fifo = dir.path().join("fifo");
        let path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let hrefs = [
            png.to_str().unwrap().to_string(),
            format!("file://{}", png.display()),
            fifo.to_str().unwrap().to_string(),
            "secret.png".to_string(),
        ];
        for href in hrefs {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="20" height="20"><image xlink:href="{href}" width="20" height="20"/></svg>"#
            );
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                sender
                    .send(render(ImageFormat::Svg, svg.as_bytes(), FIT))
                    .ok();
            });
            let drawn = receiver
                .recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|_| panic!("{href}: rendering waited on a file"))
                .unwrap();
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
        let drawn = render(ImageFormat::Svg, svg.as_bytes(), FIT).unwrap();
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
    fn an_svg_draws_its_own_shapes_at_up_to_twice_its_size_to_fit() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="5"><rect width="10" height="5" fill="blue"/></svg>"#;
        let drawn = render(ImageFormat::Svg, svg, FIT).unwrap();
        assert_eq!(drawn.dimensions(), (20, 10));
        assert_eq!(opaque_pixels(&drawn), 200);
        // BGRA: blue comes first.
        assert_eq!(&drawn.as_raw()[..4], &[255, 0, 0, 255]);
        // Fitted to a panel.
        assert_eq!(
            render(ImageFormat::Svg, svg, (8, 100))
                .unwrap()
                .dimensions(),
            (8, 4)
        );

        let huge = br#"<svg xmlns="http://www.w3.org/2000/svg" width="40000" height="20000"/>"#;
        assert_eq!(
            render(ImageFormat::Svg, huge, FIT).unwrap().dimensions(),
            (PREVIEW_SIDE, PREVIEW_SIDE / 2)
        );
    }

    #[test]
    fn an_image_whose_header_is_too_large_is_never_decoded() {
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
        // 11000 × 11000 pixels in a PNG of a few dozen bytes, or a GIF of a few hundred.
        let png = png_with(11000, 11000, 8, None, 0);
        let started = Instant::now();
        assert_eq!(
            render(ImageFormat::Png, &png, FIT),
            Err("too large to preview: 11000 × 11000".to_string())
        );
        let gif = gif_of(65535, 65535, 2);
        assert_eq!(
            render(ImageFormat::Gif, &gif, FIT),
            Err("too large to preview: 65535 × 65535".to_string())
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn an_image_past_the_decoding_budget_is_refused_before_it_decodes() {
        // 40 megapixels at 16 bits a channel would decode to 320 MB before scaling down.
        let png = png_of_zeros(6324, 6324, 16, None);
        let started = Instant::now();
        assert_eq!(
            render(ImageFormat::Png, &png, FIT),
            Err("too large to preview: 6324 × 6324".to_string())
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_large_image_keeps_only_a_preview_that_fits() {
        let drawn = render(ImageFormat::Png, &png_of(3000, 1500), FIT).unwrap();
        assert_eq!(drawn.dimensions(), (PREVIEW_SIDE, PREVIEW_SIDE / 2));
        // BGRA: red comes third.
        assert_eq!(&drawn.as_raw()[..4], &[0, 0, 255, 255]);
        // A panel's size, and never more than asked.
        let panel = render(ImageFormat::Png, &png_of(3000, 1500), (600, 200)).unwrap();
        assert_eq!(panel.dimensions(), (400, 200));
        let big_fit = render(ImageFormat::Png, &png_of(3000, 1500), (9000, 9000)).unwrap();
        assert_eq!(big_fit.dimensions(), (PREVIEW_SIDE, PREVIEW_SIDE / 2));

        let small = render(ImageFormat::Png, &png_of(3, 2), FIT).unwrap();
        assert_eq!(small.dimensions(), (3, 2));
    }

    #[test]
    fn an_exif_turned_jpeg_turns_after_it_scales_down() {
        let mut jpeg = Vec::new();
        image::RgbImage::from_pixel(400, 200, image::Rgb([10, 200, 30]))
            .write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
            .unwrap();
        // An APP1 Exif segment after SOI: big-endian TIFF, one entry, Orientation 6 (turn 90°).
        let mut tiff = b"MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\x06\0\0".to_vec();
        tiff.extend_from_slice(&[0, 0, 0, 0]);
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let mut turned = jpeg[..2].to_vec();
        turned.extend_from_slice(&[0xFF, 0xE1]);
        turned.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        turned.extend_from_slice(&app1);
        turned.extend_from_slice(&jpeg[2..]);
        // Turned upright, 200 × 400, fitted to 100 a side.
        let drawn = render(ImageFormat::Jpeg, &turned, (100, 100)).unwrap();
        assert_eq!(drawn.dimensions(), (50, 100));
    }

    #[test]
    fn a_gif_decodes_its_first_frame_only_and_counts_the_rest() {
        let gif = gif_of(4096, 4096, 8);
        assert_eq!(gif_frames(&gif), Some(8));
        let drawn = render(ImageFormat::Gif, &gif, FIT).unwrap();
        // One frame, scaled down.
        assert_eq!(drawn.dimensions(), (PREVIEW_SIDE, PREVIEW_SIDE));

        assert_eq!(gif_frames(&gif_of(3, 3, 1)), Some(1));
        assert_eq!(gif_frames(b"GIF89a\x01\x00"), None);
        assert_eq!(gif_frames(&gif[..gif.len() - 3]), None);
    }
}
