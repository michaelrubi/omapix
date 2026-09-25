//! Photoshop (.psd) file import.
//!
//! Imports flattened and simple layered Photoshop files for legacy Photoshop work.
//!
//! Supported:
//! - 8-bit RGB and Grayscale PSD files.
//! - Pixel layers with their name, opacity (0–1), visibility, canvas position/offsets,
//!   and blend modes (mapped to [`BlendMode`], defaulting unmapped modes to Normal).
//! - Flattened-only PSD files (and layered files with unsupported layer sections,
//!   such as 16-bit layer records), falling back to the flattened composite image.
//! - Clipped layers (marked with `layer.clipped`).
//! - sRGB colour space (the `psd` crate does not expose embedded ICC profile tags).
//!
//! Skipped or not supported:
//! - Layer groups: hierarchy is flattened; contained pixel layers are imported directly.
//! - Layer masks and vector masks: skipped (the `psd` crate does not parse mask channel pixels).
//! - Adjustment layers, text layers, smart objects, and layer effects/styles: skipped.
//! - 16-bit/32-bit layered files: individual layer records are not supported by the
//!   `psd` crate; falls back to the composite image if readable.
//! - Saving back to .psd: saving stays OpenRaster, TIFF, or JPEG. Saving an opened PSD
//!   prompts Save As (Ctrl+S does not overwrite the original .psd).

use std::path::Path;

use crate::blend::BlendMode;
use crate::layer::Layer;
use crate::raster::{Pixel, Raster, widen};
use crate::tiled::Tiled;
use crate::{ColorProfile, Document, Error, Result};

/// Load a Photoshop (.psd) file from disk into a [`Document`].
pub fn load(path: &Path) -> Result<Document> {
    let bytes = std::fs::read(path).map_err(|source| Error::Read {
        path: path.display().to_string(),
        source,
    })?;
    load_from_bytes(&bytes, path)
}

fn load_from_bytes(bytes: &[u8], path: &Path) -> Result<Document> {
    // The `psd` crate panics on some files it can't read, rather than
    // returning an error.
    let parse = |bytes: &[u8]| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| psd::Psd::from_bytes(bytes)))
            .ok()?
            .ok()
    };
    let psd = parse(bytes)
        .or_else(|| parse(&try_strip_layers(bytes)?))
        .ok_or_else(|| {
            Error::Unsupported("can't read this PSD (8-bit RGB and greyscale only)".into())
        })?;

    let width = psd.width();
    let height = psd.height();
    if width == 0 || height == 0 {
        return Err(Error::Unsupported("PSD has zero dimensions".into()));
    }

    let source_bits = match psd.depth() {
        psd::PsdDepth::Eight => 8,
        psd::PsdDepth::Sixteen => 16,
        psd::PsdDepth::ThirtyTwo => 32,
        _ => 8,
    };
    let profile = ColorProfile::srgb();

    // If there are pixel layers, try loading them.
    if let Some(doc) = load_layers(&psd, path, profile.clone(), source_bits) {
        return Ok(doc);
    }

    // Otherwise (flattened-only PSD, or if layer reading failed), load the flattened composite as one layer.
    load_flattened(&psd, path, profile, source_bits)
}

fn load_layers(
    psd: &psd::Psd,
    path: &Path,
    profile: ColorProfile,
    source_bits: u8,
) -> Option<Document> {
    let width = psd.width();
    let height = psd.height();
    let psd_layers = psd.layers();
    if psd_layers.is_empty() {
        return None;
    }
    let mut layers = Vec::with_capacity(psd_layers.len());

    for (idx, psd_layer) in psd_layers.iter().enumerate() {
        let rgba8 = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| psd_layer.rgba())).ok()?;
        if rgba8.len() != (width * height * 4) as usize {
            return None;
        }

        let tiled = Tiled::from_slice(width, height, [0; 4], &widen_rgba(&rgba8));

        let mut layer = Layer::from_pixels((idx + 1) as u64, psd_layer.name(), tiled);
        layer.opacity = (psd_layer.opacity() as f32 / 255.0).clamp(0.0, 1.0);
        layer.visible = psd_layer.visible();
        layer.blend = map_blend_mode(psd_layer.blend_mode() as u8);
        layer.clipped = psd_layer.is_clipping_mask();
        layers.push(layer);
    }

    if layers.is_empty() {
        return None;
    }

    // Do NOT set saved_path so Ctrl+S prompts Save As, matching TIFF/JPEG.
    Some(Document::new(
        path.to_path_buf(),
        profile,
        source_bits,
        width,
        height,
        layers,
    ))
}

