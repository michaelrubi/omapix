//! Filling a hole with texture copied from round it, at the image's own
//! size and with no model (Edit › Texture Fill, and Content-Aware Fill
//! when its model isn't installed).
//!
//! Every patch that overlaps the hole finds the patch outside it that looks
//! most like it (PatchMatch: Barnes, Shechtman, Finkelstein and Goldman,
//! 2009), then each pixel in the hole becomes the average of what those
//! matches say it should be, the closer matches counting for more, and
//! that's repeated (Wexler, Shechtman and Irani, "Space-Time Completion of
//! Video", 2007). It starts on a copy shrunk until the hole is a couple of
//! patches across and works up a size at a time, so the shape of the fill
//! is settled while it's small and the larger sizes only add detail.
//! Afterwards the fill's tone is pulled part of the way to a smooth fill
//! ([`crate::poisson`]), so it follows a backdrop's gradient.
//!
//! Three things keep the fill from going soft, which is what averaging
//! does to it otherwise, since a soft patch's best match is another soft
//! one (after Newson, Almansa, Gousseau and Pérez, "Non-Local Patch-Based
//! Image Inpainting", 2017):
//!
//! - The smallest size starts from the hole's edge inwards, each pixel
//!   copied from the patch most like what's beside it, not as a smooth
//!   fill.
//! - Each size above starts as what the size below's matches copy at this
//!   size, not as the size below's fill scaled up.
//! - Patches are matched by texture as well as colour: how much each pixel
//!   differs from its neighbours, measured at full size and averaged down,
//!   so every size knows how sharp each part of the photo is.
//!
//! The same input always gives the same fill, however many threads run it.
//!
//! Ported from PhotoCraft's `crates/algo/src/inpaint.rs` and
//! `content_aware.rs` (<https://github.com/storytold/photocraft>, commit
//! `ec477ca`), under its MIT licence, with the three changes above:
//!
//! Copyright (c) 2026 ArtCraft Team and the PhotoCraft contributors
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.

use rayon::prelude::*;

use crate::filters::box_filter_2d;
use crate::poisson::membrane_fill;

type Rgb = [f32; 3];
/// A pixel as it's matched: its colour, then how much it differs from the
/// pixels beside it and from those above and below.
type Px = [f32; 5];

/// Patches are 7 pixels square.
const RADIUS: i32 = 3;
const PATCH: usize = 2 * RADIUS as usize + 1;
/// Rounds of matching and averaging at the smallest size, halved for each
/// size above it, down to `MIN_ROUNDS`.
const ROUNDS: usize = 8;
const MIN_ROUNDS: usize = 2;
/// Passes over the matches in each round.
const PASSES: usize = 2;
/// Sizes where the hole is wider than this only touch up the matches
/// handed up to them, which keeps big holes quick.
const REFINE_EXTENT: usize = 96;
/// How far the fill's tone is pulled to the smooth fill (Photoshop's
/// Color Adaptation at its default).
const COLOR_ADAPTATION: f32 = 0.35;
/// How much texture counts in a match beside colour.
const TEXTURE: f32 = 10.0;
const SEED: u64 = 1;
/// How many sources the hole's first fill chooses between.
const PEEL_SOURCES: usize = 4000;
/// Smaller sizes than this aren't worth sharing between threads.
const PARALLEL: usize = 128 * 128;
/// Rows that one thread matches at a time.
const BAND: usize = 16;

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// From `lo` to `hi`, both included.
    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next() % (hi - lo + 1) as u64) as i32
    }
}

/// The image at one size.
struct Level {
    w: usize,
    h: usize,
    img: Vec<Px>,
    hole: Vec<bool>,
}

/// `l` at half the size: each pixel the mean of those of its four outside
/// the hole, and in the hole if any of them is.
fn halve(l: &Level) -> Level {
    let (w, h) = (l.w.div_ceil(2), l.h.div_ceil(2));
    let mut img = vec![[0.0f32; 5]; w * h];
    let mut hole = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut n = 0.0f32;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (sx, sy) = (x * 2 + dx, y * 2 + dy);
                if sx >= l.w || sy >= l.h {
                    continue;
                }
                let si = sy * l.w + sx;
                if l.hole[si] {
                    hole[y * w + x] = true;
                } else {
                    for (sum, v) in img[y * w + x].iter_mut().zip(l.img[si]) {
                        *sum += v;
                    }
                    n += 1.0;
                }
            }
            if n > 0.0 {
                img[y * w + x] = img[y * w + x].map(|v| v / n);
            }
        }
    }
    Level { w, h, img, hole }
}

