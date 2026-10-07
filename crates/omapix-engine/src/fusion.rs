//! Merge to HDR: a bracket of exposures of one scene merged into an image
//! that keeps the best-exposed parts of each (exposure fusion, after
//! Mertens, Kautz and Van Reeth).
//!
//! Each exposure has a say at each pixel by how well exposed it is there
//! (near mid grey) and how colourful. The exposures aren't mixed by those
//! shares pixel by pixel, which would flatten the picture and leave halos:
//! each is split into its detail at every size (a Laplacian pyramid), and
//! the shares, smoothed to match, mix each size of detail. So fine detail
//! comes from whichever exposure has it, and large areas change over
//! gently.

use rayon::prelude::*;

use crate::align::align;
use crate::layer::Layer;
use crate::tiled::Tiled;
use crate::transform::{Resampling, projected};
use crate::{ColorProfile, Document, Pixel};

/// A grey image and its size.
type Plane = (Vec<f32>, usize, usize);

/// Half the size: every other pixel, each the average of the five by five
/// around it, the nearer the more.
fn halved((p, w, h): &Plane) -> Plane {
    const NEAR: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
    let (w, h) = (*w, *h);
    let (w2, h2) = (w.div_ceil(2), h.div_ceil(2));
    let around = |i: usize, k: usize, n: usize| (2 * i + k).saturating_sub(2).min(n - 1);
    let mut across = vec![0.0; w2 * h];
    across.par_chunks_mut(w2).zip(p.par_chunks(w)).for_each(|(out, row)| {
        for (x, o) in out.iter_mut().enumerate() {
            *o = (0..5).map(|k| NEAR[k] * row[around(x, k, w)]).sum();
        }
    });
    let mut out = vec![0.0; w2 * h2];
    out.par_chunks_mut(w2).enumerate().for_each(|(y, out)| {
        for (k, near) in NEAR.iter().enumerate() {
            let row = &across[around(y, k, h) * w2..][..w2];
            for (o, v) in out.iter_mut().zip(row) {
                *o += near * v;
            }
        }
    });
    (out, w2, h2)
}

/// Twice the size, `w` × `h`: [`halved`] the other way.
fn doubled((p, w2, h2): &Plane, w: usize, h: usize) -> Vec<f32> {
    let (w2, h2) = (*w2, *h2);
    // The pixels of the smaller one that make pixel `i`, and how much of each.
    let from = |i: usize, n: usize| {
        let at = i / 2;
        let next = (at + 1).min(n - 1);
        if i.is_multiple_of(2) {
            [(at.saturating_sub(1), 0.125), (at, 0.75), (next, 0.125)]
        } else {
            [(at, 0.5), (next, 0.5), (at, 0.0)]
        }
    };
    let mut across = vec![0.0; w * h2];
    across.par_chunks_mut(w).zip(p.par_chunks(w2)).for_each(|(out, row)| {
        for (x, o) in out.iter_mut().enumerate() {
            *o = from(x, w2).iter().map(|&(i, k)| k * row[i]).sum();
        }
    });
    let mut out = vec![0.0; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        for (i, k) in from(y, h2) {
            for (o, v) in out.iter_mut().zip(&across[i * w..][..w]) {
                *o += k * v;
            }
        }
    });
    out
}

/// `plane` at full size, half, a quarter and so on down to a few pixels.
fn sizes(plane: Plane) -> Vec<Plane> {
    let mut levels = vec![plane];
    while levels.last().is_some_and(|(_, w, h)| *w.min(h) >= 16) {
        levels.push(halved(levels.last().expect("just checked")));
    }
    levels
}

/// `plane`'s detail at each size: what each of its [`sizes`] has that the
/// next one down doesn't, and the smallest as it is. They add back up to
/// it ([`whole`]).
fn details(plane: Plane) -> Vec<Plane> {
    let mut levels = sizes(plane);
    for i in 0..levels.len() - 1 {
        let (below, rest) = levels.split_at_mut(i + 1);
        let (p, w, h) = &mut below[i];
        let smooth = doubled(&rest[0], *w, *h);
        p.par_iter_mut().zip(smooth).for_each(|(v, s)| *v -= s);
    }
    levels
}

