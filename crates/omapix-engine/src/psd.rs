//! Photoshop (.psd, and .psb for large documents) import and export, for
//! old Photoshop work and for handing layers to other programs.
//!
//! Reads 8- and 16-bit RGB and greyscale files, with their ICC profile and
//! EXIF metadata: pixel layers (name, position, opacity, visibility, blend
//! mode, Blend If, clipping, locks and layer mask), and groups. Layers of
//! 16-bit files are kept in an `Lr16` block rather than the layer section,
//! and are usually zip-compressed with prediction; both are handled. A
//! file saved without layers opens as its flattened image.
//!
//! Adjustment and fill layers, text, effects and vector masks aren't read.
//! When a file has layers Omapix can't show, Photoshop's flattened image is
//! added as a hidden top layer, "Photoshop composite", so nothing is lost,
//! unless it's blank: without Maximize Compatibility, Photoshop saves a
//! white one.
//! 32-bit files, CMYK and Lab aren't supported.
//!
//! [`save`] writes the same, as a 16-bit RGB file with the flattened image
//! (Photoshop's Maximize Compatibility), and leaves out adjustment layers
//! and saved selections.
//!
//! What a file needs for Photoshop to open it, and which blocks have long
//! lengths in a .psb, were checked against PhotoCraft's `crates/psd`
//! (<https://github.com/storytold/photocraft>, commit `ec477ca`, MIT).

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;

use rayon::prelude::*;

use crate::blend::BlendMode;
use crate::layer::{BlendIf, BlendIfChannel, Layer, Locks, Mask};
use crate::ora::{uncrop, used_area};
use crate::tiled::Tiled;
use crate::raster::{Pixel, Raster, widen};
use crate::{ColorProfile, Document, Error, Result};

fn bad(what: impl std::fmt::Display) -> Error {
    Error::Unsupported(format!("PSD: {what}"))
}

/// Image resources: the colour profile, and what the file says about itself.
const ICC_PROFILE: u16 = 1039;
const VERSION_INFO: u16 = 1057;
const EXIF: u16 = 1058;
/// A layer's Blend If when it hides nothing, for one channel.
const NO_BLEND_IF: [u8; 8] = [0, 0, 255, 255, 0, 0, 255, 255];
/// Lock All among a layer's locks (`lspf`); the first three bits are
/// transparency, pixels and position.
const LOCK_ALL: u32 = 1 << 31;

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

    /// A length: 64 bits where a .psb has `long` ones.
    fn len(&mut self, long: bool) -> Result<usize> {
        if long { self.array().map(|b| u64::from_be_bytes(b) as usize) } else { Ok(self.u32()? as usize) }
    }

    /// A block preceded by its length.
    fn section_of(&mut self, long: bool) -> Result<&'a [u8]> {
        let len = self.len(long)?;
        self.take(len)
    }

    /// A block preceded by its 32-bit length.
    fn section(&mut self) -> Result<&'a [u8]> {
        self.section_of(false)
    }

    fn rest(&self) -> &'a [u8] {
        &self.data[self.pos.min(self.data.len())..]
    }

    fn done(&self) -> bool {
        self.pos >= self.data.len()
    }
}

/// Keys of the blocks whose lengths are 64-bit in a .psb: those in Adobe's
/// specification, then those PhotoCraft found Photoshop also writes so.
const LONG: [&[u8; 4]; 21] = [
    b"LMsk", b"Lr16", b"Lr32", b"Layr", b"Mt16", b"Mt32", b"Mtrn", b"Alph", b"FMsk", b"lnk2", b"FEid",
    b"FXid", b"PxSD", b"lnk3", b"lnkE", b"pths", b"extd", b"extn", b"FELS", b"cinf", b"artd",
];

