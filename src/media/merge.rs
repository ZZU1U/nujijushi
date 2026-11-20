//! Lossless, horizontal stitching of still images in gallery order.

use std::fs::File;
use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};

use image::{
    DynamicImage, GenericImage, ImageDecoder, ImageEncoder, ImageFormat, ImageReader, Limits,
    codecs::{
        jpeg::JpegDecoder,
        png::{CompressionType, FilterType, PngDecoder, PngEncoder},
        webp::WebPDecoder,
    },
    metadata::Orientation,
};

use crate::error::{Error, Result};
use crate::model::MediaItem;

const MAX_PHOTO_BYTES: u64 = 10_000_000;
const MAX_PHOTO_EDGES: u32 = 10_000;
const MAX_PHOTO_RATIO: u32 = 20;

pub fn supports(item: &MediaItem) -> bool {
    !item.is_video && matches!(item.ext.as_str(), "jpg" | "jpeg" | "png" | "webp")
}

fn decoder(path: &Path) -> Result<Box<dyn ImageDecoder>> {
    let reader = ImageReader::new(BufReader::new(File::open(path)?)).with_guessed_format()?;
    let format = reader.format();
    let reader = reader.into_inner();
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_PHOTO_EDGES);
    limits.max_image_height = Some(MAX_PHOTO_EDGES);
    let animated = || Error::invalid("A merge can only contain still images");
    let mut decoder: Box<dyn ImageDecoder> = match format {
        Some(ImageFormat::Jpeg) => Box::new(JpegDecoder::new(reader)?),
        Some(ImageFormat::Png) => {
            let decoder = PngDecoder::with_limits(reader, limits.clone())?;
            if decoder.is_apng()? {
                return Err(animated());
            }
            Box::new(decoder)
        }
        Some(ImageFormat::WebP) => {
            let decoder = WebPDecoder::new(reader)?;
            if decoder.has_animation() {
                return Err(animated());
            }
            Box::new(decoder)
        }
        _ => return Err(Error::invalid("Select still JPEG, PNG or WebP images")),
    };
    decoder.set_limits(limits)?;
    Ok(decoder)
}

fn dimensions(decoder: &dyn ImageDecoder, orientation: Orientation) -> (u32, u32) {
    let (width, height) = decoder.dimensions();
    match orientation {
        Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Rotate90FlipH
        | Orientation::Rotate270FlipH => (height, width),
        _ => (width, height),
    }
}

pub fn stitch(
    paths: &[PathBuf],
    upload_limit: u64,
    mut on_panel: impl FnMut(usize) -> Result<()>,
) -> Result<Vec<u8>> {
    if paths.len() < 2 {
        return Err(Error::invalid("Select at least two consecutive images"));
    }
    let mut width = 0u32;
    let mut height = 0;
    let mut sixteen_bit = false;
    // Read dimensions and orientation first so oversized outputs never allocate a canvas.
    for path in paths {
        let mut decoder = decoder(path)?;
        let orientation = decoder.orientation()?;
        let (panel_width, panel_height) = dimensions(decoder.as_ref(), orientation);
        if height != 0 && panel_height != height {
            return Err(Error::invalid(
                "All selected images must have the same height after orientation",
            ));
        }
        height = panel_height;
        width = width.saturating_add(panel_width);
        sixteen_bit |= matches!(
            decoder.color_type(),
            image::ColorType::L16
                | image::ColorType::La16
                | image::ColorType::Rgb16
                | image::ColorType::Rgba16
        );
        if width.saturating_add(height) > MAX_PHOTO_EDGES {
            return Err(Error::invalid(
                "The merged image would exceed Telegram's 10,000-pixel combined width and height limit",
            ));
        }
    }
    if width.max(height) > width.min(height) * MAX_PHOTO_RATIO {
        return Err(Error::invalid(
            "The merged image would exceed Telegram's 20:1 aspect ratio limit",
        ));
    }

    let mut output = if sixteen_bit {
        DynamicImage::new_rgba16(width, height)
    } else {
        DynamicImage::new_rgba8(width, height)
    };
    let mut x = 0;
    for (index, path) in paths.iter().enumerate() {
        let mut decoder = decoder(path)?;
        let orientation = decoder.orientation()?;
        let (panel_width, panel_height) = dimensions(decoder.as_ref(), orientation);
        if panel_height != height || panel_width > width - x {
            return Err(Error::invalid("An image changed while it was being merged"));
        }
        let mut panel = DynamicImage::from_decoder(decoder)?;
        panel.apply_orientation(orientation);
        if let Some(canvas) = output.as_mut_rgba16() {
            canvas.copy_from(&panel.into_rgba16(), x, 0)?;
        } else if let Some(canvas) = output.as_mut_rgba8() {
            canvas.copy_from(&panel.into_rgba8(), x, 0)?;
        }
        x += panel_width;
        on_panel(index + 1)?;
    }
    if x != width {
        return Err(Error::invalid("An image changed while it was being merged"));
    }

    let max = upload_limit.min(MAX_PHOTO_BYTES) as usize;
    let mut writer = LimitedPng {
        bytes: Vec::new(),
        max,
        exceeded: false,
    };
    let encoded =
        PngEncoder::new_with_quality(&mut writer, CompressionType::Best, FilterType::Adaptive)
            .write_image(output.as_bytes(), width, height, output.color().into());
    if writer.exceeded {
        return Err(Error::invalid(format!(
            "The merged PNG exceeds the {max}-byte photo limit"
        )));
    }
    encoded?;
    Ok(writer.bytes)
}

