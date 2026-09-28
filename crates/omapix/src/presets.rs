//! Adjustment presets (Image › Adjustments › Presets): a grade's adjustment
//! layers and groups, kept in `~/.config/omapix/presets/` to add to the next
//! image of a session, still editable. Masks aren't kept: the layers come
//! back with white ones.

use std::path::{Path, PathBuf};

use omapix_engine::adjust::Adjustment;
use omapix_engine::layer::BlendIf;
use omapix_engine::{BlendMode, Document, Layer};

/// The layers of a preset, bottom first, as in a document.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Preset {
    layers: Vec<PresetLayer>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct PresetLayer {
    name: String,
    visible: bool,
    opacity: f32,
    /// Its name in the menu, e.g. "Soft Light" or "Pass Through".
    blend: String,
    clipped: bool,
    /// The group it's in, as an index into the preset's layers.
    parent: Option<usize>,
    blend_if: Option<BlendIf>,
    /// `None` for a group.
    adjustment: Option<Adjustment>,
}

fn blend_named(name: &str) -> Option<BlendMode> {
    let modes = BlendMode::MENU.iter().flat_map(|g| g.iter()).copied();
    modes.chain([BlendMode::PassThrough]).find(|m| m.name() == name)
}

/// Where presets are kept.
pub fn dir() -> Option<PathBuf> {
    crate::recent::config_dir().map(|d| d.join("presets"))
}

/// The presets in `dir`, by name.
pub fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .filter_map(|p| Some(p.file_stem()?.to_string_lossy().into_owned()))
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

impl Preset {
    /// The adjustment layers and groups among layers `ids` and what's in
    /// those groups. `None` if there are no adjustment layers.
    pub fn from_layers(doc: &Document, ids: &[u64]) -> Option<Self> {
        let mut kept: Vec<&Layer> = Vec::new();
        for (i, layer) in doc.layers.iter().enumerate() {
            let chosen = ids.iter().any(|&id| {
                layer.id == id || doc.index_of(id).is_some_and(|g| doc.layers[g].is_group && doc.span(g).contains(&i))
            });
            if chosen && (layer.is_group || layer.adjustment.is_some()) {
                kept.push(layer);
            }
        }
        if kept.iter().all(|l| l.adjustment.is_none()) {
            return None;
        }
        let layers = kept
            .iter()
            .map(|l| PresetLayer {
                name: l.name.clone(),
                visible: l.visible,
                opacity: l.opacity,
                blend: l.blend.name().to_owned(),
                clipped: l.clipped,
                parent: l.parent.and_then(|p| kept.iter().position(|k| k.id == p)),
                blend_if: l.blend_if,
                adjustment: l.adjustment.clone(),
            })
            .collect();
        Some(Self { layers })
    }

    /// Add the preset's layers above the layer at `index` (or at the top of
    /// it if it's a group), as a new layer would go. Returns the top one.
    pub fn apply(&self, doc: &mut Document, index: usize) -> Option<u64> {
        let (w, h) = (doc.width, doc.height);
        let below = &doc.layers[index];
        let (at, parent) = if below.is_group {
            (index, Some(below.id))
        } else {
            (index + 1, below.parent)
        };
        let ids: Vec<u64> = self.layers.iter().map(|_| doc.next_layer_id()).collect();
        let layers: Vec<Layer> = self
            .layers
            .iter()
            .zip(&ids)
            .map(|(p, &id)| {
                let mut layer = match &p.adjustment {
                    Some(a) => Layer::adjustment(id, a.clone(), w, h),
                    None => Layer::group(id, "", w, h),
                };
                layer.name = p.name.clone();
                layer.visible = p.visible;
                layer.opacity = p.opacity;
                layer.blend = blend_named(&p.blend).unwrap_or(layer.blend);
                layer.clipped = p.clipped;
                layer.parent = p.parent.map_or(parent, |i| ids.get(i).copied());
                layer.blend_if = p.blend_if;
                layer
            })
            .collect();
        doc.layers.splice(at..at, layers);
        ids.last().copied()
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        toml::from_str(&text).map_err(|e| e.to_string())
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = toml::to_string(self).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, text).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omapix_engine::adjust::{Curves, HueSaturation};
    use omapix_engine::{ColorProfile, Raster};

    fn doc() -> Document {
        let image = Raster::new(4, 4, vec![[30000, 30000, 30000, 65535]; 16]);
        Document::from_image("t.tif".into(), &image, ColorProfile::srgb(), 16)
    }

    /// Background, a pixel layer, then a "Grade" group at 80 % holding a
    /// half-opacity Curves (with a Blend If) and a clipped Luminosity
    /// Hue/Saturation.
    fn graded() -> (Document, u64) {
        let mut doc = doc();
        let (w, h) = (4, 4);
        doc.layers.push(Layer::empty(10, "Patch", w, h));
        let mut curves = Curves::default();
        curves.master.points.insert(1, (0.5, 0.6));
        let mut c = Layer::adjustment(11, Adjustment::Curves(curves), w, h);
        c.parent = Some(13);
        c.opacity = 0.5;
        c.blend_if = Some(BlendIf::default());
        let mut hs = Layer::adjustment(12, Adjustment::HueSaturation(HueSaturation::default()), w, h);
        hs.parent = Some(13);
        hs.clipped = true;
        hs.blend = BlendMode::Luminosity;
        let mut group = Layer::group(13, "Grade", w, h);
        group.opacity = 0.8;
        doc.layers.extend([c, hs, group]);
        (doc, 13)
    }

    #[test]
    fn a_group_is_kept_with_its_adjustments_and_added_to_another_image() {
        let (source, group) = graded();
        let preset = Preset::from_layers(&source, &[group]).unwrap();
        assert_eq!(preset.layers.len(), 3, "the group and its two adjustments");

        let path = std::env::temp_dir().join(format!("omapix-preset-{}.toml", std::process::id()));
        preset.save(&path).unwrap();
        let loaded = Preset::load(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(loaded, preset);

        let mut doc = doc();
        let top = loaded.apply(&mut doc, 0).unwrap();
        let [_, c, hs, g] = &doc.layers[..] else {
            panic!("background and three layers");
        };
        assert_eq!(top, g.id);
        assert_eq!((g.is_group, g.name.as_str(), g.opacity, g.parent), (true, "Grade", 0.8, None));
        assert_eq!((c.parent, c.opacity, c.blend_if), (Some(g.id), 0.5, Some(BlendIf::default())));
        assert_eq!(c.adjustment, source.layers[2].adjustment);
        assert!(c.mask.is_some(), "with a white mask");
        assert_eq!((hs.parent, hs.clipped, hs.blend), (Some(g.id), true, BlendMode::Luminosity));
        assert_eq!(doc.composite().get(0, 0), {
            let mut expected = source.clone();
            expected.layers.remove(1);
            expected.composite().get(0, 0)
        });
    }

    #[test]
    fn pixel_layers_are_left_out_and_lone_adjustments_go_where_a_new_layer_would() {
        let (source, _) = graded();
        assert_eq!(Preset::from_layers(&source, &[10]), None, "no adjustments");
        let preset = Preset::from_layers(&source, &[10, 12, 14]).unwrap();
        assert_eq!(preset.layers.len(), 1);
        assert_eq!(preset.layers[0].parent, None, "its group wasn't chosen");

        // Into the top of a group, as a new layer would go.
        let mut doc = source.clone();
        let top = preset.apply(&mut doc, 4).unwrap();
        assert_eq!(doc.index_of(top), Some(4));
        assert_eq!(doc.layer(top).unwrap().parent, Some(13));
    }
}
