//! Custom hotkeys configuration loaded from `~/.config/omapix/hotkeys.toml`.
//!
//! Overrides command shortcuts and tool letters. Keyed by command names
//! (`Command::from_name` / `format!("{c:?}")`) and tool names (`Tool::name` /
//! `format!("{t:?}")`).
//!
//! Format:
//! ```toml
//! [commands]
//! MergeDown = "Ctrl+E"
//! NewCurves = "Ctrl+Alt+M"
//! StampVisible = ""          # empty string: no shortcut
//!
//! [tools]
//! Eyedropper = "I"
//! ```

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use egui::{Key, KeyboardShortcut, Modifiers};
use serde::Deserialize;

use crate::commands::Command;
use crate::tools::{Tool, ToolGroup};

static HOTKEYS: RwLock<Option<Hotkeys>> = RwLock::new(None);

#[cfg(test)]
pub(crate) static TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Hotkeys {
    commands: HashMap<Command, Option<KeyboardShortcut>>,
    tools: HashMap<ToolGroup, Option<Key>>,
    keyboard_order: Vec<Command>,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self::from_overrides(HashMap::new(), HashMap::new())
    }
}

impl Hotkeys {
    pub(crate) fn from_overrides(
        command_overrides: HashMap<Command, Option<KeyboardShortcut>>,
        tool_overrides: HashMap<ToolGroup, Option<Key>>,
    ) -> Self {
        let mut commands = HashMap::new();
        for &cmd in Command::ALL {
            let sc = match command_overrides.get(&cmd) {
                Some(override_sc) => *override_sc,
                None => cmd.default_shortcut(),
            };
            commands.insert(cmd, sc);
        }

        let mut tools = HashMap::new();
        for &group in ToolGroup::ALL {
            let k = match tool_overrides.get(&group) {
                Some(override_k) => *override_k,
                None => group.default_key(),
            };
            tools.insert(group, k);
        }

        let mut keyboard_order: Vec<Command> = Command::KEYBOARD_ORDER
            .iter()
            .copied()
            .filter(|c| commands.get(c).copied().flatten().is_some())
            .collect();
        for &cmd in Command::ALL {
            if !keyboard_order.contains(&cmd) && commands.get(&cmd).copied().flatten().is_some() {
                keyboard_order.push(cmd);
            }
        }

        // Sort by modifier count descending (most modifiers first).
        // Stable sort preserves a deterministic relative order.
        keyboard_order.sort_by(|a, b| {
            let count_a = commands.get(a).copied().flatten().map_or(0, |s| modifier_count(&s));
            let count_b = commands.get(b).copied().flatten().map_or(0, |s| modifier_count(&s));
            count_b.cmp(&count_a)
        });

        Self {
            commands,
            tools,
            keyboard_order,
        }
    }

    pub(crate) fn command(&self, cmd: Command) -> Option<KeyboardShortcut> {
        self.commands.get(&cmd).copied().flatten()
    }

    pub(crate) fn tool_group(&self, group: ToolGroup) -> Option<Key> {
        self.tools.get(&group).copied().flatten()
    }

    pub(crate) fn keyboard_order(&self) -> &[Command] {
        &self.keyboard_order
    }

    pub(crate) fn check_clashes(&self, warnings: &mut Vec<String>) {
        let mut by_shortcut: BTreeMap<String, (KeyboardShortcut, Vec<Command>)> = BTreeMap::new();
        for &cmd in Command::ALL {
            if let Some(sc) = self.command(cmd) {
                let key = format_shortcut(&sc);
                by_shortcut
                    .entry(key)
                    .or_insert_with(|| (sc, Vec::new()))
                    .1
                    .push(cmd);
            }
        }

        for (sc_str, (_sc, cmds)) in &by_shortcut {
            if cmds.len() > 1 {
                let names: Vec<_> = cmds.iter().map(|c| format!("{c:?}")).collect();
                warnings.push(format!(
                    "Hotkeys: clash between commands {} on shortcut {sc_str}",
                    names.join(" and ")
                ));
            }
        }

        for &group in ToolGroup::ALL {
            if let Some(tool_key) = self.tool_group(group) {
                let tool_sc = KeyboardShortcut::new(Modifiers::NONE, tool_key);
                let sc_str = format_shortcut(&tool_sc);
                if let Some((_sc, cmds)) = by_shortcut.get(&sc_str) {
                    for cmd in cmds {
                        warnings.push(format!(
                            "Hotkeys: clash between command {cmd:?} and tool {group:?} on shortcut {sc_str}"
                        ));
                    }
                }
            }
        }

        let mut by_tool_key: BTreeMap<Key, Vec<ToolGroup>> = BTreeMap::new();
        for &group in ToolGroup::ALL {
            if let Some(k) = self.tool_group(group) {
                by_tool_key.entry(k).or_default().push(group);
            }
        }
        for (k, groups) in &by_tool_key {
            if groups.len() > 1 {
                let names: Vec<_> = groups.iter().map(|g| format!("{g:?}")).collect();
                warnings.push(format!(
                    "Hotkeys: clash between tools {} on key {}",
                    names.join(" and "),
                    k.symbol_or_name()
                ));
            }
        }
    }
}

