# Omapix roadmap

What's still needed, in the order we plan to do it. [DESIGN.md](DESIGN.md)
covers the architecture and what's already built.

Status: milestones 1–4 are largely done. Omapix can open a darktable TIFF, retouch it with layers, masks, healing and frequency separation, grade it with adjustment layers, and save or export it.

## 1. Everyday UI and layers panel UX (next)

These come from using the app for real, and they make everyday work smoother.

### ~~Active tool contrast styling~~ (done)

The active tool in the left toolbar is indicated cleanly via high contrast (`foreground`) against muted inactive tools (`dark_foreground`) using frameless buttons, replacing the text-cursor style selection box.

### ~~Clicking a layer selects it~~ (done)

Clicking anywhere on a row, including the blend-mode label and empty space, now selects the layer. A pixel layer paints on its pixels; an adjustment layer paints on its mask. Clicking the mask thumbnail still selects the layer and targets its mask. Double-clicking the name renames. Double-clicking a thumbnail or empty space opens Blending Options (this was never actually hooked up before). Rows still drag to reorder. Needs a check with a real mouse (see the checklist below).

### ~~Right-click menu on layers~~ (done)

Right-clicking anywhere on a layer row opens a context menu with Blending Options…, Duplicate Layer, Delete Layer, Rename, Add/Delete Layer Mask, Disable/Enable Layer Mask, Invert Mask, View Mask (Alt+click), and Merge Down. Right-clicking the mask thumbnail shows just the mask commands, as in Photoshop.

### ~~Layer rename submission and cancel~~ (done)

Double-clicking a layer name or choosing Rename from the context menu focuses the input with the full text selected. Pressing Return or clicking anywhere off the text field commits the name, while Escape cancels back to the existing name.

### ~~Mask overlay (Photoshop's red "rubylith")~~ (done)

