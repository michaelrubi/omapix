//! Filter and retouching settings, as Photoshop keeps them: each dialog
//! opens with what it was last applied with (`~/.config/omapix/filters.toml`),
//! and its Defaults button goes back to the defaults, which are Omapix's
//! unless changed in `~/.config/omapix/defaults.toml`.

use std::path::{Path, PathBuf};

use omapix_engine::filters::{LayerFilter, SharpenRemove};
use omapix_engine::{NoiseOptions, ReduceNoiseOptions, SmartBlurMode, SmartBlurOptions, SmartBlurQuality, SmartSharpenOptions};

/// Unsharp Mask's settings: `amount` 1 = 100 %, `threshold` in levels.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UnsharpMask {
    pub amount: f32,
    pub radius: f32,
    pub threshold: f32,
}

impl From<UnsharpMask> for LayerFilter {
    fn from(UnsharpMask { amount, radius, threshold }: UnsharpMask) -> Self {
        LayerFilter::UnsharpMask { amount, radius, threshold }
    }
}

/// Each filter's and retouching setup's settings.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct FilterSettings {
    pub blur_radius: f32,
    /// Filter › High Pass and Retouch › High Pass Sharpening.
    pub high_pass_radius: f32,
    pub feather_radius: f32,
    pub expand_radius: f32,
    pub contract_radius: f32,
    pub smooth_radius: f32,
    pub border_width: f32,
    pub mask_density: f32,
    pub select_and_mask: omapix_engine::refine::EdgeOptions,
    pub select_and_mask_output: crate::select_and_mask::Output,
    /// Frequency Separation's radius; unset, it's worked out from the
    /// image's size.
    pub separation_radius: Option<f32>,
    pub unsharp_mask: UnsharpMask,
    pub smart_sharpen: SmartSharpenOptions,
    pub reduce_noise: ReduceNoiseOptions,
    pub smart_blur: SmartBlurOptions,
    pub noise: NoiseOptions,
    /// Where they're kept; `None` (the defaults, and in tests) isn't saved.
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Default for FilterSettings {
    /// Omapix's own defaults.
    fn default() -> Self {
        Self {
            blur_radius: 2.0,
            high_pass_radius: 2.0,
            feather_radius: 10.0,
            expand_radius: 5.0,
            contract_radius: 5.0,
            smooth_radius: 5.0,
            border_width: 10.0,
            mask_density: 100.0,
            select_and_mask: Default::default(),
            select_and_mask_output: Default::default(),
            separation_radius: None,
            // A moderate sharpening for a 24 MP portrait.
            unsharp_mask: UnsharpMask {
                amount: 0.8,
                radius: 1.5,
                threshold: 2.0,
            },
            smart_sharpen: SmartSharpenOptions {
                amount: 100.0,
                radius: 1.5,
                reduce_noise: 10.0,
                remove: SharpenRemove::GaussianBlur,
                shadow_fade: 0.0,
                highlight_fade: 0.0,
            },
            reduce_noise: ReduceNoiseOptions {
                strength: 5.0,
                preserve_details: 10.0,
                reduce_color_noise: 25.0,
                sharpen_details: 0.0,
            },
            smart_blur: SmartBlurOptions {
                radius: 3.0,
                threshold: 25.0,
                quality: SmartBlurQuality::Medium,
                mode: SmartBlurMode::Normal,
            },
            noise: NoiseOptions::default(),
            path: None,
        }
    }
}

const TEMPLATE_HEADER: &str = "\
# Omapix's defaults for filters and retouching setups: what their dialogs
# start with until they're first used, and what their Defaults button goes
# back to. Each is listed with Omapix's own default. To change one, remove
# the # at the start of its line (and of its [section], for those in one)
# and edit it. Restart Omapix to apply.
#
# The settings each was last used with are kept in filters.toml.

";

impl FilterSettings {
    /// The defaults from `defaults.toml` over Omapix's own, and the settings
    /// last used over those, saved where they came from.
    pub fn load() -> (Self, Self) {
        let Some(dir) = crate::recent::config_dir() else {
            return (Self::default(), Self::default());
        };
        let defaults = Self::load_defaults(&dir.join("defaults.toml"));
        let last = Self::load_from(&dir.join("filters.toml"), &defaults);
        (last, defaults)
    }

