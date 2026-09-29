# AI-assisted retouching

Design for section 7 of the [roadmap](ROADMAP.md): Evoto-style one-click
portrait cleanup, running locally on the GPU, with no subscription and no
cloud. Status: under way, see Milestones. [DESIGN.md](DESIGN.md) covers the
rest of Omapix.

## Goals

- **Masks without painting.** Skin, face parts (eyes, brows, lips, teeth),
  hair and the whole subject, as selections or layer masks, in a second or
  two.
- **Blemishes found for you.** Spots and small marks detected on skin and
  handed to the Spot Healing Brush, onto their own layer.
- **Smoother skin that keeps its texture.** Built on frequency separation
  and the skin mask, so pores stay and blotchiness goes.
- **One click for all of it** (Auto Retouch), with a few sliders for how
  strong each step is.
- **Each face on its own.** In a group photo, every face gets its own
  settings and its own layers, or one setting for all.
- **Subtle reshaping by slider.** Face symmetry, and "easy Liquify" for
  face and body shape, driven by face and body landmarks rather than
  brushes.
- **Generative Fill with a prompt.** Remove or replace something big, or
  extend the canvas, where Content-Aware Fill and healing run out.
- **Everything stays editable.** Each result is an ordinary layer, mask or
  selection that can be painted on, faded, hidden or deleted, as if it had
  been done by hand. No black boxes baked into the pixels.

### Not goals

- Face swaps.
- Changes a slider can't take back: reshaping is always a separate layer
  with its settings kept, so it can be reopened, softened or deleted.
- Cloud processing, accounts, telemetry, or downloads Omapix starts on its
  own.
- Training models inside Omapix.

## What we can build on

**This machine.** RTX 4070 Laptop GPU (8 GB), 30 GB RAM. Arch's
`onnxruntime-cuda` 1.29 is installed at `/usr/lib/libonnxruntime.so`, with
the CUDA provider.

**darktable 5.6.** It added optional AI features on the same runtime, and
they're set up here: `plugins/ai/ort_library_path=/usr/lib/libonnxruntime.so`,
provider NVIDIA. Its models live in `~/.local/share/darktable/models/`, one
folder per model: the ONNX files and a `config.json` with the id, task,
architecture, input sizes and a model card (licence, training data).
Installed here: **SAM 2.1 Hiera Small** (`mask-object-sam21-small`, click
an object to mask it), two denoisers and an upscaler. Following darktable's
choices has two benefits:

- Its model format is a ready-made, licence-aware manifest.
- Omapix can reuse the SAM 2.1 model already on disk.

**The engine.** Most of what the AI features produce already has somewhere
to go:

| Output | Goes into |
|---|---|
| A mask | `Selection::from_coverage(Tiled<u16>)`, or a layer's `Mask` |
| Blemish spots | `brush::Stroke` with `Paint::SpotHeal`, one short stroke per spot |
| Smoothed skin | `filters::gaussian_blur`, the frequency separation maths in `ops.rs` |
| Even tone | Curves adjustment layers with masks (the "Dodge & Burn" setup in section 3) |
| Model input | `DisplayTransform::to_srgb` (document colour → sRGB for the model) |
| Preview while a slider moves | the frequency separation preview, and the shrunk layers in `reduced.rs` |

## Principles

1. **Models find, the engine edits.** Models only answer "where": masks,
   face points and spot positions. The changes to pixels are the engine's
   own filters and brushes, which are exact, 16-bit, colour-managed and
   tested. So a weak model gives a worse mask, never damaged pixels.
   The fills (Content-Aware Fill, Generative Fill) are the exception: their
   job is to make new pixels, so they always land on their own layer,
   masked to the selection, over the untouched image.
2. **Results are layers.** Auto Retouch builds a layer group you could
   have built by hand. Undo is one step. Hiding the group shows the before.
3. **Opt-in and offline.** Omapix never goes online. Models are installed
   by a script the user runs, and verified by checksum. Without a model or
   the runtime, the AI commands are greyed out with a status-bar hint, and
   nothing else changes.
4. **Safe for client work by default.** Omapix is for client and personal
   work alike. Every model in the default set has weights *and* training
   data that allow commercial use. Models that don't (several popular face
   parsers, see Models below) can be installed as an opt-in. They are then
   marked "personal use only" wherever they appear, and every command that
   uses one says so in the status bar.
5. **Nothing at startup.** The runtime and models load on first use and
   unload when idle, so Omapix still starts instantly and uses no GPU
   memory until asked.

