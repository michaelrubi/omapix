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

/// Open a TIFF sent from darktable for a round trip: the layers saved
/// beside it by an earlier round trip, if there are any, or else the TIFF
/// itself. Either way it saves its layers to a .ora beside the TIFF, and
/// the flattened image back to the TIFF for darktable.
pub fn load_round_trip(tiff: &Path) -> Result<Document> {
    let ora = tiff.with_extension("ora");
    let mut doc = if ora.exists() { crate::ora::load(&ora)? } else { load(tiff)? };
    doc.saved_path = Some(ora);
    doc.round_trip = Some(tiff.to_path_buf());
    Ok(doc)
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
    let mut doc = Document::from_image(
        path.to_path_buf(),
        &Raster::new(width, height, pixels),
        profile,
        bits,
    );
    doc.exif = read_tiff_exif(path);
    Ok(doc)
}

fn read_tiff_exif(path: &Path) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let parsed = exif::Reader::new().read_from_container(&mut reader).ok()?;
    let fields: Vec<_> = parsed
        .fields()
        .filter(|f| {
            !matches!(
                f.tag.number(),
                0x0100
                    | 0x0101
                    | 0x0102
                    | 0x0103
                    | 0x0106
                    | 0x0111
                    | 0x0115
                    | 0x0116
                    | 0x0117
                    | 0x0144
                    | 0x0145
                    | 0x8773
            )
        })
        .collect();
    if fields.is_empty() {
        return None;
    }
    let mut writer = exif::experimental::Writer::new();
    for f in fields {
        writer.push_field(f);
    }
    let mut buf = Cursor::new(Vec::new());
    writer.write(&mut buf, true).ok()?;
    let mut exif = buf.into_inner();
    let _ = image::metadata::Orientation::remove_from_exif_chunk(&mut exif);
    Some(exif)
}

fn load_other(path: &Path) -> Result<Document> {
    let reader = ImageReader::new(open(path)?)
        .with_guessed_format()
        .map_err(|source| Error::Read {
            path: path.display().to_string(),
            source,
        })?;
    let (raster, profile, source_bits, exif) = decode(reader)?;
    let mut doc = Document::from_image(
        path.to_path_buf(),
        &raster,
        profile,
        source_bits,
    );
    doc.exif = exif;
    Ok(doc)
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
    let (raster, profile, _, _) = decode(reader)?;
    Ok((raster, profile))
}

