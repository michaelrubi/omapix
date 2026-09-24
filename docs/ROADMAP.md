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

### Mask overlay (Photoshop's red "rubylith")

**Problem:** the only way to see a mask is mask view (Alt+click), which hides the photo. There's no way to see the mask and the image together.

**Plan:** Photoshop's `\` key toggles a translucent red overlay of the selected layer's mask on top of the image:
- Red shows hidden (black) areas of the mask; revealed areas are clear.
- It updates live while painting the mask.
- The status bar says when it's on; `\` or Esc turns it off.
- Later: choose the overlay's colour and opacity (Photoshop's Layer Mask Display Options).
- Related, later: **Quick Mask** (`Q`), which paints a selection with the same red overlay and turns it back into a selection.

## 2. Hand-testing checklist

The UI has been tested through the same code paths with scripts
(`OMAPIX_SCRIPT`) and unit tests, but not yet with a real mouse. These need checking by hand, and any bugs fixed:
- [ ] Brush, eraser, clone, healing and spot healing with the mouse
- [ ] Lasso and marquee drags, including Shift/Alt to add and subtract
- [ ] Dragging Curves points, and dragging one off the graph to delete it
- [ ] Blend If handles, including Alt+drag to split
- [ ] Dragging layers to reorder them, including by the name
- [ ] Clicking a layer row selects it, without accidentally starting a drag; double-clicks rename or open Blending Options
- [x] Right-clicking layer rows and mask thumbnails for context menus
- [x] Active tool contrast styling and frameless toolbar icons
- [ ] Renaming layers: double-click or context menu, typing, submitting on Return or click-off, and cancelling on Esc
- [ ] Open, Save As and Export file dialogs (xdg portal)
- [ ] Dropping a file onto the window
- [ ] Switching the Omarchy theme while Omapix is open
- [ ] Undo/redo after each of the above

## 3. Retouching and editing

- **Layer groups:** folders in the Layers panel with their own blend mode, opacity and mask. OpenRaster supports groups (nested stacks).
- **Elliptical marquee**, and Shift to constrain marquee shapes to a square or circle.
- **Live preview for Gaussian Blur**, like the frequency-separation preview.
- **Brush size and hardness by dragging:** Photoshop's Alt+right-drag. This may clash with Hyprland shortcuts, so check first.
- **Pen pressure** for size and opacity. Blocked: winit (the windowing library) has no tablet support on Linux yet. Watch winit, or read tablet input directly through the Wayland tablet protocol.
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
