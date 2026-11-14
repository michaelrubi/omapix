//! Cached group results (`composite::GroupCache`) against none, on random
//! stacks as they're edited.

use omapix_engine::BlendMode;
use omapix_engine::composite::{GroupCache, composite_tiles};

use super::{Rng, document, mask, pixels, specs};

#[test]
fn cached_groups_match_none_as_stacks_are_edited() {
    let modes: Vec<BlendMode> = BlendMode::MENU.iter().flat_map(|g| g.iter()).copied().collect();
    let tiles = [(0, 0)];
    // `OMAPIX_FUZZ_SEEDS=300` tries more: 40 take about 8 s unoptimised.
    let seeds = std::env::var("OMAPIX_FUZZ_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    for seed in 0..seeds {
        let stack = specs(seed);
        let mut doc = document(&stack);
        let cache = GroupCache::default();
        let mut rng = Rng::new(seed + 1_000_000);
        let mut last_edit = String::from("initial");

        for step in 0..8 {
            let cached = composite_tiles(&doc.layers, &tiles, Some(&cache));
            let uncached = composite_tiles(&doc.layers, &tiles, None);
            if cached != uncached {
                panic!("seed {seed}, step {step}, after {last_edit}:\n{stack:#?}");
            }

            let idx = rng.below(doc.layers.len() as u64) as usize;
            let can_clip = doc.can_clip(idx);
            let layer = &mut doc.layers[idx];
            let id = layer.id;
            let salt = 1000 * (step as u64 + 1);

            last_edit = match rng.below(7) {
                0 => {
                    layer.opacity = 0.1 + 0.9 * rng.unit();
                    format!("layer {id} opacity = {:.3}", layer.opacity)
                }
                1 => {
                    layer.blend = if layer.is_group && rng.chance(30) {
                        BlendMode::PassThrough
                    } else {
                        rng.pick(&modes)
                    };
                    format!("layer {id} blend = {:?}", layer.blend)
                }
                3 if layer.mask.is_some() => {
                    let m = layer.mask.as_mut().unwrap();
                    m.enabled = !m.enabled;
                    format!("layer {id} mask.enabled = {}", m.enabled)
                }
                4 if layer.clipped || can_clip => {
                    layer.clipped = !layer.clipped;
                    format!("layer {id} clipped = {}", layer.clipped)
                }
                5 if !layer.is_group && layer.adjustment.is_none() => {
                    layer.pixels = pixels(id + salt);
                    format!("layer {id} pixels = pixels({})", id + salt)
                }
                6 if layer.mask.is_some() => {
                    layer.mask.as_mut().unwrap().pixels = mask(id + salt);
                    format!("layer {id} mask.pixels = mask({})", id + salt)
                }
                _ => {
                    layer.visible = !layer.visible;
                    format!("layer {id} visible = {}", layer.visible)
                }
            };
        }
    }
}
