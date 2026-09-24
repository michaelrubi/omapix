//! Recent files store, saved to `~/.config/omapix/recent.toml`.

use std::path::{Path, PathBuf};

const MAX_RECENT: usize = 10;

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct RecentStore {
    #[serde(default)]
    pub files: Vec<PathBuf>,
}

fn default_config_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(dir.join("omapix/recent.toml"))
}

impl RecentStore {
    pub fn load() -> Self {
        default_config_path()
            .and_then(|p| Self::load_from(&p))
            .unwrap_or_default()
    }

    pub fn load_from(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        let mut store: Self = toml::from_str(&text).ok()?;
        let mut seen = std::collections::HashSet::new();
        store.files.retain(|p| p.exists() && seen.insert(p.clone()));
        store.files.truncate(MAX_RECENT);
        Some(store)
    }

    pub fn save(&self) {
        if let Some(path) = default_config_path() {
            self.save_to(&path);
        }
    }

    pub fn save_to(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = toml::to_string(self) {
            let _ = std::fs::write(path, text);
        }
    }

    pub fn add(&mut self, path: &Path) {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.files.retain(|p| p != &canonical && p != path);
        self.files.insert(0, canonical);
        self.files.truncate(MAX_RECENT);
        self.save();
    }

    pub fn remove(&mut self, path: &Path) {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.files.retain(|p| p != &canonical && p != path);
        self.save();
    }

    pub fn clear(&mut self) {
        self.files.clear();
        self.save();
    }

    pub fn last(&self) -> Option<&PathBuf> {
        self.files.first()
    }

    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_store_add_deduplicates_and_caps() {
        let dir = std::env::temp_dir().join(format!("omapix_recent_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file1 = dir.join("a.png");
        let file2 = dir.join("b.png");
        let file3 = dir.join("c.png");
        std::fs::write(&file1, b"1").unwrap();
        std::fs::write(&file2, b"2").unwrap();
        std::fs::write(&file3, b"3").unwrap();

        let mut store = RecentStore::default();
        store.add(&file1);
        store.add(&file2);
        store.add(&file1); // re-adding moves to front

        let c1 = std::fs::canonicalize(&file1).unwrap();
        let c2 = std::fs::canonicalize(&file2).unwrap();
        assert_eq!(store.files(), &[c1.clone(), c2.clone()]);
        assert_eq!(store.last(), Some(&c1));

        let config_file = dir.join("recent.toml");
        store.save_to(&config_file);

        let loaded = RecentStore::load_from(&config_file).unwrap();
        assert_eq!(loaded.files(), &[c1, c2]);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recent_store_clear_and_remove() {
        let mut store = RecentStore::default();
        let p1 = PathBuf::from("/nonexistent/file1.png");
        let p2 = PathBuf::from("/nonexistent/file2.png");
        store.files.push(p1.clone());
        store.files.push(p2.clone());
        assert_eq!(store.files().len(), 2);

        store.remove(&p1);
        assert_eq!(store.files(), &[p2]);

        store.clear();
        assert!(store.files().is_empty());
        assert_eq!(store.last(), None);
    }
}
