//! Horizontal stitching of still images in gallery order.

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

use super::compress::{self, Compression, MAX_PHOTO_EDGES};

const MAX_DECODE_BYTES: u64 = 200 * 1024 * 1024;

pub fn supports(item: &MediaItem) -> bool {
    !item.is_video && matches!(item.ext.as_str(), "jpg" | "jpeg" | "png" | "webp")
}

pub(super) fn decoder(path: &Path, max_edge: Option<u32>) -> Result<Box<dyn ImageDecoder>> {
    let reader = ImageReader::new(BufReader::new(File::open(path)?)).with_guessed_format()?;
    let format = reader.format();
    let reader = reader.into_inner();
    let mut limits = Limits::default();
    limits.max_image_width = max_edge;
    limits.max_image_height = max_edge;
    let animated = || Error::invalid("Animated images cannot be edited");
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
    if decoder.total_bytes() > MAX_DECODE_BYTES {
        return Err(Error::invalid("The image is too large to decode"));
    }
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
    compression: Option<Compression>,
    mut on_panel: impl FnMut(usize) -> Result<()>,
) -> Result<Vec<u8>> {
    if paths.len() < 2 {
        return Err(Error::invalid("Select at least two images"));
    }
    let fit = compression.is_some_and(Compression::fits_telegram);
    let max_edge = (!fit).then_some(MAX_PHOTO_EDGES);
    let mut source_width = 0u32;
    let mut height = 0;
    let mut sixteen_bit = false;
    // Read dimensions and orientation first so oversized outputs never allocate a canvas.
    for path in paths {
        let mut decoder = decoder(path, max_edge)?;
        let orientation = decoder.orientation()?;
        let (panel_width, panel_height) = dimensions(decoder.as_ref(), orientation);
        if height != 0 && panel_height != height {
            return Err(Error::invalid(
                "All selected images must have the same height after orientation",
            ));
        }
        height = panel_height;
        source_width = source_width
            .checked_add(panel_width)
            .ok_or_else(|| Error::invalid("The selected gallery is too wide"))?;
        sixteen_bit |= matches!(
            decoder.color_type(),
            image::ColorType::L16
                | image::ColorType::La16
                | image::ColorType::Rgb16
                | image::ColorType::Rgba16
        );
    }
    let (width, output_height) = if fit {
        compress::fit_dimensions(source_width, height)
    } else {
        if let Some(error) = compress::geometry_error(source_width, height) {
            return Err(Error::invalid(format!("{error}. Try Fit to Telegram.")));
        }
        (source_width, height)
    };
    if (width as usize) < paths.len() {
        return Err(Error::invalid("Too many panels to fit in a single image"));
    }

    let mut output = if sixteen_bit && compression.is_none() {
        DynamicImage::new_rgba16(width, output_height)
    } else {
        DynamicImage::new_rgba8(width, output_height)
    };
    let mut x = 0;
    let mut source_x = 0;
    for (index, path) in paths.iter().enumerate() {
        let mut decoder = decoder(path, max_edge)?;
        let orientation = decoder.orientation()?;
        let (panel_width, panel_height) = dimensions(decoder.as_ref(), orientation);
        if panel_height != height || panel_width > source_width - source_x {
            return Err(Error::invalid("An image changed while it was being merged"));
        }
        source_x += panel_width;
        let offset = (u64::from(source_x) * u64::from(width) / u64::from(source_width)) as u32;
        let remaining = (paths.len() - index - 1) as u32;
        let end = offset.clamp(x + 1, width - remaining);
        let mut panel = DynamicImage::from_decoder(decoder)?;
        panel.apply_orientation(orientation);
        if (end - x, output_height) != (panel_width, panel_height) {
            panel = panel.resize_exact(
                end - x,
                output_height,
                image::imageops::FilterType::Lanczos3,
            );
        }
        if let Some(canvas) = output.as_mut_rgba16() {
            canvas.copy_from(&panel.into_rgba16(), x, 0)?;
        } else if let Some(canvas) = output.as_mut_rgba8() {
            canvas.copy_from(&panel.into_rgba8(), x, 0)?;
        }
        x = end;
        on_panel(index + 1)?;
    }
    if source_x != source_width {
        return Err(Error::invalid("An image changed while it was being merged"));
    }
    if let Some(settings) = compression {
        return compress::encode(output, settings, upload_limit);
    }
    let max = compress::photo_limit(upload_limit) as usize;
    let mut writer = LimitedOutput::new(max);
    let encoded =
        PngEncoder::new_with_quality(&mut writer, CompressionType::Best, FilterType::Adaptive)
            .write_image(output.as_bytes(), width, height, output.color().into());
    if writer.exceeded {
        return Err(Error::invalid(format!(
            "The merged PNG exceeds the {max}-byte photo limit. Try Fit to Telegram."
        )));
    }
    encoded?;
    Ok(writer.bytes)
}

pub(super) struct LimitedOutput {
    pub(super) bytes: Vec<u8>,
    max: usize,
    pub(super) exceeded: bool,
}

impl LimitedOutput {
    pub(super) fn new(max: usize) -> Self {
        Self {
            bytes: Vec::new(),
            max,
            exceeded: false,
        }
    }
}

impl Write for LimitedOutput {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.max - self.bytes.len() {
            self.exceeded = true;
            return Err(io::Error::other("encoded image exceeds the photo limit"));
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
    use crate::media::compress::MAX_PHOTO_BYTES;

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
        let bytes = stitch(&[left, right], MAX_PHOTO_BYTES, None, |count| {
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
        let bytes = stitch(&[rotated, still], MAX_PHOTO_BYTES, None, |_| Ok(())).unwrap();
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
                stitch(&[a.clone(), b.clone()], limit, None, |_| Ok(()))
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

    #[test]
    fn fit_stitches_oversized_panels_in_order_and_pads_the_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left.png");
        let right = dir.path().join("right.png");
        panel(&left, 5000, 2, [220, 0, 0, 255]);
        panel(&right, 5000, 2, [0, 0, 220, 255]);
        let output = stitch(
            &[left, right],
            MAX_PHOTO_BYTES,
            Some(Compression::new(90, true).unwrap()),
            |_| Ok(()),
        )
        .unwrap();
        let image = image::load_from_memory(&output).unwrap().into_rgb8();
        let (width, height) = image.dimensions();
        assert!(width < 10_000);
        assert_eq!(compress::geometry_error(width, height), None);
        let left = image.get_pixel(width / 4, (height - 1) / 2);
        let right = image.get_pixel(3 * width / 4, (height - 1) / 2);
        assert!(left[0] > left[2] + 50);
        assert!(right[2] > right[0] + 50);
    }
}
