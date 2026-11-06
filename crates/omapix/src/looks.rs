//! Named looks: Auto Retouch's settings kept under a name, such as a
//! client's, in `~/.config/omapix/looks.toml`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::auto_retouch::Retouch;

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Looks {
    looks: BTreeMap<String, Retouch>,
}

/// None under `cargo test`, so tests don't touch the real looks.
fn config_path() -> Option<PathBuf> {
    crate::recent::config_dir().filter(|_| !cfg!(test)).map(|d| d.join("looks.toml"))
}

impl Looks {
    pub fn load() -> Self {
        config_path().map(|p| crate::settings::load_from(&p, &Self::default())).unwrap_or_default()
    }

    fn save(&self) {
        crate::settings::save(config_path().as_deref(), self, "Looks");
    }

    /// The looks, by name.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Retouch)> {
        self.looks.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.looks.is_empty()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.looks.contains_key(name.trim())
    }

    /// Keep `settings` as `name`, replacing any look of that name.
    pub fn set(&mut self, name: &str, settings: Retouch) {
        let name = name.trim();
        if !name.is_empty() {
            self.looks.insert(name.to_owned(), settings);
            self.save();
        }
    }

    pub fn remove(&mut self, name: &str) {
        self.looks.remove(name.trim());
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto_retouch::Preset;

    #[test]
    fn looks_are_kept_in_a_file_by_their_trimmed_names() {
        let mut settings = Preset::Natural.settings();
        settings.keep_beauty_marks = true;
        settings.whiten_teeth.on = false;
        let mut looks = Looks::default();
        looks.set("  Ana  ", settings);
        looks.set("Ben", Preset::Strong.settings());
        looks.set("   ", Preset::Strong.settings());
        assert_eq!(looks.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["Ana", "Ben"]);

        let dir = std::env::temp_dir().join(format!("omapix_looks_test_{}", std::process::id()));
        let path = dir.join("looks.toml");
        crate::settings::save(Some(&path), &looks, "Looks");
        let loaded = crate::settings::load_from(&path, &Looks::default());
        assert_eq!(loaded, looks);
        let _ = std::fs::remove_dir_all(dir);

        looks.remove(" Ben ");
        assert!(looks.contains("Ana") && !looks.contains("Ben"));
    }
}