fn load_flattened(
    psd: &psd::Psd,
    path: &Path,
    profile: ColorProfile,
    source_bits: u8,
) -> Result<Document> {
    let width = psd.width();
    let height = psd.height();
    let rgba8 = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| psd.rgba()))
        .map_err(|_| Error::Unsupported("Failed to decode PSD composite image".into()))?;
    if rgba8.len() != (width * height * 4) as usize {
        return Err(Error::Unsupported("PSD composite size mismatch".into()));
    }

    let raster = Raster::new(width, height, widen_rgba(&rgba8));
    Ok(Document::from_image(path.to_path_buf(), &raster, profile, source_bits))
}

fn widen_rgba(rgba8: &[u8]) -> Vec<Pixel> {
    rgba8.as_chunks::<4>().0.iter().map(|p| p.map(widen)).collect()
}

/// A layer's blend mode, from the `psd` crate's number for it (its enum
/// isn't public).
fn map_blend_mode(d: u8) -> BlendMode {
    match d {
        1 => BlendMode::Normal,
        3 => BlendMode::Darken,
        4 => BlendMode::Multiply,
        5 => BlendMode::ColorBurn,
        6 => BlendMode::LinearBurn,
        8 => BlendMode::Lighten,
        9 => BlendMode::Screen,
        10 => BlendMode::ColorDodge,
        11 => BlendMode::LinearDodge,
        13 => BlendMode::Overlay,
        14 => BlendMode::SoftLight,
        15 => BlendMode::HardLight,
        16 => BlendMode::VividLight,
        17 => BlendMode::LinearLight,
        18 => BlendMode::PinLight,
        20 => BlendMode::Difference,
        21 => BlendMode::Exclusion,
        22 => BlendMode::Subtract,
        23 => BlendMode::Divide,
        24 => BlendMode::Hue,
        25 => BlendMode::Saturation,
        26 => BlendMode::Color,
        27 => BlendMode::Luminosity,
        // Pass Through (0, groups only), Dissolve (2), Darker Color (7),
        // Lighter Color (12), Hard Mix (19), or unknown.
        _ => BlendMode::Normal,
    }
}

