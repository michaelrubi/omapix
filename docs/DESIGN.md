# Omapix design

Omapix is a fast, keyboard-first raster photo editor for Omarchy, built
around portrait retouching. It should feel like Photoshop to someone with
Photoshop muscle memory, and start and run like a native Omarchy app.

It is an independent project, not part of Omarchy.

## Principles

1. **Retouch-first.** Features are prioritised by a real portrait workflow:
   darktable base edit → retouch here → export. General-purpose features
   come after that workflow is complete.
2. **Photoshop muscle memory.** Default shortcuts, tool letters and
   navigation match Photoshop. No setup needed to be productive.
3. **Opinionated, not configurable.** Good defaults over preference dialogs.
   The one-click workflows (frequency separation, dodge & burn setup) are
   built in, not plugins.
4. **Omarchy-native.** Wayland-native, colours come from the active Omarchy
   theme and follow theme switches live, installs as one package.
5. **Lightweight.** Starts instantly, stays responsive on 24 MP 16-bit
   files. Every feature must justify its cost in startup time and memory.
6. **Raster only.** No vector or layout personas.

## Architecture

```
crates/
  omapix-engine   image data, colour management, file IO, processing
                  (no UI or GPU dependencies — testable headless)
  omapix          the app: egui UI on wgpu, canvas, input, theme
```

The engine never depends on the UI. This keeps processing testable from
unit tests, and leaves room to replace the UI toolkit later.

### Pixels

- Documents are stored at 16 bits per channel, RGBA, in the document's own
  colour space (the ICC profile embedded in the file, or sRGB if none).
  8-bit files are promoted to 16-bit on load.
- Layers are held in 256×256 copy-on-write tiles (`tiled.rs`). Cloning a
  layer or the whole document shares tiles, and writing copies only the
  tile touched. Tiles never written read as a fill value, so empty layers
  and untouched masks cost nothing.
