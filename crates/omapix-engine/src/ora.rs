//! OpenRaster (.ora), Omapix's native file format.
//!
//! OpenRaster is the open layered-image format that Krita, GIMP and MyPaint
//! read, so layered Omapix files aren't locked into Omapix. It's a zip of
//! one PNG per layer plus `stack.xml` describing the stack. Omapix writes:
//!
//! - 16-bit PNGs carrying the document's ICC profile, cropped to the area
//!   each layer actually uses;
//! - blend modes under the names Krita uses (see [`BlendMode::ora_name`]);
//! - layer masks as extra 16-bit grey PNGs, referenced by `omapix:*`
//!   attributes that other apps ignore, and alpha channels (saved
//!   selections) the same way, as `omapix:channel` elements after the stack;
//! - layer groups as nested stacks, with `isolation="auto"` for Pass
//!   Through and `isolation="isolate"` for groups with their own blend mode.

use std::fs::File;
use std::io::{BufReader, BufWriter, Cursor, Read, Seek, Write};
use std::path::Path;

use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ImageDecoder, ImageEncoder};
use quick_xml::XmlVersion;
use quick_xml::events::Event;
use rayon::prelude::*;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::blend::BlendMode;
use crate::document::AlphaChannel;
use crate::layer::{Layer, Locks, Mask};
use crate::tiled::{TILE, TILE_PIXELS, Tiled};
use crate::{ColorProfile, Document, Error, Pixel, Result};

const NAMESPACE: &str = "urn:omapix:1";

fn io_error(path: &Path, source: std::io::Error) -> Error {
    Error::Read {
        path: path.display().to_string(),
        source,
    }
}

fn zip_error(e: zip::result::ZipError) -> Error {
    Error::Unsupported(format!("OpenRaster zip: {e}"))
}

/// Rectangle of whole tiles that differ from the fill value, in pixels:
/// (x, y, w, h). `None` if every tile is empty.
fn used_area<T: Copy + PartialEq + Send + Sync>(t: &Tiled<T>) -> Option<(u32, u32, u32, u32)> {
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    for row in 0..t.rows() {
        for col in 0..t.cols() {
            if t.tile(col, row).is_some() {
                let b = bounds.get_or_insert((col, row, col, row));
                *b = (b.0.min(col), b.1.min(row), b.2.max(col), b.3.max(row));
            }
        }
    }
    bounds.map(|(c0, r0, c1, r1)| {
        let x = c0 * TILE;
        let y = r0 * TILE;
        let w = ((c1 + 1) * TILE).min(t.width()) - x;
        let h = ((r1 + 1) * TILE).min(t.height()) - y;
        (x, y, w, h)
    })
}

/// Copy a rectangle out of a tiled image, row-major.
fn crop<T: Copy + PartialEq + Send + Sync>(
    t: &Tiled<T>,
    (x, y, w, h): (u32, u32, u32, u32),
) -> Vec<T> {
    let mut out = Vec::with_capacity(w as usize * h as usize);
    for py in y..y + h {
        for px in x..x + w {
            out.push(t.get(px, py));
        }
    }
    out
}

/// Paste a row-major rectangle into a new tiled image of the given size.
pub(crate) fn uncrop<T: Copy + PartialEq + Send + Sync>(
    width: u32,
    height: u32,
    fill: T,
    (x, y, w, h): (u32, u32, u32, u32),
    pixels: &[T],
) -> Tiled<T> {
    Tiled::from_tiles(width, height, fill, |col, row| {
        let (tx, ty) = (col * TILE, row * TILE);
        let overlaps = tx < x + w && tx + TILE > x && ty < y + h && ty + TILE > y;
        if !overlaps {
            return None;
        }
        let mut tile = vec![fill; TILE_PIXELS];
        for dy in 0..TILE {
            let py = ty + dy;
            if py < y || py >= y + h || py >= height {
                continue;
            }
            for dx in 0..TILE {
                let px = tx + dx;
                if px < x || px >= x + w || px >= width {
                    continue;
                }
                tile[(dy * TILE + dx) as usize] = pixels[((py - y) * w + (px - x)) as usize];
            }
        }
        tile.iter().any(|&p| p != fill).then_some(tile)
    })
}

