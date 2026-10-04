//! The colours of the monitor Omapix is on.
//!
//! Hyprland sends what applications draw straight to a monitor it takes for
//! sRGB (its default), so on a wide-gamut one every colour comes out too
//! saturated. Omapix then converts what it shows of an image to the
//! monitor's own colours itself: the canvas, thumbnails and previews all go
//! through the canvas's display transform. Where Hyprland has been told
//! what the monitor is (its `cm` setting), it does the converting, and
//! Omapix carries on drawing sRGB.
//!
//! Which monitor the window is on, and what Hyprland takes it for, come
//! from `hyprctl`; its colours from `~/.config/omapix/monitors.toml`, or
//! else from its EDID. Anywhere else (no Hyprland), it's sRGB as before.

use std::path::{Path, PathBuf};
use std::process::Command;

use omapix_engine::ColorProfile;

/// A monitor, as `hyprctl -j monitors` lists it.
#[derive(Clone, Debug, Default, serde::Deserialize)]
struct Monitor {
    id: i64,
    name: String,
    description: String,
    focused: bool,
    /// "srgb" unless Hyprland manages the monitor's colours.
    #[serde(rename = "colorManagementPreset", default)]
    preset: String,
}

/// A window, as `hyprctl -j clients` lists it.
#[derive(serde::Deserialize)]
struct Client {
    pid: i64,
    monitor: i64,
}

fn hyprctl<T: serde::de::DeserializeOwned>(what: &str) -> Option<T> {
    let out = Command::new("hyprctl").args(["-j", what]).output().ok()?;
    serde_json::from_slice(&out.stdout).ok()
}

/// The monitor Omapix's window is on, or before it has one, the focused
/// monitor, where it will open.
fn current() -> Option<Monitor> {
    let monitors: Vec<Monitor> = hyprctl("monitors")?;
    let clients: Vec<Client> = hyprctl("clients").unwrap_or_default();
    let pid = i64::from(std::process::id());
    let on = clients.iter().find(|c| c.pid == pid).map(|c| c.monitor);
    monitors.into_iter().find(|m| on.map_or(m.focused, |id| m.id == id))
}

/// The monitor's EDID, from `/sys/class/drm/card1-eDP-1/edid`.
fn edid(name: &str) -> Option<Vec<u8>> {
    std::fs::read_dir("/sys/class/drm").ok()?.flatten().find_map(|entry| {
        let file = entry.file_name();
        let (_, connector) = file.to_str()?.split_once('-')?;
        let edid = (connector == name).then(|| std::fs::read(entry.path().join("edid")).ok())??;
        (!edid.is_empty()).then_some(edid)
    })
}

/// The profile to show images in on `monitor` (`None`: sRGB), given what
/// `monitors.toml` asks for and the monitor's EDID.
fn profile(monitor: &Monitor, choices: &[(String, String)], edid: Option<&[u8]>) -> Option<ColorProfile> {
    let from_edid = || ColorProfile::from_edid(edid?, &monitor.name);
    let choice = choices
        .iter()
        .find(|(key, _)| *key == monitor.name || monitor.description.contains(key.as_str()));
    match choice.map(|(_, value)| value.as_str()) {
        Some("srgb") => None,
        Some("edid") => from_edid(),
        Some(path) => {
            let path = match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
                (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
                _ => PathBuf::from(path),
            };
            let read = std::fs::read(&path).map_err(|e| e.to_string());
            match read.and_then(|icc| ColorProfile::from_icc(icc).map_err(|e| e.to_string())) {
                Ok(profile) => Some(profile),
                Err(e) => {
                    log::warn!("monitors.toml: can't use {} for {}: {e}", path.display(), monitor.name);
                    None
                }
            }
        }
        // Hyprland converts sRGB to the monitor's colours itself.
        None if monitor.preset != "srgb" => None,
        None => from_edid(),
    }
}

