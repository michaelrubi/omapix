//! Actions, as in Photoshop: named lists of steps, recorded from what you
//! do or written by hand (or by an agent), played back from the Actions
//! panel, File › Automate or a function key.
//!
//! Each is a text file in `~/.config/omapix/actions/`, named after it, with
//! one `OMAPIX_SCRIPT` step a line:
//!
//! ```text
//! # Key: F2
//! NewCurves
//! SelectSubject
//! AddMask
//! Filter {"GaussianBlur":{"radius":7.0}}
//! Feather 5
//! ```
//!
//! `#` starts a comment; `# Key: …` gives the action a shortcut, written as
//! in `hotkeys.toml`. A filter's settings are its JSON, and a command that
//! asks for a radius takes it after its name.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use egui::{Align, Button, KeyboardShortcut, Layout, RichText, ScrollArea, Sense, Ui};
use serde_json::{Value, json};

use crate::commands::Command;
use crate::hotkeys::{format_shortcut, parse_shortcut};
use crate::theme::Theme;

#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub name: String,
    pub key: Option<KeyboardShortcut>,
    pub steps: Vec<String>,
}

/// Where actions are kept.
pub fn dir() -> Option<PathBuf> {
    crate::recent::config_dir().map(|d| d.join("actions"))
}

impl Action {
    /// An action from the text of its file.
    pub fn parse(name: &str, text: &str) -> Self {
        let mut key = None;
        let mut steps = Vec::new();
        for line in text.lines().map(str::trim) {
            if let Some(comment) = line.strip_prefix('#') {
                if let Some(k) = comment.trim().strip_prefix("Key:") {
                    key = parse_shortcut(k).ok().flatten();
                }
            } else if !line.is_empty() {
                steps.push(line.to_owned());
            }
        }
        Self {
            name: name.to_owned(),
            key,
            steps,
        }
    }

    pub fn to_text(&self) -> String {
        let mut text = String::new();
        if let Some(key) = &self.key {
            text += &format!("# Key: {}\n", format_shortcut(key));
        }
        for step in &self.steps {
            text += step;
            text.push('\n');
        }
        text
    }

    fn path(dir: &Path, name: &str) -> PathBuf {
        dir.join(format!("{}.txt", name.replace('/', "-")))
    }
}

/// The actions in `dir`, by name.
pub fn load(dir: &Path) -> Vec<Action> {
    let mut actions: Vec<Action> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .filter_map(|p| Some(Action::parse(p.file_stem()?.to_str()?, &std::fs::read_to_string(&p).ok()?)))
        .collect();
    actions.sort_by_key(|a| a.name.to_lowercase());
    actions
}