fn encode_png(data: &[u16], w: u32, h: u32, channels: u8, icc: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder =
        PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Adaptive);
    if let Some(icc) = icc {
        // Only fails for encoders without ICC support; PNG has it.
        let _ = encoder.set_icc_profile(icc.to_vec());
    }
    let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_ne_bytes()).collect();
    let color = if channels == 4 {
        image::ExtendedColorType::Rgba16
    } else {
        image::ExtendedColorType::L16
    };
    encoder.write_image(&bytes, w, h, color)?;
    Ok(out)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// An encoded PNG: archive path, bytes, and top-left position.
type Png = (String, Vec<u8>, (u32, u32));

/// Encode a mask or alpha channel as a grey PNG called `name`, cropped to
/// the area it uses, plus its fill value for the rest.
fn encode_grey(pixels: &Tiled<u16>, name: String) -> Result<(Png, u16)> {
    // An untouched one still needs a file; use one pixel.
    let area = used_area(pixels).unwrap_or((0, 0, 1, 1));
    let png = encode_png(&crop(pixels, area), area.2, area.3, 1, None)?;
    Ok(((name, png, (area.0, area.1)), pixels.fill()))
}

struct Encoded {
    layer_png: Option<Png>,
    /// Plus the mask's fill value for areas outside the PNG.
    mask_png: Option<(Png, u16)>,
}

/// Write the layers in group `parent` (top level for `None`) to stack.xml,
/// top first.
fn write_stack(xml: &mut String, doc: &Document, encoded: &[Encoded], parent: Option<u64>) {
    let layers = doc.layers.iter().zip(encoded).enumerate().rev();
    for (i, (layer, enc)) in layers.filter(|(_, (l, _))| l.parent == parent) {
        if layer.is_group {
            let isolation = if layer.blend == BlendMode::PassThrough {
                "auto"
            } else {
                "isolate"
            };
            xml.push_str(&format!("<stack isolation=\"{isolation}\""));
        } else {
            let (src, x, y) = match &enc.layer_png {
                Some((name, _, (x, y))) => (name.clone(), *x, *y),
                None => (format!("data/layer{i}.png"), 0, 0),
            };
            xml.push_str(&format!("<layer src=\"{src}\" x=\"{x}\" y=\"{y}\""));
        }
        xml.push_str(&format!(
            " name=\"{}\" opacity=\"{:.4}\" visibility=\"{}\" composite-op=\"{}\"",
            xml_escape(&layer.name),
            layer.opacity,
            if layer.visible { "visible" } else { "hidden" },
            layer.blend.ora_name(),
        ));
        if layer.clipped {
            xml.push_str(" omapix:clipped=\"true\"");
        }
        if layer.locks.transparency {
            xml.push_str(" omapix:lock-alpha=\"true\"");
        }
        if layer.locks.pixels {
            xml.push_str(" omapix:lock-pixels=\"true\"");
        }
        if layer.locks.position {
            xml.push_str(" omapix:lock-position=\"true\"");
        }
        if layer.locks.all {
            xml.push_str(" omapix:lock-all=\"true\"");
        }
        if let Some(blend_if) = layer.blend_if.filter(|b| !b.is_neutral()) {
            let json = serde_json::to_string(&blend_if).expect("blend-if always serialises");
            xml.push_str(&format!(" omapix:blend-if=\"{}\"", xml_escape(&json)));
        }
        if let Some(adjustment) = &layer.adjustment {
            xml.push_str(&format!(
                " omapix:adjustment=\"{}\"",
                xml_escape(&adjustment.to_json())
            ));
        }
        if let Some(((name, _, (mx, my)), fill)) = &enc.mask_png {
            let enabled = layer.mask.as_ref().is_some_and(|m| m.enabled);
            xml.push_str(&format!(
                " omapix:mask=\"{name}\" omapix:mask-x=\"{mx}\" omapix:mask-y=\"{my}\" omapix:mask-fill=\"{fill}\" omapix:mask-enabled=\"{enabled}\""
            ));
        }
        if layer.is_group {
            xml.push_str(">\n");
            write_stack(xml, doc, encoded, Some(layer.id));
            xml.push_str("</stack>\n");
        } else {
            xml.push_str("/>\n");
        }
    }
}

