use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Seek};
use std::path::Path;

use image::{ImageDecoder, ImageReader};
use tiff::ColorType;
use tiff::decoder::{Decoder, DecodingResult, Limits};
use tiff::tags::Tag;

use crate::raster::{OPAQUE, widen};
use crate::{ColorProfile, Document, Error, Pixel, Raster, Result};

/// Open an image file. OpenRaster and PSD keep their layers; TIFF is read directly
/// so 16-bit data and the embedded ICC profile survive; PNG and JPEG go
/// through the `image` crate.
pub fn load(path: &Path) -> Result<Document> {
    let is_tiff = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("tif") || e.eq_ignore_ascii_case("tiff"));
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("ora") => crate::ora::load(path),
        Some("psd") => crate::psd::load(path),
        _ if is_tiff => load_tiff(path),
        _ => load_other(path),
    }
}

fn open(path: &Path) -> Result<BufReader<File>> {
    File::open(path)
        .map(BufReader::new)
        .map_err(|source| Error::Read {
            path: path.display().to_string(),
            source,
        })
}

fn load_tiff(path: &Path) -> Result<Document> {
    // The default limits reject images over 256 MiB, which a 16-bit
    // upscaled portrait easily exceeds.
    let mut decoder = Decoder::new(open(path)?)?.with_limits(Limits::unlimited());
    let (width, height) = decoder.dimensions()?;
    let color_type = decoder.colortype()?;
    let icc = decoder
        .find_tag(Tag::IccProfile)?
        .map(|v| v.into_u8_vec())
        .transpose()?;

    let (channels, bits) = match color_type {
        ColorType::Gray(b) => (1, b),
        ColorType::GrayA(b) => (2, b),
        ColorType::RGB(b) => (3, b),
        ColorType::RGBA(b) => (4, b),
        other => return Err(Error::Unsupported(format!("TIFF colour type {other:?}"))),
    };

    let samples: Vec<u16> = match decoder.read_image()? {
        DecodingResult::U8(v) => v.into_iter().map(widen).collect(),
        DecodingResult::U16(v) => v,
        // Floating-point TIFFs are clipped to 0–1 until the engine gains a
        // float pipeline.
        DecodingResult::F32(v) => v
            .into_iter()
            .map(|s| (s.clamp(0.0, 1.0) * 65535.0).round() as u16)
            .collect(),
        _ => return Err(Error::Unsupported(format!("{bits}-bit TIFF samples"))),
    };

    let pixels = expand(&samples, channels);
    let profile = match icc {
        Some(icc) => ColorProfile::from_icc(icc)?,
        None => ColorProfile::srgb(),
    };
    Ok(Document::from_image(
        path.to_path_buf(),
        &Raster::new(width, height, pixels),
        profile,
        bits,
    ))
}

fn load_other(path: &Path) -> Result<Document> {
    let reader = ImageReader::new(open(path)?)
        .with_guessed_format()
        .map_err(|source| Error::Read {
            path: path.display().to_string(),
            source,
        })?;
    let (raster, profile, source_bits) = decode(reader)?;
    Ok(Document::from_image(
        path.to_path_buf(),
        &raster,
        profile,
        source_bits,
    ))
}

/// Decode a PNG or JPEG held in memory, such as an image pasted from
/// another app, with its colour space (sRGB if it has no profile).
pub fn decode_image(bytes: &[u8]) -> Result<(Raster, ColorProfile)> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|source| Error::Read {
            path: "image data".into(),
            source,
        })?;
    let (raster, profile, _) = decode(reader)?;
    Ok((raster, profile))
}

/// Decode an image with the `image` crate, returning it with its colour
/// space and bits per channel.
fn decode<R: BufRead + Seek>(reader: ImageReader<R>) -> Result<(Raster, ColorProfile, u8)> {
    let mut decoder = reader.into_decoder()?;
    let icc = decoder.icc_profile()?;
    let source_bits = decoder.color_type().bytes_per_pixel() / decoder.color_type().channel_count() * 8;
    let image = image::DynamicImage::from_decoder(decoder)?.into_rgba16();
    let (width, height) = image.dimensions();
    let pixels = image.pixels().map(|p| p.0).collect();
    let profile = match icc {
        Some(icc) => ColorProfile::from_icc(icc)?,
        None => ColorProfile::srgb(),
    };
    Ok((Raster::new(width, height, pixels), profile, source_bits))
}

/// Expand interleaved gray / gray+alpha / RGB / RGBA samples to RGBA pixels.
fn expand(samples: &[u16], channels: usize) -> Vec<Pixel> {
    samples
        .chunks_exact(channels)
        .map(|s| match *s {
            [g] => [g, g, g, OPAQUE],
            [g, a] => [g, g, g, a],
            [r, g, b] => [r, g, b, OPAQUE],
            [r, g, b, a] => [r, g, b, a],
            _ => unreachable!("channel count is 1–4"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_every_channel_layout() {
        assert_eq!(expand(&[7], 1), vec![[7, 7, 7, OPAQUE]]);
        assert_eq!(expand(&[7, 9], 2), vec![[7, 7, 7, 9]]);
        assert_eq!(expand(&[1, 2, 3], 3), vec![[1, 2, 3, OPAQUE]]);
        assert_eq!(expand(&[1, 2, 3, 4], 4), vec![[1, 2, 3, 4]]);
    }

    /// Loads a real darktable export when one is available locally.
    /// Run with `OMAPIX_TEST_TIFF=/path/to/file.tif cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn loads_real_tiff() {
        let path = std::env::var("OMAPIX_TEST_TIFF").expect("set OMAPIX_TEST_TIFF");
        let doc = load(Path::new(&path)).unwrap();
        println!(
            "{}x{} {}-bit, profile: {}",
            doc.width,
            doc.height,
            doc.source_bits,
            doc.profile.description()
        );
        assert!(doc.width > 0);
    }
}