fn whole(mut levels: Vec<Plane>) -> Vec<f32> {
    while levels.len() > 1 {
        let small = levels.pop().expect("more than one");
        let (p, w, h) = levels.last_mut().expect("more than one");
        let smooth = doubled(&small, *w, *h);
        p.par_iter_mut().zip(smooth).for_each(|(v, s)| *v += s);
    }
    levels.pop().map(|l| l.0).unwrap_or_default()
}

/// How much say a pixel has: most when it's mid grey, less the nearer
/// black or white it is, and a little more for being colourful. None
/// where it's clear.
fn say(p: Pixel) -> f32 {
    let [r, g, b] = [p[0], p[1], p[2]].map(|v| f32::from(v) / 65535.0);
    let grey = 0.299 * r + 0.587 * g + 0.114 * b;
    let mean = (r + g + b) / 3.0;
    let colour = (((r - mean).powi(2) + (g - mean).powi(2) + (b - mean).powi(2)) / 3.0).sqrt();
    let exposed = (-(grey - 0.5).powi(2) / (2.0 * 0.2 * 0.2)).exp();
    (exposed + 0.2 * colour + 1e-6) * f32::from(p[3]) / 65535.0
}

/// `images` (all one size, lined up) merged into one. Where one is clear
/// it has no say, and stands in the first one's colours so that its edge
/// isn't taken for detail; the result is clear where they all are.
pub fn fuse(images: &[&Tiled<Pixel>]) -> Tiled<Pixel> {
    let Some(first) = images.first() else {
        return Tiled::new(0, 0, [0; 4]);
    };
    let (w, h) = (first.width() as usize, first.height() as usize);
    // Each one's share of the say at each pixel.
    let mut shares: Vec<Vec<f32>> =
        images.iter().map(|image| image.to_vec().par_iter().map(|&p| say(p)).collect()).collect();
    let total: Vec<f32> = (0..w * h).into_par_iter().map(|i| shares.iter().map(|s| s[i]).sum()).collect();
    for share in &mut shares {
        share.par_iter_mut().zip(&total).for_each(|(s, t)| *s = if *t > 0.0 { *s / t } else { 0.0 });
    }
    let mut merged: [Vec<Plane>; 3] = Default::default();
    let mut alpha = vec![0u16; w * h];
    let stand_in = first.to_vec();
    for (image, share) in images.iter().zip(shares) {
        let pixels = image.to_vec();
        alpha.par_iter_mut().zip(&pixels).for_each(|(a, p)| *a = p[3].max(*a));
        let share = sizes((share, w, h));
        for (c, merged) in merged.iter_mut().enumerate() {
            let filled = |(p, under): (&Pixel, &Pixel)| {
                let a = f32::from(p[3]) / 65535.0;
                f32::from(p[c]) * a + f32::from(under[c]) * (1.0 - a)
            };
            let channel = pixels.par_iter().zip(&stand_in).map(filled).collect();
            let mut detail = details((channel, w, h));
            for ((d, ..), (s, ..)) in detail.iter_mut().zip(&share) {
                d.par_iter_mut().zip(s).for_each(|(d, s)| *d *= s);
            }
            if merged.is_empty() {
                *merged = detail;
            } else {
                for ((m, ..), (d, ..)) in merged.iter_mut().zip(detail) {
                    m.par_iter_mut().zip(d).for_each(|(m, d)| *m += d);
                }
            }
        }
    }
    let [r, g, b] = merged.map(whole);
    let out: Vec<Pixel> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let c = |v: f32| v.round().clamp(0.0, 65535.0) as u16;
            if alpha[i] == 0 { [0; 4] } else { [c(r[i]), c(g[i]), c(b[i]), alpha[i]] }
        })
        .collect();
    Tiled::from_slice(w as u32, h as u32, [0; 4], &out)
}