const TEMPLATE: &str = r#"# The colour profile of each monitor, for showing images in its colours.
#
# A monitor is named as `hyprctl monitors` lists it: its name ("eDP-1") or
# part of its description ("ASUS PB278"). Its profile is one of:
#   a path to an ICC profile (from a colorimeter, or the maker's)
#   "edid"   the colours the monitor itself reports
#   "srgb"   none: images are shown as sRGB
#
# A monitor not listed uses "edid", unless Hyprland manages its colours (a
# `cm` setting other than srgb), when it's "srgb". If Hyprland has an `icc`
# profile for a monitor, set it to "srgb" here, or it's converted twice.
#
# "eDP-1" = "~/.local/share/icc/laptop.icc"
# "ASUS PB278" = "srgb"
"#;

/// What `monitors.toml` asks for. With no file yet, one is written that
/// explains itself.
fn choices(path: &Path) -> Vec<(String, String)> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let written = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(path, TEMPLATE));
            if let Err(e) = written {
                log::warn!("couldn't write {}: {e}", path.display());
            }
            return Vec::new();
        }
        Err(e) => {
            log::warn!("couldn't read {}: {e}", path.display());
            return Vec::new();
        }
    };
    match toml::from_str::<toml::Table>(&text) {
        Ok(table) => table
            .into_iter()
            .filter_map(|(key, value)| Some((key, value.as_str()?.to_owned())))
            .collect(),
        Err(e) => {
            log::warn!("monitors.toml: {e}");
            Vec::new()
        }
    }
}

/// Keeps track of the monitor Omapix is on and its profile.
pub struct Watch {
    choices: Vec<(String, String)>,
    /// The monitor's name, once Hyprland has said.
    name: Option<String>,
    profile: Option<ColorProfile>,
    /// What egui last said of the monitor (its size and scale) and whether
    /// Omapix had the focus: when these change it may be on another.
    seen: Option<(Option<egui::Vec2>, Option<f32>, bool)>,
}

impl Watch {
    /// For the monitor Omapix is about to open on.
    pub fn new() -> Self {
        let path = crate::recent::config_dir().map(|dir| dir.join("monitors.toml"));
        let mut watch = Self {
            choices: path.as_deref().map(choices).unwrap_or_default(),
            name: None,
            profile: None,
            seen: None,
        };
        watch.look();
        watch
    }

    /// The profile to show images in (`None`: sRGB).
    pub fn profile(&self) -> Option<&ColorProfile> {
        self.profile.as_ref()
    }

    /// Ask Hyprland which monitor it is. True if it's another than before.
    fn look(&mut self) -> bool {
        let Some(monitor) = current() else {
            return false;
        };
        if self.name.as_deref() == Some(&monitor.name) {
            return false;
        }
        self.profile = profile(&monitor, &self.choices, edid(&monitor.name).as_deref());
        let shown = self.profile.as_ref().map_or("sRGB", |p| p.description());
        log::info!("showing images on {} as {shown}", monitor.name);
        self.name = Some(monitor.name);
        true
    }