/// Running totals of `m` (`w` × `h`), a row and a column larger, for
/// counting how much of a rectangle is set.
fn totals(w: usize, h: usize, m: &[bool]) -> Vec<u32> {
    let mut s = vec![0u32; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0u32;
        for x in 0..w {
            row += u32::from(m[y * w + x]);
            s[(y + 1) * (w + 1) + x + 1] = s[y * (w + 1) + x + 1] + row;
        }
    }
    s
}

/// How many pixels are set from (`x0`, `y0`) up to but not including
/// (`x1`, `y1`), as far as that's inside the image.
fn count(s: &[u32], w: usize, h: usize, x0: i32, y0: i32, x1: i32, y1: i32) -> u32 {
    let (x0, y0) = (x0.max(0) as usize, y0.max(0) as usize);
    let (x1, y1) = ((x1.max(0) as usize).min(w), (y1.max(0) as usize).min(h));
    if x1 <= x0 || y1 <= y0 {
        return 0;
    }
    let w1 = w + 1;
    s[y1 * w1 + x1] + s[y0 * w1 + x0] - s[y0 * w1 + x1] - s[y1 * w1 + x0]
}

/// The hole's longer side.
fn extent(w: usize, h: usize, hole: &[bool]) -> Option<usize> {
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (i, _) in hole.iter().enumerate().filter(|(_, hole)| **hole) {
        let (x, y) = (i % w, i / w);
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
    }
    (x1 > x0).then(|| (x1 - x0).max(y1 - y0))
}

/// Matching and averaging at one size.
struct Solver<'a> {
    w: usize,
    h: usize,
    img: &'a mut Vec<Px>,
    hole: &'a [bool],
    /// Where a patch to copy can be centred: all of it's inside the image
    /// and none of it in the hole.
    sources: Vec<bool>,
    /// Where a patch overlaps the hole, so needs a match.
    targets: Vec<bool>,
}