struct LimitedPng {
    bytes: Vec<u8>,
    max: usize,
    exceeded: bool,
}

impl Write for LimitedPng {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.max - self.bytes.len() {
            self.exceeded = true;
            return Err(io::Error::other("merged PNG exceeds the photo limit"));
        }
        self.bytes.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage, codecs::jpeg::JpegEncoder};

    use super::*;

    fn panel(path: &Path, width: u32, height: u32, color: [u8; 4]) {
        RgbaImage::from_pixel(width, height, Rgba(color))
            .save(path)
            .unwrap();
    }

    #[test]
    fn stitches_left_to_right_with_alpha_and_original_bit_depth() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.png");
        let right = dir.path().join("right.webp");
        let color = Rgba([0x1256u16, 0x3456, 0x5678, 0x789a]);
        image::ImageBuffer::from_pixel(2, 3, color)
            .save(&left)
            .unwrap();
        panel(&right, 4, 3, [22, 120, 80, 100]);
        let mut progress = Vec::new();
        let bytes = stitch(&[left, right], MAX_PHOTO_BYTES, |count| {
            progress.push(count);
            Ok(())
        })
        .unwrap();
        assert_eq!(progress, [1, 2]);
        let result = image::load_from_memory(&bytes).unwrap().into_rgba16();
        assert_eq!(result.dimensions(), (6, 3));
        assert_eq!(*result.get_pixel(1, 2), color);
        assert_eq!(
            *result.get_pixel(2, 0),
            Rgba([22 * 257, 120 * 257, 80 * 257, 100 * 257])
        );
    }

    #[test]
    fn reads_exif_before_checking_that_the_panel_heights_match() {
        let dir = tempfile::tempdir().unwrap();
        let rotated = dir.path().join("rotated.jpg");
        let still = dir.path().join("still.png");
        let mut jpeg = JpegEncoder::new(File::create(&rotated).unwrap());
        jpeg.set_exif_metadata(vec![
            b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 1, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
        ])
        .unwrap();
        jpeg.write_image(&[0; 18], 2, 3, image::ExtendedColorType::Rgb8)
            .unwrap();
        panel(&still, 2, 2, [4, 5, 6, 255]);
        let bytes = stitch(&[rotated, still], MAX_PHOTO_BYTES, |_| Ok(())).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (5, 2));
    }

    #[test]
    fn rejects_inputs_and_outputs_that_cannot_be_stitched_and_sent() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        let invalid = |needle: &str, limit| {
            assert!(
                stitch(&[a.clone(), b.clone()], limit, |_| Ok(()))
                    .unwrap_err()
                    .to_string()
                    .contains(needle)
            );
        };
        panel(&a, 4, 5, [255; 4]);
        panel(&b, 4, 6, [255; 4]);
        invalid("same height", MAX_PHOTO_BYTES);
        std::fs::write(&b, b"not an image").unwrap();
        invalid("JPEG, PNG or WebP", MAX_PHOTO_BYTES);
        panel(&b, 4, 5, [255; 4]);
        invalid("10-byte", 10);
        panel(&a, 51, 5, [255; 4]);
        panel(&b, 51, 5, [255; 4]);
        invalid("20:1", MAX_PHOTO_BYTES);
        panel(&a, 5_000, 2, [255; 4]);
        panel(&b, 5_000, 2, [255; 4]);
        invalid("10,000-pixel", MAX_PHOTO_BYTES);
    }
}
