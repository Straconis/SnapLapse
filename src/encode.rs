//! Saving frames as image files.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::codecs::webp::WebPEncoder;
use image::{ExtendedColorType, ImageEncoder};
use serde::{Deserialize, Serialize};

use crate::capture::Frame;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Format {
    #[default]
    Png,
    Jpg,
    Webp,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::Png, Format::Jpg, Format::Webp];

    pub fn extension(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpg => "jpg",
            Format::Webp => "webp",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Png => "PNG (lossless)",
            Format::Jpg => "JPG (smallest, fastest)",
            Format::Webp => "WebP (lossless)",
        }
    }

    pub fn from_extension(ext: &str) -> Option<Format> {
        match ext.to_ascii_lowercase().as_str() {
            "png" => Some(Format::Png),
            "jpg" | "jpeg" => Some(Format::Jpg),
            "webp" => Some(Format::Webp),
            _ => None,
        }
    }
}

const JPEG_QUALITY: u8 = 92;

/// Writes the frame to `path`. It writes to a temporary `.part` file first and renames it at the
/// end, so a half-written file never appears under the real name.
pub fn save(frame: &Frame, path: &Path, format: Format) -> io::Result<()> {
    let rgb = bgra_to_rgb(&frame.bgra);
    let mut partial = path.as_os_str().to_owned();
    partial.push(".part");
    let result = write_file(&rgb, frame.width, frame.height, Path::new(&partial), format)
        .and_then(|()| std::fs::rename(&partial, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result
}

fn write_file(rgb: &[u8], width: u32, height: u32, path: &Path, format: Format) -> io::Result<()> {
    let mut out = BufWriter::new(File::create(path)?);
    let color = ExtendedColorType::Rgb8;
    let encoded = match format {
        Format::Png => {
            PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Adaptive)
                .write_image(rgb, width, height, color)
        }
        Format::Jpg => JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
            .write_image(rgb, width, height, color),
        Format::Webp => WebPEncoder::new_lossless(&mut out).write_image(rgb, width, height, color),
    };
    encoded.map_err(io::Error::other)?;
    out.flush()
}

/// Screen images come as blue-green-red-alpha. Image files want red-green-blue, and the alpha
/// is always opaque here, so it's dropped.
fn bgra_to_rgb(bgra: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(bgra.len() / 4 * 3);
    for px in bgra.as_chunks::<4>().0 {
        rgb.extend_from_slice(&[px[2], px[1], px[0]]);
    }
    rgb
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Frame {
        // Two pixels: pure red, then pure blue (stored as B, G, R, A).
        Frame {
            width: 2,
            height: 1,
            bgra: vec![0, 0, 255, 255, 255, 0, 0, 255],
        }
    }

    #[test]
    fn lossless_formats_keep_exact_colors() {
        let dir = crate::test_dir("encode");
        for format in [Format::Png, Format::Webp] {
            let path = dir.join(format!("sample.{}", format.extension()));
            save(&sample(), &path, format).unwrap();
            let img = image::open(&path).unwrap().to_rgb8();
            assert_eq!(img.dimensions(), (2, 1));
            assert_eq!(img.get_pixel(0, 0).0, [255, 0, 0], "{format:?} red pixel");
            assert_eq!(img.get_pixel(1, 0).0, [0, 0, 255], "{format:?} blue pixel");
        }
    }

    #[test]
    fn jpg_saves_a_readable_image() {
        let dir = crate::test_dir("encode-jpg");
        let path = dir.join("sample.jpg");
        save(&sample(), &path, Format::Jpg).unwrap();
        assert_eq!(image::open(&path).unwrap().width(), 2);
    }

    #[test]
    fn no_partial_file_is_left_behind() {
        let dir = crate::test_dir("encode-part");
        let path = dir.join("done.png");
        save(&sample(), &path, Format::Png).unwrap();
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("done.png")]);
    }

    #[test]
    fn extensions_round_trip() {
        for format in Format::ALL {
            assert_eq!(Format::from_extension(format.extension()), Some(format));
        }
        assert_eq!(Format::from_extension("JPEG"), Some(Format::Jpg));
        assert_eq!(Format::from_extension("gif"), None);
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;

        fn frames() -> impl Strategy<Value = Frame> {
            (1u32..40, 1u32..40).prop_flat_map(|(w, h)| {
                proptest::collection::vec(any::<u8>(), (w * h * 4) as usize).prop_map(move |bgra| {
                    Frame {
                        width: w,
                        height: h,
                        bgra,
                    }
                })
            })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(200))]

            #[test]
            fn lossless_formats_save_every_pixel_exactly(frame in frames()) {
                let dir = crate::test_dir("encode-prop");
                for format in [Format::Png, Format::Webp] {
                    let path = dir.join(format!("p.{}", format.extension()));
                    save(&frame, &path, format).unwrap();
                    let img = image::open(&path).unwrap().to_rgb8();
                    prop_assert_eq!(img.dimensions(), (frame.width, frame.height));
                    prop_assert_eq!(img.into_raw(), bgra_to_rgb(&frame.bgra));
                }
            }
        }
    }
}
