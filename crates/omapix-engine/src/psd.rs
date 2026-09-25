//! Photoshop (.psd) import, for old Photoshop work.
//!
//! Reads 8- and 16-bit RGB and greyscale files, with their ICC profile:
//! pixel layers (name, position, opacity, visibility, blend mode, clipping
//! and layer mask), and groups. Layers of 16-bit files are kept in an
//! `Lr16` block rather than the layer section, and are usually
//! zip-compressed with prediction; both are handled. A file saved without
//! layers opens as its flattened image.
//!
//! Adjustment and fill layers, text, effects and vector masks aren't read.
//! When a file has layers Omapix can't show, Photoshop's flattened image is
//! added as a hidden top layer, "Photoshop composite", so nothing is lost,
//! unless it's blank: without Maximize Compatibility, Photoshop saves a
//! white one.
//! 32-bit files, CMYK and Lab, and large-document .psb files aren't
//! supported.

use std::io::Read;
use std::path::Path;

use rayon::prelude::*;

use crate::blend::BlendMode;
use crate::layer::{Layer, Mask};
use crate::ora::uncrop;
use crate::raster::{Pixel, Raster, widen};
use crate::{ColorProfile, Document, Error, Result};

fn bad(what: impl std::fmt::Display) -> Error {
    Error::Unsupported(format!("PSD: {what}"))
}

/// Reads big-endian values from a byte slice.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&end| end <= self.data.len());
        let bytes = &self.data[self.pos..end.ok_or_else(|| bad("the file is truncated"))?];
        self.pos += n;
        Ok(bytes)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into().expect("N bytes"))
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        self.array().map(u16::from_be_bytes)
    }

    fn i16(&mut self) -> Result<i16> {
        self.array().map(i16::from_be_bytes)
    }

    fn u32(&mut self) -> Result<u32> {
        self.array().map(u32::from_be_bytes)
    }

    fn i32(&mut self) -> Result<i32> {
        self.array().map(i32::from_be_bytes)
    }

    /// A block preceded by its 32-bit length.
    fn section(&mut self) -> Result<&'a [u8]> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    fn rest(&self) -> &'a [u8] {
        &self.data[self.pos.min(self.data.len())..]
    }

    fn done(&self) -> bool {
        self.pos >= self.data.len()
    }
}

/// Tagged blocks (`8BIM` + key + length + data), as `(key, data)`.
fn tagged_blocks(data: &[u8]) -> Result<Vec<([u8; 4], &[u8])>> {
    let mut r = Reader::new(data);
    let mut blocks = Vec::new();
    while data.len() - r.pos.min(data.len()) >= 12 {
        let signature = r.array::<4>()?;
        if &signature != b"8BIM" && &signature != b"8B64" {
            break;
        }
        let key = r.array()?;
        blocks.push((key, r.section()?));
        // Blocks are padded to 4 bytes.
        r.pos = r.pos.next_multiple_of(4);
    }
    Ok(blocks)
}

/// A rectangle as top, left, bottom, right.
type Rect = (i32, i32, i32, i32);

fn read_rect(r: &mut Reader) -> Result<Rect> {
    Ok((r.i32()?, r.i32()?, r.i32()?, r.i32()?))
}

fn rect_size((top, left, bottom, right): Rect) -> (usize, usize) {
    ((right - left).max(0) as usize, (bottom - top).max(0) as usize)
}

/// Keys of adjustment and fill layers, which Omapix doesn't read.
const ADJUSTMENTS: [&[u8; 4]; 20] = [
    b"levl", b"curv", b"brit", b"blnc", b"hue ", b"hue2", b"expA", b"vibA", b"selc", b"mixr",
    b"clrL", b"grdm", b"phfl", b"nvrt", b"post", b"thrs", b"blwh", b"SoCo", b"GdFl", b"PtFl",
];

