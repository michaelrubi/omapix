// Ported from PhotoCraft's `crates/psd/src/patterns.rs`
// (<https://github.com/storytold/photocraft>, commit `ec477ca`), under its
// MIT licence:
//
// Copyright (c) 2026 ArtCraft Team and the PhotoCraft contributors
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

//! Patterns: the global `Patt` / `Pat2` / `Pat3` tagged blocks and standalone `.pat` files.
//!
//! Embedded in `.abr` brush files (`patt` section) when brush presets use texture dynamics.

use crate::psd_io::{ByteReader, WriteExt, packbits};
use crate::{Error, Result};

fn bad(what: impl std::fmt::Display) -> Error {
    Error::Unsupported(format!("Pattern: {what}"))
}

/// Largest pattern edge accepted.
pub const MAX_EDGE: u32 = 30_000;
const MAX_RLE_EXPANSION: u64 = 64;
const MAX_DECODED_BYTES: u64 = 2 << 30; // 2 GiB cap

/// One pattern tile with planar, big-endian samples.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PsdPattern {
    /// PSD colour mode number (1 gray, 2 indexed, 3 RGB, 4 CMYK, 7 multichannel, 8 duotone, 9 Lab).
    pub mode: u32,
    /// Tile width in pixels.
    pub width: u32,
    /// Tile height in pixels.
    pub height: u32,
    /// Name without trailing NUL.
    pub name: String,
    /// Unique id (usually a UUID).
    pub id: String,
    /// Indexed-colour palette (768 bytes, RGB triplets).
    pub palette: Option<Vec<u8>>,
    /// Bits per sample: 1, 8, 16 or 32.
    pub depth: u16,
    /// Decoded colour planes (`width × height` samples each).
    pub channels: Vec<Vec<u8>>,
    /// Decoded transparency plane, if any.
    pub alpha: Option<Vec<u8>>,
}

/// Colour channels implied by a PSD colour mode.
pub fn mode_channels(mode: u32) -> usize {
    match mode {
        3 | 9 => 3,
        4 => 4,
        _ => 1,
    }
}

pub(crate) fn row_bytes(width: usize, depth: u16) -> usize {
    match depth {
        1 => width.div_ceil(8),
        8 => width,
        16 => width * 2,
        32 => width * 4,
        _ => width,
    }
}

pub(crate) fn decode_plane_rle(data: &[u8], width: usize, height: usize, depth: u16) -> Result<Vec<u8>> {
    let rb = row_bytes(width, depth);
    let total = height.checked_mul(rb).ok_or_else(|| bad("plane size overflow"))?;
    let mut r = ByteReader::new(data);
    r.check_count(height as u64, 2)?;
    let mut counts = Vec::with_capacity(height);
    for _ in 0..height {
        counts.push(r.u16()? as usize);
    }
    let mut out = Vec::new();
    out.try_reserve_exact(total)
        .map_err(|_| bad("not enough memory for decoded plane"))?;
    for c in counts {
        let row = r.bytes(c)?;
        packbits::decode_into(row, rb, &mut out)?;
    }
    Ok(out)
}

pub(crate) fn encode_plane_rle(data: &[u8], width: usize, height: usize, depth: u16) -> Result<Vec<u8>> {
    let rb = row_bytes(width, depth);
    let mut out = vec![0u8; height * 2];
    let mut enc = Vec::new();
    for row in 0..height {
        let start = enc.len();
        let slice = &data[row * rb..(row + 1) * rb];
        packbits::encode(slice, &mut enc);
        let n = enc.len() - start;
        let n_u16 = u16::try_from(n).map_err(|_| bad("RLE row exceeds 65535 bytes"))?;
        out[row * 2..row * 2 + 2].copy_from_slice(&n_u16.to_be_bytes());
    }
    out.extend_from_slice(&enc);
    Ok(out)
}

struct PatternChannel<'a> {
    data: &'a [u8],
    compression: u8, // 0 = Raw, 1 = Rle
    width: usize,
    height: usize,
    depth: u16,
    x: usize,
    y: usize,
}