/// Decode an image with the `image` crate, returning it with its colour
/// space, bits per channel, and raw EXIF metadata.
fn decode<R: BufRead + Seek>(
    reader: ImageReader<R>,
) -> Result<(Raster, ColorProfile, u8, Option<Vec<u8>>)> {
    let mut decoder = reader.into_decoder()?;
    let icc = decoder.icc_profile()?;
    let source_bits =
        decoder.color_type().bytes_per_pixel() / decoder.color_type().channel_count() * 8;
    let mut exif = decoder.exif_metadata().ok().flatten();
    let orientation = exif
        .as_deref_mut()
        .and_then(image::metadata::Orientation::remove_from_exif_chunk);
    let mut image = image::DynamicImage::from_decoder(decoder)?;
    if let Some(orient) = orientation {
        image.apply_orientation(orient);
    }
    let image = image.into_rgba16();
    let (width, height) = image.dimensions();
    let pixels = image.pixels().map(|p| p.0).collect();
    let profile = match icc {
        Some(icc) => ColorProfile::from_icc(icc)?,
        None => ColorProfile::srgb(),
    };
    Ok((
        Raster::new(width, height, pixels),
        profile,
        source_bits,
        exif,
    ))
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
    use image::ImageEncoder;

    #[test]
    fn expands_every_channel_layout() {
        assert_eq!(expand(&[7], 1), vec![[7, 7, 7, OPAQUE]]);
        assert_eq!(expand(&[7, 9], 2), vec![[7, 7, 7, 9]]);
        assert_eq!(expand(&[1, 2, 3], 3), vec![[1, 2, 3, OPAQUE]]);
        assert_eq!(expand(&[1, 2, 3, 4], 4), vec![[1, 2, 3, 4]]);
    }

    #[test]
    fn round_trip_keeps_its_layers_beside_the_tiff() {
        let dir = std::env::temp_dir().join(format!("omapix-round-trip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tiff = dir.join("IMG_0001.tif");
        let image = Raster::new(40, 30, vec![[20000, 30000, 40000, OPAQUE]; 40 * 30]);
        let doc = Document::from_image(tiff.clone(), &image, ColorProfile::srgb(), 16);
        crate::export::tiff(&doc, &tiff).unwrap();

        // First time: the TIFF, set to save its layers beside it.
        let mut doc = load_round_trip(&tiff).unwrap();
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.saved_path, Some(dir.join("IMG_0001.ora")));
        assert_eq!(doc.round_trip, Some(tiff.clone()));
        let id = doc.next_layer_id();
        doc.layers.push(crate::Layer::empty(id, "Retouch", 40, 30));
        crate::ora::save(&doc, &dir.join("IMG_0001.ora")).unwrap();

        // The .ora remembers the TIFF, and opening the TIFF again brings
        // the layers back.
        let ora = crate::ora::load(&dir.join("IMG_0001.ora")).unwrap();
        assert_eq!(ora.round_trip, Some(tiff.clone()));
        let again = load_round_trip(&tiff).unwrap();
        assert_eq!(again.layers.len(), 2);
        assert_eq!(again.round_trip, Some(tiff));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn synthetic_exif(date: &str) -> Vec<u8> {
        let mut writer = exif::experimental::Writer::new();
        let field = exif::Field {
            tag: exif::Tag::DateTimeOriginal,
            ifd_num: exif::In::PRIMARY,
            value: exif::Value::Ascii(vec![date.as_bytes().to_vec()]),
        };
        writer.push_field(&field);
        let mut buf = Cursor::new(Vec::new());
        writer.write(&mut buf, true).unwrap();
        buf.into_inner()
    }

    fn read_date(path: &Path) -> Option<String> {
        let mut file = BufReader::new(File::open(path).ok()?);
        let parsed = exif::Reader::new().read_from_container(&mut file).ok()?;
        let field = parsed.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY)?;
        match &field.value {
            exif::Value::Ascii(v) => v
                .first()
                .and_then(|b| std::str::from_utf8(b).ok())
                .map(|s| s.trim_matches('\0').to_string()),
            _ => None,
        }
    }

    #[test]
    fn exif_survives_open_export_and_ora_round_trip() {
        let dir = std::env::temp_dir().join(format!("omapix-exif-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let date_str = "2024:01:15 10:20:30";
        let exif_blob = synthetic_exif(date_str);

        // --- 1. Test starting from a JPEG ---
        let init_jpg = dir.join("input.jpg");
        {
            let mut enc = image::codecs::jpeg::JpegEncoder::new(File::create(&init_jpg).unwrap());
            enc.set_exif_metadata(exif_blob.clone()).unwrap();
            enc.write_image(&[100, 150, 200], 1, 1, image::ExtendedColorType::Rgb8)
                .unwrap();
        }

        let doc_jpg = load(&init_jpg).unwrap();
        assert!(doc_jpg.exif.is_some(), "JPEG loader should preserve EXIF blob");

        // JPEG -> export JPEG
        let out_jpg = dir.join("out_from_jpg.jpg");
        crate::export::jpeg(&doc_jpg, &out_jpg, 90).unwrap();
        assert_eq!(read_date(&out_jpg).as_deref(), Some(date_str));

        // JPEG -> export TIFF
        let out_tif = dir.join("out_from_jpg.tif");
        crate::export::tiff(&doc_jpg, &out_tif).unwrap();
        assert_eq!(read_date(&out_tif).as_deref(), Some(date_str));

        // JPEG -> save .ora -> reload -> export JPEG & TIFF
        let out_ora = dir.join("doc_from_jpg.ora");
        crate::ora::save(&doc_jpg, &out_ora).unwrap();
        let doc_ora = crate::ora::load(&out_ora).unwrap();
        assert!(doc_ora.exif.is_some(), ".ora loader should preserve EXIF blob");

        let re_jpg = dir.join("re_from_ora_jpg.jpg");
        crate::export::jpeg(&doc_ora, &re_jpg, 90).unwrap();
        assert_eq!(read_date(&re_jpg).as_deref(), Some(date_str));

        let re_tif = dir.join("re_from_ora_jpg.tif");
        crate::export::tiff(&doc_ora, &re_tif).unwrap();
        assert_eq!(read_date(&re_tif).as_deref(), Some(date_str));

        // --- 2. Test starting from a TIFF ---
        let init_tif = dir.join("input.tif");
        let mut doc_for_tif = Document::from_image(
            init_tif.clone(),
            &Raster::new(2, 2, vec![[10000, 20000, 30000, OPAQUE]; 4]),
            ColorProfile::srgb(),
            16,
        );
        doc_for_tif.exif = Some(exif_blob);
        crate::export::tiff(&doc_for_tif, &init_tif).unwrap();
        assert_eq!(read_date(&init_tif).as_deref(), Some(date_str));

        let doc_tif = load(&init_tif).unwrap();
        assert!(doc_tif.exif.is_some(), "TIFF loader should preserve EXIF blob");

        // TIFF -> export JPEG
        let out_jpg_from_tif = dir.join("out_from_tif.jpg");
        crate::export::jpeg(&doc_tif, &out_jpg_from_tif, 90).unwrap();
        assert_eq!(read_date(&out_jpg_from_tif).as_deref(), Some(date_str));

        // TIFF -> export TIFF
        let out_tif_from_tif = dir.join("out_from_tif.tif");
        crate::export::tiff(&doc_tif, &out_tif_from_tif).unwrap();
        assert_eq!(read_date(&out_tif_from_tif).as_deref(), Some(date_str));

        // TIFF -> save .ora -> reload -> export JPEG & TIFF
        let out_ora_tif = dir.join("doc_from_tif.ora");
        crate::ora::save(&doc_tif, &out_ora_tif).unwrap();
        let doc_ora_tif = crate::ora::load(&out_ora_tif).unwrap();
        assert!(doc_ora_tif.exif.is_some(), ".ora should keep EXIF from TIFF");

        let re_jpg_from_tif = dir.join("re_from_ora_tif.jpg");
        crate::export::jpeg(&doc_ora_tif, &re_jpg_from_tif, 90).unwrap();
        assert_eq!(read_date(&re_jpg_from_tif).as_deref(), Some(date_str));

        let re_tif_from_tif = dir.join("re_from_ora_tif.tif");
        crate::export::tiff(&doc_ora_tif, &re_tif_from_tif).unwrap();
        assert_eq!(read_date(&re_tif_from_tif).as_deref(), Some(date_str));

        // --- 3. Test darktable round trip ---
        let rt_tif = dir.join("round_trip.tif");
        crate::export::tiff(&doc_for_tif, &rt_tif).unwrap();
        let doc_rt = load_round_trip(&rt_tif).unwrap();
        assert!(doc_rt.exif.is_some(), "load_round_trip should load EXIF");

        // Save round trip (.ora beside the TIFF)
        let rt_ora = rt_tif.with_extension("ora");
        crate::ora::save(&doc_rt, &rt_ora).unwrap();
        // Export flattened image back to the TIFF for darktable
        crate::export::tiff(&doc_rt, &rt_tif).unwrap();
        assert_eq!(read_date(&rt_tif).as_deref(), Some(date_str));

        // Reload round trip from the TIFF (which loads .ora)
        let doc_rt_reloaded = load_round_trip(&rt_tif).unwrap();
        assert!(doc_rt_reloaded.exif.is_some());
        let rt_export = dir.join("round_trip_export.jpg");
        crate::export::jpeg(&doc_rt_reloaded, &rt_export, 90).unwrap();
        assert_eq!(read_date(&rt_export).as_deref(), Some(date_str));

        std::fs::remove_dir_all(&dir).ok();
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

    #[test]
    #[ignore]
    fn real_camera_file_preserves_exif_on_export() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/rubiconetic".into());
        let camera_jpg = Path::new(&home).join("Downloads/MJR03133.jpg");
        if !camera_jpg.exists() {
            println!("Skipping real camera file test; {} not found", camera_jpg.display());
            return;
        }

        let doc = load(&camera_jpg).unwrap();
        assert!(doc.exif.is_some(), "Camera JPEG must have EXIF metadata");

        let dir = std::env::temp_dir().join("omapix-real-camera-test");
        std::fs::create_dir_all(&dir).unwrap();

        let out_jpg = dir.join("camera_export.jpg");
        crate::export::jpeg(&doc, &out_jpg, 92).unwrap();
        assert_eq!(read_date(&out_jpg).as_deref(), Some("2024:02:13 15:58:41"));

        let out_tif = dir.join("camera_export.tif");
        crate::export::tiff(&doc, &out_tif).unwrap();
        assert_eq!(read_date(&out_tif).as_deref(), Some("2024:02:13 15:58:41"));

        let out_ora = dir.join("camera.ora");
        crate::ora::save(&doc, &out_ora).unwrap();
        let re_doc = crate::ora::load(&out_ora).unwrap();
        assert!(re_doc.exif.is_some());

        let re_jpg = dir.join("camera_re_export.jpg");
        crate::export::jpeg(&re_doc, &re_jpg, 92).unwrap();
        assert_eq!(read_date(&re_jpg).as_deref(), Some("2024:02:13 15:58:41"));

        // If exiftool is installed, verify its output directly
        if let Ok(output) = std::process::Command::new("exiftool")
            .arg("-DateTimeOriginal")
            .arg("-Make")
            .arg("-Model")
            .arg(&out_jpg)
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            println!("exiftool output for JPEG export:\n{}", text);
            assert!(text.contains("2024:02:13 15:58:41"));
            assert!(text.contains("SONY"));
            assert!(text.contains("ILCE-7M3"));
        }

        if let Ok(output) = std::process::Command::new("exiftool")
            .arg("-DateTimeOriginal")
            .arg("-Make")
            .arg("-Model")
            .arg(&out_tif)
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            println!("exiftool output for TIFF export:\n{}", text);
            assert!(text.contains("2024:02:13 15:58:41"));
            assert!(text.contains("SONY"));
            assert!(text.contains("ILCE-7M3"));
        }

        std::fs::remove_dir_all(&dir).ok();
    }
}