pub(crate) fn modifier_count(s: &KeyboardShortcut) -> usize {
    (s.modifiers.alt as usize)
        + (s.modifiers.shift as usize)
        + ((s.modifiers.command || s.modifiers.ctrl || s.modifiers.mac_cmd) as usize)
}

pub(crate) fn format_shortcut(s: &KeyboardShortcut) -> String {
    let mut parts = Vec::new();
    if s.modifiers.command || s.modifiers.ctrl || s.modifiers.mac_cmd {
        parts.push("Ctrl");
    }
    if s.modifiers.alt {
        parts.push("Alt");
    }
    if s.modifiers.shift {
        parts.push("Shift");
    }
    parts.push(s.logical_key.symbol_or_name());
    parts.join("+")
}

pub(crate) fn parse_shortcut(s: &str) -> Result<Option<KeyboardShortcut>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }

    let (mods_part, key_part) = if s == "+" {
        ("", "+")
    } else if let Some(stripped) = s.strip_suffix("++") {
        (stripped, "+")
    } else if let Some((m, k)) = s.rsplit_once('+') {
        (m, k)
    } else {
        ("", s)
    };

    let mut modifiers = Modifiers::NONE;
    if !mods_part.is_empty() {
        for mod_str in mods_part.split('+') {
            let m = mod_str.trim().to_ascii_lowercase();
            match m.as_str() {
                "ctrl" | "control" | "cmd" | "command" | "super" | "meta" => {
                    modifiers.command = true;
                }
                "alt" | "opt" | "option" => {
                    modifiers.alt = true;
                }
                "shift" => {
                    modifiers.shift = true;
                }
                _ => return Err(format!("unknown modifier {mod_str:?} in shortcut {s:?}")),
            }
        }
    }

    let key_str = key_part.trim();
    if key_str.is_empty() {
        return Err(format!("missing key in shortcut {s:?}"));
    }

    let key = Key::from_name(key_str)
        .ok_or_else(|| format!("unknown key {key_str:?} in shortcut {s:?}"))?;

    Ok(Some(KeyboardShortcut::new(modifiers, key)))
}

pub(crate) fn parse_tool_key(s: &str) -> Result<Option<Key>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    Key::from_name(s).map(Some).ok_or_else(|| format!("unknown key {s:?}"))
}

pub(crate) fn parse_command(name: &str) -> Option<Command> {
    let clean = name.trim();
    if let Some(cmd) = Command::from_name(clean) {
        return Some(cmd);
    }
    let normalized = clean.replace(['_', '-'], "");
    Command::ALL.iter().copied().find(|c| {
        let debug_name = format!("{c:?}");
        debug_name.eq_ignore_ascii_case(clean) || debug_name.eq_ignore_ascii_case(&normalized)
    })
}

pub(crate) fn parse_tool_group(name: &str) -> Option<ToolGroup> {
    let clean = name.trim();
    let normalized = clean.replace(['_', '-', ' '], "");
    for &group in ToolGroup::ALL {
        let g_str = format!("{group:?}");
        if g_str.eq_ignore_ascii_case(clean) || g_str.eq_ignore_ascii_case(&normalized) {
            return Some(group);
        }
    }
    for &tool in &[
        Tool::Move,
        Tool::Brush,
        Tool::Eraser,
        Tool::CloneStamp,
        Tool::SpotHealing,
        Tool::Healing,
        Tool::Marquee,
        Tool::EllipticalMarquee,
        Tool::Lasso,
        Tool::MagicWand,
        Tool::Eyedropper,
    ] {
        let t_str = format!("{tool:?}");
        let n_str = tool.name();
        if t_str.eq_ignore_ascii_case(clean)
            || t_str.eq_ignore_ascii_case(&normalized)
            || n_str.eq_ignore_ascii_case(clean)
            || n_str.replace(['_', '-', ' '], "").eq_ignore_ascii_case(&normalized)
        {
            return Some(tool.group());
        }
    }
    match clean.to_ascii_lowercase().as_str() {
        "clone" => Some(ToolGroup::CloneStamp),
        "heal" | "spotheal" => Some(ToolGroup::Healing),
        "ellipse" => Some(ToolGroup::Marquee),
        "wand" => Some(ToolGroup::Wand),
        _ => None,
    }
}

