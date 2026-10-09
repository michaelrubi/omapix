// Ported from PhotoCraft's `crates/psd/src/io.rs` and `compression.rs`
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

//! Bounds-checked big-endian binary reader and writer helpers for PSD/ABR data.

use crate::{Error, Result};

fn bad(what: impl std::fmt::Display) -> Error {
    Error::Unsupported(format!("PSD/ABR: {what}"))
}

/// A cursor over a byte slice. Every read is bounds checked.
#[derive(Debug, Clone)]
pub(crate) struct ByteReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        ByteReader { data, pos: 0 }
    }

    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    pub(crate) fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    pub(crate) fn peek_rest(&self) -> &'a [u8] {
        &self.data[self.pos.min(self.data.len())..]
    }

    pub(crate) fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&end| end <= self.data.len())
            .ok_or_else(|| bad("the data is truncated"))?;
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    pub(crate) fn bytes_u64(&mut self, n: u64) -> Result<&'a [u8]> {
        let n = usize::try_from(n).map_err(|_| bad("length exceeds address space"))?;
        self.bytes(n)
    }

    pub(crate) fn skip(&mut self, n: usize) -> Result<()> {
        self.bytes(n).map(|_| ())
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let s = self.bytes(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(s);
        Ok(out)
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16> {
        self.array().map(u16::from_be_bytes)
    }

    pub(crate) fn u32(&mut self) -> Result<u32> {
        self.array().map(u32::from_be_bytes)
    }

    pub(crate) fn i32(&mut self) -> Result<i32> {
        self.array().map(i32::from_be_bytes)
    }

    pub(crate) fn i64(&mut self) -> Result<i64> {
        self.array().map(i64::from_be_bytes)
    }

    pub(crate) fn f64(&mut self) -> Result<f64> {
        self.array().map(f64::from_be_bytes)
    }

    pub(crate) fn check_count(&self, count: u64, item_size: u64) -> Result<()> {
        if count.saturating_mul(item_size) > self.remaining() as u64 {
            return Err(bad("unexpected end of data for declared count"));
        }
        Ok(())
    }
}

pub(crate) trait WriteExt {
    fn put(&mut self, bytes: &[u8]);
    fn put_u8(&mut self, v: u8);
    fn put_u16(&mut self, v: u16);
    fn put_u32(&mut self, v: u32);
    fn put_i32(&mut self, v: i32);
    fn put_i64(&mut self, v: i64);
    fn put_f64(&mut self, v: f64);
}

impl WriteExt for Vec<u8> {
    fn put(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }
    fn put_u8(&mut self, v: u8) {
        self.push(v);
    }
    fn put_u16(&mut self, v: u16) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn put_u32(&mut self, v: u32) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn put_i32(&mut self, v: i32) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn put_i64(&mut self, v: i64) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn put_f64(&mut self, v: f64) {
        self.extend_from_slice(&v.to_be_bytes());
    }
}

pub(crate) fn read_unicode_units(r: &mut ByteReader<'_>) -> Result<Vec<u16>> {
    let n = r.u32()? as u64;
    r.check_count(n, 2)?;
    let mut v = Vec::with_capacity(n as usize);
    for _ in 0..n {
        v.push(r.u16()?);
    }
    Ok(v)
}

pub(crate) fn write_unicode_units(out: &mut Vec<u8>, units: &[u16]) {
    out.put_u32(units.len() as u32);
    for &u in units {
        out.put_u16(u);
    }
}

/// PackBits (Apple / TIFF / Photoshop) run-length encoding and decoding.
pub(crate) mod packbits {
    use super::*;

    /// Encodes `src` using PackBits RLE, appending to `out`.
    pub fn encode(src: &[u8], out: &mut Vec<u8>) {
        let n = src.len();
        let mut i = 0;
        while i < n {
            let mut run = 1;
            while i + run < n && run < 128 && src[i + run] == src[i] {
                run += 1;
            }
            if run >= 3 || (run == 2 && i + run == n) {
                out.push((1 - run as i16) as u8);
                out.push(src[i]);
                i += run;
            } else {
                let mut lit = 1;
                while i + lit < n && lit < 128 {
                    if i + lit + 2 < n && src[i + lit] == src[i + lit + 1] && src[i + lit] == src[i + lit + 2] {
                        break;
                    }
                    lit += 1;
                }
                out.push((lit - 1) as u8);
                out.extend_from_slice(&src[i..i + lit]);
                i += lit;
            }
        }
    }

    /// Decodes PackBits bytes from `src` into `out` until exactly `expected` bytes are appended.
    pub fn decode_into(src: &[u8], expected: usize, out: &mut Vec<u8>) -> Result<()> {
        let target = out.len() + expected;
        let mut i = 0;
        while out.len() < target {
            let Some(&h) = src.get(i) else {
                return Err(bad("PackBits row ended early"));
            };
            i += 1;
            let h = h as i8;
            if h >= 0 {
                let len = h as usize + 1;
                let Some(lit) = src.get(i..i + len) else {
                    return Err(bad("PackBits literal truncated"));
                };
                if out.len() + len > target {
                    return Err(bad("PackBits literal overflows row"));
                }
                out.extend_from_slice(lit);
                i += len;
            } else if h != -128 {
                let len = (1 - h as isize) as usize;
                let Some(&b) = src.get(i) else {
                    return Err(bad("PackBits run truncated"));
                };
                i += 1;
                if out.len() + len > target {
                    return Err(bad("PackBits run overflows row"));
                }
                out.extend(std::iter::repeat_n(b, len));
            }
        }
        Ok(())
    }
}