/// Save a document as OpenRaster.
pub fn save(doc: &Document, path: &Path) -> Result<()> {
    let icc = doc.profile.icc();
    let (w, h) = (doc.width, doc.height);

    // Encode every layer, mask and the merged image in parallel.
    let encoded: Vec<Encoded> = doc
        .layers
        .par_iter()
        .enumerate()
        .map(|(i, layer)| -> Result<Encoded> {
            let layer_png = match used_area(&layer.pixels) {
                Some(area) => {
                    let data: Vec<u16> = crop(&layer.pixels, area).into_iter().flatten().collect();
                    let png = encode_png(&data, area.2, area.3, 4, icc)?;
                    Some((format!("data/layer{i}.png"), png, (area.0, area.1)))
                }
                None => None,
            };
            let mask_png = match &layer.mask {
                Some(mask) => Some(encode_grey(&mask.pixels, format!("data/mask{i}.png"))?),
                None => None,
            };
            Ok(Encoded {
                layer_png,
                mask_png,
            })
        })
        .collect::<Result<_>>()?;
    let channels: Vec<(Png, u16)> = doc
        .channels
        .par_iter()
        .enumerate()
        .map(|(i, c)| encode_grey(&c.pixels, format!("data/channel{i}.png")))
        .collect::<Result<_>>()?;

    let merged = doc.composite();
    let merged_data: Vec<u16> = merged.pixels().iter().flatten().copied().collect();
    let merged_png = encode_png(&merged_data, w, h, 4, icc)?;
    let thumbnail = thumbnail_png(&merged, &doc.profile)?;

    // stack.xml lists layers top first, with groups as nested stacks.
    let mut xml = format!(
        "<?xml version='1.0' encoding='UTF-8'?>\n<image version=\"0.0.5\" w=\"{w}\" h=\"{h}\" xmlns:omapix=\"{NAMESPACE}\">\n<stack>\n"
    );
    write_stack(&mut xml, doc, &encoded, None);
    xml.push_str("</stack>\n");
    for (c, ((src, _, (x, y)), fill)) in doc.channels.iter().zip(&channels) {
        xml.push_str(&format!(
            "<omapix:channel name=\"{}\" src=\"{src}\" x=\"{x}\" y=\"{y}\" fill=\"{fill}\"/>\n",
            xml_escape(&c.name)
        ));
    }
    xml.push_str("</image>\n");

    // Write to a temporary file and rename, so a failed save never
    // destroys the previous version.
    let tmp = path.with_extension("ora.tmp");
    let file = File::create(&tmp).map_err(|e| io_error(&tmp, e))?;
    let mut zip = ZipWriter::new(BufWriter::new(file));
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let write =
        |zip: &mut ZipWriter<BufWriter<File>>, name: &str, data: &[u8], opts| -> Result<()> {
            zip.start_file(name, opts).map_err(zip_error)?;
            zip.write_all(data).map_err(|e| io_error(path, e))
        };
    // The spec requires the uncompressed mimetype entry first.
    write(&mut zip, "mimetype", b"image/openraster", stored)?;
    write(&mut zip, "stack.xml", xml.as_bytes(), deflated)?;
    for (i, (enc, layer)) in encoded.iter().zip(&doc.layers).enumerate() {
        match &enc.layer_png {
            Some((name, png, _)) => write(&mut zip, name, png, stored)?,
            None if layer.is_group => {}
            // Empty layers still need a file: one transparent pixel.
            None => write(
                &mut zip,
                &format!("data/layer{i}.png"),
                &encode_png(&[0; 4], 1, 1, 4, icc)?,
                stored,
            )?,
        }
        if let Some(((name, png, _), _)) = &enc.mask_png {
            write(&mut zip, name, png, stored)?;
        }
    }
    for ((name, png, _), _) in &channels {
        write(&mut zip, name, png, stored)?;
    }
    write(&mut zip, "mergedimage.png", &merged_png, stored)?;
    write(&mut zip, "Thumbnails/thumbnail.png", &thumbnail, stored)?;
    let mut inner = zip.finish().map_err(zip_error)?;
    inner.flush().map_err(|e| io_error(&tmp, e))?;
    drop(inner);
    std::fs::rename(&tmp, path).map_err(|e| io_error(path, e))
}