impl Solver<'_> {
    fn is_source(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h && self.sources[y as usize * self.w + x as usize]
    }

    /// How unlike the patches at `t` (as much of it as is inside the image)
    /// and `s` are: the mean squared difference. Infinite once it's sure to
    /// pass `cutoff`.
    fn difference(&self, t: (i32, i32), s: (i32, i32), cutoff: f32) -> f32 {
        let (w, h) = (self.w as i32, self.h as i32);
        let full = (PATCH * PATCH * 5) as f32;
        let (mut sum, mut n) = (0.0f32, 0usize);
        for dy in -RADIUS..=RADIUS {
            let ty = t.1 + dy;
            if ty < 0 || ty >= h {
                continue;
            }
            let sy = s.1 + dy;
            for dx in -RADIUS..=RADIUS {
                let tx = t.0 + dx;
                if tx < 0 || tx >= w {
                    continue;
                }
                let a = self.img[ty as usize * self.w + tx as usize];
                let b = self.img[sy as usize * self.w + (s.0 + dx) as usize];
                for c in 0..5 {
                    sum += (a[c] - b[c]) * (a[c] - b[c]);
                }
                n += 5;
            }
            if sum > cutoff * full {
                return f32::INFINITY;
            }
        }
        if n == 0 { f32::INFINITY } else { sum / n as f32 }
    }

    /// Start the hole off from its edge inwards, a ring a pixel wide at a
    /// time: each pixel copies the middle of the source patch most like
    /// what's already there round it. Returns where each came from. This
    /// gives the hole the texture next to it, where a smooth start would
    /// only ever match the smoothest thing in the image.
    fn peel(&mut self, sources: &[(i32, i32)]) -> Vec<Option<(i32, i32)>> {
        let (w, h) = (self.w, self.h);
        let mut known: Vec<bool> = self.hole.iter().map(|hole| !hole).collect();
        let mut from = vec![None; w * h];
        // A few thousand sources spread over the image are enough to
        // start from.
        let some: Vec<(i32, i32)> = sources.iter().copied().step_by((sources.len() / PEEL_SOURCES).max(1)).collect();
        loop {
            let near = |i: usize, known: &[bool]| {
                let (x, y) = ((i % w) as i32, (i / w) as i32);
                (-1..=1).any(|dy| (-1..=1).any(|dx| self.inside(x + dx, y + dy) && known[(y + dy) as usize * w + (x + dx) as usize]))
            };
            let ring: Vec<usize> = (0..w * h).filter(|&i| !known[i] && near(i, &known)).collect();
            if ring.is_empty() {
                return from;
            }
            let img = &*self.img;
            let picked: Vec<(i32, i32)> = ring
                .par_iter()
                .map(|&i| {
                    let (x, y) = ((i % w) as i32, (i / w) as i32);
                    let mut best = (some[0], f32::INFINITY);
                    for &s in &some {
                        let (mut sum, mut n) = (0.0f32, 0.0f32);
                        for dy in -RADIUS..=RADIUS {
                            for dx in -RADIUS..=RADIUS {
                                if !self.inside(x + dx, y + dy) || !known[(y + dy) as usize * w + (x + dx) as usize] {
                                    continue;
                                }
                                let a = img[(y + dy) as usize * w + (x + dx) as usize];
                                let b = img[(s.1 + dy) as usize * w + (s.0 + dx) as usize];
                                sum += (0..5).map(|c| (a[c] - b[c]) * (a[c] - b[c])).sum::<f32>();
                                n += 1.0;
                            }
                            if sum > best.1 * (PATCH * PATCH) as f32 {
                                break;
                            }
                        }
                        if sum / n < best.1 {
                            best = (s, sum / n);
                        }
                    }
                    best.0
                })
                .collect();
            for (&i, s) in ring.iter().zip(picked) {
                self.img[i] = self.img[s.1 as usize * w + s.0 as usize];
                from[i] = Some(s);
                known[i] = true;
            }
        }
    }

    fn inside(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h
    }

    /// Make `candidate` the target `t`'s `best` match (with its cost) if
    /// it's a source and closer.
    fn consider(&self, t: (i32, i32), candidate: (i32, i32), best: &mut ((i32, i32), f32)) {
        if self.is_source(candidate.0, candidate.1) && candidate != best.0 {
            let d = self.difference(t, candidate, best.1);
            if d < best.1 {
                *best = (candidate, d);
            }
        }
    }

    /// Improve the matches once: each target tries its neighbours' matches
    /// shifted to suit it, which spreads good ones, then random patches
    /// ever nearer its own, from `reach` pixels away. Bands of rows are
    /// done side by side, each spreading matches only within itself.
    fn improve(&self, matches: &mut [(i32, i32)], cost: &mut [f32], pass: usize, seed: u64, reach: i32) {
        let w = self.w;
        let backwards = pass % 2 == 1;
        let band = |b: usize, matches: &mut [(i32, i32)], cost: &mut [f32]| {
            let mut rng = Rng::new(seed ^ (b as u64).wrapping_mul(0x2545_F491_4F6C_DD1D) ^ ((pass as u64) << 40));
            let rows = matches.len() / w;
            let step: i32 = if backwards { -1 } else { 1 };
            for ry in 0..rows {
                let ly = if backwards { rows - 1 - ry } else { ry };
                let y = b * BAND + ly;
                for rx in 0..w {
                    let x = if backwards { w - 1 - rx } else { rx };
                    let li = ly * w + x;
                    if !self.targets[y * w + x] {
                        continue;
                    }
                    let t = (x as i32, y as i32);
                    let mut best = (matches[li], cost[li]);
                    let nx = x as i32 - step;
                    if nx >= 0 && (nx as usize) < w {
                        let m = matches[ly * w + nx as usize];
                        self.consider(t, (m.0 + step, m.1), &mut best);
                    }
                    let ny = ly as i32 - step;
                    if ny >= 0 && (ny as usize) < rows {
                        let m = matches[ny as usize * w + x];
                        self.consider(t, (m.0, m.1 + step), &mut best);
                    }
                    let mut radius = reach;
                    while radius >= 1 {
                        let (bx, by) = best.0;
                        self.consider(t, (bx + rng.range(-radius, radius), by + rng.range(-radius, radius)), &mut best);
                        radius /= 2;
                    }
                    (matches[li], cost[li]) = best;
                }
            }
        };
        if w * self.h >= PARALLEL {
            matches.par_chunks_mut(BAND * w).zip(cost.par_chunks_mut(BAND * w)).enumerate().for_each(|(b, (m, c))| band(b, m, c));
        } else {
            for (b, (m, c)) in matches.chunks_mut(BAND * w).zip(cost.chunks_mut(BAND * w)).enumerate() {
                band(b, m, c);
            }
        }
    }

    /// Make each pixel in the hole the average of what the matches of the
    /// patches over it say it is, the closer matches counting for more.
    fn average(&mut self, matches: &[(i32, i32)], cost: &[f32]) {
        let (w, h) = (self.w, self.h);
        // How close is close: by the cost three quarters of the matches
        // come in under.
        let mut costs: Vec<f32> = cost.iter().zip(&self.targets).filter(|(c, t)| **t && c.is_finite()).map(|(c, _)| *c).collect();
        let spread = if costs.is_empty() {
            1.0
        } else {
            let k = (costs.len() * 3 / 4).min(costs.len() - 1);
            costs.select_nth_unstable_by(k, |a, b| a.total_cmp(b)).1.max(1e-6)
        };
        let img = &*self.img;
        let row = |y: usize| -> Vec<(usize, Px)> {
            let mut out = Vec::new();
            for x in (0..w).filter(|x| self.hole[y * w + x]) {
                let (mut sum, mut weights) = ([0.0f32; 5], 0.0f32);
                for dy in -RADIUS..=RADIUS {
                    let ty = y as i32 + dy;
                    if ty < 0 || ty as usize >= h {
                        continue;
                    }
                    for dx in -RADIUS..=RADIUS {
                        let tx = x as i32 + dx;
                        if tx < 0 || tx as usize >= w {
                            continue;
                        }
                        let ti = ty as usize * w + tx as usize;
                        if !self.targets[ti] || !cost[ti].is_finite() {
                            continue;
                        }
                        let s = matches[ti];
                        let weight = (-cost[ti] / (2.0 * spread)).exp().max(1e-8);
                        let p = img[(s.1 - dy) as usize * w + (s.0 - dx) as usize];
                        for c in 0..5 {
                            sum[c] += p[c] * weight;
                        }
                        weights += weight;
                    }
                }
                if weights > 0.0 {
                    out.push((x, sum.map(|v| v / weights)));
                }
            }
            out
        };
        let rows: Vec<Vec<(usize, Px)>> = if w * h >= PARALLEL {
            (0..h).into_par_iter().map(row).collect()
        } else {
            (0..h).map(row).collect()
        };
        for (y, row) in rows.into_iter().enumerate() {
            for (x, p) in row {
                self.img[y * w + x] = p;
            }
        }
    }
}

