//! Flattened exports for handing work to other apps or the web.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use image::ImageEncoder;
use image::codecs::jpeg::JpegEncoder;
use rayon::prelude::*;
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
/// profile and EXIF metadata, for going back to darktable or on to print.
/// Keeps transparency only if the image has any.
pub fn tiff(doc: &Document, path: &Path) -> Result<()> {
    let image = doc.composite();
    let opaque = image.pixels().iter().all(|p| p[3] == u16::MAX);
    let mut encoder =
        TiffEncoder::new(create(path)?)?.with_compression(Compression::Deflate(DeflateLevel::Fast));
    let (w, h) = (image.width(), image.height());

    let parsed_exif = doc
        .exif
        .as_ref()
        .and_then(|bytes| exif::Reader::new().read_raw(bytes.clone()).ok());

    let exif_offset = if let Some(ref parsed) = parsed_exif {
        if parsed.fields().any(|f| f.tag.context() == exif::Context::Exif) {
            let mut extra = encoder.extra_directory()?;
            for f in parsed.fields() {
                if f.tag.context() == exif::Context::Exif {
                    let tag = Tag::Unknown(f.tag.number());
                    if f.tag.number() == 0xa002 {
                        let _ = extra.write_tag(tag, &[w][..]);
                    } else if f.tag.number() == 0xa003 {
                        let _ = extra.write_tag(tag, &[h][..]);
                    } else {
                        write_exif_field(&mut extra, tag, &f.value);
                    }
                }
            }
            Some(extra.finish_with_offsets()?.offset)
        } else {
            None
        }
    } else {
        None
    };

    let gps_offset = if let Some(ref parsed) = parsed_exif {
        if parsed.fields().any(|f| f.tag.context() == exif::Context::Gps) {
            let mut extra = encoder.extra_directory()?;
            for f in parsed.fields() {
                if f.tag.context() == exif::Context::Gps {
                    write_exif_field(&mut extra, Tag::Unknown(f.tag.number()), &f.value);
                }
            }
            Some(extra.finish_with_offsets()?.offset)
        } else {
            None
        }
    } else {
        None
    };

    if opaque {
        let data: Vec<u16> = image
            .pixels()
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        let mut img = encoder.new_image::<colortype::RGB16>(w, h)?;
        write_tiff_metadata(img.encoder(), doc, parsed_exif.as_ref(), exif_offset, gps_offset)?;
        img.write_data(&data)?;
    } else {
        let data: Vec<u16> = image.pixels().iter().flatten().copied().collect();
        let mut img = encoder.new_image::<colortype::RGBA16>(w, h)?;
        write_tiff_metadata(img.encoder(), doc, parsed_exif.as_ref(), exif_offset, gps_offset)?;
        img.write_data(&data)?;
    }
    Ok(())
}

fn write_exif_field<W: std::io::Write + std::io::Seek, K: tiff::encoder::TiffKind>(
    dir: &mut tiff::encoder::DirectoryEncoder<'_, W, K>,
    tag: Tag,
    value: &exif::Value,
) {
    match value {
        exif::Value::Ascii(v) => {
            if let Some(s) = v.first().and_then(|f| std::str::from_utf8(f).ok()) {
                let _ = dir.write_tag(tag, s.trim_matches('\0'));
            }
        }
        exif::Value::Short(v) => {
            let _ = dir.write_tag(tag, &v[..]);
        }
        exif::Value::Long(v) => {
            let _ = dir.write_tag(tag, &v[..]);
        }
        exif::Value::Rational(v) => {
            let rats: Vec<_> = v
                .iter()
                .map(|r| tiff::encoder::Rational {
                    n: r.num,
                    d: r.denom,
                })
                .collect();
            let _ = dir.write_tag(tag, &rats[..]);
        }
        exif::Value::SRational(v) => {
            let rats: Vec<_> = v
                .iter()
                .map(|r| tiff::encoder::SRational {
                    n: r.num,
                    d: r.denom,
                })
                .collect();
            let _ = dir.write_tag(tag, &rats[..]);
        }
        exif::Value::Byte(v) | exif::Value::Undefined(v, _) => {
            let _ = dir.write_tag(tag, &v[..]);
        }
        _ => {}
    }
}

fn is_tiff_raster_tag(tag_num: u16) -> bool {
    matches!(
        tag_num,
        0x0100
            | 0x0101
            | 0x0102
            | 0x0103
            | 0x0106
            | 0x0111
            | 0x0112
            | 0x0115
            | 0x0116
            | 0x0117
            | 0x011a
            | 0x011b
            | 0x0128
            | 0x0140
            | 0x0144
            | 0x0145
            | 0x0152
            | 0x0153
            | 0x8769
            | 0x8773
            | 0x8825
    )
}