pub fn save(dir: &Path, action: &Action) -> Result<(), String> {
    if action.name.trim().is_empty() {
        return Err("an action needs a name".to_owned());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = Action::path(dir, &action.name);
    std::fs::write(&path, action.to_text()).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn delete(dir: &Path, name: &str) {
    let _ = std::fs::remove_file(Action::path(dir, name));
}

/// Whether running `cmd` is a step worth recording: not undoing, looking
/// around, or choosing files.
pub fn recordable(cmd: Command) -> bool {
    !matches!(
        cmd,
        Command::Undo
            | Command::Redo
            | Command::New
            | Command::NewFromClipboard
            | Command::Open
            | Command::ReopenLast
            | Command::Close
            | Command::Quit
            | Command::SaveAs
            | Command::ExportTiff
            | Command::ExportJpeg
            | Command::ExportPng
            | Command::ExportPsd
            | Command::ExportForWeb
            | Command::BatchExport
            | Command::Photomerge
            | Command::MergeToHdr
            | Command::SaveAdjustmentPreset
            | Command::ExportAdjustmentLut
            | Command::ImportBrushes
            | Command::NextImage
            | Command::PreviousImage
            | Command::ZoomIn
            | Command::ZoomOut
            | Command::FitOnScreen
            | Command::ActualPixels
            | Command::ShowLayers
            | Command::ShowChannels
            | Command::ShowHistory
            | Command::ShowActions
            | Command::ShowNavigator
            | Command::ShowHistogram
            | Command::ShowBrushSettings
            | Command::SelectionEdges
            | Command::AiModels
            | Command::LastFilter
    )
}

/// The MCP tools' answers that only need the files: `list_actions` and
/// `save_action`. `None` for other tools.
pub fn tool(name: &str, args: &Value) -> Option<Result<Value, String>> {
    let dir = || dir().ok_or_else(|| "there's no config directory".to_owned());
    Some(match name {
        "list_actions" => dir().map(|d| {
            let actions: Vec<Value> = load(&d)
                .iter()
                .map(|a| json!({ "name": a.name, "key": a.key.as_ref().map(format_shortcut), "steps": a.steps }))
                .collect();
            json!({ "actions": actions })
        }),
        "save_action" => (|| {
            let name = args["name"].as_str().ok_or("missing \"name\"")?;
            let steps: Vec<String> = args["steps"]
                .as_array()
                .ok_or("missing \"steps\"")?
                .iter()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect();
            if let Some(bad) = steps.iter().find(|s| !crate::app::step_parses(s)) {
                return Err(format!("can't understand {bad:?}"));
            }
            let key = match args["key"].as_str() {
                Some(k) => parse_shortcut(k)?,
                None => None,
            };
            let d = dir()?;
            save(
                &d,
                &Action {
                    name: name.to_owned(),
                    key,
                    steps,
                },
            )?;
            Ok(json!({ "saved": name }))
        })(),
        _ => return None,
    })
}

/// A schema for each of [`tool`]'s tools, and `play_action`'s.
pub fn tool_schemas() -> Vec<Value> {
    vec![
        json!({
            "name": "list_actions",
            "description": "List the saved actions (~/.config/omapix/actions/*.txt): each one's name, shortcut and steps.",
            "inputSchema": { "type": "object", "properties": {} },
        }),
        json!({
            "name": "save_action",
            "description": "Save an action, replacing one of the same name. Steps are OMAPIX_SCRIPT steps: a command name (NewCurves, SelectSubject, AddMask, Invert…), `Filter <LayerFilter JSON>` such as `Filter {\"GaussianBlur\":{\"radius\":7.0}}`, or a radius command with its radius such as `Feather 5`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "steps": { "type": "array", "items": { "type": "string" } },
                    "key": { "type": "string", "description": "A shortcut such as F2 or Shift+F3 (optional)" },
                },
                "required": ["name", "steps"],
            },
        }),
    ]
}

/// The function keys an action can have, as Photoshop offers.
fn keys() -> Vec<KeyboardShortcut> {
    use egui::{Key, Modifiers};
    let f = [
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
    ];
    let taken: Vec<KeyboardShortcut> = Command::ALL.iter().filter_map(|c| c.shortcut()).collect();
    [Modifiers::NONE, Modifiers::SHIFT, Modifiers::COMMAND]
        .into_iter()
        .flat_map(|m| f.map(|k| KeyboardShortcut::new(m, k)))
        .filter(|k| !taken.contains(k))
        .collect()
}

struct Recording {
    name: String,
    steps: Vec<String>,
}

/// What the Actions panel asks the app to do.
pub enum Asked {
    Play(Action),
}

/// The Actions panel, and the action being recorded.
pub struct ActionsPanel {
    dir: Option<PathBuf>,
    pub actions: Vec<Action>,
    selected: Option<String>,
    recording: Option<Recording>,
    /// When the folder was last looked at, and how it was then, so actions
    /// an agent writes show up.
    checked: Option<(Instant, Option<SystemTime>)>,
}

impl ActionsPanel {
    pub fn new(dir: Option<PathBuf>) -> Self {
        let mut panel = Self {
            dir,
            actions: Vec::new(),
            selected: None,
            recording: None,
            checked: None,
        };
        panel.refresh();
        panel
    }

