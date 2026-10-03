//! The models Omapix knows (`models.txt`, which scripts/fetch-models.sh
//! reads too), and where they are on disk: in Omapix's own models folder,
//! or anywhere in darktable's or in a folder listed in
//! `~/.config/omapix/model_folders`, recognised by size and SHA-256 rather
//! than by name (docs/AI.md, "Models on disk").

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

/// A model, as `models.txt` lists it.
#[derive(Debug, PartialEq)]
pub struct Model {
    pub id: &'static str,
    pub name: &'static str,
    /// The commands that use it.
    pub used_by: &'static str,
    pub licence: &'static str,
    /// Only downloaded when asked for by its id: the big ones.
    pub optional: bool,
    pub files: Vec<File>,
}

#[derive(Debug, PartialEq)]
pub struct File {
    pub name: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
    /// Where scripts/fetch-models.sh downloads it from; `None` if darktable
    /// installs it.
    pub url: Option<&'static str>,
}

/// Every model Omapix uses.
pub fn models() -> &'static [Model] {
    static MODELS: OnceLock<Vec<Model>> = OnceLock::new();
    MODELS.get_or_init(|| parse(include_str!("../models.txt")))
}

fn parse(manifest: &'static str) -> Vec<Model> {
    let mut models: Vec<Model> = Vec::new();
    for line in manifest.lines().filter(|l| !l.is_empty() && !l.starts_with('#')) {
        match line.split('|').collect::<Vec<_>>()[..] {
            [kind @ ("model" | "optional"), id, name, used_by, licence, ..] => {
                let optional = kind == "optional";
                models.push(Model { id, name, used_by, licence, optional, files: Vec::new() })
            }
            ["file", id, name, bytes, sha256, url] => {
                let model = models.iter_mut().find(|m| m.id == id).expect("a file's model comes before it");
                let bytes = bytes.parse().expect("a file's size in bytes");
                let url = (!url.is_empty()).then_some(url);
                model.files.push(File { name, bytes, sha256, url });
            }
            _ => panic!("models.txt: {line}"),
        }
    }
    models
}

/// Model `id`'s files, by name, if they're all on disk.
pub fn find_model(id: &str) -> Option<HashMap<&'static str, PathBuf>> {
    let model = models().iter().find(|m| m.id == id)?;
    let data = data_dir()?;
    let mut shared = vec![data.join("darktable/models")];
    shared.extend(listed_folders());
    model
        .files
        .iter()
        .map(|file| Some((file.name, find_file(file, &data.join("omapix/models").join(id), &shared)?)))
        .collect()
}

/// Where `file` is: in `own` (put there by scripts/fetch-models.sh, which
/// checked it) if it's the right size, or else anywhere in `shared` with
/// the right size and checksum.
fn find_file(file: &File, own: &Path, shared: &[PathBuf]) -> Option<PathBuf> {
    let path = own.join(file.name);
    if path.metadata().is_ok_and(|m| m.len() == file.bytes) {
        return Some(path);
    }
    let mut candidates = Vec::new();
    for folder in shared {
        same_size(folder, file.bytes, &mut candidates);
    }
    candidates.into_iter().find(|path| sha256(path).is_some_and(|sha| sha == file.sha256))
}

/// Files in `folder` and below of `bytes`.
fn same_size(folder: &Path, bytes: u64, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            same_size(&entry.path(), bytes, found);
        } else if entry.metadata().is_ok_and(|m| m.len() == bytes) {
            found.push(entry.path());
        }
    }
}

/// `path`'s SHA-256, in hex. Remembered while the file's unchanged, since
/// hashing a model takes a moment.
fn sha256(path: &Path) -> Option<String> {
    static KNOWN: Mutex<Vec<(PathBuf, SystemTime, String)>> = Mutex::new(Vec::new());
    let modified = path.metadata().ok()?.modified().ok()?;
    let mut known = KNOWN.lock().ok()?;
    if let Some((.., sha)) = known.iter().find(|(p, m, _)| p == path && *m == modified) {
        return Some(sha.clone());
    }
    let mut hasher = Sha256::new();
    let mut reader = std::fs::File::open(path).ok()?;
    let mut buffer = vec![0; 1 << 20];
    loop {
        match reader.read(&mut buffer).ok()? {
            0 => break,
            n => hasher.update(&buffer[..n]),
        }
    }
    let sha: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    known.push((path.to_owned(), modified, sha.clone()));
    Some(sha)
}

fn data_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
}

/// The folders listed in `~/.config/omapix/model_folders`, one a line.
fn listed_folders() -> Vec<PathBuf> {
    let Some(config) = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    else {
        return Vec::new();
    };
    let home = std::env::var("HOME").unwrap_or_default();
    std::fs::read_to_string(config.join("omapix/model_folders"))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| PathBuf::from(l.strip_prefix('~').map_or_else(|| l.to_owned(), |rest| format!("{home}{rest}"))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_lists_each_model_with_its_files() {
        let sam = models().iter().find(|m| m.id == crate::sam::MODEL).unwrap();
        assert_eq!(sam.files.iter().map(|f| f.name).collect::<Vec<_>>(), ["encoder.onnx", "decoder.onnx"]);
        assert!(sam.files.iter().all(|f| f.url.is_none() && f.sha256.len() == 64));
        for id in [crate::lama::MODEL, crate::subject::MODEL, crate::flux::MODEL] {
            let model = models().iter().find(|m| m.id == id).unwrap();
            assert!(model.files.iter().all(|f| f.url.is_some_and(|u| u.starts_with("https://"))), "{id}");
            // Only the big one has to be asked for.
            assert_eq!(model.optional, id == crate::flux::MODEL);
        }
    }

    #[test]
    fn files_are_found_in_their_own_folder_by_size_and_elsewhere_by_checksum() {
        let root = std::env::temp_dir().join(format!("omapix-models-{}", std::process::id()));
        let (own, shared) = (root.join("own"), root.join("shared"));
        std::fs::create_dir_all(own.join("m")).unwrap();
        std::fs::create_dir_all(shared.join("deep/er")).unwrap();
        // SHA-256 of "hello".
        let file = File {
            name: "model.onnx",
            bytes: 5,
            sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
            url: None,
        };
        let find = || find_file(&file, &own.join("m"), std::slice::from_ref(&shared));
        assert_eq!(find(), None);

        // Anywhere in a shared folder, under any name, if it's the same file.
        std::fs::write(shared.join("deep/er/other name.onnx"), "jello").unwrap();
        assert_eq!(find(), None, "same size, different file");
        std::fs::write(shared.join("deep/renamed.onnx"), "hello").unwrap();
        assert_eq!(find(), Some(shared.join("deep/renamed.onnx")));

        // Omapix's own copy first.
        std::fs::write(own.join("m/model.onnx"), "hello").unwrap();
        assert_eq!(find(), Some(own.join("m/model.onnx")));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