/// If parsing layer records fails, strip the layer and mask section so the
/// file can be parsed as a flattened composite.
fn try_strip_layers(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < 26 || &bytes[0..4] != b"8BPS" {
        return None;
    }
    let mut pos = 26;
    if pos + 4 > bytes.len() {
        return None;
    }
    let color_len = u32::from_be_bytes(bytes[pos..pos + 4].try_into().ok()?) as usize;
    pos += 4 + color_len;
    if pos + 4 > bytes.len() {
        return None;
    }
    let res_len = u32::from_be_bytes(bytes[pos..pos + 4].try_into().ok()?) as usize;
    pos += 4 + res_len;
    if pos + 4 > bytes.len() {
        return None;
    }
    let layer_mask_len = u32::from_be_bytes(bytes[pos..pos + 4].try_into().ok()?) as usize;
    if layer_mask_len == 0 {
        return None;
    }
    let layer_start = pos;
    let data_start = pos + 4 + layer_mask_len;
    if data_start > bytes.len() {
        return None;
    }
    let mut out = Vec::with_capacity(layer_start + 4 + (bytes.len() - data_start));
    out.extend_from_slice(&bytes[..layer_start]);
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&bytes[data_start..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Constructs a minimal valid PSD byte buffer with flattened-only image data.
    fn make_flattened_psd(width: u32, height: u32, rgb_pixels: &[[u8; 3]]) -> Vec<u8> {
        let mut out = Vec::new();
        // File Header
        out.extend_from_slice(b"8BPS");
        out.extend_from_slice(&1u16.to_be_bytes()); // Version
        out.extend_from_slice(&[0u8; 6]); // Reserved
        out.extend_from_slice(&3u16.to_be_bytes()); // Channels: R, G, B
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&8u16.to_be_bytes()); // 8-bit depth
        out.extend_from_slice(&3u16.to_be_bytes()); // RGB mode

        // Color Mode Data Section (empty)
        out.extend_from_slice(&0u32.to_be_bytes());

        // Image Resources Section (empty)
        out.extend_from_slice(&0u32.to_be_bytes());

        // Layer and Mask Information Section (empty = flattened)
        out.extend_from_slice(&0u32.to_be_bytes());

        // Image Data Section: Raw compression (0) followed by planar R, G, B
        out.extend_from_slice(&0u16.to_be_bytes());
        for p in rgb_pixels {
            out.push(p[0]);
        }
        for p in rgb_pixels {
            out.push(p[1]);
        }
        for p in rgb_pixels {
            out.push(p[2]);
        }
        out
    }

    struct LayerSpec<'a> {
        name: &'a str,
        top: i32,
        left: i32,
        bottom: i32,
        right: i32,
        opacity: u8,
        visible: bool,
        blend: &'a str,
        pixels: &'a [[u8; 4]],
    }

    /// Constructs a minimal valid PSD byte buffer with multiple pixel layers.
    fn make_layered_psd(
        width: u32,
        height: u32,
        composite_rgb: &[[u8; 3]],
        layers: &[LayerSpec],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        // Header
        out.extend_from_slice(b"8BPS");
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&[0u8; 6]);
        out.extend_from_slice(&3u16.to_be_bytes()); // 3 channels for composite
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&8u16.to_be_bytes());
        out.extend_from_slice(&3u16.to_be_bytes());

        // Color Mode & Resources
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());

        // Layer & Mask section
        // Note: In PSD files, layers are stored in reverse (top to bottom) order.
        let mut records_data = Vec::new();
        let mut channels_data = Vec::new();

        for layer in layers.iter().rev() {
            let lw = (layer.right - layer.left) as usize;
            let lh = (layer.bottom - layer.top) as usize;
            assert_eq!(layer.pixels.len(), lw * lh);

            let mut chan_payloads = Vec::new();
            for chan_idx in 0..4 {
                let mut data = Vec::with_capacity(2 + layer.pixels.len());
                data.extend_from_slice(&0u16.to_be_bytes()); // Raw compression
                for px in layer.pixels {
                    data.push(px[chan_idx]);
                }
                chan_payloads.push(data);
            }

            // Layer record
            records_data.extend_from_slice(&layer.top.to_be_bytes());
            records_data.extend_from_slice(&layer.left.to_be_bytes());
            records_data.extend_from_slice(&layer.bottom.to_be_bytes());
            records_data.extend_from_slice(&layer.right.to_be_bytes());
            records_data.extend_from_slice(&4u16.to_be_bytes()); // 4 channels

            for (chan_id, payload) in [0i16, 1, 2, -1].iter().zip(&chan_payloads) {
                records_data.extend_from_slice(&chan_id.to_be_bytes());
                records_data.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            }

            records_data.extend_from_slice(b"8BIM");
            records_data.extend_from_slice(layer.blend.as_bytes());
            records_data.push(layer.opacity);
            records_data.push(0); // clipping base
            records_data.push(if layer.visible { 2 } else { 0 }); // visible flag bit 1
            records_data.push(0); // filler

            // Extra data: mask (0), blend range (0), name (Pascal string padded to 4)
            let name_bytes = layer.name.as_bytes();
            let mut pstring = Vec::new();
            pstring.push(name_bytes.len() as u8);
            pstring.extend_from_slice(name_bytes);
            let pad = (4 - (pstring.len() % 4)) % 4;
            pstring.resize(pstring.len() + pad, 0);

            let extra_len = 4 + 4 + pstring.len() as u32;
            records_data.extend_from_slice(&extra_len.to_be_bytes());
            records_data.extend_from_slice(&0u32.to_be_bytes()); // layer mask len
            records_data.extend_from_slice(&0u32.to_be_bytes()); // blend range len
            records_data.extend_from_slice(&pstring);

            for payload in chan_payloads {
                channels_data.extend_from_slice(&payload);
            }
        }

        let mut layer_info = Vec::new();
        layer_info.extend_from_slice(&(layers.len() as i16).to_be_bytes());
        layer_info.extend_from_slice(&records_data);
        layer_info.extend_from_slice(&channels_data);

        let mut layer_and_mask = Vec::new();
        layer_and_mask.extend_from_slice(&(layer_info.len() as u32).to_be_bytes());
        layer_and_mask.extend_from_slice(&layer_info);

        out.extend_from_slice(&(layer_and_mask.len() as u32).to_be_bytes());
        out.extend_from_slice(&layer_and_mask);

        // Image Data Section (flattened composite)
        out.extend_from_slice(&0u16.to_be_bytes());
        for p in composite_rgb {
            out.push(p[0]);
        }
        for p in composite_rgb {
            out.push(p[1]);
        }
        for p in composite_rgb {
            out.push(p[2]);
        }
        out
    }

    #[test]
    fn loads_flattened_psd_as_single_layer() {
        let pixels = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0]];
        let psd_bytes = make_flattened_psd(2, 2, &pixels);

        let doc = load_from_bytes(&psd_bytes, Path::new("test.psd")).unwrap();
        assert_eq!(doc.width, 2);
        assert_eq!(doc.height, 2);
        assert_eq!(doc.source_bits, 8);
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.layers[0].name, "Background");
        assert_eq!(doc.layers[0].opacity, 1.0);
        assert!(doc.layers[0].visible);
        assert_eq!(doc.saved_path, None);

        // Check pixel values (widened from 8 to 16 bit)
        let red_pixel = doc.layers[0].pixels.get(0, 0);
        assert_eq!(red_pixel, [65535, 0, 0, 65535]);
        let green_pixel = doc.layers[0].pixels.get(1, 0);
        assert_eq!(green_pixel, [0, 65535, 0, 65535]);
        let blue_pixel = doc.layers[0].pixels.get(0, 1);
        assert_eq!(blue_pixel, [0, 0, 65535, 65535]);
        let yellow_pixel = doc.layers[0].pixels.get(1, 1);
        assert_eq!(yellow_pixel, [65535, 65535, 0, 65535]);
    }

    #[test]
    fn loads_layered_psd_with_properties_and_offsets() {
        let composite = [[100, 100, 100]; 6];
        let layer0_pixels = [
            [200, 10, 20, 255],
            [200, 10, 20, 255],
            [200, 10, 20, 255],
            [200, 10, 20, 255],
            [200, 10, 20, 255],
            [200, 10, 20, 255],
        ];
        let layer1_pixels = [[50, 150, 250, 200]]; // 1x1

        let layers = [
            LayerSpec {
                name: "Base Layer",
                top: 0,
                left: 0,
                bottom: 2,
                right: 3,
                opacity: 255,
                visible: true,
                blend: "norm",
                pixels: &layer0_pixels,
            },
            LayerSpec {
                name: "Top Layer",
                top: 1,
                left: 2,
                bottom: 2,
                right: 3,
                opacity: 128,
                visible: false,
                blend: "mul ",
                pixels: &layer1_pixels,
            },
        ];

        let psd_bytes = make_layered_psd(3, 2, &composite, &layers);
        let doc = load_from_bytes(&psd_bytes, Path::new("layered.psd")).unwrap();

        assert_eq!(doc.width, 3);
        assert_eq!(doc.height, 2);
        assert_eq!(doc.layers.len(), 2);
        assert_eq!(doc.saved_path, None);

        // Layer 0: Base Layer (bottom)
        let l0 = &doc.layers[0];
        assert_eq!(l0.name, "Base Layer");
        assert_eq!(l0.opacity, 1.0);
        assert!(l0.visible);
        assert_eq!(l0.blend, BlendMode::Normal);
        assert_eq!(l0.pixels.get(0, 0), [widen(200), widen(10), widen(20), 65535]);

        // Layer 1: Top Layer
        let l1 = &doc.layers[1];
        assert_eq!(l1.name, "Top Layer");
        assert!((l1.opacity - 128.0 / 255.0).abs() < 1e-3);
        assert!(!l1.visible);
        assert_eq!(l1.blend, BlendMode::Multiply);
        // Only at offset (2, 1) should pixels exist; (0, 0) should be transparent
        assert_eq!(l1.pixels.get(0, 0), [0, 0, 0, 0]);
        assert_eq!(l1.pixels.get(2, 1), [widen(50), widen(150), widen(250), widen(200)]);
    }

    #[test]
    fn falls_back_to_composite_when_layer_section_is_unsupported() {
        let pixels = [[120, 130, 140]; 4];
        let flat_bytes = make_flattened_psd(2, 2, &pixels);

        // Splice in an unsupported/corrupted 16-byte layer section before the image data
        let mut psd_bytes = Vec::new();
        // Header, color mode, resources
        psd_bytes.extend_from_slice(&flat_bytes[..26 + 4 + 4]);
        // Layer & mask section length (16 bytes of data)
        psd_bytes.extend_from_slice(&16u32.to_be_bytes());
        psd_bytes.extend_from_slice(&[0xff; 16]); // invalid layer record bytes
        // Image data section from flat_bytes
        let img_data_pos = 26 + 4 + 4 + 4; // after empty layer section in flat_bytes
        psd_bytes.extend_from_slice(&flat_bytes[img_data_pos..]);

        // load_from_bytes should strip the invalid layer section and open the composite as one layer
        let doc = load_from_bytes(&psd_bytes, Path::new("fallback.psd")).unwrap();
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.layers[0].name, "Background");
        assert_eq!(doc.layers[0].pixels.get(0, 0), [widen(120), widen(130), widen(140), 65535]);
    }
}
