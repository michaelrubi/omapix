use crate::adjust::Adjustment;
use crate::blend::BlendMode;
use crate::tiled::Tiled;
use crate::{Pixel, Raster};

/// A mask value of full white: the layer shows through completely.
pub const MASK_WHITE: u16 = u16::MAX;

/// A pixel layer. Cloning is cheap: pixel and mask tiles are shared until
/// written.
#[derive(Clone)]
pub struct Layer {
    /// Stable identity, kept across undo so the UI can track selection.
    pub id: u64,
    pub name: String,
    pub visible: bool,
    /// 0–1.
    pub opacity: f32,
    pub blend: BlendMode,
    pub pixels: Tiled<Pixel>,
    pub mask: Option<Mask>,
    /// For adjustment layers: the change applied to everything below. Their
    /// pixels are unused (always empty).
    pub adjustment: Option<Adjustment>,
}

#[derive(Clone)]
pub struct Mask {
    pub pixels: Tiled<u16>,
    /// Shift-click on the mask in Photoshop disables it without deleting.
    pub enabled: bool,
}

impl Mask {
    pub fn white(width: u32, height: u32) -> Self {
        Self {
            pixels: Tiled::new(width, height, MASK_WHITE),
            enabled: true,
        }
    }

    /// Swap black and white (Ctrl+I on a mask).
    pub fn invert(&mut self) {
        let fill = self.pixels.fill();
        let (w, h) = (self.pixels.width(), self.pixels.height());
        let mut inverted = Tiled::new(w, h, u16::MAX - fill);
        inverted.par_update(|col, row, _| {
            Some(match self.pixels.tile(col, row) {
                Some(t) => t.iter().map(|v| u16::MAX - v).collect(),
                None => return None,
            })
        });
        self.pixels = inverted;
    }
}

impl Layer {
    pub fn empty(id: u64, name: impl Into<String>, width: u32, height: u32) -> Self {
        Self::from_pixels(id, name, Tiled::new(width, height, [0; 4]))
    }

    /// An adjustment layer with a white (reveal-all) mask, as Photoshop
    /// creates them.
    pub fn adjustment(id: u64, adjustment: Adjustment, width: u32, height: u32) -> Self {
        let mut layer = Self::empty(id, adjustment.name(), width, height);
        layer.adjustment = Some(adjustment);
        layer.mask = Some(Mask::white(width, height));
        layer
    }

    pub fn from_raster(id: u64, name: impl Into<String>, raster: &Raster) -> Self {
        Self::from_pixels(id, name, Tiled::from_raster(raster))
    }

    pub fn from_pixels(id: u64, name: impl Into<String>, pixels: Tiled<Pixel>) -> Self {
        Self {
            id,
            name: name.into(),
            visible: true,
            opacity: 1.0,
            blend: BlendMode::Normal,
            pixels,
            mask: None,
            adjustment: None,
        }
    }
}