- Flat images in a **linear** colour space (darktable's default "Linear
  ProPhoto RGB" export) are converted on open to the same primaries with
  gamma 1.8. Brushes, blurs and blend modes then behave as they do in
  Photoshop. The status bar says when this happens.

### Colour management

- Little CMS 2 (`lcms2`) converts document colour → display colour.
- The display colour is the monitor's (`monitor.rs`): an ICC profile named
  in `monitors.toml`, or the colours its EDID reports, where Hyprland takes
  the monitor for sRGB and so converts nothing itself; otherwise sRGB.
- Soft proofing (`DisplayTransform` with a `Proof`) goes by way of the
  proof's colour space, and marks what it hasn't got in grey.
- Editing maths stays in the document's space; only the display cache is
  converted.

### Display pipeline

1. On load, build a mip pyramid (each level half the size of the one before,
   2×2 box filter). When the document's profile is linear, averaging is
   physically correct; gamma-encoded documents get a small, acceptable error
   when zoomed out.
2. Split each level into 512 px display tiles with a 1 px border, so linear
   filtering never shows seams between tiles.
3. Tiles are converted to 8 bits in the display's colours and uploaded to
   the GPU lazily, the first time they are visible.
4. At each zoom, draw the smallest level that is still at least as large as
   the screen area. Above 100 %, pixels are drawn nearest-neighbour so they
   stay crisp, like Photoshop.

### Compositing and undo

- Layers composite tile by tile on the CPU in parallel (`composite.rs`),
  100–200 ms for four layers at 24 MP, depending on their modes. After
  each edit a background render redraws the canvas's image and pyramid in
  place: the tiles on screen first, nearest the middle, then the rest in
  small batches. Once what's on screen is drawn, a newer edit stops it, so
  slider drags skip states rather than queue them. Zoomed out, the part on
  screen is first previewed at the level shown, composited from layers
  shrunk to that size (`reduced.rs`): 4× less work per level, with shrunk
  tiles kept until their layer's tiles change. The full-size render then
  replaces the preview. Brush strokes recomposite only the tiles they
  touch, in place.
- Move tool drags are shown live on the GPU where they can be
  (`live.rs`, `gpu.rs`, `live.wgsl`): what's below the moving layer is
  composited once, then it, the moving layer and the layers above are
  blended each frame by a shader, a render pass per layer, into a 32-bit
  float texture drawn through the display transform as a 3D lookup table
  (adjustments go through lookup tables too). The move is made on the CPU
  once, when the drag ends. Pass Through groups cost nothing, their layers
  being blended as if ungrouped; one with an opacity or a mask gets a pass
  after its layers, fading the result back towards what was below them
  (composited beforehand, or copied aside at the group's start).
- Blend modes follow Photoshop's formulas (Soft Light included), plus
  GIMP/Krita's Grain Extract/Merge for frequency separation.
- Layer groups keep the stack one flat list, as PSD files do: a group's
  contents sit directly below it and name it as their parent
  (`groups.rs`). Compositing builds the tree once per render. A Pass
  Through group blends its contents straight onto what's below, then
  fades between before and after by its opacity and mask; any other mode
  composites the contents on their own and blends the result as one
  layer. On the canvas, what those contents composite to is kept per
  tile in 16 bits (`GroupCache` in `composite.rs`), along with the layers
  it was made from. A tile is redone only when one of their tiles is no
  longer the same shared tile; if a setting inside the group changes,
  or a layer is shown or hidden, the whole group is redone. The result is
  rounded to 16 bits with or without the cache, so both give the same
  pixels.
- A clipped layer is flagged `clipped` and clips to the first unclipped
  layer below it in its group, so clipping masks need no structure of
  their own. Compositing hangs each run of clipped layers off its base in
  the tree, composites them onto the base alone (keeping its alpha, the
  W3C's source-atop), then blends the result like the base.
- Undo keeps whole-document snapshots, which cost almost nothing because
  they share tiles (a snapshot of a 24 MP, four-layer document takes
  ~30 µs). A continuous gesture, such as dragging a slider or one brush
  stroke, is one undo step.

### Files

- **OpenRaster (.ora)** is the native format: layers as 16-bit PNGs with
  the ICC profile, cropped to the area they use, blend modes under Krita's
  names so files open correctly in Krita, layer groups as nested stacks,
  and masks (as extra PNGs), Blend If and clipping under `omapix:`
  attributes other apps ignore.
- Exports: flattened 16-bit TIFF with ICC (back to darktable or to print)
  and 8-bit sRGB JPEG (web and clients).

## Milestones

### 1. Viewer (done)

- Open 16-bit (and 8-bit) TIFF with its embedded ICC profile; PNG and JPEG
  through the `image` crate.
- Colour-managed display to sRGB.
- GPU canvas with smooth pan and zoom on large files.
- Photoshop navigation: see the shortcut table below.
- Omarchy theme colours, updated live when the theme changes.
- Open from the command line, from Ctrl+O, or by dropping a file on the
  window.
- Status bar: zoom, document size, bit depth, colour profile, pixel under
  cursor.

### 2. Layers (done)

Tile engine, pixel layers, opacity, 25 blend modes, layer masks, undo and
redo, OpenRaster save/open, TIFF/JPEG export, Gaussian Blur, one-click
frequency separation (with live preview) and dodge & burn layer, merge
down, stamp visible, drag-to-reorder, Blend If (Blending Options, saved in
OpenRaster), mask view, layer groups (Pass Through or isolated, nested,
with their own opacity, mask and Blend If), clipping masks.

### 3. Retouch tools (mostly done)

Done: brush and eraser with Photoshop's size, hardness, opacity and flow;
painting on masks; eyedropper; Clone Stamp and Healing Brush with aligned
sources, sampling the current layer or all layers; selections (rectangular
marquee, lasso, add/subtract/intersect, inverse, feather) that limit
brushes, fills and filters; Spot Healing Brush that picks its own source;
Move tool for layers and selected pixels.
Next: elliptical marquee, pen pressure (winit has no tablet support on
Linux yet).

### 4. Adjustments (mostly done)

Done: Curves, Levels, Hue/Saturation, Color Balance, Selective Color,
Channel Mixer and Color Lookup (LUT) as adjustment layers with a live
Properties panel, saved in OpenRaster. Next: histogram in Curves,
eyedroppers in Curves and Levels.

### 5. Beyond

Liquify, AI-assisted retouching (local models via ONNX Runtime on the GPU),
PSD import.

## Shortcuts

All follow Photoshop.

| Action | Keys |
|---|---|
| New / Open / Save / Save As | Ctrl+N / Ctrl+O / Ctrl+S / Ctrl+Shift+S |
| Undo / Redo | Ctrl+Z / Ctrl+Shift+Z |
| New layer / Duplicate | Ctrl+Shift+N / Ctrl+J |
| Merge down (Merge Group on a group) / Stamp visible | Ctrl+E / Ctrl+Alt+Shift+E |
| Group / Ungroup layers | Ctrl+G / Ctrl+Shift+G |
| Bring forward / Send backward | Ctrl+] / Ctrl+[ |
| Invert (layer or mask) | Ctrl+I |
| Curves / Levels / Hue/Sat / Color Balance layer | Ctrl+M / Ctrl+L / Ctrl+U / Ctrl+B |
| Select all / Deselect / Inverse | Ctrl+A / Ctrl+D / Ctrl+Shift+I |
| Feather selection | Shift+F6 |
| Fill foreground / background | Alt+Backspace / Ctrl+Backspace |
| Clear | Delete |
| Move / nudge | V / arrow keys (Shift: 10 px) |
| Brush / Eraser / Clone | B / E / S |
| Spot Healing / Healing Brush | J / Shift+J |
| Marquee / Lasso | M / L (Shift adds, Alt subtracts) |
| Brush size / hardness | [ ] / Shift+[ Shift+] |
| Opacity 10–100 % | 1–9, 0 |
| Swap / reset colours | X / D |
| Sample colour / set clone source | Alt+click |
| Fit on screen / 100 % | Ctrl+0 / Ctrl+1 |
| Zoom in / out | Ctrl+= / Ctrl+- |
| Zoom at cursor | Alt+scroll, Ctrl+scroll, pinch |
| Pan | Space+drag, middle-drag, scroll / Shift+scroll |
| Quit | Ctrl+Q |

Click a layer's mask thumbnail to paint on the mask; Alt+click shows the
mask on its own (Alt+click again or Esc returns); Shift+click disables it.
Drag layer rows to reorder them, onto a group's row to put a layer in it,
or between rows inside or outside a group. With the Move tool, Alt+drag moves a
copy and Shift keeps the drag straight or at 45°. Double-click a layer thumbnail (or Layer ›
Blending Options…) for Blend If; Alt+drag a slider handle to split it.
The Frequency Separation dialog previews the texture or colour/tone layer
live while you set the radius.

## Testing the UI

`OMAPIX_SCRIPT` runs steps once an image opens, through the same code the
mouse and menus use, for testing without a mouse:

```bash
OMAPIX_SCRIPT="DodgeAndBurn,Size 300,Opacity 70,Stroke 3000 1450 4000 1450" \
  cargo run --release -- photo.tif
```

Steps are command names (`FrequencySeparation`, `AddMask`, …), `Stroke x0
y0 x1 y1`, `Tool Move|Brush|Eraser|Clone|Heal|SpotHeal|Marquee|Lasso`, `Size n`, `Opacity percent`,
`Color r g b`, `Source x y`, `Look x y`, `View image|mask|texture r|tone r`,
`BlendIf black black_split white_split white [under]` (0–255).

## Licensing

GPL-3.0-or-later. Code may be reused from GPL-compatible projects such as
Krita, GIMP and darktable, with attribution in the file it lands in.
