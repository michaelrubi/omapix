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

### ~~Quick Mask~~ (done)

`Q` (Select › Edit in Quick Mask Mode) toggles Photoshop's Quick Mask mode, showing the selection with the translucent red overlay: unselected areas are tinted 50 % red, selected areas are clear. With no selection, everything counts as selected (nothing is red). Painting tools (brush, eraser, fills, inverting) paint directly onto the selection coverage like a mask: black deselects (adds red), white selects (clears red). Each stroke is one undo step. Leaving Quick Mask (`Q`) converts the painted result back into an active selection with marching ants (or drops it if all selected or empty). Marching ants are hidden while in Quick Mask, the status bar shows "Quick Mask — press Q to exit", and selecting another layer or targeting a layer mask exits the mode.
- Later: filters on Quick Mask (Gaussian Blur, etc., modifying the selection directly with live preview).

### ~~Move tool~~ (done)

`V` picks the Move tool. Dragging moves the active layer, and its mask with it (Photoshop links them). With a selection, it moves just the selected pixels, or the selected part of the mask when the mask is targeted, and the selection moves with them. The hole left behind is transparent, or the background colour's grey on a mask, as with Delete. Alt+drag moves a copy: a new layer without a selection, or a copy of the selected pixels. Shift keeps the drag horizontal, vertical or at 45°. Arrow keys nudge by 1 px (Shift: 10 px). Number keys set the layer's opacity rather than the brush's. Each drag or nudge is one undo step. On large images the drag updates as fast as the image re-renders, skipping positions in between rather than falling behind.
- Pixels moved past the edge of the canvas are cut off when the move ends. Photoshop keeps them in the layer, but Omapix layers are the size of the canvas.
- Each nudge of a selection lifts and drops it again, so feathered edges fade slightly with repeated nudges. Photoshop keeps the pixels "floating" until you deselect.
- ~~Auto-Select~~ (done): with the Move tool, Ctrl+click (or Ctrl+drag) picks the topmost layer showing pixels under the pointer, then moves it, as in Photoshop. Hidden layers, layers in hidden groups, and where a mask hides a layer are skipped.

### ~~Free Transform~~ (done)

Ctrl+T (Edit › Free Transform) puts a box with eight handles round the active layer's pixels, or round the selection, which transforms just the selected pixels (or mask values, with the mask targeted) and the selection with them. As in Photoshop: dragging a corner scales in proportion (Shift for free), a side stretches one way, Alt scales about the centre, dragging inside moves (Shift keeps it straight), and dragging outside rotates (Shift in 15° steps). The status bar shows the width, height and angle. Enter applies it, resampled bicubic, as one undo step; Esc or Ctrl+Z cancels. The layer's mask goes with it. Saving, exporting or closing applies it first.
- ~~Shown live on the GPU~~ (done): transforming a whole layer (no selection) is drawn live, as Move tool drags are: the layer and its mask are sampled through the transform in the shader (`live.wgsl`), and the document changes only when Enter applies it, resampled bicubic on the CPU, with the live view up until that's on screen. Zooming or scrolling meanwhile builds the live view again. A GPU test checks it against the CPU's bilinear preview in every blend mode.
  - Transforming a selection, and layers the live view can't show (in groups, clipping masks, Blend If), still preview on the CPU.
  - Applying a whole 24 MP layer takes about half a second (the bicubic resampling); later, doing that in the background.
- Later: several layers or a group at once, typed values in an options bar, moving the reference point, Skew/Distort/Perspective/Warp (Ctrl+drag a corner), Flip and Rotate 90°, and double-click to apply.

### ~~Cut, Copy and Paste~~ (done)

Ctrl+C copies the selected pixels of the active layer (or the whole layer without a selection), and Ctrl+Shift+C (Copy Merged) copies what's visible from all layers. Partly selected pixels come out partly transparent, as in Photoshop. With a mask targeted, Ctrl+C copies the mask as grey. Ctrl+X copies, then clears like Delete: pixels to transparency, a mask to the background grey. Ctrl+V pastes as a new layer above the active one, in the same place it was copied from (centred if it doesn't fit there), and deselects. They're in the Edit menu too.

Copies also go on the Wayland clipboard as an sRGB PNG (through `wl-copy`, so they stay pasteable after Omapix quits), and Ctrl+V pastes PNG or JPEG images copied in other apps, centred. While the clipboard still holds Omapix's own copy, pasting uses the full 16-bit original. Edit › Paste Special has Paste in Place (Ctrl+Shift+V) and Paste Into (Ctrl+Alt+Shift+V). Paste Into pastes centred on the selection as a new layer with a mask revealing just the selection. Paste in Place does what Paste does for now; they'll differ once pastes centre on the view.
- Later: centring pastes on the view rather than the canvas.

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

