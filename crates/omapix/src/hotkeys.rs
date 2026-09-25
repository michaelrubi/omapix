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
use std::path::PathBuf;
use std::sync::OnceLock;

use egui::{Key, KeyboardShortcut, Modifiers};
use serde::Deserialize;

use crate::commands::Command;
use crate::tools::ToolGroup;

/// Loaded once at startup (see [`load`]); Photoshop's defaults until then,
/// and in tests.
static HOTKEYS: OnceLock<Hotkeys> = OnceLock::new();

/// The shortcuts in use.
pub(crate) fn current() -> &'static Hotkeys {
    HOTKEYS.get_or_init(Hotkeys::default)
}

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

/// A tool, by its name in OMAPIX_SCRIPT (e.g. "Eyedropper", "MagicWand"),
/// as the group whose letter picks it.
fn parse_tool_group(name: &str) -> Option<ToolGroup> {
    ToolGroup::ALL
        .iter()
        .flat_map(|g| g.tools())
        .find(|t| format!("{t:?}") == name.trim())
        .map(|t| t.group())
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
        match Command::from_name(key.trim()) {
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

/// Load `hotkeys.toml` from the config folder, if there is one. Returns
/// warnings about problems in it, which leave the defaults in place.
pub(crate) fn load() -> Vec<String> {
    let Some(path) = default_path() else {
        return Vec::new();
    };
    let text = match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return vec![format!("Hotkeys: failed to read hotkeys.toml: {e}")],
        Ok(text) => text,
    };
    // With no file yet (or an empty one), write one listing everything that
    // can be changed, all commented out, so there's something to edit.
    if text.trim().is_empty() {
        let written = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&path, template()));
        if let Err(e) = written {
            log::warn!("Hotkeys: couldn't write {}: {e}", path.display());
        }
        return Vec::new();
    }
    let (hotkeys, warnings) = parse_file(&text);
    let _ = HOTKEYS.set(hotkeys);
    warnings
}

/// A `hotkeys.toml` listing every command and tool with its default
/// shortcut, all commented out.
fn template() -> String {
    let quoted = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut out = String::from(
        "# Omapix keyboard shortcuts.\n\
         #\n\
         # Every command and tool is listed with its Photoshop shortcut, which\n\
         # Omapix uses unless you change it here. To change one, remove the # at\n\
         # the start of its line and edit the shortcut; \"\" removes it. Restart\n\
         # Omapix to apply. Problems are shown in the status bar at startup.\n\
         #\n\
         # A shortcut is Ctrl, Alt and Shift joined to a key with +, such as\n\
         # \"Ctrl+Shift+Z\", \"Alt+[\", \"F7\", \"/\" or \"\\\\\".\n\n[commands]\n",
    );
    for &cmd in Command::ALL {
        let shortcut = cmd.default_shortcut().map_or(String::new(), |s| format_shortcut(&s));
        out.push_str(&format!("# {cmd:?} = {}  # {}\n", quoted(&shortcut), cmd.label()));
    }
    out.push_str("\n# A tool's letter picks it; with Shift, the next tool in its group.\n[tools]\n");
    for &group in ToolGroup::ALL {
        let tools = group.tools();
        let key = group.default_key().map_or("", |k| k.symbol_or_name());
        let mut line = format!("# {:?} = {}", tools[0], quoted(key));
        if tools.len() > 1 {
            let others: Vec<String> = tools[1..].iter().map(|t| format!("{t:?}")).collect();
            line.push_str(&format!("  # also {}", others.join(", ")));
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_lists_every_default_and_reads_back_as_them() {
        let text = template();
        let (hotkeys, warnings) = parse_file(&text);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(hotkeys, Hotkeys::default());
        // Uncommented, every line reads back as the default it shows.
        let uncommented: String = text
            .lines()
            .map(|l| l.strip_prefix("# ").filter(|l| l.contains(" = ")).unwrap_or(l))
            .map(|l| format!("{l}\n"))
            .collect();
        let (hotkeys, warnings) = parse_file(&uncommented);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(hotkeys, Hotkeys::default());
        for &cmd in Command::ALL {
            assert!(text.contains(&format!("# {cmd:?} = ")), "{cmd:?} missing");
        }
    }
    use crate::tools::Tool;

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
        let (hotkeys, warnings) = parse_file(toml);
        assert!(warnings.is_empty(), "{warnings:?}");
        let merge = KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::M);
        assert_eq!(hotkeys.command(Command::MergeDown), Some(merge));
        assert_eq!(hotkeys.command(Command::StampVisible), None);
        assert_eq!(hotkeys.command(Command::Undo), Command::Undo.default_shortcut());
        assert_eq!(hotkeys.tool_group(Tool::Eyedropper.group()), Some(Key::U));

        let ctx = egui::Context::default();
        let press = |modifiers: Modifiers, key: Key| -> Vec<Command> {
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
                pressed = Command::pressed_with(ui.ctx(), &mut false, &hotkeys);
            });
            out.textures_delta.clear();
            pressed
        };
        // The old shortcut does nothing; the new one merges, ahead of the
        // Ctrl+M (Curves) it would otherwise also match.
        assert!(!press(Modifiers::COMMAND, Key::E).contains(&Command::MergeDown));
        assert_eq!(press(Modifiers::COMMAND | Modifiers::SHIFT, Key::M), [Command::MergeDown]);
        assert_eq!(press(Modifiers::COMMAND | Modifiers::ALT, Key::M), [Command::NewCurves]);
    }
}