#[derive(Debug, Deserialize, Default)]
struct HotkeysFile {
    #[serde(default)]
    commands: BTreeMap<String, String>,
    #[serde(default)]
    tools: BTreeMap<String, String>,
}

pub(crate) fn parse_file(s: &str) -> (Hotkeys, Vec<String>) {
    let file: HotkeysFile = match toml::from_str(s) {
        Ok(f) => f,
        Err(e) => {
            return (
                Hotkeys::default(),
                vec![format!("Hotkeys: failed to parse hotkeys.toml: {e}")],
            );
        }
    };

    let mut command_overrides = HashMap::new();
    let mut tool_overrides = HashMap::new();
    let mut warnings = Vec::new();

    for (key, val) in &file.commands {
        match parse_command(key) {
            Some(cmd) => match parse_shortcut(val) {
                Ok(sc) => {
                    command_overrides.insert(cmd, sc);
                }
                Err(_) => {
                    warnings.push(format!(
                        "Hotkeys: invalid shortcut '{val}' for command '{key}'"
                    ));
                }
            },
            None => {
                warnings.push(format!("Hotkeys: unknown command '{key}'"));
            }
        }
    }

    for (key, val) in &file.tools {
        match parse_tool_group(key) {
            Some(group) => match parse_tool_key(val) {
                Ok(k) => {
                    tool_overrides.insert(group, k);
                }
                Err(_) => {
                    warnings.push(format!("Hotkeys: invalid tool letter '{val}' for tool '{key}'"));
                }
            },
            None => {
                warnings.push(format!("Hotkeys: unknown tool '{key}'"));
            }
        }
    }

    let hotkeys = Hotkeys::from_overrides(command_overrides, tool_overrides);
    hotkeys.check_clashes(&mut warnings);

    (hotkeys, warnings)
}

pub(crate) fn default_path() -> Option<PathBuf> {
    crate::recent::config_dir().map(|d| d.join("hotkeys.toml"))
}

pub(crate) fn load() -> Vec<String> {
    match default_path() {
        Some(path) => load_from(&path),
        None => Vec::new(),
    }
}

pub(crate) fn load_from(path: &Path) -> Vec<String> {
    if !path.exists() {
        let mut guard = HOTKEYS.write().unwrap();
        *guard = Some(Hotkeys::default());
        return Vec::new();
    }
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            let mut guard = HOTKEYS.write().unwrap();
            *guard = Some(Hotkeys::default());
            return vec![format!("Hotkeys: failed to read {}: {e}", path.display())];
        }
    };
    load_from_str(&content)
}

pub(crate) fn load_from_str(s: &str) -> Vec<String> {
    let (hotkeys, warnings) = parse_file(s);
    let mut guard = HOTKEYS.write().unwrap();
    *guard = Some(hotkeys);
    warnings
}

pub(crate) fn command_shortcut(cmd: Command) -> Option<KeyboardShortcut> {
    let guard = HOTKEYS.read().unwrap();
    match &*guard {
        Some(h) => h.command(cmd),
        None => cmd.default_shortcut(),
    }
}

pub(crate) fn tool_group_key(group: ToolGroup) -> Option<Key> {
    let guard = HOTKEYS.read().unwrap();
    match &*guard {
        Some(h) => h.tool_group(group),
        None => group.default_key(),
    }
}

pub(crate) fn with_keyboard_order<R>(f: impl FnOnce(&[Command]) -> R) -> R {
    let guard = HOTKEYS.read().unwrap();
    match &*guard {
        Some(h) => f(h.keyboard_order()),
        None => f(Command::KEYBOARD_ORDER),
    }
}