## Architecture

```
crates/
  omapix-engine   + guided filter, band separation, blob detection,
                    spot lists (plain CPU image maths, headless tests)
  omapix-ai       NEW: ONNX Runtime, model registry, model input/output,
                  face analysis, segmentation, blemish detection
                  (no UI; depends on omapix-engine)
  omapix          commands, dialogs, the Object Selection tool, spot
                  overlay, background jobs
```

The engine keeps its "no UI or GPU dependencies" rule: ONNX Runtime with
CUDA is a GPU dependency, so it goes in its own crate. The maths that turns
model output into edits (guided filtering, band separation, finding spots)
goes in the engine, where it can be tested without models.

### Runtime

- Rust bindings: the [`ort`](https://ort.pyke.io) crate (2.0 release
  candidates) with its `load-dynamic` feature. The library is opened at
  run time, as darktable does, from `OMAPIX_ORT_LIBRARY` or else
  `/usr/lib/libonnxruntime.so`. Nothing is bundled, so the binary stays
  small, and the system package brings the CUDA provider.
  - Checked in the spike (2026-09-25): `ort` 2.0.0-rc.13 (built for
    1.28's API) loads the system's 1.29 and runs darktable's models.
  - Arch's `onnxruntime-cuda` 1.29 builds its CUDA provider without
    linking cuDNN, so loading it fails with `undefined symbol:
    cudnnGetConvolutionBackwardDataAlgorithm_v7`. Omapix opens
    `libcudnn.so.9` (globally) before asking for CUDA, which fixes it;
    otherwise it falls back to the CPU.
- Provider: CUDA if the library has it, else CPU. No setting unless one is
  needed.
- One `Session` per model, created on first use. Sessions not used for two
  minutes are dropped, to give GPU memory back (to darktable, or to GPU
  compositing later).
  - Not yet: dropping a CUDA session (ONNX Runtime 1.29, cuDNN 9.25,
    driver's `libcuda`) corrupts the heap, and the process aborts in
    `libcuda` as it exits, about half the time (7 in 12 runs). Kept for
    the whole run, 0 in 12. So for now sessions live until Omapix quits;
    try unloading again with newer ONNX Runtime or drivers.
- All inference runs on background threads through the existing
  `Editor::edit_in_background` / job mechanism, with the status bar showing
  what's running. The first CUDA run of a model is slower while the runtime
  optimises its graph. The status bar says so.

### Models on disk

```
~/.local/share/omapix/models/<id>/config.json  + *.onnx
```

- The same `config.json` schema as darktable (`id`, `task`, `arch`,
  `version`, `attributes.input_sizes`, `model_card.license`, …), plus
  `sha256` for each file.
- **Shared model folders.** Omapix also searches other folders,
  read-only:
  - darktable's `~/.local/share/darktable/models/`, by default
  - any others listed under `model_folders` in `~/.config/omapix/`, such as
    a ComfyUI models folder
  - Models are recognised by the SHA-256 of their files, not their folder
    name, so the same model anywhere on disk is used rather than
    downloaded again. The fetch script checks these folders before it
    downloads anything.
  - Sharing could cause two problems. If darktable updates or removes a
    model, the checksum no longer matches, and Omapix asks for its own
    copy. If darktable changes its folder layout, only the search changes.
    Neither depends on how big Omapix gets, so sharing can stay.
- `scripts/fetch-models.sh` downloads the default set with `curl`, checks
  the checksums, and prints each model's size and licence before
  downloading. Omapix itself never touches the network.
- A Help › AI Models dialog lists what's installed, where from, the licence
  and GPU/CPU, and says how to run the script for what's missing.

### Getting images in and out of models

Models are trained on 8-bit sRGB photos at a few hundred pixels across.
Documents are 16-bit, often 24 MP, in ProPhoto or other profiles. Every
model call goes through one path:

1. **Input.** Take the flattened image (or a shrunk pyramid level, when the
   model's input is much smaller than the image). Crop it: the whole image
   for detection, or a padded box around each face for face models.
   Convert to sRGB floats with the document's `DisplayTransform`, then
   resize to the model's input size.
2. **Output.** A low-resolution mask or probability map for the crop,
   resized back to the crop's size.
3. **Refine.** Refine at full resolution with a **guided filter**, using
   the image itself as the guide, so mask edges follow real edges: hair,
   the jawline, eyelids. This one step makes low-resolution segmentation
   usable on 24 MP files. It is plain CPU maths in the engine
   (`filters::guided`), with tests.
4. **Result.** Paste into a document-sized `Tiled<u16>`. Tiles outside
   every crop stay empty, so they cost nothing.

### Scale

Everything that depends on how big a face is (blur radii, spot sizes,
feathering) is measured in **inter-ocular distance** (IOD), from the face
landmarks, not in pixels. The same settings then work on a headshot and a
full-length portrait, and on 12 MP and 60 MP files.

## Features

### 1. Object Selection and Select Subject

*Photoshop: the Object Selection tool (W), and Select › Subject.*

- **Object Selection (`W`, grouped with the Magic Wand).**
  - Click an object to select it. Shift+click adds points, Alt+click
    removes them, and dragging a box prompts with that box.
  - Uses SAM 2.1, which comes as two parts: an image encoder (about 160 MB,
    run once per image, cached until the image changes) and a small prompt
    decoder (about 20 MB, milliseconds per click), so clicks feel
    immediate.
  - The encoder sees the image at 1024 px. For small objects on large
    files, the second click can re-encode a crop around the first result
    for a sharper edge.
- **Select › Subject.** Selects the person in one step, with a
  portrait-matting model (BiRefNet portrait, or MODNet as the lighter
  choice). Soft hair edges come out as partial selection, as in Photoshop.
- Both produce an ordinary `Selection`, so Feather, Invert, Add Mask and
  the marching ants all just work.

Delivers the "Object Selection" item in section 3 of the roadmap, and
proves the runtime, the model registry and the model input/output path
before the face work starts.

### 2. Face and skin masks

*Photoshop: Select › Color Range › Skin Tones with Detect Faces, and
Select › Subject.*

Face analysis (one background job, cached until the image changes):

| Step | Model | Gives |
|---|---|---|
| Find faces | YuNet (OpenCV Zoo) | a box and 5 points per face |
| Face points | MediaPipe Face Landmarker | 478 points per face, iris included |
| Skin and hair | MediaPipe multiclass selfie segmentation | hair, face skin, body skin, clothes, background |

Every face found is numbered, left to right, and gets its own masks. From
these, per face:

- **Features:** eyes, irises, brows, lips, mouth (teeth) and nostrils, as
  polygons from the landmarks. They're drawn anti-aliased and feathered by
  a fraction of the IOD.
- **Skin:** face skin and body skin from the segmentation, minus the
  features (grown slightly), then refined with the guided filter. This
  covers neck, shoulders and arms too, not just the face.
- **Hair:** from the segmentation, refined. Useful for protecting the
  hairline from smoothing, and for hair adjustments.
- **Whose body skin is whose:** in group photos, body skin and hair are
  split between people with SAM 2.1, prompted by each face's box. So "this
  person" means their face, neck, arms and hair, not just the face.

New commands under Select: **Skin**, **Hair**, **Eyes**, **Lips**,
**Teeth**, **Subject**. Each makes a selection (Shift adds to the current
selection, Alt subtracts, as with the marquees). With a layer selected,
Layer › Layer Mask › **From Skin…** (and so on) makes the mask directly.

Why landmarks and not a face-parsing network: most face-parsing models
(BiSeNet, SegFormer and SegFace, trained on CelebAMask-HQ; Meta's Sapiens)
have non-commercial terms on their weights or training data. Landmark
polygons are sharper for eyes and lips anyway, and the multiclass
segmenter covers skin and hair. If a commercially usable parser appears,
it can replace step 3 without changing anything else.

### 3. Skin smoothing that keeps texture

*Evoto's skin smoothing, Photoshop's Neural Filters › Skin Smoothing.*

Split the image into three bands with Gaussian blurs, with radii measured
in IOD:

```
high  = image − blur(r_fine)            pores, fine hair: kept
mid   = blur(r_fine) − blur(r_coarse)   blotches, bumps, uneven skin: reduced
low   = blur(r_coarse)                  colour and overall shape: kept
```

The smoothed image is `low + high + (1 − amount) × mid`.

- The result goes on a new pixel layer, **Smooth Skin**, masked by the
  skin mask. The mask is grey where it should apply partly, for example
  fading out towards the hairline and the edges of features.
- The layer's opacity is the overall strength. Painting on the mask adds
  or removes smoothing by hand, exactly as with a manual frequency
  separation.
- `r_fine` sits just above pore size (about 0.02 IOD) and `r_coarse` at
  about 0.25 IOD. Both get tuned on real portraits and are exposed as
  Detail and Smoothness sliders.
- Edges of features are protected twice: by the mask, and by blurring
  within the skin mask only (a normalised, masked blur), so eyebrows and
  lips don't bleed into the skin.
- Dialog with a live preview, like Frequency Separation's. The preview
  runs on the shrunk pyramid level on screen, then the exact result.

The same band split also gives a manual **Frequency Separation (3 bands)**
setup, for people who like to work on the mid band themselves.

### 4. Blemishes to Spot Healing

*Evoto's acne and blemish removal.*

**Detection: classical first.** There is no well-known, commercially
licensed blemish detector, and a classical detector is predictable,
tunable, explainable, and needs no model. Within the skin mask, at a scale
where the IOD is about 150 px:

1. Convert to Lab. Spots are darker (L\*) and/or redder (a\*) than the skin
   around them.
2. Band-pass both channels with a difference of Gaussians tuned to spot
   sizes (about 0.02–0.12 IOD).
3. Normalise by the local variation (median absolute deviation), so oily,
   textured and smooth skin all use the same threshold.
4. Find blobs above a threshold. Keep those of spot-like size and
   roundness, away from features, the hairline and the skin mask's edge.
5. Score each blob by contrast and redness.

The output is a list of spots: position, radius and score. It is an engine
type, so a learned detector can later produce the same list.

**Healing.**
- No separate review step: healing goes straight onto a new empty
  **Blemishes** layer, and the layer is the review.
  - Its opacity fades all the healing at once.
  - Erasing on it brings any one spot back.
  - Hiding it shows the before.
- Each spot is one Spot Healing stroke, sized from its radius, sampling all
  layers below. It's all one undo step.
- The dialog's Sensitivity slider decides how many spots are healed, with
  the spots found shown as circles on the canvas while the dialog is open,
  so the setting can be judged before applying.
- Prerequisite: spot healing onto an empty layer with Sample All Layers
  (Photoshop's usual non-destructive way). Today `Stroke::finish` heals
  onto the layer it samples. Worth doing on its own, for manual retouching
  too.

**Later: a learned detector, optional.** Retouched `.ora` files are
before/after pairs: the heal strokes on their Blemishes layers mark exactly
which spots a professional removed.
- A separate, opt-in script (`scripts/export-training.sh`) collects pairs
  from folders the user chooses, locally.
- A small detector trained on them outside Omapix, and exported to ONNX,
  can replace or rerank the classical one, with no licensing question over
  the data.
- Omapix never collects anything by itself, and nothing leaves the
  machine.

### 5. Even tone (automatic dodge & burn)

*The pro workflow the manual Dodge & Burn setup mimics.*

- Within the skin mask, compare local brightness with its surroundings:
  `D = blur(L*, r_small) − blur(L*, r_large)`, radii in IOD.
- Build the curves-based **Dodge & Burn** group from section 3: a
  brightening and a darkening Curves layer.
- Instead of black, their masks get `max(−D, 0)` and `max(D, 0)`, scaled
  by the Even Tone amount and limited to the skin.
- Painting on either mask takes over by hand from there.

This lifts shadowy patches and calms hot spots, without flattening the
face's shape: large-scale light is left alone by `r_large`.

### 6. Auto Retouch

*Evoto's one click.*

Retouch › **Auto Retouch…** runs 2 → 4 → 3 → 5 on every face found. It has
no default shortcut, since Photoshop has no equivalent to match (and
`Ctrl+Alt+R` is its Select and Mask):

- The dialog has an amount slider per step, a checkbox to skip each, and
  three presets (Natural, Standard, Strong, remembering the last used).
- The dialog shows the blemishes it will heal as circles on the canvas,
  following the Sensitivity slider.
- **Faces:** a strip of face thumbnails along the top of the dialog picks
  which face the sliders apply to. The first entry, **All Faces**, applies
  a setting to every face at once, until a face is given its own. A face
  can also be switched off. Clicking a face on the canvas while the dialog
  is open selects it too.
- The result is one undo step, adding a **Retouch** group (Pass Through),
  with a group per face:

```
Retouch
  ├─ Face 1
  │    ├─ Dodge & Burn       (group: Brighten, Darken curves, with masks)
  │    ├─ Smooth Skin        (pixel layer, this face's skin mask)
  │    └─ Blemishes          (heal patches on an empty layer)
  └─ Face 2
       └─ …
```

- Each face's group opacity is its overall strength. Each layer's opacity
  is that step's strength.
- The settings used are stored on the group (an `omapix:` attribute in the
  OpenRaster file). Reopening Auto Retouch on the group starts from them,
  and can redo the group from the current image.

Every step is also its own command (Select › Skin, Filter › Retouch ›
Smooth Skin…, Heal Blemishes…, Even Tone…, Face Symmetry…, Reshape…), so
any one can be run alone.
It's scriptable through `OMAPIX_SCRIPT` (for example
`AutoRetouch Natural`), which is also how it gets its end-to-end tests.

Also from the same masks, later:
- whiten eyes and teeth (Hue/Saturation layers masked to eyes and teeth)
- reduce shine (highlights within the skin mask)
- lift under-eye shadows (landmarks mark the area)

### 7. Face symmetry

*Evoto's face symmetry.*

- The face landmarks give each face a midline: the line through the
  bridge of the nose, the tip of the nose, the middle of the lips and the
  chin, fitted by least squares.
- Each landmark's mirror image across that line says where it would sit on
  a symmetrical face. The target for each point is part way there, by the
  Symmetry amount: 0 leaves it, 100 % makes the two sides match.
  - Each side moves half way, so neither side is simply copied.
  - Separate amounts for eyes (height and size), brows, nose, mouth and
    jaw, since the eyes usually matter most.
- The moves become a smooth warp (see Warps below), limited to the face
  and faded out before the hairline and the face's edge.

### 8. Reshape (easy Liquify)

*Evoto's face and body shaping. Liquify with sliders instead of brushes.
Photoshop: Face-Aware Liquify, in the Liquify dialog.*

- The face sliders go in the Liquify dialog, as Photoshop's Face-Aware
  Liquify does, so the brushes can touch up what the sliders did in the
  same session. Symmetry sits there too.

- **Face.** Sliders per face:
  - face width, jaw and chin
  - forehead height
  - eye size and spacing
  - nose width and length
  - lip fullness and mouth width
  - smile (corners of the mouth)
- **Body.** Needs a body model: MediaPipe Pose Landmarker (33 points,
  Apache-2.0) and the person's matte from Select Subject. Sliders:
  - waist, hips, arms, legs (length and width)
  - shoulders, neck length
  - head size
- Each slider moves a set of landmarks along a fixed, hand-designed
  direction (face width moves the jaw points in towards the midline, and
  so on), scaled by the face's IOD or the body's size.
- All faces and bodies in the picture have their own sliders, as in Auto
  Retouch.

#### Warps

Symmetry, Reshape and the brush Liquify from section 3 share one warp
engine in `omapix-engine` (plain CPU maths, headless tests):

- **Displacement field.** Point moves become a smooth displacement field
  by moving least squares (rigid), on a coarse grid (every 8 px),
  interpolated in between. Points that shouldn't move, such as the far
  eye when narrowing the jaw, are pinned with zero moves.
- **Sampling.** Every output pixel is resampled from the source bicubic,
  in 16-bit, and warped tiles are only made where the field is non-zero.
- **Background protection.** Body moves are faded out with distance from
  the person's matte, so straight lines behind (door frames, horizons)
  bend as little as possible. This is the known weak spot of body
  reshaping, and needs testing on real photos.
- **Result.** A pixel layer, **Reshape**, at the top of the stack, made
  from the visible image below it, with its settings stored on the layer.
  Reopening it redoes the warp from the image below, with the saved
  settings, so earlier edits underneath can be picked up by pressing
  Update. Its opacity has no meaning for a warp, so it's the undo and
  delete that matter.
- **Live preview:** the warp applied to the shrunk pyramid level on screen
  while a slider moves, then at full size.

### 9. Generative Fill

*Photoshop: Edit › Generative Fill.*

- Make a selection, then Edit › **Generative Fill…**: a prompt ("remove the
  person", "a bare wall", "more of the backdrop") and Generate. An empty
  prompt fills from the surroundings, as in Photoshop.
- Three results to choose from, with arrows in the dialog, previewed on the
  canvas. Generate again for three more.
- The result goes on a new **Generative Fill** layer above the selected
  one, masked to the selection, like Content-Aware Fill's. The prompt and
  seed are kept on the layer, so it can be generated again later.
- Same input path as Content-Aware Fill: a square round the selection (with
  context around it), scaled to the model's size, and only the selected
  part replaced, keeping the rest fixed while it generates, as ComfyUI's
  masked fills do. Selecting past the canvas with the Crop tool, then
  filling the transparent edge, extends the image.
- Model: FLUX.2 klein 4B (Apache-2.0). An int4 ONNX export exists (7.8 GB
  on disk). It needs ONNX Runtime 1.30, which Arch doesn't have yet.
  - GPU only: on the CPU a result would take minutes, so without CUDA the
    command is greyed out with a status-bar hint.
  - To check in a spike: whether it fits beside everything else in 8 GB of
    GPU memory, how long a result takes at 1024 px, and whether it has to
    be unloaded after use (see Runtime: unloading CUDA sessions crashes
    today).
- Big fills come out at the model's resolution. On a 24 MP file a large
  fill is softer than the photo round it, so Add Noise or the Finish grain
  helps it match.

## Models

Default set, all usable for commercial work:

| Use | Model | Size | Licence | Notes |
|---|---|---|---|---|
| Object Selection | SAM 2.1 Hiera Small | 180 MB | Apache-2.0 | Already installed via darktable. Tiny and Base Plus variants exist. |
| Select Subject | BiRefNet portrait | ~200–900 MB (variant) | MIT | Best hair edges. MODNet (Apache-2.0, 25 MB) as the light option. |
| Faces | YuNet | <1 MB | MIT | Fast, fine on CPU. |
| Face points | MediaPipe Face Landmarker | 4.9 MB | Apache-2.0 | `senty-au`'s ONNX conversion, checked against the TFLite. |
| Skin, hair | MediaPipe multiclass selfie segmentation | 16 MB | Apache-2.0 | `senty-au`'s ONNX conversion, checked against the TFLite. 256×256 input, so the guided filter matters. |
| Body points (Reshape) | MediaPipe Pose Landmarker | ~6–30 MB | Apache-2.0 | 33 points. Lite/Full/Heavy variants. |
| Content-Aware Fill | LaMa | 208 MB | Apache-2.0 | Installed by `fetch-models.sh`. |
| Generative Fill | FLUX.2 klein 4B (int4 ONNX) | 7.8 GB | Apache-2.0 | Optional download, GPU only. Needs ONNX Runtime 1.30. The 9B model is non-commercial, so it's left out. |

About 600 MB total without BiRefNet's large variant and FLUX.2, on disk
only. FLUX.2 is a separate, optional download because of its size. Loaded
on first use.

Considered and left out for now, because their weights or training data
are non-commercial:
- BiSeNet, SegFormer and SegFace face parsers (CelebAMask-HQ)
- Meta's Sapiens (CC BY-NC 4.0; Sapiens2 has its own licence, still to
  read)
- InsightFace's detectors

They can be installed as opt-ins marked "personal use only" (see
Principles), if they turn out to be much better.

Licences and sizes above come from the model cards as of September 2026,
and are to be rechecked when each model is packaged. The model card in
each `config.json` keeps the licence next to the file.

## Performance

Targets on this machine (RTX 4070 Laptop, 24 MP portrait), to confirm in
the spike:

| Step | GPU target | CPU fallback |
|---|---|---|
| Face analysis (detect, points, skin/hair) | < 0.3 s (measured: 0.23 s at 24 MP, two faces) | < 2 s (measured: 0.87 s) |
| Guided filter refine, per mask at 24 MP | < 0.3 s (CPU, parallel) | same |
| SAM 2.1 encoder, once per image | < 0.5 s (measured: 0.14 s, 0.47 s the first time) | several s (measured: 1.1 s) |
| SAM 2.1 per click | < 30 ms (measured: 8 ms) | < 200 ms (measured: 40 ms) |
| Blemish detection | < 0.3 s (CPU) | same |
| Smooth Skin (blurs at 24 MP) | < 1 s (CPU) | same |
| Auto Retouch end to end, per face | < 3 s | < 10 s |
| Reshape or Symmetry preview per slider step (shrunk level) | < 50 ms | same |
| Reshape or Symmetry at 24 MP | < 1 s (CPU, parallel) | same |

- **Memory:** GPU memory well under 2 GB with all default models loaded.
  Idle unloading returns it.
- **CPU fallback:** everything works without a GPU, just slower. The
  heavy pixel work is CPU anyway.

## Testing

- **Engine maths** (guided filter, bands, masked blur, blob detection,
  warps): ordinary unit tests on synthetic images. For example:
  - a flat skin patch with drawn spots must give back exactly those spots
  - smoothing must leave the high band unchanged
  - a warp with no moves must give back the image exactly, pinned points
    must stay put, and a mirrored face must come out unchanged by Symmetry
- **Runtime plumbing:** a tiny ONNX model checked in as a test fixture (a
  few hundred bytes: identity, and a 1×1 convolution) tests loading, input
  layout, crops and colour conversion without real models.
- **Model tests:** marked `#[ignore]`. They run when the models are
  installed (`cargo test -- --ignored`), with fixed test portraits and
  loose checks: a face found, skin covering the cheeks, eyes outside the
  skin mask.
- **Quality:** a small evaluation set, 20–30 of Michael's portraits with
  hand-painted masks and his own blemish choices. It measures:
  - mask overlap (IoU)
  - spots found versus spots he healed
  - time per step

  This is a script run by hand, not a unit test, and the numbers go in
  this document as the models change.
- **End to end:** `OMAPIX_SCRIPT` runs of Auto Retouch on a test portrait,
  checking the layer structure it builds.

## Milestones

Each is useful on its own and ends with `make install` and hand testing.
Reordered on 2026-09-29 (see Decisions).

0. ~~**Spike**~~ (done, 2026-09-25): `ort` 2.0.0-rc.13 with
   `load-dynamic` runs darktable's SAM 2.1 (small) from Omapix's new
   `omapix-ai` crate, on the CPU and with CUDA (once cuDNN is loaded
   first, see Runtime). One click on a portrait's face selects its skin.
   `crates/omapix-ai/examples/spike.rs` does it again.
1. ~~**Groundwork and Object Selection.**~~ (done): Object Selection and
   Quick Selection (click, box or brush, Shift and Alt; `omapix-ai`'s
   runtime and SAM, and the engine's `refine::mask_coverage` with the
   guided filter), and Content-Aware Fill with LaMa and `fetch-models.sh`.
   Left for later: the model registry (shared folders, matched by
   checksum) and Help › AI Models.
2. **Face analysis and masks**, per face and per person. Select › Skin,
   Hair, Eyes, Lips, Teeth, and masks from them. Everything after this
   needs these masks, so it comes first.
   - Spike done (2026-09-29): `omapix-ai::face` runs all three models,
     and `crates/omapix-ai/examples/faces.rs` draws what they find. See
     "Face analysis spike" below.
3. **Blemishes.** The classical detector and Heal Blemishes, onto an empty
   layer (Sample Current & Below already heals onto one). The biggest time
   saver in everyday retouching.
4. **AI Denoise** with NIND (roadmap section 4), onto a Denoise layer. The
   model is already on disk from darktable (GPL-3.0, like Omapix) and the
   runtime works, so it's quick, and it's the first step of the finishing
   workflow.
5. **Smooth Skin**, with the dialog and preview, and the 3-band Frequency
   Separation setup.
6. **Even Tone**, the automatic Dodge & Burn.
7. **Auto Retouch**, with the face strip, per-face groups, presets and
   scripting.
8. ~~**Warps and Liquify**~~ (done): the warp engine, and brush Liquify
   (roadmap section 3), the way to touch up what the sliders do.
9. **Face-Aware Liquify and Face Symmetry**, in the Liquify dialog. Cheap
   once face points (2) and the warp engine (8) exist.
10. **Generative Fill.** After the portrait work, which is used on every
    photo, while Generative Fill is for the occasional big removal or
    extension. By then Arch's ONNX Runtime should have reached 1.30. Starts
    with a spike: memory, speed, and unloading.
11. **Select › Subject** (MODNet or BiRefNet), and **AI upscaling** for
    print enlargements in Image Size (darktable's RealPLKSR, MIT, already
    on disk).
12. **Body Reshape**, with the pose model and background protection.
13. **Later:**
    - the optional learned blemish detector
    - eye and teeth whitening, shine, under-eye
    - checking whether an opt-in face parser beats the landmark polygons

### Face analysis spike

2026-09-29, on a headshot, a tilted face with glasses and teeth, a
full-length portrait (4024×6048), and two people, one in profile
(6048×4024).

- **Models.** YuNet is OpenCV Zoo's ONNX file. MediaPipe only publishes
  the other two as TFLite. `tf2onnx` converts both cleanly, and so do the
  ONNX copies on Hugging Face from `senty-au`: all four match TFLite to
  within 5e-4 on random inputs. `fetch-models.sh` downloads those copies,
  pinned by revision and checksum, so it needs no Python or TensorFlow.
- **Inputs.** From the TFLite metadata: the segmenter wants RGB in −1 to
  1, and the landmarker RGB in 0 to 1. YuNet wants BGR in 0 to 255, in a
  640 × 640 square. The segmenter's classes are background, hair, body
  skin, face skin, clothes and others (glasses, jewellery).
- **Crops.** The face crop is 1.5 times the face's box, turned so the eyes
  are level, then found again from the first pass's points, as MediaPipe
  does. Tilted faces come out as well as upright ones.
- **Outlines.** Rings of landmark indices for the eyes, brows, lips, mouth
  opening, irises and face oval are in `face::outline`. All of them fit
  on the test photos.
- **Speed**, warm, with CUDA: 24 MP, both faces, 0.23 s (0.87 s on the
  CPU). Most of it is shrinking the image for the detector and
  segmenter, which a mip level would make cheaper. Headshots take about
  50 ms. Refining the skin mask at 24 MP takes 0.09 s. The first run of
  each model takes 0.2–0.6 s more.
- **Found wanting:**
  - A face in profile is detected (score 0.64), but the landmarker says
    there's no face in its crop, so it has no feature outlines. Its skin
    still comes from the segmenter.
  - At 256 px across the whole image, the segmenter misses small areas of
    skin, such as a shin in the full-length shot. A second pass on a crop
    around each person helps there.
  - An orange pumpkin (a costume) comes out partly as skin. Splitting skin
    between people with SAM, prompted by their faces, should drop it.

## Decisions

From Michael, 24 September 2026:

1. **Client and personal work.** The default set is commercially usable.
   Non-commercial models are an opt-in, marked "personal use only".
2. **After the roadmap's four features:**
   - editing each face individually
   - face symmetry
   - slider-driven face and body reshaping ("easy Liquify")

   These are features 7 and 8 above, and part of Auto Retouch.
3. **Share model folders** with darktable, and with any other folders
   listed, matched by checksum so nothing is downloaded twice.
4. **No review step.** Automated edits land on their own layers and
   groups, and opacity, erasing and hiding are the review.
5. **Training on your own retouches is optional**, opt-in and local only.

From Michael, 29 September 2026:

6. **Generative Fill is a goal** (feature 9), no longer a non-goal. It
   comes after the portrait work (milestone 10).
7. **Milestone order:** face masks, then blemishes, AI denoise, smooth
   skin, even tone and Auto Retouch; then Face-Aware Liquify, Generative
   Fill, Select Subject and upscaling, and body reshaping last.

## Decisions

1. **Reshape layers go stale** when the image below changes. Omapix layers
   are pixels, so Reshape keeps the warp's settings and offers Update.
   Decided (2026-09-24): Update first, the simplest version. Live,
   smart-object-like layers can be planned later.
2. **Body reshaping and backgrounds.** Warp-based reshaping (Photoshop
   Liquify, PortraitPro Body, Facetune) bends what's behind the body; a
   bent door frame is the classic sign of a reshaped photo. Protection
   (Liquify's Freeze Mask, or fading moves out away from the person, as
   here) keeps it small. Decided (2026-09-24): accept slight bending, with
   protection and Liquify to fix what's left. Cutting the person out and
   filling the background with inpainting, so nothing bends at all, is a
   possible later upgrade.

## Sources

- darktable AI features: [how AI works](https://docs.darktable.org/usermanual/development/en/special-topics/ai/how-ai-works/), [darktable-ai models](https://github.com/darktable-org/darktable-ai), and the `config.json` files installed on this machine
- [`ort` crate: linking and `load-dynamic`](https://ort.pyke.io/setup/linking)
- [SAM 2](https://github.com/facebookresearch/sam2) (Apache-2.0 weights)
- [BiRefNet](https://github.com/ZhengPeng7/BiRefNet) (MIT), [MODNet](https://github.com/ZHKKKe/MODNet) (Apache-2.0)
- [YuNet in OpenCV Zoo](https://github.com/opencv/opencv_zoo/tree/main/models/face_detection_yunet) (MIT)
- [MediaPipe Pose Landmarker](https://ai.google.dev/edge/mediapipe/solutions/vision/pose_landmarker) (Apache-2.0)
- [MediaPipe Face Landmarker](https://ai.google.dev/edge/mediapipe/solutions/vision/face_landmarker), [multiclass selfie segmentation ONNX](https://huggingface.co/senty-au/selfie_multiclass_256x256-ONNX) (Apache-2.0)
- FLUX.2 klein from Black Forest Labs (4B: Apache-2.0; 9B: non-commercial), to be rechecked when packaged
- Excluded: [face-parsing](https://github.com/yakhyo/face-parsing) (MIT code, CelebAMask-HQ weights), [Sapiens](https://huggingface.co/facebook/sapiens), [Sapiens2](https://huggingface.co/facebook/sapiens2)