    /// Load the actions again if their folder changed, looking at most once
    /// a second.
    pub fn refresh(&mut self) {
        let Some(dir) = &self.dir else { return };
        if self.checked.is_some_and(|(at, _)| at.elapsed() < Duration::from_secs(1)) {
            return;
        }
        let modified = std::fs::metadata(dir).and_then(|m| m.modified()).ok();
        if self.checked.is_none_or(|(_, m)| m != modified) {
            self.actions = load(dir);
        }
        self.checked = Some((Instant::now(), modified));
    }

    fn reload(&mut self) {
        self.checked = None;
        self.refresh();
    }

    pub fn find(&self, name: &str) -> Option<&Action> {
        self.actions.iter().find(|a| a.name == name)
    }

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// Add a step to the action being recorded, if one is.
    pub fn record(&mut self, step: String) {
        if let Some(r) = &mut self.recording {
            r.steps.push(step);
        }
    }

    #[cfg(test)]
    pub fn recorded(&self) -> Vec<String> {
        self.recording.as_ref().map_or(Vec::new(), |r| r.steps.clone())
    }

    pub fn start_recording(&mut self) {
        let mut n = self.actions.len() + 1;
        while self.find(&format!("Action {n}")).is_some() {
            n += 1;
        }
        self.recording = Some(Recording {
            name: format!("Action {n}"),
            steps: Vec::new(),
        });
    }

    /// Stop recording and save what was recorded.
    pub fn stop_recording(&mut self) -> Result<(), String> {
        let Some(r) = self.recording.take() else { return Ok(()) };
        let dir = self.dir.clone().ok_or("there's no config directory")?;
        let action = Action {
            name: r.name.trim().to_owned(),
            key: self.find(r.name.trim()).and_then(|a| a.key),
            steps: r.steps,
        };
        save(&dir, &action)?;
        self.selected = Some(action.name);
        self.reload();
        Ok(())
    }

    /// The action whose key was pressed this frame, consuming the key.
    pub fn pressed(&self, ctx: &egui::Context) -> Option<Action> {
        ctx.input_mut(|i| self.actions.iter().find(|a| a.key.is_some_and(|k| i.consume_shortcut(&k))).cloned())
    }