/// One layer record, and its channels' compressed data.
struct Record<'a> {
    name: String,
    rect: Rect,
    /// Channel id (0, 1, 2 colour, -1 transparency, -2 mask) and data.
    channels: Vec<(i16, &'a [u8])>,
    blend: [u8; 4],
    opacity: u8,
    clipped: bool,
    hidden: bool,
    /// The mask's rectangle, the value outside it, and whether it's on.
    mask: Option<(Rect, u8, bool)>,
    /// 1 or 2 is a group's own record, 3 the divider that closes it.
    section: u32,
    adjustment: bool,
}

/// The layer records in a layer info block, bottom first.
fn records(info: &[u8]) -> Result<Vec<Record<'_>>> {
    let mut r = Reader::new(info);
    // Negative when the first alpha channel is the flattened transparency.
    let count = r.i16()?.unsigned_abs() as usize;
    let mut out = Vec::with_capacity(count);
    let mut lengths = Vec::with_capacity(count);
    for _ in 0..count {
        let rect = read_rect(&mut r)?;
        let channels: Vec<(i16, usize)> = (0..r.u16()?)
            .map(|_| Ok((r.i16()?, r.u32()? as usize)))
            .collect::<Result<_>>()?;
        if r.take(4)? != b"8BIM" {
            return Err(bad("a layer record is damaged"));
        }
        let blend = r.array()?;
        let opacity = r.u8()?;
        let clipped = r.u8()? == 1;
        let flags = r.u8()?;
        r.u8()?;
        let mut extra = Reader::new(r.section()?);
        let mask_data = extra.section()?;
        let mask = if mask_data.len() >= 18 {
            let mut m = Reader::new(mask_data);
            Some((read_rect(&mut m)?, m.u8()?, m.u8()? & 2 == 0))
        } else {
            None
        };
        extra.section()?; // blending ranges
        // A Pascal string padded to 4 bytes, replaced by `luni` if there.
        let name_start = extra.pos;
        let name_len = extra.u8()? as usize;
        let mut name = String::from_utf8_lossy(extra.take(name_len)?).into_owned();
        extra.pos = name_start + (1 + name_len).next_multiple_of(4);
        let (mut section, mut adjustment) = (0, false);
        for (key, data) in tagged_blocks(extra.rest())? {
            let mut d = Reader::new(data);
            match &key {
                b"luni" => {
                    let units: Vec<u16> = (0..d.u32()?).map(|_| d.u16()).collect::<Result<_>>()?;
                    name = String::from_utf16_lossy(&units);
                }
                b"lsct" => section = d.u32()?,
                key if ADJUSTMENTS.contains(&key) => adjustment = true,
                _ => {}
            }
        }
        lengths.push(channels);
        out.push(Record {
            name,
            rect,
            channels: Vec::new(),
            blend,
            opacity,
            clipped,
            hidden: flags & 2 != 0,
            mask,
            section,
            adjustment,
        });
    }
    // The channels' data follows all the records, in the same order.
    for (record, lengths) in out.iter_mut().zip(lengths) {
        for (id, len) in lengths {
            record.channels.push((id, r.take(len)?));
        }
    }
    Ok(out)
}

/// PackBits run-length decoding, until `len` bytes are out.
fn unpack_bits(data: &[u8], len: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(len);
    let mut r = Reader::new(data);
    while out.len() < len && !r.done() {
        match r.u8()? as i8 {
            n @ 0.. => out.extend_from_slice(r.take(n as usize + 1)?),
            -128 => {}
            n => {
                let byte = r.u8()?;
                out.extend(std::iter::repeat_n(byte, (1 - n as isize) as usize));
            }
        }
    }
    if out.len() < len {
        return Err(bad("the image data is truncated"));
    }
    out.truncate(len);
    Ok(out)
}

/// Undo zip prediction: each value was stored as the difference from the
/// one before it in the row.
fn add_up<T: Copy>(values: &mut [T], w: usize, add: impl Fn(T, T) -> T) {
    for row in values.chunks_mut(w.max(1)) {
        for i in 1..row.len() {
            row[i] = add(row[i], row[i - 1]);
        }
    }
}