`\` toggles a translucent red overlay of the selected layer's mask on top of the image, so the mask and the photo can be seen together. Hidden (black) areas of the mask are tinted 50 % red; revealed areas are clear. It updates live while painting the mask. The status bar says when it's on; `\` or Esc turns it off, and it goes away when another layer is selected or the mask is deleted, as in Photoshop. It's also in the View menu and the layer and mask context menus.
- Later: choose the overlay's colour and opacity (Photoshop's Layer Mask Display Options).
- Related, later: **Quick Mask** (`Q`), which paints a selection with the same red overlay and turns it back into a selection.

### ~~Move tool~~ (done)

`V` picks the Move tool. Dragging moves the active layer, and its mask with it (Photoshop links them). With a selection, it moves just the selected pixels, or the selected part of the mask when the mask is targeted, and the selection moves with them. The hole left behind is transparent, or the background colour's grey on a mask, as with Delete. Alt+drag moves a copy: a new layer without a selection, or a copy of the selected pixels. Shift keeps the drag horizontal, vertical or at 45°. Arrow keys nudge by 1 px (Shift: 10 px). Number keys set the layer's opacity rather than the brush's. Each drag or nudge is one undo step. On large images the drag updates as fast as the image re-renders, skipping positions in between rather than falling behind.
- Pixels moved past the edge of the canvas are cut off when the move ends. Photoshop keeps them in the layer, but Omapix layers are the size of the canvas.
- Each nudge of a selection lifts and drops it again, so feathered edges fade slightly with repeated nudges. Photoshop keeps the pixels "floating" until you deselect.
- Later: Auto-Select (Ctrl+click picks the layer under the pointer) and Free Transform (Ctrl+T).

### Cut, Copy and Paste

**Problem:** there's no clipboard. Moving or duplicating part of a layer means duplicating the whole layer and masking it.

**Plan:** Photoshop's shortcuts, working on the selection (or the whole layer without one):
- Ctrl+C copies the selected pixels of the active layer; Ctrl+Shift+C (Copy Merged) copies what's visible from all layers.
- Ctrl+X cuts: copies, then clears the selection to transparency (or to the background grey on a mask, like Delete).
- Ctrl+V pastes as a new layer in the same place it was copied from, as in Photoshop.
- Also paste images copied from other apps, and copy out to them, through the Wayland clipboard.

### Marching ants for the marquees

**Problem:** selections and the marquee or lasso being dragged show a dashed black-and-white outline, but it stands still, so it's easy to lose against a busy photo.

**Plan:** animate the dashes so they march, as in Photoshop:
- For the selection and for the marquee (rectangular or elliptical) or lasso shape while it's being dragged.
- Repaint only while a selection is on screen, at a low rate, so an idle Omapix stays idle.
- Later: Ctrl+H hides the selection edges (Photoshop's Show Extras) while keeping the selection.

## 2. Hand-testing checklist

The UI has been tested through the same code paths with scripts
(`OMAPIX_SCRIPT`) and unit tests, but not yet with a real mouse. These need checking by hand, and any bugs fixed:
- [ ] Brush, eraser, clone, healing and spot healing with the mouse
- [ ] Lasso and marquee drags, including Shift/Alt to add and subtract
- [ ] Move tool: dragging layers and selected pixels, Alt+drag copies, Shift constraint, arrow-key nudges, and how smooth it is on a 24 MP image
- [ ] Dragging Curves points, and dragging one off the graph to delete it
- [ ] Blend If handles, including Alt+drag to split
- [ ] Dragging layers to reorder them, including by the name
- [ ] Clicking a layer row selects it, without accidentally starting a drag; double-clicks rename or open Blending Options
- [x] Right-clicking layer rows and mask thumbnails for context menus
- [x] Active tool contrast styling and frameless toolbar icons
- [ ] Renaming layers: double-click or context menu, typing, submitting on Return or click-off, and cancelling on Esc
- [x] Mask overlay: `\` and Esc, live updates while painting the mask
- [x] Elliptical marquee, and Shift to constrain marquees to a square or circle
- [ ] Open, Save As and Export file dialogs (xdg portal)
- [ ] Dropping a file onto the window
- [ ] Switching the Omarchy theme while Omapix is open
- [ ] Undo/redo after each of the above

## 3. Retouching and editing

- **Layer groups:** folders in the Layers panel with their own blend mode, opacity and mask. OpenRaster supports groups (nested stacks).
- ~~**Elliptical marquee**~~ (done): Shift+M switches between the rectangular and elliptical marquees, and Shift constrains either to a square or circle.
- **Live preview for Gaussian Blur**, like the frequency-separation preview.
- **Brush size and hardness by dragging:** Photoshop's Alt+right-drag. This may clash with Hyprland shortcuts, so check first.
- **Tablet support:** pen pressure for size and opacity (and later tilt). Blocked: winit (the windowing library) has no tablet support on Linux yet. Watch winit, or read tablet input directly through the Wayland tablet protocol (`tablet-v2`) on the same Wayland connection. Tablet buttons (ExpressKeys, stylus buttons) are best mapped to keystrokes outside Omapix (Hyprland binds or OpenTabletDriver), so they work through custom hotkeys rather than needing pad support in Omapix.
- **Liquify:** forward warp, push, bloat and pucker, with a mesh that can be edited again later.

## 4. Colour and adjustments

- **Histogram in Curves and Levels**, drawn behind the curve.
- **Eyedroppers in Curves and Levels:** set black, grey and white points by clicking the image.
- **Selective Color** and **Channel Mixer** adjustment layers.
- **LUT adjustment layer:** load `.cube` files.
- **Monitor colour management:** read the display's ICC profile instead of assuming sRGB.
- **Soft proofing** for print and web.

## 5. Workflow and files

- **darktable round trip:** "Edit in Omapix" from darktable, and export back to a TIFF next to the raw file.
- **Batch export:** apply a saved action (for example "resize, sharpen, JPEG") to many files.
- **Recent files** and reopening the last document.
- **PSD import**, at least flattened and simple layers, for old Photoshop work.
- **History panel:** a list of undo steps you can click back to.
- **Custom hotkeys:** override any command's shortcut or tool letter from a TOML file in `~/.config/omapix/`, with Photoshop's shortcuts as the defaults. Warn about clashes at startup.

## 6. Performance

- **Faster slider drags:** opacity and adjustment changes recomposite the whole image (about 100 ms at 24 MP). Recomposite only the visible area first, then the rest.
- **GPU compositing:** move blend modes to the GPU for display, keeping the CPU path for export (see DESIGN.md).
- **Memory:** free display textures that have been off-screen for a while.

## 7. The big one: AI-assisted retouching

The goal is Evoto-style one-click cleanup, running locally on the GPU with no subscription. This will need its own design document before any code.
- Skin and face-part segmentation (skin, eyes, lips, hair) to make masks automatically.
- Automatic blemish detection that feeds the Spot Healing Brush.
- Skin smoothing that keeps texture, built on frequency separation plus the segmentation masks.
- Models run through ONNX Runtime on the GPU (already set up on this machine for darktable).

## Out of scope for now
- Vector and layout tools (Omapix is raster only).
- Plugins and scripting beyond `OMAPIX_SCRIPT`.