/// An 8-bit sRGB preview, at most 256 px on its longest side.
fn thumbnail_png(merged: &crate::Raster, profile: &ColorProfile) -> Result<Vec<u8>> {
    let (w, h) = (merged.width(), merged.height());
    let scale = 256.0 / w.max(h) as f32;
    let (tw, th) = (
        ((w as f32 * scale) as u32).max(1),
        ((h as f32 * scale) as u32).max(1),
    );
    let small: Vec<Pixel> = (0..tw * th)
        .map(|i| {
            let (x, y) = (i % tw, i / tw);
            merged.get(
                ((x as f32 / scale) as u32).min(w - 1),
                ((y as f32 / scale) as u32).min(h - 1),
            )
        })
        .collect();
    let transform = crate::DisplayTransform::to_srgb(profile)?;
    let mut rgba = vec![[0u8; 4]; small.len()];
    transform.convert(&small, &mut rgba);
    let bytes: Vec<u8> = rgba.into_iter().flatten().collect();
    let mut out = Vec::new();
    PngEncoder::new(&mut out).write_image(&bytes, tw, th, image::ExtendedColorType::Rgba8)?;
    Ok(out)
}

struct LayerEntry {
    name: String,
    /// The pixels' PNG; `None` for a group.
    src: Option<String>,
    x: u32,
    y: u32,
    opacity: f32,
    visible: bool,
    blend: BlendMode,
    mask: Option<(String, u32, u32, u16, bool)>,
    adjustment: Option<crate::adjust::Adjustment>,
    blend_if: Option<crate::layer::BlendIf>,
    clipped: bool,
    locks: Locks,
    /// The entry of the group it's in.
    parent: Option<usize>,
}

/// An alpha channel in stack.xml: name, PNG, position and fill.
type ChannelEntry = (String, String, (u32, u32), u16);