/// A channel's `w` × `h` samples at 16 bits. `data` is what follows the
/// compression method; for RLE, `rows` row lengths come first.
fn samples(compression: u16, data: &[u8], (w, h): (usize, usize), depth: u16, rows: usize) -> Result<Vec<u16>> {
    let len = w * h * depth as usize / 8;
    let mut bytes = match compression {
        0 => data.get(..len).ok_or_else(|| bad("the image data is truncated"))?.to_vec(),
        1 => unpack_bits(data.get(rows * 2..).unwrap_or_default(), len)?,
        2 | 3 => {
            let mut out = Vec::with_capacity(len);
            flate2::read::ZlibDecoder::new(data)
                .read_to_end(&mut out)
                .map_err(|e| bad(format!("zip data: {e}")))?;
            out
        }
        _ => return Err(bad(format!("compression method {compression} isn't supported"))),
    };
    if bytes.len() < len {
        return Err(bad("the image data is truncated"));
    }
    bytes.truncate(len);
    if depth == 8 {
        if compression == 3 {
            add_up(&mut bytes, w, u8::wrapping_add);
        }
        return Ok(bytes.into_iter().map(widen).collect());
    }
    let mut values: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|b| u16::from_be_bytes(*b)).collect();
    if compression == 3 {
        add_up(&mut values, w, u16::wrapping_add);
    }
    Ok(values)
}

/// A layer channel's samples over `rect`.
fn channel_samples(data: &[u8], rect: Rect, depth: u16) -> Result<Vec<u16>> {
    let compression = Reader::new(data).u16()?;
    let size = rect_size(rect);
    samples(compression, &data[2..], size, depth, size.1)
}

/// The part of `rect` inside the canvas as (x, y, w, h), and `values`
/// (over `rect`) cropped to it.
fn crop<T: Copy>(rect: Rect, values: &[T], width: u32, height: u32) -> ((u32, u32, u32, u32), Vec<T>) {
    let (top, left, bottom, right) = rect;
    let (x0, y0) = (left.clamp(0, width as i32), top.clamp(0, height as i32));
    let (x1, y1) = (right.clamp(x0, width as i32), bottom.clamp(y0, height as i32));
    let rw = rect_size(rect).0;
    let cropped = (y0..y1)
        .flat_map(|y| {
            let row = (y - top) as usize * rw;
            (x0..x1).map(move |x| values[row + (x - left) as usize])
        })
        .collect();
    ((x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32), cropped)
}

/// A pixel layer, with its mask, from its record.
fn pixel_layer(id: u64, rec: &Record, colours: usize, depth: u16, (width, height): (u32, u32)) -> Result<Layer> {
    let channel = |id: i16, rect: Rect| -> Result<Option<Vec<u16>>> {
        let data = rec.channels.iter().find(|(c, _)| *c == id);
        data.map(|(_, data)| channel_samples(data, rect, depth)).transpose()
    };
    let colour: Vec<Vec<u16>> = (0..colours as i16)
        .map(|c| channel(c, rec.rect)?.ok_or_else(|| bad("a layer is missing a colour channel")))
        .collect::<Result<_>>()?;
    let alpha = channel(-1, rec.rect)?;
    let (w, h) = rect_size(rec.rect);
    let pixels: Vec<Pixel> = (0..w * h)
        .map(|i| {
            let a = alpha.as_ref().map_or(u16::MAX, |a| a[i]);
            let c = |k: usize| colour[k.min(colours - 1)][i];
            [c(0), c(1), c(2), a]
        })
        .collect();
    let (area, cropped) = crop(rec.rect, &pixels, width, height);
    let mut layer = Layer::from_pixels(id, rec.name.clone(), uncrop(width, height, [0; 4], area, &cropped));
    if let Some((rect, outside, enabled)) = rec.mask
        && let Some(values) = channel(-2, rect)?
    {
        let (area, cropped) = crop(rect, &values, width, height);
        layer.mask = Some(Mask {
            pixels: uncrop(width, height, widen(outside), area, &cropped),
            enabled,
        });
    }
    Ok(layer)
}

