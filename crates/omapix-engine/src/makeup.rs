//! Makeup (docs/AI.md, milestone 15), as it's painted by hand: for each
//! product a layer with its colour painted where it goes on the faces, in a
//! blend mode that lets the skin's own light and texture through, gathered
//! in a "Makeup" group. A layer's opacity is how much of it there is; the
//! brush and the eraser add to it and take it away, and with its
//! transparency locked a fill gives it another colour.

use crate::blend::BlendMode;
use crate::document::Document;
use crate::layer::Layer;
use crate::selection::Selection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Product {
    Blush,
    Brows,
    EyeShadow,
    Eyeliner,
    Lipstick,
}

impl Product {
    /// Its layer's name.
    pub fn name(self) -> &'static str {
        match self {
            Product::Blush => "Blush",
            Product::Brows => "Brows",
            Product::EyeShadow => "Eye Shadow",
            Product::Eyeliner => "Eyeliner",
            Product::Lipstick => "Lipstick",
        }
    }

    /// Its colour (8-bit sRGB), how its layer blends, and the layer's
    /// opacity to start with (0–100): there to see, not to notice.
    fn look(self) -> ([u8; 3], BlendMode, f32) {
        match self {
            Product::Blush => ([222, 110, 110], BlendMode::SoftLight, 30.0),
            Product::Brows => ([70, 50, 40], BlendMode::Multiply, 30.0),
            Product::EyeShadow => ([120, 84, 70], BlendMode::Multiply, 35.0),
            Product::Eyeliner => ([40, 30, 28], BlendMode::Multiply, 60.0),
            Product::Lipstick => ([176, 48, 64], BlendMode::SoftLight, 40.0),
        }
    }
}

/// A "Makeup" group above the layer at `above`, with a layer for each of
/// `products` in order, bottom first: its colour painted where its
/// selection is. Those with nowhere to go are left out. Returns the group's
/// id, or `None` if there's nothing to put in it.
pub fn add_layers(doc: &mut Document, above: usize, products: &[(Product, Selection)]) -> Option<u64> {
    if products.iter().all(|(_, on)| on.is_empty()) {
        return None;
    }
    let group = doc.next_layer_id();
    doc.insert_above(above, Layer::group(group, "Makeup", doc.width, doc.height));
    // Each goes in at the top of the group.
    for (product, on) in products.iter().filter(|(_, on)| !on.is_empty()) {
        let (colour, blend, amount) = product.look();
        let [r, g, b, _] = doc.profile.from_srgb8(colour).unwrap_or([colour[0], colour[1], colour[2], 255].map(|v| u16::from(v) * 257));
        let id = doc.next_layer_id();
        let mut layer = Layer::from_pixels(id, product.name(), on.coverage.map(|a| if a == 0 { [0; 4] } else { [r, g, b, a] }));
        layer.blend = blend;
        layer.opacity = amount / 100.0;
        let at = doc.index_of(group).expect("just added");
        doc.insert_above(at, layer);
    }
    Some(group)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::ColorProfile;
    use crate::raster::Raster;
    use crate::tiled::Tiled;

    const W: u32 = 600;
    const H: u32 = 400;

    #[test]
    fn each_product_is_a_layer_of_its_colour_where_it_goes_in_a_makeup_group() {
        let skin = Raster::new(W, H, vec![[52000, 40000, 34000, 65535]; (W * H) as usize]);
        let mut doc = Document::from_image("t.tif".into(), &skin, ColorProfile::srgb(), 16);
        let lips = Selection::rectangle(W, H, (250.0, 300.0), (350.0, 330.0));
        let cheek = Selection::ellipse(W, H, (100.0, 150.0), (200.0, 250.0)).feather(10.0);
        let none = Selection::from_coverage(Tiled::new(W, H, 0));
        let products = [(Product::Blush, cheek), (Product::Brows, none.clone()), (Product::Lipstick, lips)];
        let group = add_layers(&mut doc, 0, &products).unwrap();

        // Blush below Lipstick in the group; nothing for the brows.
        let names: Vec<_> = doc.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Background", "Blush", "Lipstick", "Makeup"]);
        assert!(doc.layers[1..3].iter().all(|l| l.parent == Some(group)));
        let lipstick = &doc.layers[2];
        assert_eq!((lipstick.blend, lipstick.opacity), (BlendMode::SoftLight, 0.4));
        assert!(lipstick.mask.is_none());
        // Paint only where it goes, as much as it's selected there.
        assert_eq!(lipstick.pixels.get(300, 315)[3], 65535);
        assert_eq!(lipstick.pixels.get(300, 200), [0; 4]);
        let edge = doc.layers[1].pixels.get(100, 200)[3];
        assert!((16000..50000).contains(&edge), "{edge}");

        // The lips are redder and no lighter, the cheek a little pinker, and
        // the rest as it was.
        let after = doc.composite();
        let (was, lip, blush) = (skin.get(300, 315), after.get(300, 315), after.get(150, 200));
        let red = |p: [u16; 4]| f32::from(p[0]) / f32::from(p[1]);
        assert!(red(lip) > red(was) * 1.1 && lip[1] < was[1], "{lip:?}");
        assert!(red(blush) > red(was) * 1.02, "{blush:?}");
        assert_eq!(after.get(300, 100), was);
        // More of it at full opacity.
        doc.layers[2].opacity = 1.0;
        assert!(red(doc.composite().get(300, 315)) > red(lip));

        // With nowhere for any of it to go there's no group.
        assert_eq!(add_layers(&mut doc, 0, &[(Product::Eyeliner, none)]), None);
        assert_eq!(doc.layers.len(), 4);
    }
}