/// The layers in stack.xml, top first, groups before what's in them, and
/// the alpha channels.
fn parse_stack(xml: &str) -> Result<(u32, u32, Vec<LayerEntry>, Vec<ChannelEntry>)> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let (mut w, mut h) = (0, 0);
    let mut layers = Vec::new();
    let mut channels = Vec::new();
    // The stacks we're inside: `None` for the image's own stack, or the
    // entry of a group.
    let mut open: Vec<Option<usize>> = Vec::new();
    loop {
        let event = reader
            .read_event()
            .map_err(|e| Error::Unsupported(format!("stack.xml: {e}")))?;
        let (e, has_children) = match event {
            Event::Start(e) => (e, true),
            Event::Empty(e) => (e, false),
            Event::End(e) if e.name().0 == "stack" => {
                open.pop();
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };
        let mut attrs = std::collections::HashMap::new();
        for a in e.attributes().flatten() {
            let key = a.key.0.to_owned();
            let value = a
                .normalized_value(XmlVersion::Implicit1_0)
                .map(|v| v.into_owned())
                .unwrap_or_default();
            attrs.insert(key, value);
        }
        let num = |k: &str| attrs.get(k).and_then(|v| v.parse::<f64>().ok());
        let is_group = match e.name().0 {
            "image" => {
                w = num("w").unwrap_or(0.0) as u32;
                h = num("h").unwrap_or(0.0) as u32;
                continue;
            }
            "stack" if open.is_empty() => {
                open.push(None);
                continue;
            }
            "stack" => true,
            "layer" => false,
            "omapix:channel" => {
                channels.push((
                    attrs.get("name").cloned().unwrap_or_default(),
                    attrs.get("src").cloned().unwrap_or_default(),
                    (num("x").unwrap_or(0.0) as u32, num("y").unwrap_or(0.0) as u32),
                    num("fill").unwrap_or(0.0) as u16,
                ));
                continue;
            }
            _ => continue,
        };
        let mask = attrs.get("omapix:mask").map(|src| {
            (
                src.clone(),
                num("omapix:mask-x").unwrap_or(0.0) as u32,
                num("omapix:mask-y").unwrap_or(0.0) as u32,
                num("omapix:mask-fill").unwrap_or(65535.0) as u16,
                attrs
                    .get("omapix:mask-enabled")
                    .is_none_or(|v| v != "false"),
            )
        });
        let blend = attrs
            .get("composite-op")
            .and_then(|op| BlendMode::from_ora_name(op));
        // A group that isn't isolated and has no mode of its own passes
        // through.
        let isolated = attrs.get("isolation").is_some_and(|v| v == "isolate");
        let blend = match blend {
            None | Some(BlendMode::Normal) if is_group && !isolated => BlendMode::PassThrough,
            _ => blend.unwrap_or_default(),
        };
        layers.push(LayerEntry {
            name: attrs.get("name").cloned().unwrap_or_else(|| {
                if is_group { "Group" } else { "Layer" }.into()
            }),
            src: (!is_group).then(|| attrs.get("src").cloned().unwrap_or_default()),
            x: num("x").unwrap_or(0.0).max(0.0) as u32,
            y: num("y").unwrap_or(0.0).max(0.0) as u32,
            opacity: num("opacity").unwrap_or(1.0) as f32,
            visible: attrs.get("visibility").is_none_or(|v| v != "hidden"),
            blend,
            mask,
            adjustment: attrs
                .get("omapix:adjustment")
                .and_then(|json| crate::adjust::Adjustment::from_json(json)),
            blend_if: attrs
                .get("omapix:blend-if")
                .and_then(|json| serde_json::from_str(json).ok()),
            clipped: attrs.get("omapix:clipped").is_some_and(|v| v == "true"),
            locks: Locks {
                transparency: attrs.get("omapix:lock-alpha").is_some_and(|v| v == "true"),
                pixels: attrs.get("omapix:lock-pixels").is_some_and(|v| v == "true"),
                position: attrs.get("omapix:lock-position").is_some_and(|v| v == "true"),
                all: attrs.get("omapix:lock-all").is_some_and(|v| v == "true"),
            },
            parent: open.last().copied().flatten(),
        });
        if is_group && has_children {
            open.push(Some(layers.len() - 1));
        }
    }
    if w == 0 || h == 0 {
        return Err(Error::Unsupported("stack.xml has no image size".into()));
    }
    Ok((w, h, layers, channels))
}

fn read_entry<R: Read + Seek>(zip: &mut ZipArchive<R>, name: &str) -> Result<Vec<u8>> {
    let mut entry = zip.by_name(name).map_err(zip_error)?;
    let mut data = Vec::new();
    entry
        .read_to_end(&mut data)
        .map_err(|e| Error::Unsupported(format!("{name}: {e}")))?;
    Ok(data)
}

/// A decoded PNG: width, height, 16-bit samples, embedded ICC profile.
type DecodedPng = (u32, u32, Vec<u16>, Option<Vec<u8>>);