fn read_pattern(r: &mut ByteReader<'_>) -> Result<PsdPattern> {
    let version = r.u32()?;
    if version != 1 {
        return Err(bad(format!("pattern version {version}")));
    }
    let mode = r.u32()?;
    let _h = r.u16()?;
    let _w = r.u16()?;
    let n = r.u32()? as usize;
    if n > 65_536 {
        return Err(bad("pattern name length exceeds limit"));
    }
    let units: Vec<u16> = r.bytes(n * 2)?.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
    let name = String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_string();
    let idl = r.u8()? as usize;
    let id = String::from_utf8_lossy(r.bytes(idl)?).to_string();
    let palette = if mode == 2 {
        let p = r.bytes(768)?.to_vec();
        // Some writers follow the palette with 4 extra bytes: anything
        // that isn't the version that comes next.
        if r.peek_rest().get(..4).is_some_and(|v| v != [0, 0, 0, 3]) {
            r.skip(4)?;
        }
        Some(p)
    } else {
        None
    };
    let vver = r.u32()?;
    if vver != 3 {
        return Err(bad(format!("virtual memory array list version {vver}")));
    }
    let len = r.u32()? as usize;
    let body = r.bytes(len)?;
    let mut v = ByteReader::new(body);
    let top = v.i32()?;
    let left = v.i32()?;
    let bottom = v.i32()?;
    let right = v.i32()?;
    let w = right
        .checked_sub(left)
        .ok_or_else(|| bad("pattern width overflow"))?;
    let h = bottom
        .checked_sub(top)
        .ok_or_else(|| bad("pattern height overflow"))?;
    if w <= 0 || h <= 0 || w as u32 > MAX_EDGE || h as u32 > MAX_EDGE {
        return Err(bad("invalid pattern dimensions"));
    }
    let (w, h) = (w as u32, h as u32);
    let count = v.u32()?;
    if count > 64 {
        return Err(bad("pattern channel count exceeds limit"));
    }

    let mut channels = Vec::new();
    let mut depth = 8u16;
    let mut decoded_bytes = 0u64;
    for _ in 0..count + 2 {
        if v.is_empty() {
            break;
        }
        if v.u32()? == 0 {
            continue;
        }
        let alen = v.u32()? as usize;
        if alen == 0 {
            continue;
        }
        if alen < 23 {
            return Err(bad("short virtual memory array"));
        }
        let d32 = v.u32()?;
        let ct = v.i32()?;
        let cl = v.i32()?;
        let cb = v.i32()?;
        let cr = v.i32()?;
        let d16 = v.u16()?;
        let comp = v.u8()?;
        let data = v.bytes(alen - 23)?;
        let d = if matches!(d16, 1 | 8 | 16 | 32) {
            d16
        } else {
            d32 as u16
        };
        if !matches!(d, 1 | 8 | 16 | 32) {
            return Err(bad(format!("unsupported pattern depth {d}")));
        }
        depth = d;
        if ct < top || cl < left || cb > bottom || cr > right || cb < ct || cr < cl {
            return Err(bad("pattern channel rectangle falls outside bounds"));
        }
        let cw = (cr - cl) as usize;
        let ch = (cb - ct) as usize;
        let channel_bytes = (ch as u64)
            .checked_mul(row_bytes(cw, d) as u64)
            .ok_or_else(|| bad("channel size overflow"))?;
        let plane_bytes = (h as u64)
            .checked_mul(row_bytes(w as usize, d) as u64)
            .ok_or_else(|| bad("plane size overflow"))?;
        decoded_bytes = decoded_bytes
            .checked_add(plane_bytes)
            .ok_or_else(|| bad("decoded bytes overflow"))?;
        if decoded_bytes > MAX_DECODED_BYTES {
            return Err(bad("pattern data exceeds limit"));
        }
        if comp != 0 && comp != 1 {
            return Err(bad(format!("unsupported pattern compression {comp}")));
        }
        if comp == 1 {
            let max_exp = u64::try_from(data.len())
                .unwrap_or(0)
                .saturating_mul(MAX_RLE_EXPANSION);
            if channel_bytes > max_exp {
                return Err(bad("pattern channel exceeds expansion limit"));
            }
        }
        let x = (cl - left) as usize;
        let y = (ct - top) as usize;
        channels.push(PatternChannel {
            data,
            compression: comp,
            width: cw,
            height: ch,
            depth: d,
            x,
            y,
        });
    }

    let nc = mode_channels(mode);
    if channels.len() < nc {
        return Err(bad("pattern has fewer channels than colour mode requires"));
    }

    let mut planes = Vec::new();
    for ch in channels {
        let plane = if ch.compression == 1 {
            decode_plane_rle(ch.data, ch.width, ch.height, ch.depth)?
        } else {
            let expected = ch.height * row_bytes(ch.width, ch.depth);
            if ch.data.len() < expected {
                return Err(bad("raw pattern channel truncated"));
            }
            ch.data[..expected].to_vec()
        };
        planes.push(place_channel_plane(plane, (ch.width, ch.height), ch.depth, (ch.x, ch.y), (w as usize, h as usize))?);
    }

    let alpha = (planes.len() > nc).then(|| planes.remove(nc));
    planes.truncate(nc);
    Ok(PsdPattern {
        mode,
        width: w,
        height: h,
        name,
        id,
        palette,
        depth,
        channels: planes,
        alpha,
    })
}

