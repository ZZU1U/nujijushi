//! Adjustable JPEG compression for Telegram photo uploads.

use std::path::Path;

use image::{DynamicImage, GenericImage, Rgb, RgbImage, codecs::jpeg::JpegEncoder};

use crate::error::{Error, Result};

pub(super) const MAX_PHOTO_BYTES: u64 = 10_000_000;
pub(super) const MAX_PHOTO_EDGES: u32 = 10_000;
pub(super) const MAX_PHOTO_RATIO: u32 = 20;
const FIT_QUALITY_FLOOR: u8 = 70;

#[derive(Debug, Clone, Copy)]
pub struct Compression {
    quality: u8,
    fit: bool,
}

impl Compression {
    pub fn new(quality: u8, fit: bool) -> Result<Self> {
        if !(1..=100).contains(&quality) {
            return Err(Error::invalid("JPEG quality must be from 1 to 100"));
        }
        Ok(Self { quality, fit })
    }

    pub fn quality(self) -> u8 {
        self.quality
    }

    pub fn fits_telegram(self) -> bool {
        self.fit
    }
}

pub fn photo_limit(upload_limit: u64) -> u64 {
    upload_limit.min(MAX_PHOTO_BYTES)
}

pub fn geometry_error(width: u32, height: u32) -> Option<&'static str> {
    if width == 0 || height == 0 {
        Some("The image has invalid dimensions")
    } else if width.saturating_add(height) > MAX_PHOTO_EDGES {
        Some("The image exceeds Telegram's 10,000-pixel combined width and height limit")
    } else if width.max(height) > width.min(height) * MAX_PHOTO_RATIO {
        Some("The image exceeds Telegram's 20:1 aspect ratio limit")
    } else {
        None
    }
}

pub fn compress(path: &Path, settings: Compression, upload_limit: u64) -> Result<Vec<u8>> {
    let mut decoder = super::merge::decoder(path, None)?;
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    encode(image, settings, upload_limit)
}

// Return scaled content dimensions. A border, added later, brings panoramas
// within the aspect limit without cutting off any of the artwork.
pub(super) fn fit_dimensions(width: u32, height: u32) -> (u32, u32) {
    let (padded_width, padded_height) = padded_dimensions(width, height);
    let edges = u64::from(padded_width) + u64::from(padded_height);
    if edges <= u64::from(MAX_PHOTO_EDGES) {
        return (width, height);
    }
    let scale = |edge| ((u64::from(edge) * u64::from(MAX_PHOTO_EDGES - 2) / edges) as u32).max(1);
    (scale(width), scale(height))
}

fn padded_dimensions(width: u32, height: u32) -> (u32, u32) {
    (
        width.max(height.div_ceil(MAX_PHOTO_RATIO)),
        height.max(width.div_ceil(MAX_PHOTO_RATIO)),
    )
}

pub(super) fn encode(
    mut image: DynamicImage,
    settings: Compression,
    upload_limit: u64,
) -> Result<Vec<u8>> {
    let limit = photo_limit(upload_limit) as usize;
    let max_quality = settings.quality;
    if !settings.fit {
        if let Some(error) = geometry_error(image.width(), image.height()) {
            return Err(Error::invalid(format!("{error}. Try Fit to Telegram.")));
        }
        return jpeg(&flatten(image), max_quality, limit)?.ok_or_else(|| {
            Error::invalid(format!(
                "The JPEG exceeds the {limit}-byte photo limit. Lower JPEG quality or choose Fit to Telegram."
            ))
        });
    }

    let (width, height) = fit_dimensions(image.width(), image.height());
    if (width, height) != (image.width(), image.height()) {
        image = image.resize_exact(width, height, image::imageops::FilterType::Lanczos3);
    }
    let mut photo = flatten(image);
    let min_quality = FIT_QUALITY_FLOOR.min(max_quality);
    loop {
        let padded = pad(&photo)?;
        if let Some(bytes) = jpeg(&padded, max_quality, limit)? {
            return Ok(bytes);
        }
        if min_quality < max_quality
            && let Some(mut best) = jpeg(&padded, min_quality, limit)?
        {
            let (mut low, mut high) = (min_quality, max_quality);
            while high - low > 1 {
                let quality = low + (high - low) / 2;
                if let Some(bytes) = jpeg(&padded, quality, limit)? {
                    best = bytes;
                    low = quality;
                } else {
                    high = quality;
                }
            }
            return Ok(best);
        }
        let (width, height) = photo.dimensions();
        if (width, height) == (1, 1) {
            return Err(Error::invalid(format!(
                "Even a one-pixel JPEG exceeds the {limit}-byte photo limit"
            )));
        }
        photo = image::imageops::resize(
            &photo,
            (width * 3 / 4).max(1),
            (height * 3 / 4).max(1),
            image::imageops::FilterType::Lanczos3,
        );
    }
}