/// Decode a PNG to 16-bit samples: RGBA if `rgba`, otherwise grey.
fn decode_png(data: &[u8], rgba: bool) -> Result<DecodedPng> {
    let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(data))?;
    let icc = decoder.icc_profile()?;
    let image = image::DynamicImage::from_decoder(decoder)?;
    let (w, h) = (image.width(), image.height());
    let samples = if rgba {
        image.into_rgba16().into_raw()
    } else {
        image.into_luma16().into_raw()
    };
    Ok((w, h, samples, icc))
}

/// Decode a grey PNG written by [`encode_grey`] at `(x, y)` into a
/// `width` × `height` plane, `fill` elsewhere.
fn decode_grey(png: &[u8], (x, y): (u32, u32), fill: u16, width: u32, height: u32) -> Result<Tiled<u16>> {
    let (w, h, samples, _) = decode_png(png, false)?;
    let area = (x, y, w.min(width.saturating_sub(x)), h.min(height.saturating_sub(y)));
    let clipped: Vec<u16> = (0..area.3)
        .flat_map(|row| samples[(row * w) as usize..(row * w + area.2) as usize].iter().copied())
        .collect();
    Ok(uncrop(width, height, fill, area, &clipped))
}

/// Open an OpenRaster file.
pub fn load(path: &Path) -> Result<Document> {
    let file = File::open(path).map_err(|e| io_error(path, e))?;
    let mut zip = ZipArchive::new(BufReader::new(file)).map_err(zip_error)?;
    let xml = String::from_utf8_lossy(&read_entry(&mut zip, "stack.xml")?).into_owned();
    let (width, height, entries, channel_entries) = parse_stack(&xml)?;

    // Read compressed data sequentially, then decode in parallel.
    let mut files = Vec::new();
    for e in &entries {
        let pixels = match &e.src {
            Some(src) => read_entry(&mut zip, src)?,
            None => Vec::new(),
        };
        let mask = match &e.mask {
            Some((src, ..)) => Some(read_entry(&mut zip, src)?),
            None => None,
        };
        files.push((pixels, mask));
    }

    let decoded: Vec<(Layer, Option<Vec<u8>>)> = entries
        .par_iter()
        .zip(files.par_iter())
        .enumerate()
        .map(
            |(i, (e, (png, mask_png)))| -> Result<(Layer, Option<Vec<u8>>)> {
                // Stack lists top first; ids count from the bottom.
                let id_of = |entry: usize| (entries.len() - entry) as u64;
                let (tiled, icc) = if e.src.is_some() {
                    let (w, h, samples, icc) = decode_png(png, true)?;
                    let pixels: Vec<Pixel> = samples.as_chunks::<4>().0.to_vec();
                    // Layers may be placed partly outside the canvas; clip them.
                    let area = (
                        e.x.min(width),
                        e.y.min(height),
                        w.min(width.saturating_sub(e.x)),
                        h.min(height.saturating_sub(e.y)),
                    );
                    let clipped: Vec<Pixel> = (0..area.3)
                        .flat_map(|row| {
                            pixels[(row * w) as usize..(row * w + area.2) as usize]
                                .iter()
                                .copied()
                        })
                        .collect();
                    (uncrop(width, height, [0; 4], area, &clipped), icc)
                } else {
                    (Tiled::new(width, height, [0; 4]), None)
                };
                let mut layer = Layer::from_pixels(id_of(i), e.name.clone(), tiled);
                layer.is_group = e.src.is_none();
                layer.parent = e.parent.map(id_of);
                layer.opacity = e.opacity.clamp(0.0, 1.0);
                layer.visible = e.visible;
                layer.blend = e.blend;
                layer.adjustment = e.adjustment.clone();
                layer.blend_if = e.blend_if;
                layer.clipped = e.clipped;
                layer.locks = e.locks;
                if let (Some((_, mx, my, fill, enabled)), Some(mask_png)) = (&e.mask, mask_png) {
                    layer.mask = Some(Mask {
                        pixels: decode_grey(mask_png, (*mx, *my), *fill, width, height)?,
                        enabled: *enabled,
                    });
                }
                Ok((layer, icc))
            },
        )
        .collect::<Result<_>>()?;

    let icc = decoded.iter().find_map(|(_, icc)| icc.clone());
    let profile = match icc {
        Some(icc) => ColorProfile::from_icc(icc)?,
        None => ColorProfile::srgb(),
    };
    let layers: Vec<Layer> = decoded.into_iter().rev().map(|(l, _)| l).collect();
    let mut doc = Document::new(path.to_path_buf(), profile, 16, width, height, layers);
    doc.saved_path = Some(path.to_path_buf());
    for (name, src, at, fill) in channel_entries {
        let pixels = decode_grey(&read_entry(&mut zip, &src)?, at, fill, width, height)?;
        let id = doc.next_layer_id();
        doc.channels.push(AlphaChannel { id, name, pixels });
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Raster;
    use crate::ops;

    #[test]
    fn round_trips_layers_modes_and_masks() {
        let (w, h) = (600, 300);
        let px: Vec<Pixel> = (0..w * h)
            .map(|i| [(i % 65000) as u16, (i / 3 % 65000) as u16, 1234, 65535])
            .collect();
        let mut doc = Document::from_image(
            "in.tif".into(),
            &Raster::new(w, h, px),
            ColorProfile::srgb(),
            16,
        );
        ops::frequency_separation(&mut doc, 0, 3.0);
        ops::dodge_and_burn_layer(&mut doc, 2);
        doc.layers[1].opacity = 0.5;
        doc.layers[1].blend_if = Some(crate::layer::BlendIf {
            this: [0.1, 0.2, 0.8, 0.95],
            ..Default::default()
        });
        doc.layers[2].visible = false;
        doc.layers[2].name = "Tex & <stuff>".into();
        let mut mask = Mask::white(w, h);
        mask.pixels.tile_mut(1, 0)[0] = 1000;
        mask.enabled = false;
        doc.layers[3].mask = Some(mask);
        let id = doc.next_layer_id();
        let mut curves = crate::adjust::Curves::default();
        curves.master.points.insert(1, (0.4, 0.5));
        doc.layers.push(Layer::adjustment(
            id,
            crate::adjust::Adjustment::Curves(curves),
            w,
            h,
        ));
        doc.layers.last_mut().unwrap().clipped = true;
        doc.layers[0].locks.transparency = true;
        doc.layers[1].locks.pixels = true;
        doc.layers[2].locks.position = true;
        doc.layers[3].locks.all = true;
        // A small, empty-ish layer to exercise cropping and empty layers.
        let id = doc.next_layer_id();
        doc.layers.push(Layer::empty(id, "Empty", w, h));
        // Two alpha channels: a rectangle, and one that's all selected.
        doc.selection = Some(crate::Selection::rectangle(w, h, (300.0, 10.0), (420.0, 290.0)));
        doc.save_selection();
        doc.selection = Some(crate::Selection::all(w, h));
        doc.save_selection();
        doc.channels[1].name = "Sky & <sea>".into();

        let dir = std::env::temp_dir().join(format!("omapix-ora-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.ora");
        save(&doc, &path).unwrap();
        let back = load(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!((back.width, back.height), (w, h));
        assert_eq!(back.layers.len(), doc.layers.len());
        for (a, b) in doc.layers.iter().zip(&back.layers) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.blend, b.blend);
            assert_eq!(a.visible, b.visible);
            assert!((a.opacity - b.opacity).abs() < 1e-3);
            assert_eq!(a.pixels.to_vec(), b.pixels.to_vec(), "pixels of {}", a.name);
            assert_eq!(a.mask.is_some(), b.mask.is_some());
            assert_eq!(a.adjustment, b.adjustment);
            assert_eq!(a.blend_if, b.blend_if);
            assert_eq!(a.locks, b.locks);
            assert_eq!(a.clipped, b.clipped);
            if let (Some(ma), Some(mb)) = (&a.mask, &b.mask) {
                assert_eq!(ma.enabled, mb.enabled);
                assert_eq!(ma.pixels.to_vec(), mb.pixels.to_vec());
            }
        }
        assert_eq!(doc.composite().pixels(), back.composite().pixels());
        assert_eq!(back.channels.len(), 2);
        for (a, b) in doc.channels.iter().zip(&back.channels) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.pixels.to_vec(), b.pixels.to_vec(), "channel {}", a.name);
        }
    }

    #[test]
    fn round_trips_nested_groups() {
        let (w, h) = (300, 200);
        let px: Vec<Pixel> = (0..w * h)
            .map(|i| [(i % 65000) as u16, 20000, (i / 7 % 65000) as u16, 65535])
            .collect();
        let mut doc = Document::from_image(
            "in.tif".into(),
            &Raster::new(w, h, px),
            ColorProfile::srgb(),
            16,
        );
        // Background, then an isolated Multiply group holding a grey layer
        // and a Curves layer, inside a pass-through group at 60 % with a
        // mask, then an empty group on top.
        ops::dodge_and_burn_layer(&mut doc, 0);
        let inner = doc.group_layer(1);
        let mut curves = crate::adjust::Curves::default();
        curves.master.points.insert(1, (0.3, 0.6));
        let id = doc.next_layer_id();
        let adjustment = crate::adjust::Adjustment::Curves(curves);
        doc.insert_above(1, Layer::adjustment(id, adjustment, w, h));
        let outer = doc.group_layer(doc.index_of(inner).unwrap());
        let inner = doc.layer_mut(inner).unwrap();
        inner.blend = BlendMode::Multiply;
        inner.name = "Inner & <co>".into();
        let outer = doc.layer_mut(outer).unwrap();
        outer.opacity = 0.6;
        let mut mask = Mask::white(w, h);
        mask.pixels.tile_mut(0, 0)[10] = 0;
        outer.mask = Some(mask);
        let top = doc.layers.len() - 1;
        doc.new_group(top);
        doc.layers.last_mut().unwrap().visible = false;

        let dir = std::env::temp_dir().join(format!("omapix-ora-groups-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("groups.ora");
        save(&doc, &path).unwrap();
        let back = load(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        let parent_name = |d: &Document, l: &Layer| {
            l.parent.and_then(|p| d.layer(p)).map(|p| p.name.clone())
        };
        assert_eq!(back.layers.len(), doc.layers.len());
        for (a, b) in doc.layers.iter().zip(&back.layers) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.is_group, b.is_group, "{}", a.name);
            assert_eq!(a.blend, b.blend, "{}", a.name);
            assert_eq!(a.visible, b.visible);
            assert_eq!(parent_name(&doc, a), parent_name(&back, b), "{}", a.name);
            assert_eq!(a.mask.is_some(), b.mask.is_some());
            assert_eq!(a.adjustment, b.adjustment);
        }
        assert_eq!(doc.composite().pixels(), back.composite().pixels());
    }

    #[test]
    fn reads_groups_written_by_other_apps() {
        let xml = r#"<image w="10" h="10"><stack>
            <stack name="Folder" composite-op="svg:src-over">
                <layer name="Inside" src="data/1.png"/>
                <stack name="Isolated" isolation="isolate"/>
                <stack name="Screen" composite-op="svg:screen"></stack>
            </stack>
            <layer name="Bottom" src="data/2.png"/>
        </stack></image>"#;
        let (_, _, entries, _) = parse_stack(xml).unwrap();
        let summary: Vec<_> = entries
            .iter()
            .map(|e| (e.name.as_str(), e.src.is_none(), e.blend, e.parent))
            .collect();
        assert_eq!(
            summary,
            [
                ("Folder", true, BlendMode::PassThrough, None),
                ("Inside", false, BlendMode::Normal, Some(0)),
                ("Isolated", true, BlendMode::Normal, Some(0)),
                ("Screen", true, BlendMode::Screen, Some(0)),
                ("Bottom", false, BlendMode::Normal, None),
            ]
        );
    }
}
