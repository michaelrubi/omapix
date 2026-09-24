use serde::{Deserialize, Serialize};

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
    /// Photoshop's Blend If (Blending Options): hide this layer where it or
    /// the layers below are too dark or too light.
    pub blend_if: Option<BlendIf>,
    /// A layer group (a folder in the Layers panel). What's in it sits
    /// directly below it in the stack; its own pixels are unused (always
    /// empty).
    pub is_group: bool,
    /// The group this layer is in, if any.
    pub parent: Option<u64>,
    /// Clipped to the layer below (Photoshop's clipping mask): it shows only
    /// where that layer does. A run of clipped layers all clip to the first
    /// unclipped layer below them in the same group.
    pub clipped: bool,
    /// Photoshop's Lock Transparent Pixels (`/`): painting and fills change
    /// colour only, keeping each pixel's transparency.
    pub lock_alpha: bool,
}

/// Which values Blend If compares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendIfChannel {
    #[default]
    Gray,
    Red,
    Green,
    Blue,
}

/// Photoshop's Blend If. Each range is `[black, black_split, white_split,
/// white]` on 0–1: the layer shows fully between the inner points, fades
/// out between each pair (split sliders, Alt+drag in Photoshop), and is
/// hidden beyond them.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlendIf {
    pub channel: BlendIfChannel,
    pub this: [f32; 4],
    pub underlying: [f32; 4],
}

impl Default for BlendIf {
    fn default() -> Self {
        Self {
            channel: BlendIfChannel::Gray,
            this: [0.0, 0.0, 1.0, 1.0],
            underlying: [0.0, 0.0, 1.0, 1.0],
        }
    }
}

impl BlendIf {
    /// True when it hides nothing.
    pub fn is_neutral(&self) -> bool {
        let full = |r: [f32; 4]| r[1] <= 0.0 && r[2] >= 1.0;
        full(self.this) && full(self.underlying)
    }

    fn pick(&self, c: [f32; 3]) -> f32 {
        match self.channel {
            BlendIfChannel::Gray => 0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2],
            BlendIfChannel::Red => c[0],
            BlendIfChannel::Green => c[1],
            BlendIfChannel::Blue => c[2],
        }
    }

    fn range([black, black_split, white_split, white]: [f32; 4], v: f32) -> f32 {
        let rise = if v >= black_split {
            1.0
        } else if v < black {
            0.0
        } else {
            (v - black) / (black_split - black).max(1e-6)
        };
        let fall = if v <= white_split {
            1.0
        } else if v > white {
            0.0
        } else {
            (white - v) / (white - white_split).max(1e-6)
        };
        rise * fall
    }

    /// How much of the layer shows (0–1) for this layer's colour over the
    /// colour below it.
    #[inline]
    pub fn factor(&self, this: [f32; 3], underlying: [f32; 3]) -> f32 {
        Self::range(self.this, self.pick(this))
            * Self::range(self.underlying, self.pick(underlying))
    }
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

    /// An empty layer group, in Pass Through mode as Photoshop creates them.
    pub fn group(id: u64, name: impl Into<String>, width: u32, height: u32) -> Self {
        let mut layer = Self::empty(id, name, width, height);
        layer.is_group = true;
        layer.blend = BlendMode::PassThrough;
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
            blend_if: None,
            is_group: false,
            parent: None,
            clipped: false,
            lock_alpha: false,
        }
    }

    /// Whether this layer has pixels of its own to paint on, fill or
    /// filter. Adjustment layers and groups don't.
    pub fn has_pixels(&self) -> bool {
        self.adjustment.is_none() && !self.is_group
    }
}