/// `img` (`w` × `h`) with `hole` filled from patches outside it. `None` if
/// there's nowhere outside it to fit a patch.
fn complete(w: usize, h: usize, img: &[Px], hole: &[bool]) -> Option<Vec<Px>> {
    let mut across = extent(w, h, hole)?;
    // Halve it until the hole is a couple of patches across.
    let mut levels = vec![Level { w, h, img: img.to_vec(), hole: hole.to_vec() }];
    let mut extents = vec![across];
    while let Some(l) = levels.last() {
        if across <= 2 * PATCH || l.w / 2 < 3 * PATCH || l.h / 2 < 3 * PATCH {
            break;
        }
        let half = halve(l);
        levels.push(half);
        across = across.div_ceil(2);
        extents.push(across);
    }
    let smallest = levels.len() - 1;
    let mut matches: Vec<(i32, i32)> = Vec::new();
    // The size below, once filled.
    let mut below: Option<(usize, usize, Vec<Px>)> = None;
    while let Some(Level { w: lw, h: lh, mut img, hole }) = levels.pop() {
        let li = levels.len();
        // The hole starts as the smooth fill at the smallest size, and as
        // the size below's fill after that.
        match &below {
            None => img = membrane_fill(lw, lh, &img, &hole),
            Some((bw, bh, filled)) => {
                for i in (0..lw * lh).filter(|&i| hole[i]) {
                    img[i] = filled[(i / lw / 2).min(bh - 1) * bw + (i % lw / 2).min(bw - 1)];
                }
            }
        }
        let holes = totals(lw, lh, &hole);
        let mut sources = vec![false; lw * lh];
        let mut source_list = Vec::new();
        let mut targets = vec![false; lw * lh];
        for y in 0..lh as i32 {
            for x in 0..lw as i32 {
                let in_hole = count(&holes, lw, lh, x - RADIUS, y - RADIUS, x + RADIUS + 1, y + RADIUS + 1);
                let inside = x >= RADIUS && y >= RADIUS && x + RADIUS < lw as i32 && y + RADIUS < lh as i32;
                let i = y as usize * lw + x as usize;
                if inside && in_hole == 0 {
                    sources[i] = true;
                    source_list.push((x, y));
                }
                targets[i] = in_hole > 0;
            }
        }
        if source_list.is_empty() {
            if li == 0 {
                return None;
            }
            below = Some((lw, lh, img));
            matches.clear();
            continue;
        }
        let mut solver = Solver { w: lw, h: lh, img: &mut img, hole: &hole, sources, targets };
        // Matches start as the size below's, doubled, and at random where
        // that doesn't land on a source.
        let mut rng = Rng::new(SEED ^ (li as u64) << 20);
        let bw = below.as_ref().map_or(0, |b| b.0);
        let old = std::mem::take(&mut matches);
        matches = vec![(0, 0); lw * lh];
        let mut cost = vec![f32::INFINITY; lw * lh];
        for i in (0..lw * lh).filter(|&i| solver.targets[i]) {
            let (x, y) = (i % lw, i / lw);
            let handed_up = old
                .get((y / 2) * bw + x / 2)
                .map(|&(sx, sy)| (sx * 2 + (x % 2) as i32, sy * 2 + (y % 2) as i32))
                .filter(|&(sx, sy)| bw > 0 && solver.is_source(sx, sy));
            if handed_up.is_some() {
                cost[i] = 0.0;
            }
            matches[i] = handed_up.unwrap_or_else(|| source_list[(rng.next() % source_list.len() as u64) as usize]);
        }
        if !old.is_empty() {
            // The fill at this size's detail: what the size below's
            // matches copy here. Its own fill, doubled, would be soft, and
            // soft patches only match soft ones.
            solver.average(&matches, &cost);
        } else {
            for (i, from) in solver.peel(&source_list).into_iter().enumerate() {
                matches[i] = from.unwrap_or(matches[i]);
            }
        }
        let refining = li < smallest && extents[li] > REFINE_EXTENT;
        let rounds = if li == smallest {
            ROUNDS
        } else if refining {
            1
        } else {
            (ROUNDS >> (smallest - li)).max(MIN_ROUNDS)
        };
        let passes = if refining { 1 } else { PASSES };
        let anywhere = lw.max(lh) as i32;
        for round in 0..rounds {
            // The hole has changed since the matches were costed.
            for i in (0..lw * lh).filter(|&i| solver.targets[i]) {
                cost[i] = solver.difference(((i % lw) as i32, (i / lw) as i32), matches[i], f32::INFINITY);
            }
            // Look anywhere at the smallest size and in each size's first
            // round, then only nearby.
            let reach = if refining {
                2
            } else if li == smallest || round == 0 {
                anywhere
            } else {
                (4 * PATCH as i32).min(anywhere)
            };
            for pass in 0..passes {
                solver.improve(&mut matches, &mut cost, pass + round * passes, SEED.wrapping_add((li * 1000 + round) as u64), reach);
            }
            solver.average(&matches, &cost);
        }
        below = Some((lw, lh, img));
    }
    below.map(|(_, _, img)| img)
}

