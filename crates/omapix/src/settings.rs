//! Filter settings as last used, kept between runs in
//! `~/.config/omapix/filters.toml`, as Photoshop remembers them.

use std::path::{Path, PathBuf};

use omapix_engine::filters::{LayerFilter, SharpenRemove};
use omapix_engine::{NoiseOptions, ReduceNoiseOptions, SmartBlurMode, SmartBlurOptions, SmartBlurQuality, SmartSharpenOptions};

/// Unsharp Mask's settings until it's first used: a moderate sharpening
/// for a 24 MP portrait.
pub const DEFAULT_UNSHARP_MASK: LayerFilter = LayerFilter::UnsharpMask {
    amount: 0.8,
    radius: 1.5,
    threshold: 2.0,
};

/// Smart Sharpen's settings until it's first used.
pub const DEFAULT_SMART_SHARPEN: LayerFilter = LayerFilter::SmartSharpen(SmartSharpenOptions {
    amount: 100.0,
    radius: 1.5,
    reduce_noise: 10.0,
    remove: SharpenRemove::GaussianBlur,
    shadow_fade: 0.0,
    highlight_fade: 0.0,
});

/// Reduce Noise's settings until it's first used.
pub const DEFAULT_REDUCE_NOISE: LayerFilter = LayerFilter::ReduceNoise(ReduceNoiseOptions {
    strength: 5.0,
    preserve_details: 10.0,
    reduce_color_noise: 25.0,
    sharpen_details: 0.0,
});

/// Smart Blur's settings until it's first used.
pub const DEFAULT_SMART_BLUR: LayerFilter = LayerFilter::SmartBlur(SmartBlurOptions {
    radius: 3.0,
    threshold: 25.0,
    quality: SmartBlurQuality::Medium,
    mode: SmartBlurMode::Normal,
});

/// Each filter's settings as last applied. Anything missing from the file
/// (or the file itself) falls back to the defaults.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct FilterSettings {
    pub blur_radius: f32,
    pub high_pass_radius: f32,
    pub feather_radius: f32,
    pub mask_density: f32,
    pub unsharp_mask: LayerFilter,
    pub smart_sharpen: LayerFilter,
    pub reduce_noise: LayerFilter,
    pub smart_blur: LayerFilter,
    pub noise: NoiseOptions,
    /// Where they're saved; `None` (as in tests) keeps them in memory.
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Default for FilterSettings {
    fn default() -> Self {
        Self {
            blur_radius: 2.0,
            high_pass_radius: 2.0,
            feather_radius: 10.0,
            mask_density: 100.0,
            unsharp_mask: DEFAULT_UNSHARP_MASK,
            smart_sharpen: DEFAULT_SMART_SHARPEN,
            reduce_noise: DEFAULT_REDUCE_NOISE,
            smart_blur: DEFAULT_SMART_BLUR,
            noise: NoiseOptions::default(),
            path: None,
        }
    }
}

impl FilterSettings {
    /// The settings saved last time, to be saved there again.
    pub fn load() -> Self {
        crate::recent::config_dir().map_or_else(Self::default, |dir| Self::load_from(&dir.join("filters.toml")))
    }

    fn load_from(path: &Path) -> Self {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let settings = toml::from_str(&text).unwrap_or_else(|e| {
            if !text.is_empty() {
                log::warn!("{}: {e}", path.display());
            }
            Self::default()
        });
        Self {
            path: Some(path.to_path_buf()),
            ..settings
        }
    }

    pub fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match toml::to_string(self) {
            Ok(text) => {
                if let Err(e) = std::fs::write(path, text) {
                    log::warn!("{}: {e}", path.display());
                }
            }
            Err(e) => log::warn!("filter settings: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_are_kept_between_runs() {
        let dir = std::env::temp_dir().join(format!("omapix-filters-{}", std::process::id()));
        let path = dir.join("filters.toml");
        let mut settings = FilterSettings::load_from(&path);
        assert_eq!(settings, FilterSettings { path: Some(path.clone()), ..FilterSettings::default() });
        settings.blur_radius = 7.5;
        settings.smart_blur = LayerFilter::SmartBlur(SmartBlurOptions {
            radius: 12.0,
            threshold: 40.0,
            quality: SmartBlurQuality::High,
            mode: SmartBlurMode::EdgeOnly,
        });
        settings.noise.amount = 0.3;
        settings.save();
        assert_eq!(FilterSettings::load_from(&path), settings);

        // A file from an older Omapix, missing some, keeps what it has.
        std::fs::write(&path, "high_pass_radius = 3.0\n").unwrap();
        let old = FilterSettings::load_from(&path);
        assert_eq!((old.high_pass_radius, old.blur_radius), (3.0, 2.0));
        // Unreadable, it's the defaults.
        std::fs::write(&path, "blur_radius = \"wide\"\n").unwrap();
        assert_eq!(FilterSettings::load_from(&path).blur_radius, 2.0);
        std::fs::remove_dir_all(&dir).unwrap();

        // Without a file, nothing is written.
        FilterSettings::default().save();
    }
}