fn blend_mode(key: &[u8; 4]) -> BlendMode {
    match key {
        b"pass" => BlendMode::PassThrough,
        b"dark" => BlendMode::Darken,
        b"mul " => BlendMode::Multiply,
        b"idiv" => BlendMode::ColorBurn,
        b"lbrn" => BlendMode::LinearBurn,
        b"lite" => BlendMode::Lighten,
        b"scrn" => BlendMode::Screen,
        b"div " => BlendMode::ColorDodge,
        b"lddg" => BlendMode::LinearDodge,
        b"over" => BlendMode::Overlay,
        b"sLit" => BlendMode::SoftLight,
        b"hLit" => BlendMode::HardLight,
        b"vLit" => BlendMode::VividLight,
        b"lLit" => BlendMode::LinearLight,
        b"pLit" => BlendMode::PinLight,
        b"diff" => BlendMode::Difference,
        b"smud" => BlendMode::Exclusion,
        b"fsub" => BlendMode::Subtract,
        b"fdiv" => BlendMode::Divide,
        b"hue " => BlendMode::Hue,
        b"sat " => BlendMode::Saturation,
        b"colr" => BlendMode::Color,
        b"lum " => BlendMode::Luminosity,
        // Normal, and Dissolve, Darker/Lighter Color and Hard Mix, which
        // Omapix doesn't have.
        _ => BlendMode::Normal,
    }
}

/// The document's layers, bottom first, and whether any were left out.
fn layers(records: &[Record], colours: usize, depth: u16, size: (u32, u32)) -> Result<(Vec<Layer>, bool)> {
    // Pixel layers decode in parallel; groups and adjustments have none.
    let decoded: Vec<Option<Layer>> = records
        .par_iter()
        .enumerate()
        .map(|(i, rec)| {
            let plain = rec.section == 0 && !rec.adjustment;
            plain.then(|| pixel_layer(i as u64 + 1, rec, colours, depth, size)).transpose()
        })
        .collect::<Result<_>>()?;
    let mut out = Vec::new();
    let mut skipped = false;
    // Records run bottom first, so a group's divider comes before what's in
    // it, and the group's own record after.
    let mut open: Vec<u64> = Vec::new();
    for (i, (rec, layer)) in records.iter().zip(decoded).enumerate() {
        let mut layer = match (rec.section, layer) {
            (3, _) => {
                open.push(i as u64 + 1);
                continue;
            }
            (1 | 2, _) => {
                let id = open.pop().unwrap_or(i as u64 + 1);
                let mut group = Layer::group(id, rec.name.clone(), size.0, size.1);
                group.blend = blend_mode(&rec.blend);
                group
            }
            (_, Some(mut layer)) => {
                layer.blend = match blend_mode(&rec.blend) {
                    BlendMode::PassThrough => BlendMode::Normal,
                    mode => mode,
                };
                layer.clipped = rec.clipped;
                layer
            }
            (_, None) => {
                skipped = true;
                continue;
            }
        };
        layer.opacity = f32::from(rec.opacity) / 255.0;
        layer.visible = !rec.hidden;
        layer.parent = open.last().copied();
        out.push(layer);
    }
    Ok((out, skipped))
}

/// The flattened image stored after the layers.
fn composite(data: &[u8], channels: usize, colours: usize, depth: u16, (w, h): (usize, usize)) -> Result<Raster> {
    let mut r = Reader::new(data);
    let compression = r.u16()?;
    let rest = r.rest();
    let plane = |c: usize| -> Result<Vec<u16>> {
        let start = match compression {
            // Raw planes, one after another.
            0 => c * w * h * depth as usize / 8,
            // Every plane's row lengths, then the planes.
            _ => {
                let lengths = rest.get(..channels * h * 2).ok_or_else(|| bad("the image data is truncated"))?;
                let before = lengths[..c * h * 2].as_chunks::<2>().0.iter();
                channels * h * 2 + before.map(|b| u16::from_be_bytes(*b) as usize).sum::<usize>()
            }
        };
        samples(compression, rest.get(start..).unwrap_or_default(), (w, h), depth, 0)
    };
    let planes: Vec<Vec<u16>> = (0..colours).into_par_iter().map(plane).collect::<Result<_>>()?;
    let pixels = (0..w * h)
        .map(|i| {
            let c = |k: usize| planes[k.min(colours - 1)][i];
            [c(0), c(1), c(2), u16::MAX]
        })
        .collect();
    Ok(Raster::new(w as u32, h as u32, pixels))
}