/// `img` (`w` × `h`, in any colour space) with `hole` filled from the rest
/// of it, apart from the pixels marked `empty`, which have nothing in them
/// to copy and are left as they are. A hole with nowhere to copy from is
/// filled smoothly.
pub fn fill(w: usize, h: usize, img: &[Rgb], hole: &[bool], empty: &[bool]) -> Vec<Rgb> {
    assert_eq!(img.len(), w * h);
    assert_eq!(hole.len(), w * h);
    assert_eq!(empty.len(), w * h);
    if !hole.contains(&true) {
        return img.to_vec();
    }
    // Empty pixels are filled as well, so that no patch copies them, and
    // then put back.
    let unknown: Vec<bool> = hole.iter().zip(empty).map(|(hole, empty)| *hole || *empty).collect();
    let smooth = membrane_fill(w, h, img, &unknown);
    let filled = complete(w, h, &with_texture(w, h, img, &unknown), &unknown);
    let filled: Vec<Rgb> = filled.map_or_else(|| smooth.clone(), |f| f.iter().map(|p| [p[0], p[1], p[2]]).collect());
    // Pull the fill's tone, blurred to leave its texture out, to the
    // smooth fill's.
    let across = extent(w, h, hole).unwrap_or(0);
    let tone = box_filter_2d(filled.iter().map(|p| [p[0], p[1], p[2], 0.0]).collect(), w, h, (across / 6).clamp(1, 32));
    let mut out = img.to_vec();
    for i in (0..w * h).filter(|&i| hole[i]) {
        for c in 0..3 {
            out[i][c] = (filled[i][c] + COLOR_ADAPTATION * (smooth[i][c] - tone[i][c])).clamp(0.0, 1.0);
        }
    }
    out
}

