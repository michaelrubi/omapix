# Actions

An action is a list of steps Omapix plays back for you, as in Photoshop. Use one for anything you do the same way on every photo, such as a Curves layer masked to the subject when colour grading.

## Recording one

1. Open **Window › Actions** (Alt+F9).
2. Press **●**, and type a name over "Action 1".
3. Do the steps: commands from the menus or their shortcuts, and filters. A filter or a radius dialog (Feather, Frequency Separation) is recorded with the settings you OK. The steps appear in the panel as you go.
4. Press **■ Stop** to save it.

Painting, tool changes and what you change in the panels (Properties, a layer's opacity) aren't recorded.

## Playing one

- Select it in the panel and press **▶**, or double-click it.
- **File › Automate** lists every action.
- Give it a key: select it and choose one in the box beside Delete (F2–F12, with Shift or Ctrl).

Each step waits for the one before it to finish, so Select Subject is done before the next step adds a mask. If a step can't run (for example Feather with nothing selected), the action stops there and the status bar says which step.

## The files

Each action is a text file in `~/.config/omapix/actions/`, named after it, with one step a line. You (or an agent) can write or edit them; the panel picks up changes within a second.

```text
# Key: F2
SelectSubject
NewCurves
Invert
```

- A command is its name in `crates/omapix/src/commands.rs`: `NewCurves`, `SelectSubject`, `AddMask`, `Invert`, `Feather`…
- A command that asks for a radius takes it after its name: `Feather 5`, `FrequencySeparation3 4 24`.
- A filter is `Filter` and its settings: `Filter {"GaussianBlur":{"radius":7.0}}`. Record one to see how its settings are written.
- The other `OMAPIX_SCRIPT` steps work too (`Tool Brush`, `Size 300`, `AutoRetouch Natural`…).
- `#` starts a comment, and `# Key: Shift+F3` gives the action a key.

## From an agent

The MCP server (`omapix --mcp`, and the window's live session) has `list_actions` and `save_action`. `play_action` plays an action on the image open in the window; `apply_action` plays one on a file and writes the result.
