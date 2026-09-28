//! Moving part of a layer (the Move tool with a selection): the selected
//! pixels are lifted out, leaving the rest behind, and put back down at an
//! offset, or transformed (Free Transform). Moving a whole layer is just
//! [`Tiled::translated`].

use crate::Pixel;
use crate::tiled::{TILE_PIXELS, Tiled};
use crate::transform::{Affine, Resampling, transformed};

const MAX: u32 = u16::MAX as u32;

/// Pixels or mask values lifted out of a layer by a selection.
#[derive(Clone)]
pub struct Lifted<T> {
    /// What stays where it was.
    below: Tiled<T>,
    /// What moves.
    lifted: Tiled<T>,
    /// How much of each lifted value moves (the selection's coverage).
    /// Pixels carry this in their alpha instead.
    coverage: Tiled<u16>,
}

/// Coverage of a selection tile, or `None` where it selects nothing.
fn selected(selection: &Tiled<u16>, col: u32, row: u32) -> Option<Vec<u16>> {
    match selection.tile(col, row) {
        Some(t) => Some(t.to_vec()),
        None if selection.fill() == 0 => None,
        None => Some(vec![selection.fill(); TILE_PIXELS]),
    }
}

/// Scale a 16-bit value by a coverage, both out of `u16::MAX`.
fn scale(v: u16, k: u16) -> u16 {
    ((u32::from(v) * u32::from(k) + MAX / 2) / MAX) as u16
}

impl Lifted<Pixel> {
    /// Lift the selected pixels. Partly selected pixels are split between
    /// what moves and what stays by alpha. With `copy` (Alt+drag), the
    /// original stays whole.
    pub fn pixels(pixels: &Tiled<Pixel>, selection: &Tiled<u16>, copy: bool) -> Self {
        let (w, h, fill) = (pixels.width(), pixels.height(), pixels.fill());
        let tile = |col, row| {
            pixels
                .tile(col, row)
                .map_or_else(|| vec![fill; TILE_PIXELS], <[Pixel]>::to_vec)
        };
        let alpha = |keep: bool| {
            move |col, row| {
                let s = selected(selection, col, row)?;
                let mut t = tile(col, row);
                for (p, k) in t.iter_mut().zip(s) {
                    p[3] = scale(p[3], if keep { u16::MAX - k } else { k });
                }
                Some(t)
            }
        };
        let lifted = Tiled::from_tiles(w, h, [0; 4], alpha(false));
        let below = if copy {
            pixels.clone()
        } else {
            let mut below = pixels.clone();
            let keep = alpha(true);
            below.par_update(|col, row, _| keep(col, row));
            below
        };
        Self {
            below,
            lifted,
            coverage: Tiled::new(w, h, 0),
        }
    }

    /// The layer with the lifted pixels put down (`dx`, `dy`) from where
    /// they came from, over what stayed behind.
    pub fn drop_at(&self, dx: i32, dy: i32) -> Tiled<Pixel> {
        self.drop_transformed(&Affine::translate(dx.into(), dy.into()), Resampling::Bilinear)
    }

    /// The layer with the lifted pixels put down transformed by `t` (Free
    /// Transform), over what stayed behind.
    pub fn drop_transformed(&self, t: &Affine, resampling: Resampling) -> Tiled<Pixel> {
        let moved = transformed(&self.lifted, t, [0; 4], resampling);
        let mut out = self.below.clone();
        let fill = out.fill();
        out.par_update(|col, row, below| {
            let top = moved.tile(col, row)?;
            Some(
                (0..TILE_PIXELS)
                    .map(|i| over(top[i], below.map_or(fill, |t| t[i])))
                    .collect(),
            )
        });
        out
    }
}