fn place_channel_plane(
    mut plane: Vec<u8>,
    (sw, sh): (usize, usize),
    depth: u16,
    (x, y): (usize, usize),
    (dw, dh): (usize, usize),
) -> Result<Vec<u8>> {
    if x == 0 && y == 0 && sw == dw && sh == dh {
        if depth == 1 && !sw.is_multiple_of(8) {
            let mask = u8::MAX << (8 - sw % 8);
            for row in plane.chunks_exact_mut(row_bytes(sw, depth)) {
                if let Some(last) = row.last_mut() {
                    *last &= mask;
                }
            }
        }
        return Ok(plane);
    }
    let dest_len = dh * row_bytes(dw, depth);
    let mut out = vec![0u8; dest_len];
    if depth == 1 {
        let srb = row_bytes(sw, depth);
        let drb = row_bytes(dw, depth);
        for row in 0..sh {
            for col in 0..sw {
                let s_pos = row * srb + (col / 8);
                let d_col = x + col;
                let d_pos = (y + row) * drb + (d_col / 8);
                let s_bit = 7 - (col % 8);
                let d_bit = 7 - (d_col % 8);
                let val = (plane[s_pos] >> s_bit) & 1;
                out[d_pos] |= val << d_bit;
            }
        }
    } else {
        let bpp = (depth / 8) as usize;
        let srb = row_bytes(sw, depth);
        let drb = row_bytes(dw, depth);
        let row_len = sw * bpp;
        let x_bytes = x * bpp;
        for row in 0..sh {
            let s_start = row * srb;
            let d_start = (y + row) * drb + x_bytes;
            out[d_start..d_start + row_len].copy_from_slice(&plane[s_start..s_start + row_len]);
        }
    }
    Ok(out)
}