fn write_tiff_metadata<W: std::io::Write + std::io::Seek, K: tiff::encoder::TiffKind>(
    dir: &mut tiff::encoder::DirectoryEncoder<'_, W, K>,
    doc: &Document,
    parsed_exif: Option<&exif::Exif>,
    exif_offset: Option<u32>,
    gps_offset: Option<u32>,
) -> Result<()> {
    if let Some(icc) = doc.profile.icc() {
        dir.write_tag(Tag::IccProfile, icc)?;
    }
    if let Some(offset) = exif_offset {
        dir.write_tag(Tag::ExifDirectory, offset)?;
    }
    if let Some(offset) = gps_offset {
        dir.write_tag(Tag::GpsDirectory, offset)?;
    }
    if let Some(parsed) = parsed_exif {
        for f in parsed.fields() {
            if f.tag.context() == exif::Context::Tiff && !is_tiff_raster_tag(f.tag.number()) {
                let tag = Tag::from_u16(f.tag.number()).unwrap_or(Tag::Unknown(f.tag.number()));
                write_exif_field(dir, tag, &f.value);
            }
        }
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
    if let Some(exif) = doc.exif.as_deref().and_then(fit_jpeg_exif) {
        let _ = encoder.set_exif_metadata(exif);
    }
    encoder.write_image(
        &rgb,
        image.width(),
        image.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(())
}

/// The most EXIF one JPEG APP1 segment holds: 65535, less the length field
/// and the "Exif\0\0" header. The encoder silently wraps the segment length
/// past this, corrupting the file.
const MAX_JPEG_EXIF: usize = 65535 - 2 - 6;

/// EXIF small enough for a JPEG. A darktable TIFF's metadata carries its
/// XMP edit history and the camera's maker notes, which together can pass
/// 64 KB, so drop those bulky blobs if needed, or all of it as a last resort.
fn fit_jpeg_exif(exif: &[u8]) -> Option<Vec<u8>> {
    if exif.len() <= MAX_JPEG_EXIF {
        return Some(exif.to_vec());
    }
    let parsed = exif::Reader::new().read_raw(exif.to_vec()).ok()?;
    let mut writer = exif::experimental::Writer::new();
    // XMP, IPTC, PrintIM and MakerNote.
    for f in parsed
        .fields()
        .filter(|f| !matches!(f.tag.number(), 0x02bc | 0x83bb | 0xc4a5 | 0x927c))
    {
        writer.push_field(f);
    }
    let mut buf = std::io::Cursor::new(Vec::new());
    writer.write(&mut buf, parsed.little_endian()).ok()?;
    let slim = buf.into_inner();
    (slim.len() <= MAX_JPEG_EXIF).then_some(slim)
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

    /// Walk a JPEG's segments up to the scan: a wrapped length lands off a
    /// marker.
    fn segments_ok(bytes: &[u8]) -> bool {
        let mut i = 2;
        while i + 4 <= bytes.len() && bytes[i] == 0xff {
            if bytes[i + 1] == 0xda {
                return true;
            }
            i += 2 + usize::from(u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]));
        }
        false
    }

    #[test]
    fn jpeg_with_oversized_exif_stays_valid() {
        // Like a darktable TIFF: a date worth keeping plus 70 KB of XMP.
        let date = exif::Field {
            tag: exif::Tag::DateTimeOriginal,
            ifd_num: exif::In::PRIMARY,
            value: exif::Value::Ascii(vec![b"2024:02:13 15:58:41".to_vec()]),
        };
        let xmp = exif::Field {
            tag: exif::Tag(exif::Context::Tiff, 0x02bc),
            ifd_num: exif::In::PRIMARY,
            value: exif::Value::Byte(vec![b'x'; 70_000]),
        };
        let mut writer = exif::experimental::Writer::new();
        writer.push_field(&date);
        writer.push_field(&xmp);
        let mut buf = std::io::Cursor::new(Vec::new());
        writer.write(&mut buf, true).unwrap();

        let mut doc = Document::from_image(
            "x.tif".into(),
            &Raster::new(8, 8, vec![[30000; 4]; 64]),
            ColorProfile::srgb(),
            16,
        );
        doc.exif = Some(buf.into_inner());
        let dir = std::env::temp_dir().join(format!("omapix-bigexif-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.jpg");
        jpeg(&doc, &path, 90).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let parsed = exif::Reader::new()
            .read_from_container(&mut std::io::Cursor::new(&bytes))
            .unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert!(segments_ok(&bytes));
        assert!(parsed.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY).is_some());
    }
}
