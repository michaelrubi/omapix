//! Flattened exports for handing work to other apps or the web.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use image::ImageEncoder;
use rayon::prelude::*;
use image::codecs::jpeg::JpegEncoder;
use tiff::encoder::{Compression, DeflateLevel, TiffEncoder, colortype};
use tiff::tags::Tag;

use crate::{DisplayTransform, Document, Error, Result};

fn create(path: &Path) -> Result<BufWriter<File>> {
    File::create(path)
        .map(BufWriter::new)
        .map_err(|source| Error::Read {
            path: path.display().to_string(),
            source,
        })
}

/// Flattened 16-bit TIFF in the document's colour space, with its ICC
/// profile, for going back to darktable or on to print. Keeps transparency
/// only if the image has any.
pub fn tiff(doc: &Document, path: &Path) -> Result<()> {
    let image = doc.composite();
    let opaque = image.pixels().iter().all(|p| p[3] == u16::MAX);
    let mut encoder =
        TiffEncoder::new(create(path)?)?.with_compression(Compression::Deflate(DeflateLevel::Fast));
    let (w, h) = (image.width(), image.height());
    if opaque {
        let data: Vec<u16> = image
            .pixels()
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        let mut img = encoder.new_image::<colortype::RGB16>(w, h)?;
        if let Some(icc) = doc.profile.icc() {
            img.encoder().write_tag(Tag::IccProfile, icc)?;
        }
        img.write_data(&data)?;
    } else {
        let data: Vec<u16> = image.pixels().iter().flatten().copied().collect();
        let mut img = encoder.new_image::<colortype::RGBA16>(w, h)?;
        if let Some(icc) = doc.profile.icc() {
            img.encoder().write_tag(Tag::IccProfile, icc)?;
        }
        img.write_data(&data)?;
    }
    Ok(())
}

/// Flattened 8-bit sRGB JPEG for the web and clients, with transparency
/// flattened onto white.
pub fn jpeg(doc: &Document, path: &Path, quality: u8) -> Result<()> {
    let image = doc.composite();
    let transform = DisplayTransform::to_srgb(&doc.profile)?;
    let mut rgba = vec![[0u8; 4]; image.pixels().len()];
    rgba.par_chunks_mut(65536)
        .zip(image.pixels().par_chunks(65536))
        .for_each(|(out, src)| transform.convert(src, out));
    let rgb: Vec<u8> = rgba
        .iter()
        .flat_map(|p| {
            let a = u32::from(p[3]);
            let over_white = |c: u8| ((u32::from(c) * a + 255 * (255 - a) + 127) / 255) as u8;
            [over_white(p[0]), over_white(p[1]), over_white(p[2])]
        })
        .collect();
    let mut encoder = JpegEncoder::new_with_quality(create(path)?, quality);
    let srgb = lcms2::Profile::new_srgb().icc().map_err(Error::Color)?;
    let _ = encoder.set_icc_profile(srgb);
    encoder.write_image(
        &rgb,
        image.width(),
        image.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ColorProfile, Raster};

    #[test]
    fn tiff_round_trips_through_loader() {
        let (w, h) = (40, 30);
        let px: Vec<crate::Pixel> = (0..w * h)
            .map(|i| [i as u16 * 50, 30000, 65535 - i as u16, 65535])
            .collect();
        let doc = Document::from_image(
            "x.tif".into(),
            &Raster::new(w, h, px.clone()),
            ColorProfile::srgb(),
            16,
        );
        let dir = std::env::temp_dir().join(format!("omapix-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.tif");
        tiff(&doc, &path).unwrap();
        let back = crate::io::load(&path).unwrap();
        let jpeg_path = dir.join("out.jpg");
        jpeg(&doc, &jpeg_path, 90).unwrap();
        let jpeg_back = crate::io::load(&jpeg_path).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(back.composite().pixels(), &px[..]);
        assert_eq!((jpeg_back.width, jpeg_back.height), (w, h));
    }
}