fn write_pattern(p: &PsdPattern, out: &mut Vec<u8>) -> Result<()> {
    if p.width > MAX_EDGE || p.height > MAX_EDGE || p.width > i16::MAX as u32 || p.height > i16::MAX as u32 {
        return Err(bad("pattern size exceeds maximum"));
    }
    out.put_u32(1);
    out.put_u32(p.mode);
    out.put_u16(p.height as u16);
    out.put_u16(p.width as u16);
    let units: Vec<u16> = p.name.encode_utf16().chain(std::iter::once(0)).collect();
    out.put_u32(units.len() as u32);
    for u in units {
        out.put_u16(u);
    }
    let id_bytes = p.id.as_bytes();
    let idl = id_bytes.len().min(255);
    out.put_u8(idl as u8);
    out.put(&id_bytes[..idl]);
    if p.mode == 2 {
        let mut pal = p.palette.clone().unwrap_or_default();
        pal.resize(768, 0);
        out.put(&pal);
    }
    let rect = [0i32, 0, p.height as i32, p.width as i32];
    let mut body = Vec::new();
    for v in rect {
        body.put_i32(v);
    }
    const SLOTS: u32 = 24;
    body.put_u32(SLOTS);

    let array = |plane: &[u8], body: &mut Vec<u8>| -> Result<()> {
        let (comp, data) = if p.depth == 8 {
            (1u8, encode_plane_rle(plane, p.width as usize, p.height as usize, p.depth)?)
        } else {
            (0u8, plane.to_vec())
        };
        body.put_u32(1);
        body.put_u32((23 + data.len()) as u32);
        body.put_u32(u32::from(p.depth));
        for v in rect {
            body.put_i32(v);
        }
        body.put_u16(p.depth);
        body.put_u8(comp);
        body.put(&data);
        Ok(())
    };

    let nc = p.channels.len().min(SLOTS as usize);
    for c in &p.channels[..nc] {
        array(c, &mut body)?;
    }
    for _ in nc..SLOTS as usize {
        body.put_u32(0);
    }
    match &p.alpha {
        Some(a) => array(a, &mut body)?,
        None => body.put_u32(0),
    }
    body.put_u32(0); // sheet mask slot unused
    out.put_u32(3);
    out.put_u32(body.len() as u32);
    out.put(&body);
    Ok(())
}

/// Parses the data of a `Patt` / `Pat2` / `Pat3` block.
pub fn parse_pattern_block(data: &[u8]) -> Result<Vec<PsdPattern>> {
    let mut r = ByteReader::new(data);
    let mut out = Vec::new();
    while r.remaining() >= 4 {
        let len = r.u32()? as usize;
        if len == 0 {
            break;
        }
        let body = r.bytes(len)?;
        out.push(read_pattern(&mut ByteReader::new(body))?);
        let pad = (4 - len % 4) % 4;
        r.skip(pad.min(r.remaining()))?;
    }
    Ok(out)
}

/// Serializes patterns as `Patt` / `Pat2` / `Pat3` block data.
pub fn write_pattern_block(patterns: &[PsdPattern]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for p in patterns {
        let mut one = Vec::new();
        write_pattern(p, &mut one)?;
        out.put_u32(one.len() as u32);
        let pad = (4 - one.len() % 4) % 4;
        out.put(&one);
        out.put(&vec![0u8; pad]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_block_round_trip() {
        let p = PsdPattern {
            mode: 3, // RGB
            width: 4,
            height: 4,
            name: "Test Pattern".into(),
            id: "uuid-1234".into(),
            palette: None,
            depth: 8,
            channels: vec![vec![100; 16], vec![150; 16], vec![200; 16]],
            alpha: Some(vec![255; 16]),
        };
        let bytes = write_pattern_block(std::slice::from_ref(&p)).unwrap();
        let decoded = parse_pattern_block(&bytes).unwrap();
        assert_eq!(decoded, vec![p]);
    }

    #[test]
    fn an_indexed_pattern_is_read_with_or_without_bytes_after_its_palette() {
        let p = PsdPattern {
            mode: 2,
            width: 2,
            height: 2,
            name: "Indexed".into(),
            id: "abc".into(),
            palette: Some((0..768).map(|i| i as u8).collect()),
            depth: 8,
            channels: vec![vec![0, 1, 2, 3]],
            alpha: None,
        };
        let bytes = write_pattern_block(std::slice::from_ref(&p)).unwrap();
        assert_eq!(parse_pattern_block(&bytes).unwrap(), vec![p.clone()]);

        // Four more bytes after the palette, as some writers leave: the
        // pattern is 4 longer, and stays a multiple of 4.
        let palette = bytes.windows(768).position(|w| w == &p.palette.as_ref().unwrap()[..]).unwrap() + 768;
        let mut extra = bytes.clone();
        extra.splice(palette..palette, [9, 9, 9, 9]);
        let len = u32::from_be_bytes(extra[..4].try_into().unwrap()) + 4;
        extra[..4].copy_from_slice(&len.to_be_bytes());
        assert_eq!(parse_pattern_block(&extra).unwrap(), vec![p]);
    }
}
