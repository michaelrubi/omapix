//! Colours from the active Omarchy theme, reloaded when the theme changes.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, SystemTime};

use egui::{Color32, CornerRadius, Stroke, Visuals};

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub dark: bool,
    pub accent: Color32,
    pub selection: Color32,
    pub muted: Color32,
    pub background: Color32,
    pub dark_background: Color32,
    pub darker_background: Color32,
    pub lighter_background: Color32,
    pub foreground: Color32,
    pub dark_foreground: Color32,
    pub red: Color32,
}

impl Default for Theme {
    /// Used when no Omarchy theme is found (Catppuccin Mocha, Omarchy's default).
    fn default() -> Self {
        Self {
            dark: true,
            accent: hex("#89b4fa").unwrap(),
            selection: hex("#45475a").unwrap(),
            muted: hex("#585b70").unwrap(),
            background: hex("#1e1e2e").unwrap(),
            dark_background: hex("#161622").unwrap(),
            darker_background: hex("#101019").unwrap(),
            lighter_background: hex("#313244").unwrap(),
            foreground: hex("#cdd6f4").unwrap(),
            dark_foreground: hex("#6c7086").unwrap(),
            red: hex("#f38ba8").unwrap(),
        }
    }
}

fn theme_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/omarchy/current/theme"))
}

fn colors_path() -> Option<PathBuf> {
    theme_dir().map(|d| d.join("colors.toml"))
}

fn hex(s: &str) -> Option<Color32> {
    Color32::from_hex(s.trim()).ok()
}

impl Theme {
    /// Read the active Omarchy theme, falling back to defaults for anything missing.
    pub fn load() -> Self {
        let mut theme = Self::default();
        let Some(text) = colors_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
            return theme;
        };
        let table: toml::Table = match text.parse() {
            Ok(t) => t,
            Err(err) => {
                log::warn!("ignoring unreadable Omarchy colors.toml: {err}");
                return theme;
            }
        };
        let color = |key: &str| table.get(key).and_then(|v| v.as_str()).and_then(hex);
        if let Some(mode) = table.get("mode").and_then(|v| v.as_str()) {
            theme.dark = mode != "light";
        }
        for (field, key) in [
            (&mut theme.accent, "accent"),
            (&mut theme.selection, "selection"),
            (&mut theme.muted, "muted"),
            (&mut theme.background, "background"),
            (&mut theme.dark_background, "dark_background"),
            (&mut theme.darker_background, "darker_background"),
            (&mut theme.lighter_background, "lighter_background"),
            (&mut theme.foreground, "foreground"),
            (&mut theme.dark_foreground, "dark_foreground"),
            (&mut theme.red, "red"),
        ] {
            if let Some(c) = color(key) {
                *field = c;
            }
        }
        theme
    }

    /// The area around the image. Tinted backgrounds shift how colours in
    /// the photo are perceived, so this is a neutral grey with the same
    /// lightness as the theme's darkest background, like Photoshop's grey
    /// pasteboard.
    pub fn pasteboard(&self) -> Color32 {
        let c = self.darker_background;
        let luma =
            0.2126 * f32::from(c.r()) + 0.7152 * f32::from(c.g()) + 0.0722 * f32::from(c.b());
        Color32::from_gray(luma.round() as u8)
    }

    pub fn visuals(&self) -> Visuals {
        let mut v = if self.dark {
            Visuals::dark()
        } else {
            Visuals::light()
        };
        let square = CornerRadius::ZERO;

        v.override_text_color = Some(self.foreground);
        v.hyperlink_color = self.accent;
        v.panel_fill = self.dark_background;
        v.window_fill = self.background;
        v.window_stroke = Stroke::new(1.0, self.muted);
        v.window_corner_radius = square;
        v.menu_corner_radius = square;
        v.extreme_bg_color = self.darker_background;
        v.faint_bg_color = self.background;
        v.code_bg_color = self.darker_background;
        v.selection.bg_fill = self.accent.gamma_multiply(0.5);
        v.selection.stroke = Stroke::new(1.0, self.foreground);

        let w = &mut v.widgets;
        w.noninteractive.bg_fill = self.dark_background;
        w.noninteractive.weak_bg_fill = self.dark_background;
        w.noninteractive.bg_stroke = Stroke::new(1.0, self.selection);
        w.noninteractive.fg_stroke = Stroke::new(1.0, self.dark_foreground);
        for (state, fill) in [
            (&mut w.inactive, self.background),
            (&mut w.hovered, self.selection),
            (&mut w.active, self.lighter_background),
            (&mut w.open, self.selection),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.fg_stroke = Stroke::new(1.0, self.foreground);
        }
        w.hovered.bg_stroke = Stroke::new(1.0, self.accent);
        w.active.bg_stroke = Stroke::new(1.0, self.accent);
        for state in [
            &mut w.noninteractive,
            &mut w.inactive,
            &mut w.hovered,
            &mut w.active,
            &mut w.open,
        ] {
            state.corner_radius = square;
            state.expansion = 0.0;
        }
        v
    }
}

/// Watches the Omarchy theme files and delivers a freshly loaded theme
/// whenever they change. Polls once a second on its own thread, and only
/// wakes the UI when something actually changed.
pub fn watch(ctx: egui::Context) -> Receiver<Theme> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("theme-watch".into())
        .spawn(move || {
            let stamp = || -> Option<(SystemTime, SystemTime)> {
                let dir = theme_dir()?;
                let name = std::fs::metadata(dir.with_file_name("theme.name"))
                    .ok()?
                    .modified()
                    .ok()?;
                let colors = std::fs::metadata(dir.join("colors.toml"))
                    .ok()?
                    .modified()
                    .ok()?;
                Some((name, colors))
            };
            let mut last = stamp();
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let now = stamp();
                if now != last {
                    last = now;
                    if tx.send(Theme::load()).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            }
        })
        .expect("spawn theme watcher");
    rx
}

/// Use the system monospace font (the Omarchy font) for all UI text.
pub fn install_font(ctx: &egui::Context) {
    let Ok(out) = std::process::Command::new("fc-match")
        .args(["monospace", "-f", "%{file}"])
        .output()
    else {
        return;
    };
    let path = String::from_utf8_lossy(&out.stdout).into_owned();
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("omarchy".into(), egui::FontData::from_owned(bytes).into());
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, "omarchy".into());
    }
    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasteboard_is_neutral_grey() {
        let theme = Theme::default();
        let p = theme.pasteboard();
        assert_eq!(p.r(), p.g());
        assert_eq!(p.g(), p.b());
    }
}