impl Lifted<u16> {
    /// Lift the selected part of a mask, leaving `background` behind (the
    /// background colour's grey, as Clear does). With `copy`, the mask
    /// stays whole.
    pub fn mask(mask: &Tiled<u16>, selection: &Tiled<u16>, copy: bool, background: u16) -> Self {
        let below = if copy {
            mask.clone()
        } else {
            crate::ops::fill_mask(
                mask,
                background,
                Some(&crate::selection::Selection {
                    coverage: selection.clone(),
                    outlines: Vec::new(),
                }),
            )
        };
        Self {
            below,
            lifted: mask.clone(),
            coverage: selection.clone(),
        }
    }

    /// The mask with the lifted values put down (`dx`, `dy`) from where
    /// they came from.
    pub fn drop_at(&self, dx: i32, dy: i32) -> Tiled<u16> {
        self.drop_transformed(&Affine::translate(dx.into(), dy.into()), Resampling::Bilinear)
    }

    /// The mask with the lifted values put down transformed by `t`.
    pub fn drop_transformed(&self, t: &Affine, resampling: Resampling) -> Tiled<u16> {
        let coverage = transformed(&self.coverage, t, 0, resampling);
        let moved = transformed(&self.lifted, t, self.lifted.fill(), resampling);
        let mut out = self.below.clone();
        let fill = out.fill();
        out.par_update(|col, row, below| {
            let k = selected(&coverage, col, row)?;
            let m = moved.tile(col, row);
            Some(
                (0..TILE_PIXELS)
                    .map(|i| {
                        let (a, b) = (
                            below.map_or(fill, |t| t[i]),
                            m.map_or(moved.fill(), |t| t[i]),
                        );
                        (i64::from(a)
                            + (i64::from(b) - i64::from(a)) * i64::from(k[i]) / i64::from(MAX))
                            as u16
                    })
                    .collect(),
            )
        });
        out
    }
}