/// Open a Photoshop file.
pub fn load(path: &Path) -> Result<Document> {
    let data = std::fs::read(path).map_err(|source| Error::Read {
        path: path.display().to_string(),
        source,
    })?;
    parse(&data, path)
}

fn parse(data: &[u8], path: &Path) -> Result<Document> {
    let mut r = Reader::new(data);
    if r.take(4)? != b"8BPS" {
        return Err(bad("not a Photoshop file"));
    }
    if r.u16()? != 1 {
        return Err(bad("large-document .psb files aren't supported"));
    }
    r.take(6)?;
    let channels = r.u16()? as usize;
    let (height, width) = (r.u32()?, r.u32()?);
    let depth = r.u16()?;
    let colours = match r.u16()? {
        1 => 1,
        3 => 3,
        _ => return Err(bad("only RGB and greyscale files are supported")),
    };
    if depth != 8 && depth != 16 {
        return Err(bad(format!("{depth}-bit files aren't supported")));
    }
    r.section()?; // colour mode data
    let mut profile = ColorProfile::srgb();
    let mut resources = Reader::new(r.section()?);
    while !resources.done() {
        resources.take(4)?; // 8BIM
        let id = resources.u16()?;
        // A Pascal name and the data, each padded to an even length.
        let name_len = resources.u8()? as usize;
        resources.take((1 + name_len).next_multiple_of(2) - 1)?;
        let data = resources.section()?;
        resources.take(data.len() % 2)?;
        if id == 1039 {
            profile = ColorProfile::from_icc(data.to_vec())?;
        }
    }
    let mut section = Reader::new(r.section()?);
    let mut info = if section.done() { &[][..] } else { section.section()? };
    // 16- and 32-bit files keep their layers in a tagged block after the
    // global mask info instead.
    if info.is_empty() && !section.done() {
        section.section()?;
        let blocks = tagged_blocks(section.rest())?;
        if let Some((_, data)) = blocks.into_iter().find(|(key, _)| key == b"Lr16" || key == b"Lr32") {
            info = data;
        }
    }
    let size = (width, height);
    let flat = composite(r.rest(), channels, colours, depth, (width as usize, height as usize))?;
    let (mut layers, skipped) = if info.is_empty() {
        (Vec::new(), false)
    } else {
        layers(&records(info)?, colours, depth, size)?
    };
    let blank = flat.pixels().iter().all(|p| *p == [u16::MAX; 4]);
    if layers.is_empty() || skipped && !blank {
        let id = layers.iter().map(|l| l.id).max().unwrap_or(0) + 1;
        let name = if layers.is_empty() { "Background" } else { "Photoshop composite" };
        let mut top = Layer::from_raster(id, name, &flat);
        top.visible = layers.is_empty();
        layers.push(top);
    }
    // No saved_path, so Ctrl+S asks where rather than overwriting the .psd.
    Ok(Document::new(path.to_path_buf(), profile, depth as u8, width, height, layers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpacks_packbits() {
        // The example from Apple's PackBits technote.
        let packed = [0xFE, 0xAA, 0x02, 0x80, 0x00, 0x2A, 0xFD, 0xAA, 0x03, 0x80, 0x00, 0x2A, 0x22, 0xF7, 0xAA];
        let unpacked = [
            0xAA, 0xAA, 0xAA, 0x80, 0x00, 0x2A, 0xAA, 0xAA, 0xAA, 0xAA, 0x80, 0x00, 0x2A, 0x22, 0xAA, 0xAA,
            0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA,
        ];
        assert_eq!(unpack_bits(&packed, unpacked.len()).unwrap(), unpacked);
    }

    #[test]
    fn a_photoshop_file_opens_in_order_visible_and_blended() {
        // Saved by Photoshop, from the `psd` crate's test files (MIT or
        // Apache-2.0): a 50 % red layer over a 50 % blue one, in Multiply.
        let bytes = include_bytes!("../testdata/blue-red-1x1-multiply.psd");
        let doc = parse(bytes, Path::new("multiply.psd")).unwrap();
        let names: Vec<_> = doc.layers.iter().map(|l| (l.name.as_str(), l.blend, l.visible)).collect();
        assert_eq!(names, [("Bottom Layer", BlendMode::Normal, true), ("Top Layer", BlendMode::Multiply, true)]);
        // What the `psd` crate's own test expects Photoshop to show.
        let pixel = doc.composite().get(0, 0).map(|v| (v as u32 * 255 + 32767) / 65535);
        assert_eq!(pixel, [85, 0, 85, 192]);
        assert_eq!(doc.saved_path, None);
    }

    /// A layer for [`sixteen_bit_file`]: name, rect, blend key, flags,
    /// channels, mask rect and outside value, and `lsct` section type.
    type Spec<'a> = (&'a str, Rect, &'a [u8; 4], u8, Vec<(i16, Vec<u16>)>, Option<(Rect, u8)>, u32);

    fn block(key: &[u8; 4], data: &[u8]) -> Vec<u8> {
        [b"8BIM", &key[..], &(data.len() as u32).to_be_bytes(), data].concat()
    }

    /// A 4 × 2 16-bit RGB file laid out as Photoshop writes one: layers in
    /// an `Lr16` block, zip-compressed with prediction, and a mid-grey
    /// RLE composite.
    fn sixteen_bit_file(specs: &[Spec]) -> Vec<u8> {
        let be = |v: u32| v.to_be_bytes();
        let mut info = (specs.len() as i16).to_be_bytes().to_vec();
        let mut channel_data = Vec::new();
        for (name, rect, blend, flags, channels, mask, section) in specs {
            info.extend([rect.0, rect.1, rect.2, rect.3].map(i32::to_be_bytes).concat());
            info.extend((channels.len() as u16).to_be_bytes());
            for (id, values) in channels {
                let r = if *id == -2 { mask.unwrap().0 } else { *rect };
                let mut deltas = values.clone();
                for row in deltas.chunks_mut(rect_size(r).0) {
                    for i in (1..row.len()).rev() {
                        row[i] = row[i].wrapping_sub(row[i - 1]);
                    }
                }
                let bytes: Vec<u8> = deltas.iter().flat_map(|v| v.to_be_bytes()).collect();
                let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
                std::io::Write::write_all(&mut z, &bytes).unwrap();
                let data = [3u16.to_be_bytes().to_vec(), z.finish().unwrap()].concat();
                info.extend(id.to_be_bytes());
                info.extend(be(data.len() as u32));
                channel_data.extend(data);
            }
            info.extend(b"8BIM");
            info.extend(*blend);
            info.extend([255, 0, *flags, 0]);
            let mut extra = match mask {
                Some((r, outside)) => [&be(20)[..], &[r.0, r.1, r.2, r.3].map(i32::to_be_bytes).concat(), &[*outside, 0, 0, 0]].concat(),
                None => be(0).to_vec(),
            };
            extra.extend(be(0)); // blending ranges
            extra.extend([0, 0, 0, 0]); // empty Pascal name, padded
            let units: Vec<u16> = name.encode_utf16().collect();
            let mut luni = be(units.len() as u32).to_vec();
            luni.extend(units.iter().flat_map(|u| u.to_be_bytes()));
            luni.resize(luni.len().next_multiple_of(4), 0);
            extra.extend(block(b"luni", &luni));
            if *section != 0 {
                extra.extend(block(b"lsct", &be(*section)));
            }
            if name.starts_with("Curves") {
                extra.extend(block(b"curv", &[0; 4]));
            }
            info.extend(be(extra.len() as u32));
            info.extend(extra);
        }
        info.extend(channel_data);
        info.resize(info.len().next_multiple_of(4), 0);
        // No layers in the layer info, then the global mask info.
        let layer_and_mask = [&be(0)[..], &be(0), &block(b"Lr16", &info)].concat();

        let mut out = b"8BPS".to_vec();
        out.extend(1u16.to_be_bytes());
        out.extend([0; 6]);
        out.extend(3u16.to_be_bytes());
        out.extend(be(2)); // height
        out.extend(be(4)); // width
        out.extend(16u16.to_be_bytes());
        out.extend(3u16.to_be_bytes());
        out.extend(be(0));
        out.extend(be(0));
        out.extend(be(layer_and_mask.len() as u32));
        out.extend(layer_and_mask);
        // Each row of each plane is one run: 8 bytes of 0x80.
        out.extend(1u16.to_be_bytes());
        out.extend([0, 2].repeat(6));
        out.extend([0xF9, 0x80].repeat(6));
        out
    }

    #[test]
    fn a_16_bit_file_keeps_its_layers_groups_and_masks() {
        let colours = |r: u16, g: u16, b: u16, n: usize| vec![(0, vec![r; n]), (1, vec![g; n]), (2, vec![b; n])];
        let mut patch = vec![(0, vec![60000, 60001]), (1, vec![1, 2]), (2, vec![3, 4]), (-1, vec![65535, 0])];
        patch.push((-2, vec![0, 1000, 2000, 3000]));
        let file = sixteen_bit_file(&[
            ("Background", (0, 0, 2, 4), b"norm", 0, colours(10000, 20000, 30000, 8), None, 0),
            // A hidden group: the divider, a layer in it, the group's record.
            ("</Layer group>", (0, 0, 0, 0), b"norm", 0, vec![], None, 3),
            ("Patch ✓", (1, 1, 2, 3), b"sLit", 0, patch, Some(((0, 0, 1, 4), 255)), 0),
            ("Dodge", (0, 0, 0, 0), b"pass", 2, vec![], None, 1),
            ("Curves 1", (0, 0, 0, 0), b"norm", 0, vec![], None, 0),
        ]);
        let doc = parse(&file, Path::new("retouch.psd")).unwrap();
        assert_eq!((doc.width, doc.height, doc.source_bits), (4, 2, 16));
        let summary: Vec<_> = doc.layers.iter().map(|l| (l.name.as_str(), l.is_group, l.blend, l.visible)).collect();
        assert_eq!(
            summary,
            [
                ("Background", false, BlendMode::Normal, true),
                ("Patch ✓", false, BlendMode::SoftLight, true),
                ("Dodge", true, BlendMode::PassThrough, false),
                // For the Curves layer, which isn't read.
                ("Photoshop composite", false, BlendMode::Normal, false),
            ]
        );
        let (background, patch, group) = (&doc.layers[0], &doc.layers[1], &doc.layers[2]);
        assert_eq!(background.pixels.get(3, 1), [10000, 20000, 30000, 65535]);
        assert_eq!(patch.parent, Some(group.id));
        assert_eq!(patch.pixels.get(1, 1), [60000, 1, 3, 65535]);
        assert_eq!(patch.pixels.get(2, 1), [60001, 2, 4, 0]);
        assert_eq!(patch.pixels.get(0, 0), [0; 4], "outside the layer");
        // The mask covers the top row, and is white below it.
        let mask = patch.mask.as_ref().unwrap();
        assert!(mask.enabled);
        assert_eq!((mask.pixels.get(1, 0), mask.pixels.get(3, 0), mask.pixels.get(1, 1)), (1000, 3000, 65535));
        assert_eq!(doc.layers[3].pixels.get(0, 0), [0x8080, 0x8080, 0x8080, 65535]);
    }
}