    pub fn show(&mut self, ui: &mut Ui, theme: &Theme) -> Option<Asked> {
        let mut asked = None;
        ui.add_space(4.0);
        ui.label(RichText::new("Actions").strong());
        ui.add_space(4.0);

        if let Some(r) = &mut self.recording {
            ui.horizontal(|ui| {
                ui.label(RichText::new("●").color(egui::Color32::from_rgb(220, 60, 60)));
                ui.text_edit_singleline(&mut r.name);
            });
            ScrollArea::vertical().max_height((ui.available_height() - 36.0).max(60.0)).show(ui, |ui| {
                if r.steps.is_empty() {
                    ui.label(RichText::new("Run commands and filters to record them.").color(theme.dark_foreground));
                }
                for step in &r.steps {
                    ui.label(RichText::new(step).color(theme.dark_foreground).monospace());
                }
            });
            ui.horizontal(|ui| {
                if ui.button("■ Stop").on_hover_text("Stop recording and save").clicked()
                    && let Err(e) = self.stop_recording()
                {
                    log::warn!("{e}");
                }
                if ui.button("Cancel").clicked() {
                    self.recording = None;
                }
            });
            return None;
        }

        let bottom = 30.0;
        ScrollArea::vertical()
            .max_height((ui.available_height() - bottom).max(60.0))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.actions.is_empty() {
                    ui.label(RichText::new("No actions yet. Press ● to record one.").color(theme.dark_foreground));
                }
                for action in &self.actions {
                    let selected = self.selected.as_deref() == Some(action.name.as_str());
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), Sense::hover());
                    let response = ui.interact(rect, egui::Id::new("action_row").with(&action.name), Sense::click());
                    if selected {
                        ui.painter().rect_filled(rect, 2.0, theme.selection);
                    } else if response.hovered() {
                        ui.painter().rect_filled(rect, 2.0, theme.lighter_background);
                    }
                    let font = egui::TextStyle::Body.resolve(ui.style());
                    ui.painter().text(
                        rect.left_center() + egui::vec2(6.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        &action.name,
                        font.clone(),
                        theme.foreground,
                    );
                    if let Some(key) = &action.key {
                        ui.painter().text(
                            rect.right_center() - egui::vec2(6.0, 0.0),
                            egui::Align2::RIGHT_CENTER,
                            format_shortcut(key),
                            font,
                            theme.dark_foreground,
                        );
                    }
                    if response.double_clicked() {
                        asked = Some(Asked::Play(action.clone()));
                    } else if response.clicked() {
                        self.selected = if selected { None } else { Some(action.name.clone()) };
                    }
                    if selected {
                        for step in &action.steps {
                            ui.label(RichText::new(format!("    {step}")).color(theme.dark_foreground).monospace());
                        }
                    }
                }
            });

        let selected = self.selected.as_ref().and_then(|n| self.find(n)).cloned();
        ui.horizontal(|ui| {
            if ui.button("●").on_hover_text("Record a new action").clicked() {
                self.start_recording();
            }
            if ui.add_enabled(selected.is_some(), Button::new("▶")).on_hover_text("Play the action").clicked()
                && let Some(action) = &selected
            {
                asked = Some(Asked::Play(action.clone()));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let Some(action) = &selected else { return };
                let Some(dir) = self.dir.clone() else { return };
                if ui.button("Delete").on_hover_text("Delete the action").clicked() {
                    delete(&dir, &action.name);
                    self.selected = None;
                    self.reload();
                    return;
                }
                let mut key = action.key;
                egui::ComboBox::from_id_salt("action_key")
                    .selected_text(key.as_ref().map_or("No key".to_owned(), format_shortcut))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut key, None, "No key");
                        for k in keys() {
                            let used = self.actions.iter().any(|a| a.name != action.name && a.key == Some(k));
                            ui.add_enabled_ui(!used, |ui| ui.selectable_value(&mut key, Some(k), format_shortcut(&k)));
                        }
                    });
                if key != action.key {
                    let changed = Action { key, ..action.clone() };
                    if let Err(e) = save(&dir, &changed) {
                        log::warn!("{e}");
                    }
                    self.reload();
                }
            });
        });
        asked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_action_file_round_trips_with_its_key_and_comments() {
        let text = "# Key: Shift+F3\n# Colour grade\nNewCurves\n\nSelectSubject\n  AddMask\n";
        let action = Action::parse("Grade", text);
        assert_eq!(action.key, Some(KeyboardShortcut::new(egui::Modifiers::SHIFT, egui::Key::F3)));
        assert_eq!(action.steps, ["NewCurves", "SelectSubject", "AddMask"]);
        assert_eq!(Action::parse("Grade", &action.to_text()), action);
    }

    #[test]
    fn actions_are_saved_listed_and_deleted() {
        let dir = std::env::temp_dir().join(format!("omapix-actions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let action = Action {
            name: "b/c".to_owned(),
            key: None,
            steps: vec!["Invert".to_owned()],
        };
        save(&dir, &action).unwrap();
        save(
            &dir,
            &Action {
                name: "A".to_owned(),
                ..action.clone()
            },
        )
        .unwrap();
        let names: Vec<String> = load(&dir).into_iter().map(|a| a.name).collect();
        assert_eq!(names, ["A", "b-c"]);
        delete(&dir, "A");
        assert_eq!(load(&dir).len(), 1);
        assert!(
            save(
                &dir,
                &Action {
                    name: " ".to_owned(),
                    ..action
                }
            )
            .is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn function_keys_skip_those_commands_use() {
        let keys = keys();
        assert!(keys.contains(&KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::F2)));
        assert!(!keys.contains(&KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::F7)));
    }
}