/// `top` over `bottom`, straight alpha.
pub(crate) fn over(top: Pixel, bottom: Pixel) -> Pixel {
    if top[3] == u16::MAX || bottom[3] == 0 {
        return top;
    }
    if top[3] == 0 {
        return bottom;
    }
    let m = MAX as f32;
    let (ta, ba) = (f32::from(top[3]) / m, f32::from(bottom[3]) / m);
    let under = ba * (1.0 - ta);
    let a = ta + under;
    let mut out = [0; 4];
    for c in 0..3 {
        out[c] = ((f32::from(top[c]) * ta + f32::from(bottom[c]) * under) / a).round() as u16;
    }
    out[3] = (a * m).round() as u16;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::Selection;

    const RED: Pixel = [65535, 0, 0, 65535];
    const BLUE: Pixel = [0, 0, 65535, 65535];

    /// Blue on the left half, red on the right, 600 × 300 (several tiles).
    fn halves() -> Tiled<Pixel> {
        let (w, h) = (600, 300);
        let px: Vec<Pixel> = (0..w * h)
            .map(|i| if i % w < w / 2 { BLUE } else { RED })
            .collect();
        Tiled::from_slice(w, h, [0; 4], &px)
    }

    #[test]
    fn translating_moves_pixels_and_uncovers_the_fill() {
        let t = halves();
        for (dx, dy) in [(37, -5), (-300, 0), (256, 256), (-1, 299)] {
            let moved = t.translated(dx, dy, [0; 4]);
            for (x, y) in [
                (0, 0),
                (299, 10),
                (300, 150),
                (599, 299),
                (255, 256),
                (400, 100),
            ] {
                let (sx, sy) = (x as i32 - dx, y as i32 - dy);
                let expected = if sx < 0 || sy < 0 || sx >= 600 || sy >= 300 {
                    [0; 4]
                } else {
                    t.get(sx as u32, sy as u32)
                };
                assert_eq!(moved.get(x, y), expected, "({dx}, {dy}) at ({x}, {y})");
            }
        }
        // Moved back, only what went past the edge is lost.
        let there_and_back = t.translated(-10, 0, [0; 4]).translated(10, 0, [0; 4]);
        assert_eq!(there_and_back.get(9, 0), [0; 4]);
        assert_eq!(there_and_back.get(10, 0), BLUE);
        assert_eq!(there_and_back.get(599, 0), RED);
    }

    #[test]
    fn translating_keeps_unwritten_tiles_unwritten() {
        // An empty layer stays empty, and a white mask stays white with
        // white uncovered.
        let empty = Tiled::new(600, 300, [0u16; 4]).translated(13, 7, [0; 4]);
        assert!((0..2).all(|r| (0..3).all(|c| empty.tile(c, r).is_none())));
        let white = Tiled::new(600, 300, u16::MAX).translated(-13, 7, u16::MAX);
        assert!((0..2).all(|r| (0..3).all(|c| white.tile(c, r).is_none())));
        // Select All moved uncovers unselected area.
        let all = Tiled::new(600, 300, u16::MAX).translated(20, 0, 0);
        assert_eq!((all.get(19, 5), all.get(20, 5)), (0, u16::MAX));
        assert_eq!(all.get(599, 299), u16::MAX);
    }

    #[test]
    fn lifted_pixels_move_and_leave_a_hole() {
        let t = halves();
        let sel = Selection::rectangle(600, 300, (250.0, 0.0), (350.0, 100.0));
        let lifted = Lifted::pixels(&t, &sel.coverage, false);
        // Put back where it was: the same image.
        let same = lifted.drop_at(0, 0);
        assert_eq!(same.get(260, 50), BLUE);
        assert_eq!(same.get(340, 50), RED);
        // Moved down 150: a transparent hole above, the pieces below.
        let moved = lifted.drop_at(0, 150);
        assert_eq!(moved.get(260, 50)[3], 0);
        assert_eq!(moved.get(340, 50)[3], 0);
        assert_eq!(moved.get(340, 200), RED);
        assert_eq!(moved.get(260, 200), BLUE);
        assert_eq!(moved.get(100, 50), BLUE);
        // Moved left 100: red over blue.
        let left = lifted.drop_at(-100, 0);
        assert_eq!(left.get(220, 50), RED);
        assert_eq!(left.get(160, 50), BLUE);
        assert_eq!(left.get(320, 50)[3], 0);
        // A copy leaves the original.
        let copy = Lifted::pixels(&t, &sel.coverage, true).drop_at(0, 150);
        assert_eq!(copy.get(260, 50), BLUE);
        assert_eq!(copy.get(340, 200), RED);
    }

    #[test]
    fn lifted_mask_values_move_and_leave_the_background() {
        let mut mask = Tiled::new(600, 300, u16::MAX);
        for y in 0..50 {
            for x in 0..50 {
                mask.tile_mut(0, 0)[y * 256 + x] = 0;
            }
        }
        let sel = Selection::rectangle(600, 300, (0.0, 0.0), (100.0, 100.0));
        let moved = Lifted::mask(&mask, &sel.coverage, false, 1000).drop_at(300, 100);
        assert_eq!(moved.get(10, 10), 1000);
        assert_eq!(moved.get(80, 80), 1000);
        assert_eq!(moved.get(310, 110), 0);
        assert_eq!(moved.get(380, 180), u16::MAX);
        assert_eq!(moved.get(500, 10), u16::MAX);
        let copy = Lifted::mask(&mask, &sel.coverage, true, 1000).drop_at(300, 100);
        assert_eq!((copy.get(10, 10), copy.get(80, 80)), (0, u16::MAX));
        assert_eq!(copy.get(310, 110), 0);
    }

    #[test]
    fn over_blends_straight_alpha() {
        assert_eq!(over(RED, BLUE), RED);
        assert_eq!(over([0; 4], BLUE), BLUE);
        let half_red = [65535, 0, 0, 32768];
        let p = over(half_red, BLUE);
        assert_eq!(p[3], 65535);
        assert!(
            p[0].abs_diff(32768) <= 1 && p[2].abs_diff(32767) <= 1,
            "{p:?}"
        );
    }
}