#[cfg(test)]
pub(crate) fn with_test_hotkeys<R>(toml: &str, f: impl FnOnce() -> R) -> R {
    let _guard = TEST_MUTEX.lock().unwrap();
    let prev = { HOTKEYS.read().unwrap().clone() };
    load_from_str(toml);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    {
        let mut guard = HOTKEYS.write().unwrap();
        *guard = prev;
    }
    match result {
        Ok(val) => val,
        Err(err) => std::panic::resume_unwind(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_shortcuts_modifiers_brackets_functions_symbols_and_empty() {
        // Modifiers and basic keys
        let s = parse_shortcut("Ctrl+E").unwrap().unwrap();
        assert_eq!(s.modifiers, Modifiers::COMMAND);
        assert_eq!(s.logical_key, Key::E);

        let s = parse_shortcut("Ctrl+Shift+Z").unwrap().unwrap();
        assert_eq!(
            s.modifiers,
            Modifiers {
                shift: true,
                ..Modifiers::COMMAND
            }
        );
        assert_eq!(s.logical_key, Key::Z);

        let s = parse_shortcut("Ctrl+Alt+M").unwrap().unwrap();
        assert_eq!(
            s.modifiers,
            Modifiers {
                alt: true,
                ..Modifiers::COMMAND
            }
        );
        assert_eq!(s.logical_key, Key::M);

        // Brackets
        let s = parse_shortcut("Ctrl+Shift+[").unwrap().unwrap();
        assert_eq!(s.logical_key, Key::OpenBracket);
        assert!(s.modifiers.shift);
        assert!(s.modifiers.command);

        let s = parse_shortcut("Alt+]").unwrap().unwrap();
        assert_eq!(s.logical_key, Key::CloseBracket);
        assert_eq!(s.modifiers, Modifiers::ALT);

        // Function keys
        let s = parse_shortcut("F7").unwrap().unwrap();
        assert_eq!(s.modifiers, Modifiers::NONE);
        assert_eq!(s.logical_key, Key::F7);

        let s = parse_shortcut("Shift+F6").unwrap().unwrap();
        assert_eq!(s.modifiers, Modifiers::SHIFT);
        assert_eq!(s.logical_key, Key::F6);

        // Slash and backslash
        let s = parse_shortcut("/").unwrap().unwrap();
        assert_eq!(s.modifiers, Modifiers::NONE);
        assert_eq!(s.logical_key, Key::Slash);

        let s = parse_shortcut("\\").unwrap().unwrap();
        assert_eq!(s.modifiers, Modifiers::NONE);
        assert_eq!(s.logical_key, Key::Backslash);

        // Plus key
        let s = parse_shortcut("+").unwrap().unwrap();
        assert_eq!(s.modifiers, Modifiers::NONE);
        assert_eq!(s.logical_key, Key::Plus);

        let s = parse_shortcut("Ctrl++").unwrap().unwrap();
        assert_eq!(s.modifiers, Modifiers::COMMAND);
        assert_eq!(s.logical_key, Key::Plus);

        // Empty string
        assert_eq!(parse_shortcut("").unwrap(), None);
        assert_eq!(parse_shortcut("   ").unwrap(), None);

        // Errors
        assert!(parse_shortcut("Ctrl+").is_err());
        assert!(parse_shortcut("InvalidMod+K").is_err());
        assert!(parse_shortcut("Ctrl+NonExistentKey").is_err());
    }

    #[test]
    fn parse_tool_keys() {
        assert_eq!(parse_tool_key("I").unwrap(), Some(Key::I));
        assert_eq!(parse_tool_key("i").unwrap(), Some(Key::I));
        assert_eq!(parse_tool_key("").unwrap(), None);
        assert_eq!(parse_tool_key("   ").unwrap(), None);
        assert!(parse_tool_key("Ctrl+I").is_err());
        assert!(parse_tool_key("Invalid").is_err());
    }

    #[test]
    fn clash_detection_reported() {
        let toml = r#"
[commands]
NewCurves = "Ctrl+E"
[tools]
Eyedropper = "/"
"#;
        let (_, warnings) = parse_file(toml);
        // Clash 1: NewCurves and MergeDown both on Ctrl+E
        assert!(
            warnings.iter().any(|w| w.contains("clash between commands") && w.contains("Ctrl+E")),
            "expected clash on Ctrl+E, got: {warnings:?}"
        );
        // Clash 2: Eyedropper tool letter is / which clashes with LockTransparent command (/)
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("clash between command") && w.contains("LockTransparent") && w.contains("Eyedropper")),
            "expected clash between LockTransparent and Eyedropper, got: {warnings:?}"
        );
    }

    #[test]
    fn unknown_names_reported() {
        let toml = r#"
[commands]
MadeUpCommand = "Ctrl+K"
[tools]
MadeUpTool = "X"
"#;
        let (_, warnings) = parse_file(toml);
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("unknown command 'MadeUpCommand'")),
            "got: {warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("unknown tool 'MadeUpTool'")),
            "got: {warnings:?}"
        );
    }

    #[test]
    fn modifier_count_order_preserved() {
        let toml = r#"
[commands]
MergeDown = "Ctrl+Alt+Shift+E"
StampVisible = "E"
"#;
        let (hotkeys, _) = parse_file(toml);
        let order = hotkeys.keyboard_order();
        let pos_merge = order.iter().position(|&c| c == Command::MergeDown).unwrap();
        let pos_stamp = order.iter().position(|&c| c == Command::StampVisible).unwrap();
        assert!(
            pos_merge < pos_stamp,
            "MergeDown (3 mods) should come before StampVisible (0 mods)"
        );
    }

    #[test]
    fn overrides_applied_to_matching_and_menu_text() {
        let toml = r#"
[commands]
MergeDown = "Ctrl+Shift+M"
NewCurves = "Ctrl+Alt+M"
StampVisible = ""

[tools]
Eyedropper = "U"
"#;
        with_test_hotkeys(toml, || {
            // Verify command shortcuts overridden
            let merge_sc = Command::MergeDown.shortcut().expect("MergeDown shortcut");
            assert_eq!(merge_sc.logical_key, Key::M);
            assert!(merge_sc.modifiers.command);
            assert!(merge_sc.modifiers.shift);
            assert!(!merge_sc.modifiers.alt);

            let curves_sc = Command::NewCurves.shortcut().expect("NewCurves shortcut");
            assert_eq!(curves_sc.logical_key, Key::M);
            assert!(curves_sc.modifiers.command);
            assert!(curves_sc.modifiers.alt);
            assert!(!curves_sc.modifiers.shift);

            // StampVisible empty string -> no shortcut
            assert_eq!(Command::StampVisible.shortcut(), None);

            // Tool letter overridden
            assert_eq!(Tool::Eyedropper.shortcut_letter(), Some("U"));

            // Verify matching in Command::pressed
            let ctx = egui::Context::default();
            let press = |modifiers: Modifiers, key: Key| -> Vec<Command> {
                let mut v_down = false;
                let events = vec![
                    egui::Event::ModifiersChanged(modifiers),
                    egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers,
                    },
                ];
                let input = egui::RawInput {
                    events,
                    ..Default::default()
                };
                let mut pressed = Vec::new();
                let mut out = ctx.run_ui(input, |ui| {
                    pressed = Command::pressed(ui.ctx(), &mut v_down);
                });
                out.textures_delta.clear();
                pressed
            };

            // Pressing old default Ctrl+E should NOT trigger MergeDown
            let pressed = press(Modifiers::COMMAND, Key::E);
            assert!(!pressed.contains(&Command::MergeDown));

            // Pressing new shortcut Ctrl+Shift+M SHOULD trigger MergeDown
            let pressed = press(
                Modifiers {
                    shift: true,
                    ..Modifiers::COMMAND
                },
                Key::M,
            );
            assert_eq!(pressed, vec![Command::MergeDown]);

            // Pressing tool key U selects Eyedropper
            let mut tools = crate::tools::Tools::default();
            assert_ne!(tools.tool, Tool::Eyedropper);
            let input = egui::RawInput {
                events: vec![egui::Event::Key {
                    key: Key::U,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }],
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                tools.keys(ui.ctx());
            });
            out.textures_delta.clear();
            assert_eq!(tools.tool, Tool::Eyedropper);

            // Test menu text formatting matches the override
            assert_eq!(ctx.format_shortcut(&merge_sc), "Ctrl+Shift+M");
            assert_eq!(ctx.format_shortcut(&curves_sc), "Ctrl+Alt+M");

            // Test rendering menu item button displays overridden shortcut text
            let mut app = crate::app::test_app();
            let raw = egui::RawInput::default();
            let mut output = ctx.run_ui(raw, |ui| {
                app.menu_item(ui, Command::MergeDown, None);
            });
            output.textures_delta.clear();
            let found = output.shapes.iter().any(|s| match &s.shape {
                egui::Shape::Text(t) => t.galley.text().contains("Ctrl+Shift+M"),
                _ => false,
            });
            assert!(found, "Menu item button should display overridden shortcut text");
        });
    }
}