### ~~Hide All masks and loading masks from their menu~~ (done)

Alt+click on the mask button adds a black mask that hides the whole layer (Photoshop's Hide All), or with a selection, one that hides just the selection; it's also Layer › Add Layer Mask (Hide All). Right-clicking a mask thumbnail offers Add Mask to Selection, Subtract Mask from Selection and Intersect Mask with Selection, the same as Ctrl+click, Ctrl+Alt+click and Ctrl+Shift+Alt+click on it. The other way round, it also offers Replace Mask with Selection, Add Selection to Mask, Subtract Selection from Mask and Intersect Selection with Mask, each one undo step, keeping the selection.

### ~~Tool cursor modifier badges (`+` and `-`)~~ (done)

Over the canvas, selection tools (Rectangular & Elliptical Marquee, Lasso, Magic Wand) show a small badge at the lower right of the crosshair: `+` while holding Shift (add), `-` while holding Alt (subtract), and `×` while holding Shift+Alt (intersect), matching the mode applied when clicking or dragging. The Move tool shows a copy badge while holding Alt (Alt+drag copy).

### ~~Histogram and Navigator panels~~ (done)

A collapsible strip above the right panel's tabs holding Navigator and Histogram, as in Photoshop. Window › Navigator and Window › Histogram toggle the strip, and clicking the active tab collapses it to nothing. The Navigator shows a whole-image thumbnail from the smallest pyramid level with a red rectangle for the visible canvas area (clicking or dragging centres the view), plus zoom slider, zoom field, and Fit / 100 % buttons using the canvas zoom steps. The Histogram draws the composite image's RGB histogram (overlaid with grey/white overlap in Colors view, plus Luminosity), computed off the UI thread and throttled on document edits, with mean, std dev, median and pixel count underneath.

### ~~Image rotation and canvas flips~~ (done)

Photoshop's Image › Image Rotation submenu: 180°, 90° Clockwise, 90° Counter Clockwise, Flip Canvas Horizontal, and Flip Canvas Vertical. Flips and rotations permute document pixels directly with no resampling, operating tile by tile in parallel with rayon and keeping untouched tiles empty. Each command rotates or flips every layer's pixels (including groups and adjustment layers' masks), every layer mask, the selection (rebuilding outlines), and saved alpha channels in a single undo step. 90° rotations swap document dimensions and resize the canvas pyramid while keeping the view fitted to the window, fully preserved through undo and redo.


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
- [ ] Switching the Omarchy theme while Omapix is open
- [x] Retouching setups: Frequency Separation lands in its group, and Dodge & Burn Curves paints lighter and darker on its masks
- [ ] High Pass Sharpening at 100 %, with opacity and a mask, and Filter › Other › High Pass
- [ ] Reduce Noise at 100 % on a portrait: Strength, Preserve Details, Reduce Color Noise, and Sharpen Details with live preview
- [x] Unsharp Mask at 100 % on a portrait: Amount, Radius and Threshold with the live preview, no colour fringes on edges
- [x] Smart Sharpen at 100 % on a portrait: Amount, Radius, Reduce Noise, Remove (Gaussian Blur or Lens Blur), and Shadows/Highlights Fade Amount with live preview
- [ ] Smart Blur at 100 % on a portrait: Radius, Threshold, Quality (Low, Medium, High), and Mode (Normal, Edge Only, Overlay Edge) with live preview
- [x] Filters on masks: Gaussian Blur to soften a mask edge, Unsharp Mask and High Pass on a mask, and Add Noise on a gradient mask
- [ ] Mask Density: slider 0–100 %, live preview, within a selection, from Layer menu and mask context menu, and undo
- [x] Alt+right-drag: brush size left and right, hardness up and down, for the brush, eraser, clone and healing tools
- [x] Lock transparent pixels: `/` and the lock button, then the brush, eraser, fills and Delete on a pasted patch or hair layer
- [x] Layer locks: Lock All (`Ctrl+/`), Lock Image Pixels and Lock Position: buttons and shortcuts, refusing edits with messages, Move tool blocked, and OpenRaster round trip
- [x] Alt+click on a group's triangle: opens or closes that group and every group inside it, and Alt+click between rows still clips
- [x] Add Noise: dialog controls (Amount, Uniform/Gaussian, Monochromatic, Grain Size, Roughness, shadow/highlight falloff), live preview, and the new Grain layer in Overlay mode
- [x] Selection edges: Ctrl+H hides marching ants while keeping selection, status bar notes it, Omapix goes idle, and a new selection shows them again
- [x] Eyedropper tool: click or drag sets foreground colour live, Alt+click sets background, Sample Size averages across edges, and Current Layer vs All Layers
- [ ] Sample "Current & Below": Clone Stamp, Healing Brush, Spot Healing and Eyedropper with Current Layer, Current & Below and All Layers, and Magic Wand Sample All Layers checkbox
- [ ] Select › Modify: Border (1–200 px), Smooth (1–100 px), Expand (1–100 px), Contract (1–100 px), Feather (Shift+F6); verify Defaults button and undo step labels match Photoshop
- [x] Load selection: Ctrl+click layer thumbnail for transparency, mask thumbnail for mask (with Shift/Alt/Shift+Alt), Select › Load Selection (RGB, Luminosity, Transparency, Mask), and edges show again if hidden
- [x] Live GPU Move tool drags on a 24 MP retouch (pasted patch under a Dodge & Burn group and Curves), at fit and at 100 %: smooth, looks the same as the CPU render, no flash when released
- [ ] Custom hotkeys: override command shortcuts and tool letters in ~/.config/omapix/hotkeys.toml, menu labels and tooltips update, and startup warnings for clashes
- [ ] Live GPU slider drags on a 24 MP retouch: opacity, Curves and Levels, Hue/Saturation, and opacity or blend mode in Blending Options, at fit and at 100 %: smooth, no flash when released
- [ ] Channels panel: view Red, Green and Blue (click and Ctrl+2…5), Ctrl+click with Shift/Alt to load them and RGB's luminosity, Save Selection as Alpha 1 and 2, view, load and delete them, and reopen the .ora with them kept
- [ ] Close document (Ctrl+W) with and without unsaved changes, and dropping images onto the window (placed as centred layers, Shift+drop to open, multiple files)
- [ ] PSD import: a flattened 16-bit .psd and a layered one (layers, groups, masks and colours as in Photoshop), and Ctrl+S asking where to save rather than overwriting the .psd
- [ ] Alt+click on the mask button: a black mask, or one hiding the selection; and the mask menu's Add, Subtract and Intersect Mask with Selection
- [ ] Free Transform live on the GPU: dragging handles on a whole 24 MP layer at fit and at 100 %, zooming mid-transform, Enter with no flash, and Esc
- [ ] Free Transform: Ctrl+T on a pasted patch and on a selection, corner and side handles with Shift and Alt, rotating with Shift, the cursors, Enter, Esc and Ctrl+Z, and how smooth it is on a 24 MP layer
- [ ] darktable round trip: export with "edit in Omapix", retouch with layers, Ctrl+S and quit, the TIFF grouped with the raw; then "edit in Omapix" on the TIFF brings the layers back, and the thumbnail updates
- [ ] Paste Special: Paste in Place (Ctrl+Shift+V), and Paste Into (Ctrl+Alt+Shift+V) a copy from elsewhere in the image and from another app
- [ ] Quick Mask: Q to enter with and without selection, brush/eraser/fill strokes with undo, status bar and hidden marching ants, Q to exit with updated selection, layer switch auto-exit
- [ ] Content-Aware Fill: a prop, a mark on the backdrop and a stray hair on a 24 MP portrait, the first use (loading), the layer and its mask, undo, and the message without the model
- [ ] Holding I (or another tool key) to use a tool and going back on release, Alt's pipette cursor with the Brush, and the right-click menu on the image with and without a selection
- [ ] Move tool Ctrl+click picking the layer under the pointer, on a retouch with patches, a group and a masked layer
- [ ] Select and Mask: a rough selection round a head of hair with Radius 20–60, Smooth, Feather, Contrast and Shift Edge, each output, Cancel, and the Defaults button
- [ ] Pen tablet: hovering shows the brush outline, pressure thins and lightens strokes with each pressure button on and off, the side buttons, and the mouse still working afterwards
- [ ] Filter settings kept between runs: apply a few filters and Frequency Separation, quit, reopen and check their dialogs; the Defaults buttons; and a default changed in defaults.toml
- [ ] Tool settings kept between runs: change brush sizes/pressure buttons on a few tools, restart, check
- [ ] Quick Selection and Threshold: painting, adding, Alt, Shift, and the threshold slider on a click, a box and a painted selection
- [ ] Object Selection: W then Shift+W, click a face, a dress and a prop, drag boxes, Shift and Alt, the first use (model loading) and after editing (analysed again), and how the edges look at 100 %
- [ ] Navigator and Histogram panels: Window menu toggles strip, Navigator thumbnail drag pans canvas, zoom slider and field, and Histogram Colors and Luminosity views with statistics
- [ ] Gradient tool: `G` shortcut, dragging linear and radial gradients on pixels and masks, Shift constraint to 45°, Foreground to Background and Foreground to Transparent, Reverse, Opacity with number keys, within a selection, and one undo step
- [ ] Image rotation (180°, 90° CW, 90° CCW) and canvas flips (horizontal, vertical): layers, masks, selections, alpha channels, canvas fit, and undo/redo
- [ ] EXIF metadata preservation: open a camera JPEG/TIFF with capture date, camera and lens, save to .ora, reopen, export JPEG/TIFF, and verify with exiftool that DateTimeOriginal, camera and lens survive
- [ ] Curves: grabbing points quickly and from a little off, typing Input and Output for a selected point, switching channel or layer clears it
- [ ] Mask menu: Replace Mask with Selection, and Add, Subtract and Intersect Selection with the mask, on a feathered selection
- [ ] Sample per tool: set Spot Healing to Current Layer and the Eyedropper to Current & Below, switch between them, restart, and check each kept its own
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
  - ~~Lock All, Lock Image Pixels and Lock Position~~ (done).
  - Later: filters and the Move tool keeping transparency too.
- ~~**Elliptical marquee**~~ (done): Shift+M switches between the rectangular and elliptical marquees, and Shift constrains either to a square or circle.
- ~~**Magic Wand**~~ (done): `W` picks the Magic Wand (grouped with Object Selection). Click to select similar colours, with Tolerance (0–255), Anti-alias, Contiguous and Sample All Layers in the options bar, and Shift/Alt to add, subtract and intersect as with the marquees. Click outside the canvas deselects. Smooth anti-aliased edges feed Feather and layer masks directly.
- ~~**Object Selection**~~ (done): Photoshop's Object Selection tool, grouped with the Magic Wand (`W`, then Shift+W). Click an object (a face, the skin, a dress, a prop) to select it, or drag a box round it; Shift adds another object and Alt subtracts one, as with the other selection tools. It runs SAM 2.1 (the model darktable installs for its AI masks, `mask-object-sam21-small`) through the system's ONNX Runtime, on the GPU with CUDA when it can: the image is analysed once after each change (about 0.15 s on the GPU, 1 s on the CPU, plus half a second to load the model the first time), then each click takes about a tenth of a second. The model's rough mask is snapped to the photo's edges with a guided filter, so edges come out smooth and partly selected where they're soft. The status bar says while it's working.
  - ~~Quick Selection~~ (done): the brush in the same group (Shift+W). Paint over an object and SAM selects it; painting again adds to the same object, Alt takes away, Shift starts another added to the selection. The outline follows while painting; letting go makes it the selection, one undo step a stroke.
  - ~~Threshold~~ (done): a slider for both, cutting the model's confidence lower (more) or higher (less). Dragging it re-cuts the last AI selection live; letting go applies it.
  - It looks at the whole visible image (Photoshop's "Sample All Layers").
  - Later: Select › Subject (needs a matting model), refining with more clicks on the same object, Help › AI Models, and a script to download models when darktable doesn't have them.
- ~~**Load selection from channels and masks**~~ (done): Ctrl+click a layer's thumbnail to select its opaque pixels, and Ctrl+click a mask thumbnail to select the mask, with Ctrl+Shift to add, Ctrl+Alt to subtract and Ctrl+Shift+Alt to intersect. Select › Load Selection offers the visible image's Red, Green, Blue and Luminosity channels, the layer's transparency and its mask.
  - ~~**Select and Mask**~~ (done): Select › Select and Mask… (Ctrl+Alt+R, or right-click the image) is Photoshop's workspace cut down to its sliders: Radius (Edge Detection), Smooth, Feather, Contrast and Shift Edge, with Output To Selection, Layer Mask, or New Layer with Layer Mask (the layer copied with the mask and the original hidden). While it's open the image shows what isn't selected in Quick Mask's red, updated as the sliders move (about 0.1 s at 24 MP with a 40 px radius). Radius finds the edge again in the photo: within that distance of the selection's edge, each pixel is selected as far as its colour goes from the sure-unselected colour nearby to the sure-selected one, so strands of hair are picked out from the backdrop between them. Settings are remembered, with a Defaults button.
    - Later: the Refine Edge brush, to add radius only where the hair is (a big radius across skin with no edge in the photo can drop part of it), Decontaminate Colors, and other views (on black, on white, black & white).
- ~~**Select › Modify**~~ (done): Border…, Smooth…, Expand… and Contract…, with Feather… (Shift+F6), in Photoshop's Select › Modify submenu. Expand and Contract grow or shrink the selection round its edge (an exact distance transform: under 0.1 s at 24 MP and 100 px), Smooth rounds off corners and clears specks smaller than its radius, and Border selects a soft-edged band along the edge. Each remembers its radius and has a Defaults button.
- ~~**Channels panel**~~ (done): a Channels tab beside Layers and History, as in Photoshop. Click Red, Green or Blue to see that channel of the image on its own in grey (Ctrl+3, Ctrl+4, Ctrl+5; RGB or Ctrl+2 goes back), and Ctrl+click one to load it as a selection, with Shift to add, Alt to subtract and both to intersect; Ctrl+click on RGB loads the luminosity. Select › Save Selection (or the panel's save button) keeps the selection as an alpha channel ("Alpha 1"…), which can be viewed, loaded and deleted the same way, and is saved in the .ora. For sky replacement: find the channel where the sky is brightest against the subject, Ctrl+click it, add a layer mask and refine the mask with the brush or a filter.
  - Later: painting and adjusting alpha channels themselves (Levels on a duplicated Blue channel, as in Photoshop), renaming them, and thumbnails in the panel.
- ~~**Sample "Current & Below"**~~ (done): for the Clone Stamp, Healing Brush, Spot Healing and Eyedropper, alongside Current Layer and All Layers (and Magic Wand Sample All Layers), so cloning or healing onto an empty layer ignores layers and adjustments above it. Each tool keeps its own Sample setting, as in Photoshop, so Spot Healing can stay on Current Layer while the Eyedropper is on Current & Below; the Brush's and Eraser's Alt+click use the Eyedropper's.
- ~~**Eyedropper tool**~~ (done): `I` selects the tool. Click or drag on the canvas sets the foreground colour live; Alt+click sets the background colour. Sample Size (Point, 3×3, 5×5, 11×11 average) and Sample (Current Layer or All Layers) in the options bar. Painting tools' Alt+click sampling uses the same sample size setting; while Alt is held with the Brush, Eraser or Spot Healing Brush, the cursor becomes a crosshair with a pipette.
  - ~~Grey colours on masks~~ (done): as in Photoshop, while a layer mask or Quick Mask is targeted the foreground and background colours show and paint as their grey levels, any colour picked turns grey, and D gives white over black. Going back to the layer brings the colours back.
  - ~~Spring-loaded tools~~ (done): as in Photoshop, holding a tool's key (longer than 0.4 s) switches to that tool only while it's held, and letting go goes back, so holding `I` picks a colour and returns to the Brush. A quick tap switches for good, as before.
- ~~**Filters on masks**~~ (done): with a mask targeted, Filter › Gaussian Blur, High Pass and Unsharp Mask work on the mask, previewed live and within the selection, as in Photoshop: blur a mask to soften its edge. Add Noise on a mask adds the noise straight into it (no Grain layer), which breaks up banding in a smooth gradient mask.
  - Later: previewing a filter on a mask while viewing the mask itself (Alt+click the mask thumbnail); the preview shows the image for now.
- ~~**Mask Density**~~ (done): Layer › Mask Density… (or right-click a mask thumbnail) lowers a mask's strength, changing its pixels: at 50 % black becomes mid grey, so it hides half as much, and at 0 % the mask is all white. White stays white. Previewed live, within the selection, one undo step.
  - Later: non-destructive density, as in Photoshop's Properties panel.
- ~~**Live preview for Gaussian Blur**~~ (done): updates the canvas live as the radius changes, with a Preview checkbox in the dialog, respecting layer masks, blend modes, adjustments and selections.
- ~~**Brush size and hardness by dragging**~~ (done): Alt+right-drag over the canvas with a painting tool, as in Photoshop: left and right change the size, up and down the hardness (down is harder), with the brush outline staying where the drag began. Hyprland's mouse bindings all use Super, so Alt+right-drag is free.
- ~~**Gradient tool**~~ (done): `G` selects the Gradient tool (its own slot in the left toolbar after the Eraser). Dragging on the canvas fills the active layer's pixels, or its mask when targeted (graduated masks from black to white for retouching), with a line showing the drag. Shift constrains the angle to 45° steps. Options bar: Type (Linear, Radial), Colours (Foreground to Background, Foreground to Transparent), Reverse, and Opacity (number keys 1–9 and 0 set opacity, as with painting tools). Fills respect selections and Lock Transparent Pixels, with one undo step.
- ~~**Tablet support**~~ (done): pen pressure sets each dab's size and how far its coverage builds up (opacity), on every brush, eraser, clone and healing tool, with Photoshop's pressure buttons in the options bar (✒ beside Opacity and ⊙ at the end, both on by default), and one beside Flow (off by default) that scales each dab's flow, so light strokes build up slowly. winit 0.30 has no tablet input, so Omapix reads the Wayland tablet protocol (`tablet-v2`) itself, on a thread sharing winit's connection, and hands the pen to egui as the pointer (the compositor stops moving the mouse pointer with the pen once a client does this), setting its cursor with `cursor-shape-v1`. The pen's lower side button is a right-click and the upper one a middle-click. Tablet buttons (ExpressKeys) are best mapped to keystrokes outside Omapix (Hyprland binds or OpenTabletDriver).
  - Later: tilt, the eraser end of the pen switching to the Eraser, and a minimum size for light pressure. Replace the protocol code with winit's once eframe moves to winit 0.31.
- **Liquify:** forward warp, push, bloat and pucker, with a mesh that can be edited again later. Shares its warp engine with the slider-driven Symmetry and Reshape in section 7 ([AI.md](AI.md)), and is how their results get touched up by hand.

## 4. Colour and adjustments

- ~~**Histogram in Curves and Levels**~~ (done): drawn behind the curve, and in Levels above the input controls.
- ~~**Eyedroppers in Curves and Levels**~~ (done): set black, grey and white points by clicking the image (Curves adjusts red, green and blue curves to neutralise casts while preserving lightness; single-channel Levels sets master black/white and midtone gamma to map luminance to mid grey, with the limitation that Levels lacks per-channel data to neutralize colour casts).
- ~~**Selective Color** and **Channel Mixer** adjustment layers~~ (done).
- ~~**LUT adjustment layer:** load `.cube` files~~ (done).
- ~~**Curves points: easier to grab, and typed Input and Output**~~ (done): a point is picked where the button went down (egui only starts a drag once the pointer has moved, and a quick drag used to add a new point instead), from 12 points away rather than 8, and it keeps its distance from the pointer. Clicking or dragging a point selects it, filled in, and its Input and Output (0–255) can be typed or dragged under the graph, as in Photoshop.
- **Monitor colour management:** read the display's ICC profile instead of assuming sRGB.
- **Soft proofing** for print and web.

### Denoise, sharpen and add noise

Michael's finishing workflow, from years of Topaz and Nik Collection: denoise first, then retouch and grade, then sharpen, and add noise last, which makes the photo look more natural. Each goes on its own layer by default, so its opacity is its strength and a mask can keep it off the eyes or the background.

- **Denoise** (Filter › Noise › Reduce Noise…), the first step.
  - AI denoising with the NIND model darktable already installed here (`denoise-nind`: 768 px tiles, GPL-3.0 like Omapix), found through the shared model folders and run on the GPU with the same runtime as section 7 ([AI.md](AI.md)).
  - Separate luminance and colour noise amounts. The result goes on a **Denoise** layer above the image.
  - ~~A classical fallback on the CPU~~ (done): Filter › Noise › Reduce Noise… with Strength (0–10), Preserve Details, Reduce Color Noise, and Sharpen Details, using an edge-preserving guided filter on luminance, chroma smoothing, and luminance unsharp masking.
  - For raws, darktable's raw denoise before export is better still. Omapix's is for files that arrive already developed.
- ~~Photoshop's **Smart Blur**~~ (done): Filter › Blur › Smart Blur… with Radius, Threshold, Quality (Low, Medium, High) and Mode (Normal, Edge Only, Overlay Edge). It blurs only among pixels of similar tone, so skin smooths while edges and texture stay sharp. Previewed live, on pixels or a mask; settings are remembered. Takes about 0.2 s at 24 MP for small radii and under 1 s at 100 px, since big radii are sampled more sparsely.
- **Sharpen** (Filter › Sharpen), near the end.
  - ~~Photoshop's **Unsharp Mask**~~ (done): Filter › Sharpen › Unsharp Mask… with Amount, Radius and Threshold, on luminance only (the same offset goes to red, green and blue), so edges don't get colour fringes. Previewed live on the canvas; settings are remembered.
  - ~~**Smart Sharpen**~~ (done): Filter › Sharpen › Smart Sharpen… with Amount, Radius, Reduce Noise, Gaussian or Lens Blur removal, and shadow/highlight fading, on luminance only. Previewed live on the canvas; settings are remembered.
  - ~~A one-click **High Pass sharpening** setup~~ (done): Retouch › High Pass Sharpening… asks for a radius (1–3 px for fine detail) and adds a "High Pass Sharpening" layer in Overlay above the selected layer, holding the High Pass of the visible image's luminance, so it sharpens without colour fringes. Its opacity sets the strength and a mask keeps it off skin. Filter › Other › High Pass… applies the filter itself to a layer. Filter › Other › High Pass… is previewed live on the canvas, like Gaussian Blur and Unsharp Mask; the sharpening setup isn't yet.
  - Previewed at 100 % in the dialog (Photoshop's filter preview box), since sharpening can't be judged zoomed out.
  - Later, output sharpening for the export size, once exports can resize (see Batch export).
- ~~**Add Noise**~~ (done): Filter › Noise › Add Noise… opens a dialog with Photoshop's controls (Amount, Uniform or Gaussian, Monochromatic) plus film-like Grain Size and Roughness, and tonal falloff in shadows/highlights. By default the result goes on a new Grain layer in Overlay mode carrying the grain, with live preview while the dialog is open.
  - Later: grain added at output size once exports can resize.
- Later, a **Finish** action that runs Sharpen then Add Noise with saved settings, which Batch export can reuse.

## 5. Workflow and files

- ~~**darktable round trip**~~ (done): in darktable's export module, choose the target storage "edit in Omapix" (16-bit TIFF). The image is exported beside its raw and opened in Omapix; Ctrl+S saves the layers to a .ora beside the TIFF and writes the flattened image back to the TIFF, and when Omapix quits the TIFF is imported into darktable, grouped with its raw. To retouch it again, select the TIFF and press "edit in Omapix" in the selected image[s] module: Omapix opens the layers from the .ora, and darktable redraws the thumbnail afterwards. The .ora remembers its TIFF, so saving it however it's opened keeps the TIFF up to date. `make install` adds the darktable script (`assets/darktable/omapix.lua`, loaded from `~/.config/darktable/luarc`); restart darktable to pick it up.
  - Omapix is opened as `omapix --round-trip image.tif`, which other apps can use too.
  - Later: opening several exports in one Omapix window, and a darkroom shortcut.
- ~~**Keep EXIF metadata on export and save**~~ (done): raw EXIF metadata (camera make and model, lens, exposure, and capture date / `DateTimeOriginal`) is read when loading TIFF and JPEG files and preserved through .ora saves (`metadata/exif.bin` in the zip, referenced via `omapix:exif` in `stack.xml`), flattened TIFF and JPEG exports, and the darktable round-trip TIFF. Image orientation in the EXIF block is normalized to 1 (normal) so viewers display upright pixels correctly.
- **Batch export:** apply a saved action (for example "resize, sharpen, JPEG") to many files.
- ~~**Recent files** and reopening the last document~~ (done).
- ~~**Close document**~~ (done): Ctrl+W (File › Close) closes the open image and returns to the empty start state, asking to save unsaved changes first.
- ~~**Dropping files onto the window**~~ (done): dropping an image onto an open document places it centred as a new layer named after the file; Shift+drop or dropping with nothing open opens it instead.
- ~~**PSD import**~~ (done): opens 8- and 16-bit RGB and greyscale .psd files with their colour profile: pixel layers (names, position, opacity, visibility, blend modes, clipping and layer masks) and groups, or the flattened image for files saved without layers. Ctrl+S asks where to save, so the .psd is never overwritten.
  - Adjustment and fill layers, text and effects aren't read. When a file has them, Photoshop's flattened image comes in as a hidden "Photoshop composite" layer on top, unless Photoshop saved it blank (Maximize Compatibility off).
  - Later: turning Photoshop's Curves, Levels and Hue/Saturation layers into Omapix's.
- ~~**History panel:** a list of undo steps you can click back to~~ (done).
- ~~**Filter and retouching settings remembered between runs**~~ (done): each filter's and retouching setup's settings as last applied (Gaussian Blur, Smart Blur, High Pass, Unsharp Mask, Smart Sharpen, Reduce Noise, Add Noise, Mask Density, Feather, High Pass Sharpening and Frequency Separation's radius) are saved to `~/.config/omapix/filters.toml` and come back next time, as in Photoshop. Tool options (brush size, hardness, opacity, flow, pressure buttons for each tool, AI threshold, sample modes, wand settings, gradient options) are saved to `~/.config/omapix/tools.toml`. Every one of the filter dialogs has a Defaults button. The defaults are Omapix's own unless changed in `~/.config/omapix/defaults.toml`, which Omapix writes on first run with every setting listed and commented out, to uncomment and edit (like `hotkeys.toml`). Frequency Separation's radius starts from the image's size until it's set.
- ~~**Right-click menu on the image**~~ (done): as in Photoshop, right-clicking the image offers Deselect, Select Inverse, Feather…, Save Selection, Content-Aware Fill, the fills, Clear, Copy, Cut and Free Transform with a selection, or Select All, Paste and Free Transform without. Alt+right-drag still resizes the brush.
- ~~**Custom hotkeys**~~ (done): override any command's shortcut or tool letter from `~/.config/omapix/hotkeys.toml`, with Photoshop's shortcuts as defaults and startup warnings for clashes.
  - When there's no `hotkeys.toml` (or it's empty), Omapix writes one listing every command and tool with its default shortcut, commented out, so what can be changed is there to see and edit.

## 6. Performance

- ~~**Faster slider drags**~~ (done): edits redraw the canvas in place, the part on screen first, then the rest in the background, which a newer edit cuts short. Zoomed out, what's on screen is first previewed from layers shrunk to the size shown (kept until they change), then replaced by the exact full-size result. With four layers at 24 MP, the screen follows a slider in about 60 ms at fit to screen (was about 300 ms), 20 ms at 25 % and 45 ms at 100 %. The Move tool's drags speed up the same way.
  - Zoomed out, the preview blends shrunk layers rather than shrinking the blended image, so non-linear modes (Soft Light, Overlay…) can shift very slightly when the exact result lands.
  - Shrunk layers cost a quarter of each layer's memory at 50 % (a sixteenth at 25 %) while zoomed out.
- **GPU compositing:** move blend modes to the GPU for display, keeping the CPU path for export (see DESIGN.md).
  - ~~Move tool drags~~ (done): when a drag starts, what's below the moving layer is composited once and it, the moving layer and the layers above are uploaded as textures at the zoom level on screen (adjustments as 3D lookup tables). Each frame a shader (`live.wgsl`) blends them with the moving layer offset, with the same formulas as `composite.rs`, and draws the result through the display transform. When the drag ends the move is made for real, and the live view stays up until the canvas has drawn the exact CPU render, so there's no flash. Zoomed out, live moves go in whole pixels of the level on screen, so the move made lands exactly where the live view showed it. A step costs next to nothing instead of a whole-layer translate (~15 ms at 24 MP) and recomposite. Building the stack takes ~30 ms once. A GPU test checks every blend mode against the CPU (to half a 16-bit level) and the display against the CPU display (to 2 levels in 255).
    - Moves the shader doesn't handle stay on the CPU: layers in groups or clipping masks, Blend If, groups other than default Pass Through ones above, selections, and Alt+drag copies.
  - ~~Slider drags~~ (done): dragging a layer's opacity, or an adjustment layer's settings in the Properties panel (Curves points, Levels, Hue/Saturation and the rest), and changing opacity or blend mode in Blending Options, goes through the same live stack with the edited layer's settings updated each frame; an adjustment's lookup table is remade only when it changes. The CPU renders once, when the drag ends (or the dialog closes), and the live view stays up until that's on screen. Blend If edits, masks and layers in groups stay on the CPU.
  - Next: the other cases above, then showing every edit this way.
  - Plan for all of it: upload each layer and mask at the pyramid level on screen (the shrunk layers from `reduced.rs` are that input), composite at screen resolution in a WGSL shader through egui's wgpu paint callback, bake each adjustment and the display colour transform into a 3D LUT on the CPU so the shader has one path for all of them, and build isolated groups in offscreen textures. The CPU composite stays for export, the pixel readout and eyedroppers. Costs about 190 MB of GPU memory per full-size 24 MP layer, far less zoomed out.
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
- ~~**Content-Aware Fill**~~ (done): Edit › Content-Aware Fill fills the selection from its surroundings with LaMa (Apache-2.0, 208 MB, installed by `scripts/fetch-models.sh`, which checks its SHA-256), for props, backdrop marks and stray objects. It looks at the visible image in a square twice the selection's size (at least 512 px), scaled to the model's 512 px, and puts the answer on a new "Content-Aware Fill" layer above the selected one, masked to the selection, so it can be painted back or deleted. 0.2–0.3 s on the GPU once the model's loaded, about 4 s the first time. Select a little beyond the object: anything of it left outside the selection gets smeared into the fill. Big fills come out soft, since the model works at 512 px; the healing tools stay better for skin.
  - Later: **Generative Fill** with a prompt, using FLUX.2 klein 4B (Apache-2.0; an int4 ONNX export exists, 7.8 GB) once Arch's ONNX Runtime reaches 1.30, keeping the unselected part fixed while it generates, as ComfyUI's masked fills do. Several results to choose from, as in Photoshop.

## Out of scope for now
- Vector and layout tools (Omapix is raster only).
- Plugins and scripting beyond `OMAPIX_SCRIPT`.