fn flatten(image: DynamicImage) -> RgbImage {
    if !image.color().has_alpha() {
        return image.into_rgb8();
    }
    let image = image.into_rgba8();
    RgbImage::from_fn(image.width(), image.height(), |x, y| {
        let [r, g, b, alpha] = image[(x, y)].0;
        let alpha = u16::from(alpha);
        Rgb([r, g, b]
            .map(|channel| ((u16::from(channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8))
    })
}

fn pad(image: &RgbImage) -> Result<std::borrow::Cow<'_, RgbImage>> {
    let (width, height) = image.dimensions();
    let (padded_width, padded_height) = padded_dimensions(width, height);
    if (width, height) == (padded_width, padded_height) {
        return Ok(std::borrow::Cow::Borrowed(image));
    }
    let mut output = RgbImage::from_pixel(padded_width, padded_height, Rgb([255; 3]));
    output.copy_from(
        image,
        (padded_width - width) / 2,
        (padded_height - height) / 2,
    )?;
    Ok(std::borrow::Cow::Owned(output))
}

fn jpeg(image: &RgbImage, quality: u8, limit: usize) -> Result<Option<Vec<u8>>> {
    let mut output = super::merge::LimitedOutput::new(limit);
    let result = JpegEncoder::new_with_quality(&mut output, quality).encode_image(image);
    if output.exceeded {
        return Ok(None);
    }
    result?;
    Ok(Some(output.bytes))
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};

    use super::*;

    fn noisy_image() -> DynamicImage {
        let mut random = 123456789u32;
        RgbImage::from_fn(160, 120, |_, _| {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            let [r, g, b, _] = random.to_le_bytes();
            Rgb([r, g, b])
        })
        .into()
    }

    #[test]
    fn exact_quality_changes_size_and_keeps_pixel_dimensions() {
        let image = noisy_image();
        let high = Compression::new(95, false).unwrap();
        let low = Compression::new(35, false).unwrap();
        let detailed = encode(image.clone(), high, MAX_PHOTO_BYTES).unwrap();
        let smaller = encode(image.clone(), low, MAX_PHOTO_BYTES).unwrap();
        assert!(smaller.len() < detailed.len());
        let decoded = image::load_from_memory(&smaller).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (160, 120));
        let error = encode(image, high, smaller.len() as u64).unwrap_err();
        assert!(error.to_string().contains("Lower JPEG quality"));
    }

    #[test]
    fn fit_tries_quality_before_resizing_to_meet_the_byte_limit() {
        let image = noisy_image();
        let floor_size = encode(
            image.clone(),
            Compression::new(70, false).unwrap(),
            MAX_PHOTO_BYTES,
        )
        .unwrap()
        .len();
        let settings = Compression::new(95, true).unwrap();
        let full = encode(image.clone(), settings, floor_size as u64).unwrap();
        let decoded = image::load_from_memory(&full).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (160, 120));
        assert!(full.len() <= floor_size);
        let reduced = encode(image.clone(), settings, 2_000).unwrap();
        assert!(reduced.len() <= 2_000);
        assert!(image::load_from_memory(&reduced).unwrap().width() < 160);
        assert!(encode(image, settings, 100).is_err());
    }

    #[test]
    fn fit_pads_panoramas_and_flattens_transparency_onto_white() {
        let mut image = RgbaImage::from_pixel(400, 8, Rgba([255, 12, 18, 255]));
        for x in 200..400 {
            for y in 0..8 {
                image.put_pixel(x, y, Rgba([0, 0, 0, 0]));
            }
        }
        let bytes = encode(
            image.into(),
            Compression::new(85, true).unwrap(),
            MAX_PHOTO_BYTES,
        )
        .unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap().into_rgb8();
        assert_eq!(decoded.dimensions(), (400, 20));
        assert!(decoded.get_pixel(35, 10)[1] < 30);
        assert!(decoded.get_pixel(350, 10).0.iter().all(|&v| v > 245));
        let (width, height) = fit_dimensions(20_000, 1);
        let (width, height) = padded_dimensions(width, height);
        assert!(geometry_error(width, height).is_none());
    }
}