/// `img` with each known pixel's texture after its colour: how much its
/// lightness differs from the known pixels beside it, and from those above
/// and below, times [`TEXTURE`]. Halved along with the colours, it tells
/// sharp parts of the image from soft ones at every size, so a hole in a
/// soft part isn't filled from a sharp one, or the other way round.
fn with_texture(w: usize, h: usize, img: &[Rgb], unknown: &[bool]) -> Vec<Px> {
    let light = |i: usize| (img[i][0] + img[i][1] + img[i][2]) / 3.0;
    (0..w * h)
        .into_par_iter()
        .map(|i| {
            let [r, g, b] = img[i];
            if unknown[i] {
                return [r, g, b, 0.0, 0.0];
            }
            let (x, y) = (i % w, i / w);
            let step = |to: [Option<usize>; 2]| {
                let (sum, n) = to.into_iter().flatten().filter(|&j| !unknown[j]).fold((0.0f32, 0.0f32), |(s, n), j| (s + (light(j) - light(i)).abs(), n + 1.0));
                if n == 0.0 { 0.0 } else { sum / n * TEXTURE }
            };
            let across = step([(x > 0).then(|| i - 1), (x + 1 < w).then(|| i + 1)]);
            let down = step([(y > 0).then(|| i - w), (y + 1 < h).then(|| i + w)]);
            [r, g, b, across, down]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gradient with fine noise over it.
    fn texture(w: usize, h: usize) -> Vec<Rgb> {
        let mut rng = Rng::new(7);
        (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                let n = (rng.next() % 1000) as f32 / 1000.0 * 0.04 - 0.02;
                [0.3 + 0.004 * x + n, 0.5 + 0.002 * y + n, 0.4 + n]
            })
            .collect()
    }

    fn disc(w: usize, h: usize, cx: f32, cy: f32, r: f32) -> Vec<bool> {
        (0..w * h).map(|i| ((i % w) as f32 - cx).hypot((i / w) as f32 - cy) < r).collect()
    }

    /// The root mean square difference between `a` and `b` in the hole.
    fn error(a: &[Rgb], b: &[Rgb], hole: &[bool]) -> f32 {
        let (mut e, mut n) = (0.0f32, 0.0f32);
        for i in (0..hole.len()).filter(|&i| hole[i]) {
            for c in 0..3 {
                e += (a[i][c] - b[i][c]).powi(2);
                n += 1.0;
            }
        }
        (e / n).sqrt()
    }

    /// How rough the hole is: the root mean square step between pixels
    /// next to each other in it.
    fn roughness(w: usize, img: &[Rgb], hole: &[bool]) -> f32 {
        let (mut e, mut n) = (0.0f32, 0.0f32);
        for i in (0..hole.len() - 1).filter(|&i| hole[i] && hole[i + 1] && (i + 1) % w != 0) {
            e += (img[i][0] - img[i + 1][0]).powi(2);
            n += 1.0;
        }
        (e / n).sqrt()
    }

    #[test]
    fn a_dark_spot_on_texture_is_filled_with_the_texture() {
        let (w, h) = (96, 96);
        let clean = texture(w, h);
        let hole = disc(w, h, 48.0, 48.0, 9.0);
        let dirty: Vec<Rgb> = clean.iter().zip(&hole).map(|(p, &hole)| if hole { [0.02; 3] } else { *p }).collect();
        let none = vec![false; w * h];
        let out = fill(w, h, &dirty, &hole, &none);
        let (before, after) = (error(&dirty, &clean, &hole), error(&out, &clean, &hole));
        assert!(after < 0.06 && after < before * 0.15, "from {before} to {after}");
        // It has the texture's grain, which a smooth fill hasn't.
        let (grain, filled) = (roughness(w, &clean, &hole), roughness(w, &out, &hole));
        let smooth = roughness(w, &membrane_fill(w, h, &dirty, &hole), &hole);
        assert!(filled > grain * 0.5 && smooth < grain * 0.5, "{grain} {filled} {smooth}");
        // Nothing outside the hole has changed, and it's the same again.
        assert!((0..w * h).all(|i| hole[i] || out[i] == dirty[i]));
        assert_eq!(out, fill(w, h, &dirty, &hole, &none));
        // Nothing to fill: nothing done.
        assert_eq!(fill(w, h, &dirty, &none, &none), dirty);
    }

    #[test]
    fn stripes_carry_on_across_the_hole() {
        let (w, h) = (80, 80);
        let clean: Vec<Rgb> = (0..w * h).map(|i| if (i % w / 4) % 2 == 0 { [0.2; 3] } else { [0.8; 3] }).collect();
        let hole = disc(w, h, 40.0, 40.0, 7.0);
        let dirty: Vec<Rgb> = clean.iter().zip(&hole).map(|(p, &hole)| if hole { [0.5; 3] } else { *p }).collect();
        let out = fill(w, h, &dirty, &hole, &vec![false; w * h]);
        assert!(error(&out, &clean, &hole) < 0.15, "{}", error(&out, &clean, &hole));
    }

    #[test]
    fn empty_pixels_are_not_copied_and_stay_as_they_were() {
        // Bright on the left, and nothing on the right (black, as empty
        // pixels are): the hole between them is filled bright.
        let (w, h) = (60, 30);
        let img: Vec<Rgb> = (0..w * h).map(|i| if i % w < 36 { [0.9; 3] } else { [0.0; 3] }).collect();
        let empty: Vec<bool> = (0..w * h).map(|i| i % w >= 36).collect();
        let hole: Vec<bool> = (0..w * h).map(|i| (24..36).contains(&(i % w)) && (9..21).contains(&(i / w))).collect();
        let out = fill(w, h, &img, &hole, &empty);
        for i in 0..w * h {
            if hole[i] {
                assert!(out[i][0] > 0.85, "{:?}", out[i]);
            } else {
                assert_eq!(out[i], img[i]);
            }
        }
    }

    #[test]
    fn the_fill_follows_a_gradient() {
        let (w, h) = (80, 40);
        let img: Vec<Rgb> = (0..w * h).map(|i| [(i % w) as f32 / w as f32; 3]).collect();
        let hole: Vec<bool> = (0..w * h).map(|i| (28..52).contains(&(i % w)) && (10..30).contains(&(i / w))).collect();
        let out = fill(w, h, &img, &hole, &vec![false; w * h]);
        assert!(error(&out, &img, &hole) < 0.08, "{}", error(&out, &img, &hole));
    }

    #[test]
    fn a_hole_with_nowhere_to_copy_from_is_filled_smoothly() {
        // All but a rim one pixel wide: no room for a patch.
        let (w, h) = (20, 20);
        let img = vec![[0.25, 0.5, 0.75]; w * h];
        let hole: Vec<bool> = (0..w * h).map(|i| (1..19).contains(&(i % w)) && (1..19).contains(&(i / w))).collect();
        let dirty: Vec<Rgb> = img.iter().zip(&hole).map(|(p, &hole)| if hole { [0.0; 3] } else { *p }).collect();
        let out = fill(w, h, &dirty, &hole, &vec![false; w * h]);
        assert!(error(&out, &img, &hole) < 0.01, "{}", error(&out, &img, &hole));
    }
}