    /// The defaults in `path`, which is written (all commented out) if it's
    /// missing or empty, so there's something to edit.
    fn load_defaults(path: &Path) -> Self {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text.trim().is_empty() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path, Self::template());
        }
        parse(path, &text, &Self::default())
    }

    fn load_from(path: &Path, defaults: &Self) -> Self {
        Self {
            path: Some(path.to_path_buf()),
            ..load_from(path, defaults)
        }
    }

    /// Omapix's defaults as a `defaults.toml`, every line commented out.
    fn template() -> String {
        let text = toml::to_string(&Self::default()).unwrap_or_default();
        let mut out = TEMPLATE_HEADER.to_owned();
        out.push_str("# separation_radius = 8.6  (unset: from the image's size)\n");
        for line in text.lines() {
            out.push_str(if line.is_empty() { "" } else { "# " });
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    pub fn save(&self) {
        save(self.path.as_deref(), self, "filter settings");
    }

    /// A filter like `kind`, with these settings.
    pub fn filter(&self, kind: &LayerFilter) -> LayerFilter {
        match kind {
            LayerFilter::GaussianBlur { .. } => LayerFilter::GaussianBlur { radius: self.blur_radius },
            LayerFilter::HighPass { .. } => LayerFilter::HighPass { radius: self.high_pass_radius },
            LayerFilter::UnsharpMask { .. } => self.unsharp_mask.into(),
            LayerFilter::AddNoise(_) => LayerFilter::AddNoise(self.noise),
            LayerFilter::SmartSharpen(_) => LayerFilter::SmartSharpen(self.smart_sharpen),
            LayerFilter::ReduceNoise(_) => LayerFilter::ReduceNoise(self.reduce_noise),
            LayerFilter::MaskDensity { .. } => LayerFilter::MaskDensity { density: self.mask_density },
            LayerFilter::SmartBlur(_) => LayerFilter::SmartBlur(self.smart_blur),
        }
    }

    /// Keep `filter`'s settings for next time, and save them.
    pub fn remember(&mut self, filter: &LayerFilter) {
        match *filter {
            LayerFilter::GaussianBlur { radius } => self.blur_radius = radius,
            LayerFilter::HighPass { radius } => self.high_pass_radius = radius,
            LayerFilter::UnsharpMask { amount, radius, threshold } => {
                self.unsharp_mask = UnsharpMask { amount, radius, threshold };
            }
            LayerFilter::AddNoise(options) => self.noise = options,
            LayerFilter::SmartSharpen(options) => self.smart_sharpen = options,
            LayerFilter::ReduceNoise(options) => self.reduce_noise = options,
            LayerFilter::MaskDensity { density } => self.mask_density = density,
            LayerFilter::SmartBlur(options) => self.smart_blur = options,
        }
        self.save();
    }
}

/// `over`'s values in place of `base`'s, key by key, into nested tables.
pub(crate) fn merge(base: &mut toml::Table, over: toml::Table) {
    for (key, value) in over {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(inner)), toml::Value::Table(value)) => merge(inner, value),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

/// `text`'s settings, over `base` for any it leaves out.
pub(crate) fn parse<T>(path: &Path, text: &str, base: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned + Clone,
{
    let parsed = toml::from_str::<toml::Table>(text).and_then(|mut over| {
        // The first filters.toml kept filters as [unsharp_mask.UnsharpMask];
        // their settings are what's inside.
        for (_, value) in over.iter_mut() {
            if let toml::Value::Table(t) = value
                && t.len() == 1
                && let Some((name, toml::Value::Table(inner))) = t.iter().next()
                && name.starts_with(char::is_uppercase)
            {
                *value = toml::Value::Table(inner.clone());
            }
        }
        let mut table = toml::Table::try_from(base).expect("settings are a table");
        merge(&mut table, over);
        table.try_into::<T>()
    });
    parsed.unwrap_or_else(|e| {
        log::warn!("{}: {e}", path.display());
        base.clone()
    })
}

pub(crate) fn load_from<T>(path: &Path, defaults: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned + Clone,
{
    let text = std::fs::read_to_string(path).unwrap_or_default();
    parse(path, &text, defaults)
}

pub(crate) fn save<T: serde::Serialize>(path: Option<&Path>, value: &T, label: &str) {
    let Some(path) = path else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match toml::to_string(value) {
        Ok(text) => {
            if let Err(e) = std::fs::write(path, text) {
                log::warn!("{}: {e}", path.display());
            }
        }
        Err(e) => log::warn!("{label}: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omapix-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn settings_are_kept_between_runs() {
        let dir = dir("filters");
        let path = dir.join("filters.toml");
        let defaults = FilterSettings::default();
        let mut settings = FilterSettings::load_from(&path, &defaults);
        assert_eq!(settings, FilterSettings { path: Some(path.clone()), ..FilterSettings::default() });
        settings.blur_radius = 7.5;
        settings.separation_radius = Some(12.0);
        settings.expand_radius = 8.0;
        settings.contract_radius = 4.0;
        settings.smooth_radius = 6.0;
        settings.border_width = 15.0;
        settings.remember(&LayerFilter::SmartBlur(SmartBlurOptions {
            radius: 12.0,
            threshold: 40.0,
            quality: SmartBlurQuality::High,
            mode: SmartBlurMode::EdgeOnly,
        }));
        assert_eq!(FilterSettings::load_from(&path, &defaults), settings);

        // A file missing some keeps what it has; unreadable, it's the defaults.
        std::fs::write(&path, "high_pass_radius = 3.0\n[unsharp_mask]\namount = 1.2\n").unwrap();
        let old = FilterSettings::load_from(&path, &defaults);
        assert_eq!((old.high_pass_radius, old.blur_radius), (3.0, 2.0));
        assert_eq!((old.unsharp_mask.amount, old.unsharp_mask.radius), (1.2, 1.5));
        // As the first version wrote them.
        std::fs::write(&path, "[smart_blur.SmartBlur]\nradius = 2.4\nthreshold = 25.0\nquality = \"High\"\nmode = \"Normal\"\n").unwrap();
        let first = FilterSettings::load_from(&path, &defaults);
        assert_eq!((first.smart_blur.radius, first.smart_blur.quality), (2.4, SmartBlurQuality::High));
        std::fs::write(&path, "blur_radius = \"wide\"\n").unwrap();
        assert_eq!(FilterSettings::load_from(&path, &defaults).blur_radius, 2.0);
        std::fs::remove_dir_all(&dir).unwrap();

        // Without a file, nothing is written.
        FilterSettings::default().save();
    }

    #[test]
    fn defaults_come_from_their_file_with_a_template_to_edit() {
        let dir = dir("defaults");
        let path = dir.join("defaults.toml");
        // Missing: written all commented out, and Omapix's own apply.
        assert_eq!(FilterSettings::load_defaults(&path), FilterSettings::default());
        let template = std::fs::read_to_string(&path).unwrap();
        assert!(template.lines().all(|l| l.is_empty() || l.starts_with('#')), "{template}");
        assert!(template.contains("# blur_radius = 2.0") && template.contains("# [smart_blur]"));
        assert!(template.contains("# expand_radius = 5.0") && template.contains("# border_width = 10.0"));

        // Uncommented lines change the defaults, and the settings last used
        // fall back to them.
        let edited = template
            .replace("# blur_radius = 2.0", "blur_radius = 4.0")
            .replace("# expand_radius = 5.0", "expand_radius = 12.0")
            .replace(
                "# separation_radius = 8.6  (unset: from the image's size)",
                "separation_radius = 9.5",
            );
        std::fs::write(&path, edited).unwrap();
        let defaults = FilterSettings::load_defaults(&path);
        assert_eq!(
            (defaults.blur_radius, defaults.expand_radius, defaults.separation_radius),
            (4.0, 12.0, Some(9.5))
        );
        let last = FilterSettings::load_from(&dir.join("filters.toml"), &defaults);
        assert_eq!(last.blur_radius, 4.0);
        assert_eq!(last.expand_radius, 12.0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn filters_take_and_give_their_settings() {
        let mut settings = FilterSettings::default();
        let sharper = LayerFilter::UnsharpMask { amount: 2.0, radius: 0.8, threshold: 4.0 };
        settings.remember(&sharper);
        assert_eq!(settings.filter(&LayerFilter::UnsharpMask { amount: 0.0, radius: 0.0, threshold: 0.0 }), sharper);
        assert_eq!(settings.filter(&LayerFilter::GaussianBlur { radius: 9.0 }), LayerFilter::GaussianBlur { radius: 2.0 });
    }
}