/// Tagged blocks (`8BIM` + key + length + data), as `(key, data)`.
fn tagged_blocks(data: &[u8], psb: bool) -> Result<Vec<([u8; 4], &[u8])>> {
    let mut r = Reader::new(data);
    let mut blocks = Vec::new();
    while data.len() - r.pos.min(data.len()) >= 12 {
        let signature = r.array::<4>()?;
        if &signature != b"8BIM" && &signature != b"8B64" {
            break;
        }
        let key = r.array()?;
        blocks.push((key, r.section_of(psb && LONG.contains(&&key))?));
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
    blend_if: Option<BlendIf>,
    locks: Locks,
    /// 1 or 2 is a group's own record, 3 the divider that closes it.
    section: u32,
    adjustment: bool,
}

/// The layer records in a layer info block, bottom first.
fn records(info: &[u8], psb: bool) -> Result<Vec<Record<'_>>> {
    let mut r = Reader::new(info);
    // Negative when the first alpha channel is the flattened transparency.
    let count = r.i16()?.unsigned_abs() as usize;
    let mut out = Vec::with_capacity(count);
    let mut lengths = Vec::with_capacity(count);
    for _ in 0..count {
        let rect = read_rect(&mut r)?;
        let channels: Vec<(i16, usize)> = (0..r.u16()?)
            .map(|_| Ok((r.i16()?, r.len(psb)?)))
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
        // Blend If: black and white points for this layer and those below,
        // for grey and then each channel. Omapix has one: the first set.
        let ranges = extra.section()?.as_chunks::<8>().0.iter().take(4);
        let of = [BlendIfChannel::Gray, BlendIfChannel::Red, BlendIfChannel::Green, BlendIfChannel::Blue];
        let blend_if = ranges.zip(of).find(|(r, _)| **r != NO_BLEND_IF).map(|(r, channel)| {
            let points = |at: usize| [0, 1, 2, 3].map(|i| f32::from(r[at + i]) / 255.0);
            BlendIf { channel, this: points(0), underlying: points(4) }
        });
        // A Pascal string padded to 4 bytes, replaced by `luni` if there.
        let name_start = extra.pos;
        let name_len = extra.u8()? as usize;
        let mut name = String::from_utf8_lossy(extra.take(name_len)?).into_owned();
        extra.pos = name_start + (1 + name_len).next_multiple_of(4);
        let (mut section, mut adjustment, mut locks) = (0, false, Locks::default());
        for (key, data) in tagged_blocks(extra.rest(), psb)? {
            let mut d = Reader::new(data);
            match &key {
                b"luni" => {
                    let units: Vec<u16> = (0..d.u32()?).map(|_| d.u16()).collect::<Result<_>>()?;
                    name = String::from_utf16_lossy(&units);
                }
                b"lsct" => section = d.u32()?,
                b"lspf" => {
                    let bits = d.u32()?;
                    locks = Locks { transparency: bits & 1 != 0, pixels: bits & 2 != 0, position: bits & 4 != 0, all: false };
                    if bits & LOCK_ALL != 0 {
                        locks.set_all(true);
                    }
                }
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
            blend_if,
            locks,
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
/// compression method; for RLE, `lengths` bytes of row lengths come first.
fn samples(compression: u16, data: &[u8], (w, h): (usize, usize), depth: u16, lengths: usize) -> Result<Vec<u16>> {
    let len = w * h * depth as usize / 8;
    let mut bytes = match compression {
        0 => data.get(..len).ok_or_else(|| bad("the image data is truncated"))?.to_vec(),
        1 => unpack_bits(data.get(lengths..).unwrap_or_default(), len)?,
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
fn channel_samples(data: &[u8], rect: Rect, depth: u16, psb: bool) -> Result<Vec<u16>> {
    let compression = Reader::new(data).u16()?;
    let size = rect_size(rect);
    samples(compression, &data[2..], size, depth, size.1 * if psb { 4 } else { 2 })
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

/// A record's layer mask.
fn mask(rec: &Record, (depth, psb): (u16, bool), (width, height): (u32, u32)) -> Result<Option<Mask>> {
    let data = rec.channels.iter().find(|(c, _)| *c == -2);
    let (Some((rect, outside, enabled)), Some((_, data))) = (rec.mask, data) else { return Ok(None) };
    let (area, cropped) = crop(rect, &channel_samples(data, rect, depth, psb)?, width, height);
    Ok(Some(Mask { pixels: uncrop(width, height, widen(outside), area, &cropped), enabled }))
}

/// A pixel layer, with its mask, from its record.
fn pixel_layer(id: u64, rec: &Record, colours: usize, (depth, psb): (u16, bool), (width, height): (u32, u32)) -> Result<Layer> {
    let channel = |id: i16, rect: Rect| -> Result<Option<Vec<u16>>> {
        let data = rec.channels.iter().find(|(c, _)| *c == id);
        data.map(|(_, data)| channel_samples(data, rect, depth, psb)).transpose()
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
    layer.mask = mask(rec, (depth, psb), (width, height))?;
    Ok(layer)
}

/// Photoshop's keys for the blend modes it shares with Omapix.
const BLEND_KEYS: [(&[u8; 4], BlendMode); 24] = [
    (b"norm", BlendMode::Normal),
    (b"pass", BlendMode::PassThrough),
    (b"dark", BlendMode::Darken),
    (b"mul ", BlendMode::Multiply),
    (b"idiv", BlendMode::ColorBurn),
    (b"lbrn", BlendMode::LinearBurn),
    (b"lite", BlendMode::Lighten),
    (b"scrn", BlendMode::Screen),
    (b"div ", BlendMode::ColorDodge),
    (b"lddg", BlendMode::LinearDodge),
    (b"over", BlendMode::Overlay),
    (b"sLit", BlendMode::SoftLight),
    (b"hLit", BlendMode::HardLight),
    (b"vLit", BlendMode::VividLight),
    (b"lLit", BlendMode::LinearLight),
    (b"pLit", BlendMode::PinLight),
    (b"diff", BlendMode::Difference),
    (b"smud", BlendMode::Exclusion),
    (b"fsub", BlendMode::Subtract),
    (b"fdiv", BlendMode::Divide),
    (b"hue ", BlendMode::Hue),
    (b"sat ", BlendMode::Saturation),
    (b"colr", BlendMode::Color),
    (b"lum ", BlendMode::Luminosity),
];

fn blend_mode(key: &[u8; 4]) -> BlendMode {
    // Dissolve, Darker/Lighter Color and Hard Mix, which Omapix doesn't
    // have, open as Normal.
    BLEND_KEYS.iter().find(|(k, _)| *k == key).map_or(BlendMode::Normal, |(_, mode)| *mode)
}

/// The document's layers, bottom first, and whether any were left out.
fn layers(records: &[Record], colours: usize, depth: (u16, bool), size: (u32, u32)) -> Result<(Vec<Layer>, bool)> {
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
                group.mask = mask(rec, depth, size)?;
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
        layer.blend_if = rec.blend_if;
        layer.locks = rec.locks;
        layer.parent = open.last().copied();
        out.push(layer);
    }
    Ok((out, skipped))
}

/// The flattened image stored after the layers.
fn composite(data: &[u8], channels: usize, colours: usize, (depth, psb): (u16, bool), (w, h): (usize, usize)) -> Result<Raster> {
    let mut r = Reader::new(data);
    let compression = r.u16()?;
    let rest = r.rest();
    let plane = |c: usize| -> Result<Vec<u16>> {
        let start = match compression {
            // Raw planes, one after another.
            0 => c * w * h * depth as usize / 8,
            // Every plane's row lengths, then the planes.
            _ => {
                let n = if psb { 4 } else { 2 };
                let lengths = rest.get(..channels * h * n).ok_or_else(|| bad("the image data is truncated"))?;
                let before = lengths[..c * h * n].chunks(n);
                channels * h * n + before.map(|b| b.iter().fold(0, |v, b| v << 8 | *b as usize)).sum::<usize>()
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
    let psb = match r.u16()? {
        1 => false,
        2 => true,
        _ => return Err(bad("not a Photoshop file")),
    };
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
    let (mut profile, mut exif) = (ColorProfile::srgb(), None);
    let mut resources = Reader::new(r.section()?);
    while !resources.done() {
        resources.take(4)?; // 8BIM
        let id = resources.u16()?;
        // A Pascal name and the data, each padded to an even length.
        let name_len = resources.u8()? as usize;
        resources.take((1 + name_len).next_multiple_of(2) - 1)?;
        let data = resources.section()?;
        resources.take(data.len() % 2)?;
        if id == ICC_PROFILE {
            profile = ColorProfile::from_icc(data.to_vec())?;
        }
        if id == EXIF {
            // The pixels are stored upright.
            let mut data = data.to_vec();
            let _ = image::metadata::Orientation::remove_from_exif_chunk(&mut data);
            exif = Some(data);
        }
    }
    let mut section = Reader::new(r.section_of(psb)?);
    let mut info = if section.done() { &[][..] } else { section.section_of(psb)? };
    // 16- and 32-bit files keep their layers in a tagged block after the
    // global mask info instead.
    if info.is_empty() && !section.done() {
        section.section()?;
        let blocks = tagged_blocks(section.rest(), psb)?;
        if let Some((_, data)) = blocks.into_iter().find(|(key, _)| key == b"Lr16" || key == b"Lr32") {
            info = data;
        }
    }
    let size = (width, height);
    let flat = composite(r.rest(), channels, colours, (depth, psb), (width as usize, height as usize))?;
    let (mut layers, skipped) = if info.is_empty() {
        (Vec::new(), false)
    } else {
        layers(&records(info, psb)?, colours, (depth, psb), size)?
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
    let mut doc = Document::new(path.to_path_buf(), profile, depth as u8, width, height, layers);
    doc.exif = exif;
    Ok(doc)
}

/// A tagged block. `data` is already a multiple of 4 bytes long.
fn block(key: &[u8; 4], data: &[u8]) -> Vec<u8> {
    [b"8BIM", &key[..], &(data.len() as u32).to_be_bytes(), data].concat()
}

fn blend_key(mode: BlendMode) -> &'static [u8; 4] {
    // Grain Merge and Grain Extract are written as Linear Light (see
    // `record`).
    BLEND_KEYS.iter().find(|(_, m)| *m == mode).map_or(b"lLit", |(key, _)| key)
}

/// The rectangle of `t` that isn't transparent, and its pixels there.
fn content(t: &Tiled<Pixel>) -> (Rect, Vec<Pixel>) {
    let nothing = ((0, 0, 0, 0), Vec::new());
    let Some((x, y, w, h)) = used_area(t) else { return nothing };
    // That's in whole tiles: trim it to the rows and columns in use.
    let values = t.crop(x, y, w, h);
    let (w, h) = (w as usize, h as usize);
    let row = |r: &usize| values[r * w..(r + 1) * w].iter().any(|p| p[3] != 0);
    let column = |c: &usize| (0..h).any(|r| values[r * w + c][3] != 0);
    let (Some(top), Some(left)) = ((0..h).find(row), (0..w).find(column)) else { return nothing };
    let (bottom, right) = ((0..h).rfind(row).unwrap_or(top) + 1, (0..w).rfind(column).unwrap_or(left) + 1);
    let trimmed = (top..bottom).flat_map(|r| values[r * w + left..r * w + right].iter().copied()).collect();
    let (x, y) = (x as usize, y as usize);
    (((y + top) as i32, (x + left) as i32, (y + bottom) as i32, (x + right) as i32), trimmed)
}

/// A channel's rows of `w` values as a layer stores them: zip-compressed,
/// each value as its difference from the one before, which is what
/// Photoshop does at 16 bits.
fn zipped(values: &[u16], w: usize) -> Vec<u8> {
    // Nothing there: stored raw, and empty.
    if values.is_empty() {
        return vec![0, 0];
    }
    let mut bytes = Vec::with_capacity(values.len() * 2);
    for row in values.chunks(w) {
        let mut before = 0u16;
        for &v in row {
            bytes.extend(v.wrapping_sub(before).to_be_bytes());
            before = v;
        }
    }
    let mut z = flate2::write::ZlibEncoder::new(vec![0, 3], flate2::Compression::default());
    z.write_all(&bytes).and_then(|_| z.finish()).expect("writing to memory")
}

/// A layer record: its rectangle, its channels (id, then the data with its
/// compression method), and what follows the channels' lengths.
struct Written {
    rect: Rect,
    channels: Vec<(i16, Vec<u8>)>,
    rest: Vec<u8>,
}

/// The record for `layer`: a pixel layer (`section` 0), a group (1), or the
/// divider that comes before what's in a group (3).
fn record(layer: &Layer, section: u32, id: u32) -> Written {
    let be = |v: u32| v.to_be_bytes();
    let (rect, pixels) = if section == 0 { content(&layer.pixels) } else { ((0, 0, 0, 0), Vec::new()) };
    // Photoshop has no Grain Merge or Grain Extract. Linear Light adds
    // twice what a layer is above half, so it does the same with the
    // layer's values halved about the middle (and turned over, to extract).
    let plane = |c: usize| -> Vec<u16> {
        let value = |p: &Pixel| match layer.blend {
            BlendMode::GrainMerge if c < 3 => 16384 + p[c] / 2,
            BlendMode::GrainExtract if c < 3 => 49151 - p[c] / 2,
            _ => p[c],
        };
        pixels.iter().map(value).collect()
    };
    let w = rect_size(rect).0;
    // Transparency first, as Photoshop has them.
    let mut channels: Vec<(i16, Vec<u8>)> =
        [(-1, 3), (0, 0), (1, 1), (2, 2)].into_par_iter().map(|(id, c)| (id, zipped(&plane(c), w))).collect();

    let mut extra = Vec::new();
    match &layer.mask {
        Some(mask) => {
            // Over the whole canvas, as Photoshop writes them: Krita
            // doesn't open a file with a smaller one. Then what it is
            // beyond that, and whether it's off.
            let (w, h) = (mask.pixels.width(), mask.pixels.height());
            channels.push((-2, zipped(&mask.pixels.to_vec(), w as usize)));
            extra.extend(be(20));
            extra.extend([0, 0, h, w].map(u32::to_be_bytes).concat());
            extra.extend([(mask.pixels.fill() >> 8) as u8, if mask.enabled { 0 } else { 2 }, 0, 0]);
        }
        None => extra.extend(be(0)),
    }
    // Blend If, for grey, each channel and the transparency.
    let mut ranges = [NO_BLEND_IF; 5];
    if let Some(blend_if) = &layer.blend_if {
        let points = [blend_if.this, blend_if.underlying].concat();
        ranges[blend_if.channel as usize] = std::array::from_fn(|i| (points[i].clamp(0.0, 1.0) * 255.0).round() as u8);
    }
    extra.extend(be(40));
    extra.extend(ranges.as_flattened());
    // The name as old versions read it, padded to 4 bytes, then in full.
    let ascii = layer.name.chars().take(31).map(|c| if c.is_ascii() { c as u8 } else { b'?' });
    extra.push(ascii.clone().count() as u8);
    extra.extend(ascii);
    extra.resize(extra.len().next_multiple_of(4), 0);
    let units: Vec<u16> = layer.name.encode_utf16().collect();
    let mut name = be(units.len() as u32).to_vec();
    name.extend(units.iter().flat_map(|u| u.to_be_bytes()));
    name.resize(name.len().next_multiple_of(4), 0);
    extra.extend(block(b"luni", &name));
    extra.extend(block(b"lyid", &be(id)));
    match section {
        0 => {}
        3 => extra.extend(block(b"lsct", &be(3))),
        _ => extra.extend(block(b"lsct", &[&be(1)[..], b"8BIM", blend_key(layer.blend)].concat())),
    }
    let locks = layer.locks;
    if locks.any() {
        let bits = u32::from(locks.transparency) | u32::from(locks.pixels) << 1 | u32::from(locks.position) << 2;
        extra.extend(block(b"lspf", &be(if locks.all { bits | LOCK_ALL } else { bits })));
    }

    // Flags: transparency locked, hidden, and (with the 8) whether the
    // pixels matter to how the document looks, which a group's don't.
    let flags = 8 | u8::from(layer.lock_alpha()) | if layer.visible { 0 } else { 2 } | if section == 0 { 0 } else { 16 };
    let mut rest = [b"8BIM", &blend_key(layer.blend)[..]].concat();
    rest.extend([(layer.opacity.clamp(0.0, 1.0) * 255.0).round() as u8, u8::from(layer.clipped), flags, 0]);
    rest.extend(be(extra.len() as u32));
    rest.extend(extra);
    Written { rect, channels, rest }
}

/// Save as a Photoshop file, 16-bit RGB with the flattened image: a
/// large-document .psb if that's the path's extension, or else a .psd.
/// Adjustment layers and saved selections are left out.
pub fn save(doc: &Document, path: &Path) -> Result<()> {
    let psb = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("psb"));
    let be = |v: u32| v.to_be_bytes();
    let len = |n: usize| if psb { (n as u64).to_be_bytes().to_vec() } else { be(n as u32).to_vec() };

    // Records run bottom first: the divider that begins a group, what's
    // in the group, then the group itself.
    let divider = Layer::empty(0, "</Layer group>", 0, 0);
    let mut open: Vec<u64> = Vec::new();
    let mut listed: Vec<(&Layer, u32)> = Vec::new();
    for layer in doc.layers.iter().filter(|l| l.adjustment.is_none()) {
        // The groups this layer is in that haven't begun, innermost first.
        let mut begin = Vec::new();
        let mut group = if layer.is_group { Some(layer) } else { layer.parent.and_then(|id| doc.layer(id)) };
        while let Some(g) = group.filter(|g| !open.contains(&g.id)) {
            begin.push(g.id);
            group = g.parent.and_then(|id| doc.layer(id));
        }
        for id in begin.into_iter().rev() {
            listed.push((&divider, 3));
            open.push(id);
        }
        if layer.is_group {
            open.pop();
        }
        listed.push((layer, u32::from(layer.is_group)));
    }
    let records: Vec<Written> =
        listed.par_iter().enumerate().map(|(i, (layer, section))| record(layer, *section, i as u32 + 1)).collect();

    // With any transparency, the flattened image has a fourth channel
    // for it, and the layer count says so by being negative.
    let merged = doc.composite();
    let opaque = merged.pixels().iter().all(|p| p[3] == u16::MAX);
    let planes = if opaque { 3 } else { 4 };

    let mut head = b"8BPS".to_vec();
    head.extend([0, if psb { 2 } else { 1 }]);
    head.extend([0; 6]);
    head.extend([0, planes as u8]);
    head.extend(be(doc.height));
    head.extend(be(doc.width));
    head.extend([0, 16, 0, 3]); // 16 bits a channel, RGB
    head.extend(be(0)); // colour mode data
    let mut resources = Vec::new();
    let mut resource = |id: u16, data: &[u8]| {
        resources.extend(b"8BIM");
        resources.extend(id.to_be_bytes());
        resources.extend([0, 0]); // no name
        resources.extend(be(data.len() as u32));
        resources.extend(data);
        resources.resize(resources.len().next_multiple_of(2), 0);
    };
    if let Some(icc) = doc.profile.icc() {
        resource(ICC_PROFILE, icc);
    }
    if let Some(exif) = &doc.exif {
        resource(EXIF, exif);
    }
    // That the flattened image is a real one, and who wrote and can read it.
    let omapix: Vec<u8> = "Omapix".encode_utf16().flat_map(u16::to_be_bytes).collect();
    let name = [&be(6)[..], &omapix].concat();
    resource(VERSION_INFO, &[&be(1)[..], &[1], &name, &name, &be(1)].concat());
    head.extend(be(resources.len() as u32));
    head.extend(resources);

    // The layers, in an `Lr16` block after an empty layer info and an
    // empty global mask: the count, the records, then the channels' data,
    // padded to 4 bytes as Photoshop pads it.
    let count = records.len() as i16;
    let mut info = (if opaque { count } else { -count }).to_be_bytes().to_vec();
    for r in &records {
        info.extend([r.rect.0, r.rect.1, r.rect.2, r.rect.3].map(i32::to_be_bytes).concat());
        info.extend((r.channels.len() as u16).to_be_bytes());
        for (id, data) in &r.channels {
            info.extend(id.to_be_bytes());
            info.extend(len(data.len()));
        }
        info.extend(&r.rest);
    }
    let data = || records.iter().flat_map(|r| &r.channels).map(|(_, data)| data);
    let unpadded = info.len() + data().map(Vec::len).sum::<usize>();
    let block_len = unpadded.next_multiple_of(4);
    let mut section = Vec::new();
    if !records.is_empty() {
        section.extend(len(0));
        section.extend(be(0));
        section.extend(b"8BIM");
        section.extend(b"Lr16");
        section.extend(len(block_len));
        head.extend(len(section.len() + block_len));
    } else {
        head.extend(len(0));
    }

    let pixels = doc.width as usize * doc.height as usize;
    let total = head.len() + section.len() + block_len + 2 + planes * pixels * 2;
    if !psb && (total > i32::MAX as usize || doc.width.max(doc.height) > 30_000) {
        return Err(bad("this is too big for a .psd (2 GB, or 30,000 pixels a side): export it as a .psb"));
    }

    let failed = |source| Error::Read { path: path.display().to_string(), source };
    let mut file = BufWriter::new(File::create(path).map_err(failed)?);
    let mut write = |bytes: &[u8]| file.write_all(bytes).map_err(failed);
    write(&head)?;
    if !records.is_empty() {
        write(&section)?;
        write(&info)?;
        for channel in data() {
            write(channel)?;
        }
        write(&vec![0; block_len - unpadded])?;
    }
    // The flattened image, uncompressed, a channel at a time. Where it's
    // transparent it's over white, as Photoshop's is.
    write(&[0, 0])?;
    for c in 0..planes {
        let value = |p: &Pixel| {
            let a = u32::from(p[3]);
            if c == 3 || opaque { p[c] } else { ((u32::from(p[c]) * a + 32767) / 65535 + 65535 - a) as u16 }
        };
        let plane: Vec<u8> = merged.pixels().par_iter().flat_map_iter(|p| value(p).to_be_bytes()).collect();
        write(&plane)?;
    }
    file.flush().map_err(failed)
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

    /// A 4 × 2 16-bit RGB file laid out as Photoshop writes one: layers in
    /// an `Lr16` block, zip-compressed with prediction, and a mid-grey
    /// RLE composite. With `psb`, a large document.
    fn sixteen_bit_file(specs: &[Spec], psb: bool) -> Vec<u8> {
        let be = |v: u32| v.to_be_bytes();
        let len = |n: usize| if psb { (n as u64).to_be_bytes().to_vec() } else { be(n as u32).to_vec() };
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
                info.extend(len(data.len()));
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
        let layer_and_mask = [&len(0)[..], &be(0), b"8BIMLr16", &len(info.len()), &info].concat();

        let mut out = b"8BPS".to_vec();
        out.extend([0, if psb { 2 } else { 1 }]);
        out.extend([0; 6]);
        out.extend(3u16.to_be_bytes());
        out.extend(be(2)); // height
        out.extend(be(4)); // width
        out.extend(16u16.to_be_bytes());
        out.extend(3u16.to_be_bytes());
        out.extend(be(0));
        out.extend(be(0));
        out.extend(len(layer_and_mask.len()));
        out.extend(layer_and_mask);
        // Each row of each plane is one run: 8 bytes of 0x80.
        out.extend(1u16.to_be_bytes());
        out.extend(if psb { [0, 0, 0, 2].repeat(6) } else { [0, 2].repeat(6) });
        out.extend([0xF9, 0x80].repeat(6));
        out
    }

    #[test]
    fn a_16_bit_file_keeps_its_layers_groups_and_masks() {
        keeps_its_layers(false);
    }

    #[test]
    fn so_does_a_large_document() {
        keeps_its_layers(true);
    }

    fn keeps_its_layers(psb: bool) {
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
        ], psb);
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

    /// `doc` saved as `name` and opened again, and the file.
    fn saved(doc: &Document, name: &str) -> (Result<Document>, Vec<u8>) {
        let dir = std::env::temp_dir().join(format!("omapix-psd-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let back = save(doc, &path).and_then(|_| load(&path));
        let bytes = std::fs::read(&path).unwrap_or_default();
        std::fs::remove_dir_all(&dir).ok();
        (back, bytes)
    }

    fn photo(w: u32, h: u32) -> Document {
        let px: Vec<Pixel> = (0..w * h).map(|i| [(i % 65000) as u16, (i / 3 % 65000) as u16, 1234, 65535]).collect();
        Document::from_image("in.tif".into(), &Raster::new(w, h, px), ColorProfile::srgb(), 16)
    }

    #[test]
    fn a_document_comes_back_from_a_psd_and_a_psb_as_it_was() {
        let (w, h) = (600, 300);
        let mut doc = photo(w, h);
        doc.exif = Some(b"II*\0\x08\0\0\0\0\0\0\0\0\0".to_vec());
        // A small layer with soft edges and a mask, and one clipped to it,
        // in a group in a group; then an empty layer and an empty group.
        let patch: Vec<Pixel> = (0..40 * 30).map(|i| [60000, i as u16, 3, (i * 50) as u16 + 1]).collect();
        let mut patch = Layer::from_pixels(2, "Patch ✓", uncrop(w, h, [0; 4], (300, 100, 40, 30), &patch));
        (patch.blend, patch.opacity, patch.parent) = (BlendMode::SoftLight, 0.6, Some(4));
        patch.mask = Some(Mask { pixels: uncrop(w, h, 0, (290, 90, 20, 20), &[40000; 400]), enabled: false });
        patch.locks.position = true;
        let mut clipped = Layer::from_pixels(3, "Clipped", uncrop(w, h, [0; 4], (0, 0, 600, 2), &[[1, 2, 3, 65535]; 1200]));
        (clipped.blend, clipped.clipped, clipped.visible, clipped.parent) = (BlendMode::Multiply, true, false, Some(4));
        clipped.blend_if = Some(BlendIf { channel: BlendIfChannel::Green, this: [0.0, 0.2, 0.8, 1.0], underlying: [0.2, 0.2, 1.0, 1.0] });
        let mut inner = Layer::group(4, "Inner", w, h);
        (inner.blend, inner.opacity, inner.parent) = (BlendMode::Normal, 0.8, Some(5));
        let mut white = Mask::white(w, h);
        white.pixels.tile_mut(1, 0)[5] = 1000;
        inner.mask = Some(white);
        let mut outer = Layer::group(5, "Outer", w, h);
        outer.locks.set_all(true);
        let mut nothing = Layer::empty(6, "Nothing", w, h);
        nothing.locks.transparency = true;
        doc.layers.extend([patch, clipped, inner, outer, nothing, Layer::group(7, "Empty", w, h)]);

        for (name, version) in [("out.psd", 1), ("out.PSB", 2)] {
            let (back, bytes) = saved(&doc, name);
            let back = back.unwrap();
            assert_eq!(bytes[..6], [b'8', b'B', b'P', b'S', 0, version]);
            assert_eq!((back.width, back.height, back.source_bits, &back.exif), (w, h, 16, &doc.exif));
            assert_eq!(back.layers.len(), doc.layers.len());
            let parent = |d: &Document, l: &Layer| l.parent.and_then(|id| d.layer(id)).map(|g| g.name.clone());
            for (a, b) in doc.layers.iter().zip(&back.layers) {
                assert_eq!((&a.name, a.is_group, a.blend, a.visible), (&b.name, b.is_group, b.blend, b.visible));
                assert_eq!((a.clipped, a.locks, parent(&doc, a)), (b.clipped, b.locks, parent(&back, b)), "{}", a.name);
                assert!((a.opacity - b.opacity).abs() < 0.003, "{}: {}", a.name, b.opacity);
                assert!(a.pixels.to_vec() == b.pixels.to_vec(), "{}", a.name);
                assert_eq!(a.mask.is_some(), b.mask.is_some(), "{}", a.name);
                if let (Some(a), Some(b)) = (&a.mask, &b.mask) {
                    assert!(a.enabled == b.enabled && a.pixels.to_vec() == b.pixels.to_vec());
                }
                let points = |l: &Layer| l.blend_if.map(|b| [b.this, b.underlying].concat().iter().map(|v| (v * 255.0).round()).sum::<f32>());
                assert_eq!((a.blend_if.map(|b| b.channel), points(a)), (b.blend_if.map(|b| b.channel), points(b)));
            }
            assert!(back.composite().pixels() == doc.composite().pixels());
        }
    }

    #[test]
    fn grain_merge_and_extract_look_the_same_as_linear_light() {
        // Frequency separation's texture layer is in Grain Merge.
        let mut doc = photo(600, 300);
        crate::ops::frequency_separation(&mut doc, 0, 3.0);
        let mut extract = doc.layers[0].clone();
        (extract.id, extract.blend, extract.opacity) = (doc.next_layer_id(), BlendMode::GrainExtract, 0.6);
        doc.layers.push(extract);
        assert!(doc.layers.iter().any(|l| l.blend == BlendMode::GrainMerge));

        let back = saved(&doc, "fs.psd").0.unwrap();
        for (a, b) in doc.layers.iter().zip(&back.layers) {
            let grain = matches!(a.blend, BlendMode::GrainMerge | BlendMode::GrainExtract);
            assert_eq!(b.blend, if grain { BlendMode::LinearLight } else { a.blend }, "{}", a.name);
        }
        let (before, after) = (doc.composite(), back.composite());
        let worst = before.pixels().iter().zip(after.pixels()).flat_map(|(a, b)| (0..4).map(|c| a[c].abs_diff(b[c]))).max();
        assert!(worst.unwrap() <= 2, "{worst:?}");
    }

    #[test]
    fn adjustment_layers_are_left_out_and_transparency_is_kept() {
        let image = Raster::new(2, 1, vec![[10000, 20000, 30000, 65535], [40000, 50000, 60000, 0]]);
        let mut doc = Document::from_image("in.tif".into(), &image, ColorProfile::srgb(), 16);
        let curves = crate::adjust::Adjustment::Curves(Default::default());
        doc.layers.push(Layer::adjustment(2, curves, 2, 1));
        let (back, bytes) = saved(&doc, "cut-out.psd");
        let back = back.unwrap();
        assert_eq!(back.layers.len(), 1);
        assert_eq!(back.layers[0].pixels.to_vec(), [[10000, 20000, 30000, 65535], [0; 4]]);
        // Four channels, and the flattened image at the end: over white
        // where it's transparent, then the transparency.
        assert_eq!(bytes[12..14], [0, 4]);
        let flat: Vec<u16> = bytes[bytes.len() - 16..].as_chunks::<2>().0.iter().map(|b| u16::from_be_bytes(*b)).collect();
        assert_eq!(flat, [10000, 65535, 20000, 65535, 30000, 65535, 65535, 0]);
    }

    #[test]
    fn a_document_too_big_for_a_psd_is_refused_and_fits_a_psb() {
        let doc = photo(30_001, 1);
        let (back, bytes) = saved(&doc, "wide.psd");
        assert!(back.err().unwrap().to_string().contains(".psb"));
        assert!(bytes.is_empty(), "nothing was written");
        assert_eq!(saved(&doc, "wide.psb").0.unwrap().width, 30_001);
    }
}