    /// Each frame: look again when the window may have changed monitor.
    /// True when it has, and the profile is that of the new one.
    pub fn check(&mut self, ctx: &egui::Context) -> bool {
        let now = ctx.input(|i| {
            let v = i.viewport();
            (v.monitor_size, v.native_pixels_per_point, v.focused.unwrap_or(true))
        });
        let before = self.seen.replace(now);
        let moved = before.is_some_and(|b| (b.0, b.1) != (now.0, now.1));
        let focused = now.2 && !before.is_some_and(|b| b.2);
        (moved || focused) && self.look()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An EDID with a wide-gamut panel's colours (see `color.rs`).
    fn edid() -> Vec<u8> {
        let mut edid = vec![0u8; 128];
        edid[..8].copy_from_slice(&[0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0]);
        edid[23] = 120;
        // Red (0.680, 0.320), green (0.237, 0.723), blue (0.140, 0.050),
        // white (0.3125, 0.329), eight bits each.
        edid[27..35].copy_from_slice(&[174, 82, 61, 185, 36, 13, 80, 84]);
        edid
    }

    fn monitor(preset: &str) -> Monitor {
        Monitor {
            id: 0,
            name: "eDP-2".into(),
            description: "Samsung Display Corp. 0x4190".into(),
            focused: true,
            preset: preset.into(),
        }
    }

    #[test]
    fn a_monitor_hyprland_takes_for_srgb_is_shown_in_its_edid_colours() {
        let edid = edid();
        let found = profile(&monitor("srgb"), &[], Some(&edid)).expect("a profile");
        assert_eq!(found.description(), "eDP-2 (EDID)");
        // Managed by Hyprland, or with no EDID to go by, it's sRGB.
        assert!(profile(&monitor("wide"), &[], Some(&edid)).is_none());
        assert!(profile(&monitor("srgb"), &[], None).is_none());
    }

    #[test]
    fn monitors_toml_decides_by_name_or_description() {
        let edid = edid();
        let choice = |key: &str, value: &str| vec![(key.to_owned(), value.to_owned())];
        assert!(profile(&monitor("srgb"), &choice("eDP-2", "srgb"), Some(&edid)).is_none());
        assert!(profile(&monitor("srgb"), &choice("Samsung", "srgb"), Some(&edid)).is_none());
        assert!(profile(&monitor("srgb"), &choice("DP-1", "srgb"), Some(&edid)).is_some(), "another's");
        assert!(profile(&monitor("wide"), &choice("eDP-2", "edid"), Some(&edid)).is_some());

        // An ICC profile's path, or sRGB if it can't be read.
        let dir = std::env::temp_dir().join(format!("omapix-monitor-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let icc = dir.join("monitor.icc");
        let made = ColorProfile::from_edid(&edid, "made").unwrap();
        std::fs::write(&icc, made.icc().unwrap()).unwrap();
        let found = profile(&monitor("srgb"), &choice("eDP-2", icc.to_str().unwrap()), None).unwrap();
        assert_eq!(found.description(), "made (EDID)");
        assert!(profile(&monitor("srgb"), &choice("eDP-2", "/nowhere.icc"), Some(&edid)).is_none());

        // The file is made when missing, and read back.
        let path = dir.join("monitors.toml");
        assert!(choices(&path).is_empty());
        assert!(std::fs::read_to_string(&path).unwrap().contains("\"edid\""));
        assert!(choices(&path).is_empty(), "all commented out");
        std::fs::write(&path, "\"eDP-2\" = \"srgb\"\n\"ASUS PB278\" = \"~/a.icc\"\n").unwrap();
        let read = choices(&path);
        assert!(read.contains(&("eDP-2".into(), "srgb".into())) && read.len() == 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `cargo test -p omapix on_this_machine -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn on_this_machine() {
        let monitor = current().expect("Hyprland's monitors");
        let edid = super::edid(&monitor.name);
        let found = profile(&monitor, &[], edid.as_deref());
        println!("{monitor:?}: EDID of {} bytes", edid.map_or(0, |e| e.len()));
        println!("shown as {}", found.as_ref().map_or("sRGB", |p| p.description()));
    }

    #[test]
    fn hyprctls_lists_are_understood() {
        let monitors: Vec<Monitor> = serde_json::from_str(
            r#"[{"id": 0, "name": "eDP-2", "description": "Samsung", "focused": true,
                 "colorManagementPreset": "srgb", "scale": 1.6},
                {"id": 1, "name": "DP-1", "description": "ASUS", "focused": false}]"#,
        )
        .unwrap();
        assert_eq!((monitors[0].preset.as_str(), monitors[1].preset.as_str()), ("srgb", ""));
        let clients: Vec<Client> = serde_json::from_str(r#"[{"pid": 42, "monitor": 1, "class": "omapix"}]"#).unwrap();
        assert_eq!((clients[0].pid, clients[0].monitor), (42, 1));
    }
}
