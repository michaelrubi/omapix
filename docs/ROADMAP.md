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

### ~~Cut, Copy and Paste~~ (done)

Ctrl+C copies the selected pixels of the active layer (or the whole layer without a selection), and Ctrl+Shift+C (Copy Merged) copies what's visible from all layers. Partly selected pixels come out partly transparent, as in Photoshop. With a mask targeted, Ctrl+C copies the mask as grey. Ctrl+X copies, then clears like Delete: pixels to transparency, a mask to the background grey. Ctrl+V pastes as a new layer above the active one, in the same place it was copied from (centred if it doesn't fit there), and deselects. They're in the Edit menu too.

Copies also go on the Wayland clipboard as an sRGB PNG (through `wl-copy`, so they stay pasteable after Omapix quits), and Ctrl+V pastes PNG or JPEG images copied in other apps, centred. While the clipboard still holds Omapix's own copy, pasting uses the full 16-bit original.
- Later: Paste in Place and Paste Into (Ctrl+Shift+V, Ctrl+Alt+Shift+V), and centring pastes on the view rather than the canvas.

### ~~Marching ants for the marquees~~ (done)

The selection, and the marquee (rectangular or elliptical) or lasso shape being dragged, are drawn with white dashes over a black line that march along the outline, as in Photoshop. Omapix repaints for them only while an outline is on screen, about 12 times a second, so an idle Omapix stays idle.
- Ctrl+H hides the selection edges (done; Photoshop's Show Extras) while keeping the selection.

### ~~One outline for combined selections~~ (done)

The marching ants follow the edge of the whole selection, traced from its coverage where it crosses 50 % after every change, as in Photoshop. Shapes added together with Shift share one outline, without lines through the overlap, and subtracting, intersecting, inverting, feathering and moving all show the right edge; a selection with a hole gets an outline round the hole. Outlines are cut off at the canvas edge. As in Photoshop, the outline follows the edges of the pixels that are more than half selected, so corners are square and curves are stepped when zoomed in. Tracing a 24 MP selection takes about 5 ms.

### ~~Selection tools grouped in the toolbar~~ (done)

Each group shares one slot in the left toolbar showing the tool last used, with a small bottom-right corner triangle for multi-tool groups. Right-clicking or holding down the mouse (> 0.35 s) opens a pop-up menu of the group's tools with their shortcut letters. Plain shortcut keys switch to the group's last-used tool, and Shift+key cycles through the tools in that group (e.g. `M` / `Shift+M` for Rectangular/Elliptical Marquee, `J` / `Shift+J` for Spot Healing/Healing Brush).

### ~~Selecting several layers~~ (done)

Ctrl+click on a layer row adds it to the selected layers or takes it out, and Shift+click selects the rows from the last one clicked (Ctrl+Shift+click adds them), as in Photoshop. Every selected row is highlighted; the last one clicked is the active layer (bold, with its thumbnail outlined), which painting, adjustments, the blend mode and opacity, masks and clipping still apply to. Selecting a layer any other way (a plain click, a new layer, undo to another layer) selects just that one.
- Ctrl+G puts them all in one new group, in stack order, where the top one was.
- Ctrl+J duplicates them all, and selects the copies. Delete (the key, the trash button or the menu) deletes them all.
- Ctrl+E merges them into one layer where the top one was, with its name (Merge Layers). They're flattened on their own, as if nothing else were there, so hidden layers are dropped and blend modes are baked in.
- Dragging one of the selected rows drags them all, keeping their order. Dropped next to one of them, they go there among the layers that stay.
- The Move tool (and arrow-key nudges, and Alt+drag copies) moves all of them. With a pixel selection it moves just the active layer's selected pixels, as before.
- Right-clicking one of the selected rows keeps them all selected, and its menu says Duplicate Layers, Delete Layers and Merge Layers.
- A layer inside a selected group goes with the group.
- Later: the blend mode and opacity for all of them at once, and moving selected pixels on several layers.

### ~~Photoshop's layer navigation shortcuts~~ (done)

Alt+] and Alt+[ select the layer above or below following the rows in the Layers panel (stepping into open groups, skipping closed ones, stopping at the ends without wrapping; single selection targeting pixels on pixel layers). Ctrl+Shift+] and Ctrl+Shift+[ bring the active layer (and other selected layers) to the front or send them to the back of their group, with undo/redo.

### ~~Tool cursor modifier badges (`+` and `-`)~~ (done)

Over the canvas, selection tools (Rectangular & Elliptical Marquee, Lasso, Magic Wand) show a small badge at the lower right of the crosshair: `+` while holding Shift (add), `-` while holding Alt (subtract), and `×` while holding Shift+Alt (intersect), matching the mode applied when clicking or dragging. The Move tool shows a copy badge while holding Alt (Alt+drag copy).


## 2. Hand-testing checklist

The UI has been tested through the same code paths with scripts
(`OMAPIX_SCRIPT`) and unit tests, but not yet with a real mouse. These need checking by hand, and any bugs fixed:
- [x] Brush, eraser, clone, healing and spot healing with the mouse
- [x] Lasso and marquee drags, including Shift/Alt to add and subtract
- [x] Move tool: dragging layers and selected pixels, Alt+drag copies, Shift constraint, arrow-key nudges, and how smooth it is on a 24 MP image
- [x] Dragging Curves points, and dragging one off the graph to delete it
- [x] Blend If handles, including Alt+drag to split
- [x] Dragging layers to reorder them, including by the name
- [x] Layer groups: Ctrl+G and Ctrl+Shift+G, opening and closing groups, dragging layers into, out of and between groups, and the Move tool on a group
- [x] Clicking a layer row selects it, without accidentally starting a drag; double-clicks rename or open Blending Options
- [x] Right-clicking layer rows and mask thumbnails for context menus
- [x] Active tool contrast styling and frameless toolbar icons
- [x] Renaming layers: double-click or context menu, typing, submitting on Return or click-off, and cancelling on Esc
- [x] Mask overlay: `\` and Esc, live updates while painting the mask
- [x] Elliptical marquee, and Shift to constrain marquees to a square or circle
- [x] Marching ants move, and Omapix goes idle again once there's no selection
- [x] Tool groups in the toolbar: one slot per group with corner triangle, right-click and hold-to-open menus, last-used tool remembered, and Shift+key cycling
- [x] Slider drags (opacity, Curves, Hue/Saturation) and Move tool drags on a 24 MP image, at fit and at 100 %: smooth, and the image settles to the exact result
  - Hand-tested: sliders work but the image could follow them faster; Move tool drags work but feel sluggish. Both are what GPU compositing should fix.
- [x] One outline for overlapping marquees and lassos, after adding, subtracting, inverting and feathering
- [x] Cut, copy, Copy Merged and paste, within Omapix and to and from other apps (a browser, a screenshot)
- [x] Clipping masks: Ctrl+Alt+G, Alt+click between rows (and its cursor and line), the arrow and underline, and a Curves clipped to the dodge & burn layer
- [x] Open, Save As and Export file dialogs (xdg portal)
- [x] Selecting several layers: Ctrl+click and Shift+click, then Ctrl+G, Ctrl+J, Delete, Ctrl+E, dragging the rows, and the Move tool
- [ ] Layer navigation shortcuts: Alt+] and Alt+[ select layer above or below (stepping into open groups, skipping closed ones, no wrap), and Ctrl+Shift+] and Ctrl+Shift+[ bring to front and send to back of group
- [x] Tool cursor modifier badges: Shift (+), Alt (-), and Shift+Alt (×) with selection tools, and Alt copy badge with the Move tool
- [x] Eyedroppers in Curves and Levels: set black, gray and white points with crosshair cursor, one undo step per click, and Esc to disarm
- [x] Cached group results: slider drags above a big isolated group (say a Multiply group), then painting, changing settings and hiding layers inside it, zoomed out and at 100 %: faster, and the image always ends up right
- [ ] Dropping a file onto the window
- [ ] Switching the Omarchy theme while Omapix is open
- [x] Retouching setups: Frequency Separation lands in its group, and Dodge & Burn Curves paints lighter and darker on its masks
- [ ] High Pass Sharpening at 100 %, with opacity and a mask, and Filter › Other › High Pass
- [ ] Unsharp Mask at 100 % on a portrait: Amount, Radius and Threshold with the live preview, no colour fringes on edges
- [x] Lock transparent pixels: `/` and the lock button, then the brush, eraser, fills and Delete on a pasted patch or hair layer
- [ ] Alt+click on a group's triangle: opens or closes that group and every group inside it, and Alt+click between rows still clips
- [x] Add Noise: dialog controls (Amount, Uniform/Gaussian, Monochromatic, Grain Size, Roughness, shadow/highlight falloff), live preview, and the new Grain layer in Overlay mode
- [ ] Selection edges: Ctrl+H hides marching ants while keeping selection, status bar notes it, Omapix goes idle, and a new selection shows them again
- [ ] Undo/redo after each of the above (done for everything checked)

## 3. Retouching and editing

- ~~**Layer groups**~~ (done): folders in the Layers panel with their own blend mode, opacity, mask and Blend If, nested to any depth. Groups are Pass Through by default, so what's in them blends onto the layers below as if ungrouped; any other mode composites the group on its own first, so adjustment layers inside it change only the group. Ctrl+G groups the selected layer and Ctrl+Shift+G ungroups; Layer › New Group and the folder button make an empty one. New layers go into the top of a selected group. Groups start closed; the triangle opens them, and they open by themselves to show the selected layer. Drag a layer onto a group's row to put it in, or between rows to place it; Ctrl+] and Ctrl+[ step into and out of groups. Duplicating, deleting and moving a group (including with the Move tool and Alt+drag) take everything in it, and Ctrl+E on a group merges it into one layer (Merge Group). Saved in OpenRaster as nested stacks, which Krita and GIMP read.
  - ~~Alt+click on a triangle to open or close every group inside~~ (done).
- ~~**Clipping masks**~~ (done): Ctrl+Alt+G (Layer › Create Clipping Mask, or the layer's context menu) clips a layer to the one below, so it shows only where that layer does; on a clipped layer it releases it and the clipped layers above it. Alt+click the line between two rows does the same. Clipped rows get an arrow and are indented, and the layer they clip to is underlined. Several clipped layers in a row all clip to the first unclipped one below them in the same group, and a new layer made inside a clipping mask joins it. As with Photoshop's default "Blend Clipped Layers as Group", the clipped layers are composited onto the base on their own, then blended like the base, with its mode, opacity, mask and Blend If, so a Curves clipped to the Soft Light dodge & burn layer changes just that layer. Hiding the base hides them all. Merge Down of a clipped layer keeps it inside its base. Saved in OpenRaster as `omapix:clipped` (Krita doesn't save its "inherit alpha" in OpenRaster, so there was nothing to match; Krita shows them unclipped).
  - Clipping to a group clips to what's in it, composited as if it weren't Pass Through. Clipping to an adjustment layer clips to its mask.
  - Later: the Blending Options for "Blend Clipped Layers as Group" off, and dragging a layer into a clipping mask making it clipped (as new layers are).
- ~~**Retouching setups as groups**~~ (done): Retouch › Frequency Separation now puts its two layers in a Pass Through "Frequency Separation" group, so hiding the group shows the image before. Retouch › Dodge & Burn Curves makes the pro setup in one step: a "Dodge & Burn" group with a "Dodge" Curves layer (midtones 50 % → 65 %) above a "Burn" one (50 % → 35 %), each with a black mask. It selects the Dodge mask, ready to paint white with a soft, low-opacity brush; select Burn to darken. The grey Soft Light Dodge & Burn Layer is still there too.
  - Later: choosing how strong the curves are, and Luminosity mode for either layer if darkening shifts colour too much (both can be set by hand for now).
- ~~**Lock transparent pixels**~~ (done): `/` (Layer › Lock Transparent Pixels, or the lock button under Opacity) locks the selected layers' transparency, as in Photoshop. Brushes, clone and healing then change only colour, as if the pixels were opaque, keeping each pixel's transparency; fills do the same. The eraser and Delete paint the background colour instead. Locked rows show a lock. Saved in OpenRaster as `omapix:lock-alpha`.
  - Later: Lock All, Lock Image Pixels and Lock Position; filters and the Move tool keeping transparency too.
- ~~**Elliptical marquee**~~ (done): Shift+M switches between the rectangular and elliptical marquees, and Shift constrains either to a square or circle.
- ~~**Magic Wand**~~ (done): `W` picks the Magic Wand (grouped with Object Selection). Click to select similar colours, with Tolerance (0–255), Anti-alias, Contiguous and Sample All Layers in the options bar, and Shift/Alt to add, subtract and intersect as with the marquees. Click outside the canvas deselects. Smooth anti-aliased edges feed Feather and layer masks directly.
- **Object Selection** (Photoshop's "smart" select, `W`): drag a rough box or lasso around something (a person, a face, hair) and it selects just that object. Needs a local segmentation model such as SAM run through ONNX Runtime, so it shares groundwork with the AI retouching in section 7 and is best built alongside it.
- ~~**Live preview for Gaussian Blur**~~ (done): updates the canvas live as the radius changes, with a Preview checkbox in the dialog, respecting layer masks, blend modes, adjustments and selections.
- **Brush size and hardness by dragging:** Photoshop's Alt+right-drag. This may clash with Hyprland shortcuts, so check first.
- **Tablet support:** pen pressure for size and opacity (and later tilt). Blocked: winit (the windowing library) has no tablet support on Linux yet. Watch winit, or read tablet input directly through the Wayland tablet protocol (`tablet-v2`) on the same Wayland connection. Tablet buttons (ExpressKeys, stylus buttons) are best mapped to keystrokes outside Omapix (Hyprland binds or OpenTabletDriver), so they work through custom hotkeys rather than needing pad support in Omapix.
- **Liquify:** forward warp, push, bloat and pucker, with a mesh that can be edited again later. Shares its warp engine with the slider-driven Symmetry and Reshape in section 7 ([AI.md](AI.md)), and is how their results get touched up by hand.

## 4. Colour and adjustments

- ~~**Histogram in Curves and Levels**~~ (done): drawn behind the curve, and in Levels above the input controls.
- ~~**Eyedroppers in Curves and Levels**~~ (done): set black, grey and white points by clicking the image (Curves adjusts red, green and blue curves to neutralise casts while preserving lightness; single-channel Levels sets master black/white and midtone gamma to map luminance to mid grey, with the limitation that Levels lacks per-channel data to neutralize colour casts).
- ~~**Selective Color** and **Channel Mixer** adjustment layers~~ (done).
- ~~**LUT adjustment layer:** load `.cube` files~~ (done).
- **Monitor colour management:** read the display's ICC profile instead of assuming sRGB.
- **Soft proofing** for print and web.

### Denoise, sharpen and add noise

Michael's finishing workflow, from years of Topaz and Nik Collection: denoise first, then retouch and grade, then sharpen, and add noise last, which makes the photo look more natural. Each goes on its own layer by default, so its opacity is its strength and a mask can keep it off the eyes or the background.

- **Denoise** (Filter › Noise › Reduce Noise…), the first step.
  - AI denoising with the NIND model darktable already installed here (`denoise-nind`: 768 px tiles, GPL-3.0 like Omapix), found through the shared model folders and run on the GPU with the same runtime as section 7 ([AI.md](AI.md)).
  - Separate luminance and colour noise amounts. The result goes on a **Denoise** layer above the image.
  - A classical fallback on the CPU (wavelet or non-local means) when there's no model.
  - For raws, darktable's raw denoise before export is better still. Omapix's is for files that arrive already developed.
- **Sharpen** (Filter › Sharpen), near the end.
  - ~~Photoshop's **Unsharp Mask**~~ (done): Filter › Sharpen › Unsharp Mask… with Amount, Radius and Threshold, on luminance only (the same offset goes to red, green and blue), so edges don't get colour fringes. Previewed live on the canvas; settings are remembered.
  - **Smart Sharpen** (Gaussian or lens blur, noise reduction, fading in shadows and highlights), also on luminance only.
  - ~~A one-click **High Pass sharpening** setup~~ (done): Retouch › High Pass Sharpening… asks for a radius (1–3 px for fine detail) and adds a "High Pass Sharpening" layer in Overlay above the selected layer, holding the High Pass of the visible image's luminance, so it sharpens without colour fringes. Its opacity sets the strength and a mask keeps it off skin. Filter › Other › High Pass… applies the filter itself to a layer. Filter › Other › High Pass… is previewed live on the canvas, like Gaussian Blur and Unsharp Mask; the sharpening setup isn't yet.
  - Previewed at 100 % in the dialog (Photoshop's filter preview box), since sharpening can't be judged zoomed out.
  - Later, output sharpening for the export size, once exports can resize (see Batch export).
- ~~**Add Noise**~~ (done): Filter › Noise › Add Noise… opens a dialog with Photoshop's controls (Amount, Uniform or Gaussian, Monochromatic) plus film-like Grain Size and Roughness, and tonal falloff in shadows/highlights. By default the result goes on a new Grain layer in Overlay mode carrying the grain, with live preview while the dialog is open.
  - Later: grain added at output size once exports can resize.
- Later, a **Finish** action that runs Sharpen then Add Noise with saved settings, which Batch export can reuse.

## 5. Workflow and files

- **darktable round trip:** "Edit in Omapix" from darktable, and export back to a TIFF next to the raw file.
- **Batch export:** apply a saved action (for example "resize, sharpen, JPEG") to many files.
- ~~**Recent files** and reopening the last document~~ (done).
- **PSD import**, at least flattened and simple layers, for old Photoshop work.
- ~~**History panel:** a list of undo steps you can click back to~~ (done).
- **Custom hotkeys:** override any command's shortcut or tool letter from a TOML file in `~/.config/omapix/`, with Photoshop's shortcuts as the defaults. Warn about clashes at startup.

## 6. Performance

- ~~**Faster slider drags**~~ (done): edits redraw the canvas in place, the part on screen first, then the rest in the background, which a newer edit cuts short. Zoomed out, what's on screen is first previewed from layers shrunk to the size shown (kept until they change), then replaced by the exact full-size result. With four layers at 24 MP, the screen follows a slider in about 60 ms at fit to screen (was about 300 ms), 20 ms at 25 % and 45 ms at 100 %. The Move tool's drags speed up the same way.
  - Zoomed out, the preview blends shrunk layers rather than shrinking the blended image, so non-linear modes (Soft Light, Overlay…) can shift very slightly when the exact result lands.
  - Shrunk layers cost a quarter of each layer's memory at 50 % (a sixteenth at 25 %) while zoomed out.
- **GPU compositing:** move blend modes to the GPU for display, keeping the CPU path for export (see DESIGN.md).
  - Plan: upload each layer and mask at the pyramid level on screen (the shrunk layers from `reduced.rs` are that input), composite at screen resolution in a WGSL shader through egui's wgpu paint callback, bake each adjustment and the display colour transform into a 3D LUT on the CPU so the shader has one path for all of them, and build isolated groups in offscreen textures. The CPU composite stays for export, the pixel readout and eyedroppers. Costs about 190 MB of GPU memory per full-size 24 MP layer, far less zoomed out.
- **Memory:** free display textures that have been off-screen for a while.
- ~~**Cache group results**~~ (done): a group composited on its own (any mode but Pass Through, clipped, or with layers clipped to it) keeps what its contents composite to, tile by tile, while nothing in it changes. Dragging a slider on a layer above a big group, or painting above it, then just blends the kept result. Painting inside the group redoes only the tiles painted, and changing a setting inside it redoes the whole group. With four Soft Light layers in a Normal group at 24 MP, a render goes from about 340 ms to 170 ms; the bigger the group, the bigger the saving. The full-size render and the zoomed-out preview each keep their own.
  - Each group's kept result costs up to as much memory as a 16-bit layer (transparent tiles cost nothing), plus a quarter of that or less for the preview while zoomed out.
  - An isolated group's result is now rounded to 16 bits before it's blended, as it's stored, which can change a pixel by 1 in 65535.

## 7. The big one: AI-assisted retouching

The goal is Evoto-style one-click cleanup, running locally on the GPU with no subscription, for client and personal work alike. Design: [AI.md](AI.md) (proposal, with milestones and the decisions made so far).
- Object Selection (`W`) and Select › Subject, which prove the groundwork first.
- Skin and face-part segmentation (skin, eyes, lips, teeth, hair) to make masks automatically, per face and per person.
- Automatic blemish detection that feeds the Spot Healing Brush, onto its own layer.
- Skin smoothing that keeps texture, built on frequency separation plus the segmentation masks.
- Even tone: the Dodge & Burn setup with its masks filled in automatically.
- Auto Retouch: all of the above in one click, with each face edited on its own or all together, as layer groups whose opacity sets the strength.
- Face symmetry, and Reshape ("easy Liquify"): sliders for face and body shape, driven by face and body landmarks.
- Models run through ONNX Runtime on the GPU (already set up on this machine for darktable), shared with darktable's model folder.

## Out of scope for now
- Vector and layout tools (Omapix is raster only).
- Plugins and scripting beyond `OMAPIX_SCRIPT`.