/// Photoshop's File › Automate › Merge to HDR, as far as 16 bits go:
/// `frames` (each a name and its pixels, in `profile`), a bracket of
/// exposures of one scene, as the layers of a new image the size of the
/// one the others are lined up with, hidden, under a "Merged" layer of
/// them all [`fuse`]d. Returns the image and the names of the frames
/// nothing lined up with, which are left out.
pub fn merge_to_hdr(frames: &[(String, Tiled<Pixel>)], profile: ColorProfile) -> Result<(Document, Vec<String>), String> {
    let images: Vec<&Tiled<Pixel>> = frames.iter().map(|f| &f.1).collect();
    let moves = align(&images, None);
    let left: Vec<String> = (0..frames.len()).filter(|&i| moves[i].is_none()).map(|i| frames[i].0.clone()).collect();
    let stays = moves.iter().position(|t| *t == Some(crate::transform::Projective::IDENTITY));
    let Some(stays) = stays.filter(|_| frames.len() - left.len() >= 2) else {
        return Err("the photos don't line up with each other".into());
    };
    let (width, height) = (images[stays].width(), images[stays].height());
    // Room for the largest to be moved in.
    let (most_wide, most_high) = images.iter().fold((0, 0), |(w, h), i| (w.max(i.width()), h.max(i.height())));
    let mut layers: Vec<Layer> = Vec::new();
    for (i, t) in moves.iter().enumerate() {
        let Some(t) = t else { continue };
        let pixels = if i == stays {
            images[i].clone()
        } else {
            let framed = images[i].reframed(most_wide, most_high, 0, 0, [0; 4]);
            projected(&framed, t, [0; 4], Resampling::Bicubic).reframed(width, height, 0, 0, [0; 4])
        };
        let mut layer = Layer::from_pixels(layers.len() as u64 + 1, frames[i].0.clone(), pixels);
        layer.visible = false;
        layers.push(layer);
    }
    // The one that stayed first: it's whole, to stand in for the others'
    // clear edges.
    let mut lined_up: Vec<&Tiled<Pixel>> = layers.iter().map(|l| &l.pixels).collect();
    lined_up.sort_by_key(|l| !l.same_tiles(images[stays]));
    let merged = fuse(&lined_up);
    layers.push(Layer::from_pixels(layers.len() as u64 + 1, "Merged", merged));
    Ok((Document::new(Default::default(), profile, 16, width, height, layers), left))
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 300;
    const H: u32 = 200;

    fn image(f: impl Fn(u32, u32) -> Pixel) -> Tiled<Pixel> {
        let px: Vec<Pixel> = (0..W * H).map(|i| f(i % W, i / W)).collect();
        Tiled::from_slice(W, H, [0; 4], &px)
    }

    fn grey(v: u16) -> Pixel {
        [v, v, v, 65535]
    }

    #[test]
    fn detail_at_every_size_adds_back_up_to_the_image() {
        // An odd size, so the halves don't divide evenly.
        let (w, h) = (301, 173);
        let plane: Vec<f32> = (0..w * h).map(|i| ((i * 7919) % 1000) as f32 + (i % w) as f32).collect();
        let levels = details((plane.clone(), w, h));
        assert!(levels.len() > 3 && levels.last().is_some_and(|(_, w, h)| *w.min(h) < 16));
        let back = whole(levels);
        let off = plane.iter().zip(&back).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(off < 0.01, "{off}");
    }

    #[test]
    fn each_part_comes_from_the_exposure_that_has_it() {
        // The lighter exposure burnt out on the right, the darker one
        // black on the left: what each has left is mid grey.
        let light = image(|x, _| grey(if x < 150 { 30000 } else { 65535 }));
        let dark = image(|x, _| grey(if x < 150 { 0 } else { 34000 }));
        let merged = fuse(&[&light, &dark]);
        let (left, right) = (merged.get(60, 100), merged.get(240, 100));
        assert!(left[0].abs_diff(30000) < 3000 && right[0].abs_diff(34000) < 3000, "{left:?} {right:?}");
        assert_eq!((left[3], right[3]), (65535, 65535));

        // Two the same merge into the same.
        let scene = image(|x, y| [(x * 200) as u16, (y * 300) as u16, 20000, 65535]);
        let same = fuse(&[&scene, &scene]);
        let off = |x, y| (0..3).map(|c| scene.get(x, y)[c].abs_diff(same.get(x, y)[c])).max().unwrap();
        assert!((0..W).step_by(7).all(|x| (0..H).step_by(5).all(|y| off(x, y) <= 2)));
    }

    #[test]
    fn a_clear_part_has_no_say_and_leaves_no_edge() {
        // The second exposure moved, so a strip of it is clear.
        let whole = image(|_, _| grey(30000));
        let moved = image(|x, _| if x < 40 { [0; 4] } else { grey(36000) });
        let merged = fuse(&[&whole, &moved]);
        assert!(merged.get(5, 100)[0].abs_diff(30000) <= 2 && merged.get(5, 100)[3] == 65535);
        // Between the two where both have a say, with nothing darker at
        // the strip's edge.
        let across: Vec<u16> = (0..W).map(|x| merged.get(x, 100)[0]).collect();
        assert!(across.iter().all(|v| (29990..=36010).contains(v)), "{:?}", across.iter().min());
        assert!(across[200].abs_diff(33000) < 1500, "{}", across[200]);
        // Clear in them all is clear.
        let none = fuse(&[&moved, &moved]);
        assert_eq!(none.get(5, 100), [0; 4]);
    }

    #[test]
    fn merge_to_hdr_lines_the_bracket_up_under_a_merged_layer() {
        // Grey rectangles, and the same a stop or so darker from where
        // the camera moved to.
        let mut px = vec![grey(30000); (W * H) as usize];
        let mut seed = 7u64;
        let mut random = |n: u32| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as u32 % n
        };
        for _ in 0..300 {
            let (x, y, v) = (random(W), random(H), 5000 + random(55000) as u16);
            for j in y..(y + 6 + random(30)).min(H) {
                for i in x..(x + 6 + random(30)).min(W) {
                    px[(j * W + i) as usize] = grey(v);
                }
            }
        }
        let light = Tiled::from_slice(W, H, [0; 4], &px);
        let dark = light.translated(6, -4, [0; 4]).map(|p| [p[0] / 2, p[1] / 2, p[2] / 2, p[3]]);
        let named = |name: &str, pixels: &Tiled<Pixel>| (name.to_owned(), pixels.clone());
        let frames = [named("Light", &light), named("Flat", &image(|_, _| grey(30000))), named("Dark", &dark)];
        let (doc, left) = merge_to_hdr(&frames, ColorProfile::srgb()).unwrap();
        assert_eq!(left, ["Flat"]);
        assert_eq!((doc.width, doc.height), (W, H));
        let layers: Vec<(&str, bool)> = doc.layers.iter().map(|l| (l.name.as_str(), l.visible)).collect();
        assert_eq!(layers, [("Light", false), ("Dark", false), ("Merged", true)]);
        // The dark one moved back onto the light one, and the merged
        // layer is between the two.
        let (x, y) = (150, 100);
        let [l, d, m] = [0, 1, 2].map(|i| doc.layers[i].pixels.get(x, y)[0]);
        assert!(d.abs_diff(l / 2) < 400, "{l} {d}");
        assert!(m >= d.min(l) && m <= d.max(l), "{l} {d} {m}");
        assert_eq!(doc.layers[2].pixels.get(2, 2)[3], 65535);

        assert!(merge_to_hdr(&frames[..2], ColorProfile::srgb()).is_err());
    }
}
