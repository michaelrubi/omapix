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
   by a script the user runs, and verified by checksum. Without a model,
   the AI commands are greyed out, saying which model they need when
   hovered, and nothing else changes.
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
- `crates/omapix-ai/models.txt` lists each model's files, with their
  sizes, SHA-256 and download URLs. Omapix and the script both read it,
  so they can't disagree. A shared copy is found by its size first (just
  a directory walk), then its checksum, which Omapix remembers while the
  file's unchanged. Files in Omapix's own folder were checked when they
  were downloaded, so only their size is checked. Extra folders go in
  `~/.config/omapix/model_folders`, one a line.
- `scripts/fetch-models.sh` downloads the default set with `curl`, checks
  the checksums, and prints each model's size and licence before
  downloading. Omapix itself never touches the network. SAM 2.1 has no
  download URL yet: darktable installs it (as a zipped `.dtmodel`).
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
  - Done with BiRefNet portrait's fp16 export (490 MB): about 0.8 s at
    1024 px on the GPU. The matte is refined like Object Selection's
    mask, but kept soft (`refine::matte_coverage`).
  - On the CPU, the fp16 export is unusably slow (over 10 minutes). A
    CPU fallback needs the fp32 export (970 MB) or MODNet.
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
  about 0.05 IOD, tuned on real portraits (see "Smooth Skin" below). They
  are the Detail and Smoothness sliders.
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

As built, the group's opacity is the Even Tone amount, and the masks are
what would bring each patch level (see "Even Tone" below).

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

As built (see "Auto Retouch" below), the strip only shows when there's
more than one face, and the settings aren't kept on the group yet.

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

As built (see "Face-Aware Liquify" below), the mirror is a plane fitted in
depth to every pair of points, each side of a feature moves as a whole, and
less is done the more a face is turned from the camera. For matching one
eye or brow to the other by hand, each has its own sliders (milestone 9.5).

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

As built for faces (see "Face-Aware Liquify" below): the field is a
thin-plate spline through the points, on Liquify's 4 px grid; the warp is
applied to the layer being liquified, under the brushes' warp, rather than
to a Reshape layer; and while a slider's dragged it's shown on the shrunk
level on screen, as designed, and the layer warped when it's let go.

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
| Object Selection | SAM 2.1 Hiera Small | 180 MB | Apache-2.0 | Already installed via darktable. Tiny and Base Plus variants exist. Its training data includes SA-1B, under Meta's research-only licence (darktable's model card), so it doesn't strictly meet principle 4. |
| Select Subject | BiRefNet portrait | 490 MB (fp16) | MIT | Trained on P3M-10k (MIT) and TR-humans (Apache-2.0). Best hair edges. MODNet (Apache-2.0, 25 MB) as the light option. |
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
| (measured) | 15–60 ms; 0.85 s at 24 MP with the face analysis and skin mask it needs | |
| Healing the spots found (CPU) | 0.1–0.3 s for 10–20 spots | same |
| Denoise (NIND) at 24 MP | (measured: 7 s, 54 squares at 0.12 s each) | minutes |
| Smooth Skin (blurs at 24 MP) | < 1 s (CPU) (measured: 0.4 s) | same |
| Even Tone (masks at 24 MP) | < 0.3 s (CPU) (measured: under 0.1 s) | same |
| Auto Retouch end to end, per face | < 3 s (measured: 0.5–2.5 s in all, for one to three faces) | < 10 s |
| Reshape or Symmetry preview per slider step (shrunk level) | < 50 ms (measured: 15–50 ms fitted in a 1600 × 1000 canvas) | same |
| Reshape or Symmetry at 24 MP | < 1 s (CPU, parallel) (measured: 0.03–0.3 s, the most for a face that fills the frame) | same |

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
1. ~~**Groundwork and Object Selection.**~~ (done, 2026-09-29): Object
   Selection (click or box, Shift and Alt; `omapix-ai`'s runtime and SAM,
   and the engine's `refine::mask_coverage` with the guided filter), the
   model registry with checksums (`models.txt`), `fetch-models.sh`,
   Select › Subject and Help › AI Models.
2. **Face analysis and masks**, per face and per person. Select › Skin,
   Hair, Eyes, Lips, Teeth, and masks from them. Everything after this
   needs these masks, so it comes first.
   - Spike done (2026-09-29): `omapix-ai::face` runs all three models,
     and `crates/omapix-ai/examples/faces.rs` draws what they find. See
     "Face analysis spike" below.
   - Done: Select › Skin and Hair, from the whole image's segmentation,
     with each face's eyes, brows and lips taken out of Skin (grown 0.03
     IOD, feathered 0.015 IOD). 0.45 s at 24 MP once loaded.
   - Done: Select › Eyes, Lips and Teeth, from the landmark outlines
     (lips less the mouth; teeth only where the inner lips are more than
     0.03 IOD apart), feathered 0.01 IOD.
   - Left for when they're needed: Layer Mask › From Skin and the rest
     (Select, then Add Mask, does it now); a second segmentation pass
     round each person; skin split between people with SAM.
3. **Blemishes.** The classical detector and Heal Blemishes, onto an empty
   layer (Sample Current & Below already heals onto one). The biggest time
   saver in everyday retouching.
   - Done (2026-09-29): `omapix_engine::blemish` finds the spots and heals
     them onto a new Blemishes layer; Retouch › Heal Blemishes… circles
     them on the canvas under a Sensitivity slider. Redone the same day on
     real acne photos (see "Blemish detection" below). Faces only.
   - Left: long marks, profiles, necks and bodies once skin is split
     between people.
4. **AI Denoise** with NIND (roadmap section 4), onto a Denoise layer. The
   model is already on disk from darktable (GPL-3.0, like Omapix) and the
   runtime works, so it's quick, and it's the first step of the finishing
   workflow.
   - Done (2026-09-29): Filter › Noise › Denoise…, with Luminance and
     Color and a 100 % preview box, onto a new Denoise layer above the
     selected one. See "Denoise" below.
5. ~~**Smooth Skin**, with the dialog and preview, and the 3-band Frequency
   Separation setup.~~ (done, 2026-09-29)
   - Retouch › Smooth Skin…, onto a new Smooth Skin layer masked to the
     skin, with Amount (its opacity), Smoothness and Detail and a 100 %
     preview box. See "Smooth Skin" below.
   - Retouch › Frequency Separation (3 Bands)…, with Fine and Coarse
     radii, a preview of each band, and Low, Mid and High layers in a Pass
     Through group, the Mid one selected.
6. ~~**Even Tone**, the automatic Dodge & Burn.~~ (done, 2026-10-03)
   - Retouch › Even Tone…, as a Dodge & Burn group above the selected
     layer with its two masks filled in on the skin, with Amount (the
     group's opacity) and Size, and a preview box of the whole face. See
     "Even Tone" below.
7. ~~**Auto Retouch**, with the face strip, per-face groups, presets and
   scripting.~~ (done, 2026-10-03)
   - Retouch › Auto Retouch…, as a Retouch group above the selected layer
     with a group for each face: Blemishes, Smooth Skin and Dodge & Burn,
     each made from what the last shows. A switch and a strength for each
     step, Natural, Standard and Strong, a strip of faces to give one its
     own settings or leave it out, and `AutoRetouch Natural` in
     `OMAPIX_SCRIPT`. See "Auto Retouch" below.
   - Left: the settings kept on the group, and redoing it from them; a
     second segmentation pass round each person, and skin split between
     people with SAM.
8. ~~**Warps and Liquify**~~ (done): the warp engine, and brush Liquify
   (roadmap section 3), the way to touch up what the sliders do.
9. ~~**Face-Aware Liquify and Face Symmetry**, in the Liquify dialog.~~
   (done, 2026-10-03)
   - Face-Aware (A) in Liquify's options bar opens a panel of sliders for
     each face found in the layer: Symmetry for the eyes, brows, nose,
     mouth and jaw, and eleven for its shape. They warp the layer under
     the brushes' warp, a drag is a step for Ctrl+Z, and Liquify carries on
     with them when it's opened again. See "Face-Aware Liquify" below.
   - Done (2026-10-03): while a slider's dragged, a quick look at the size
     on screen, and the layer warped once it's let go.
   - Left: a Reshape layer that keeps its settings and can be updated (it
     comes with Body Reshape); thumbnails and clicking a face to pick it;
     faces in profile.

9.5. ~~**Each eye and brow on its own**, in Face-Aware Liquify.~~ (done,
2026-10-03)

- Symmetry works, but it wasn't scoped well: one slider for a feature
  evens everything about it at once, half way each side, so there was no
  way to ask for one thing on one side, such as one eye a little larger
  to match the other. Proportions are scale, angle and position, so each
  eye and each eyebrow has those as sliders of its own: Left Eye and Right
  Eye (Size, Height, Width, Tilt, Lift), Left Brow and Right Brow (Lift,
  Tilt).
- The panel's groups fold away, since there are thirty sliders now. The
  new four start folded, and a folded group with a slider set has a dot.
- Symmetry says when a face is turned and it's being held back, and by
  how much.
- Left: both eyes' Height, Width and Tilt, and both brows', as one slider
  (Photoshop links the two eyes); an eye raised under a brow lowered,
  both all the way, run into each other, and the brow on the far side of
  a turned face kinks at its outer end with Lift and Tilt both at 100.

10. **Generative Fill.** After the portrait work, which is used on every
    photo, while Generative Fill is for the occasional big removal or
    extension. By then Arch's ONNX Runtime should have reached 1.30. Starts
    with a spike: memory, speed, and unloading.
11. **AI upscaling** for print enlargements in Image Size (darktable's RealPLKSR, MIT, already
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
  within 5e-4 on random inputs. `models.txt` points `fetch-models.sh` at
  those copies, pinned by revision and checksum, so it needs no Python or
  TensorFlow.
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

### Blemish detection

As built (`omapix_engine::blemish`). The first version (a single light
and heavy blur, blobs by flood fill) was tuned on spots painted onto clean
portraits, and on real acne it circled pores and stubble while missing the
pimples. It was redone on twelve freely licensed photos of acne, marks and
scars (Pexels and Wikimedia Commons, kept in `~/Pictures/blemish-tests`,
not in the repository) and four clean portraits of Michael's, looking at
each through the ignored test `find_blemishes_in_photos`:

```
OMAPIX_FACE_PHOTO=~/Pictures/blemish-tests OMAPIX_FACE_OUT=out \
  cargo test --release -p omapix find_blemishes_in_photos -- --ignored
```

- **Where.** Face skin within each face's landmark outline (not the ears),
  less the eyes, brows and lips, the bottom of the nose (MediaPipe's nose
  outline, grown 0.06 IOD: nostrils and the creases beside them) and a
  margin of 0.1 IOD round each eye (lashes, eyeliner, the lid's crease).
  Faces the landmarker can't see (full profiles) are left out: YuNet's
  points there are guesses, one mouth "corner" landed mid-cheek, and
  without outlines nostrils and lips were healed into smears.
- **Scale.** Eyes about 150 px apart. The distance between the eyes is
  taken as at least a third of the face box's height, since a turned
  face's eyes look closer together (face height ÷ eye distance is 2.8–3
  facing the camera, up to 5.6 turned).
- **Signal.** Blob detection at nine sizes half an octave apart (Gaussian
  σ 0.0057–0.09 IOD): each point blurred by σ against the ring round it
  blurred by 2.5σ, in Lab, with skin weighting every blur. Darker counts;
  lighter counts only if also redder (a raised pimple on dark skin), so
  oily highlights and light pores don't. Each size is measured against how
  much the skin varies at that size (median absolute difference), so the
  pores and stubble that are everywhere score low.
- **Spots.** Peaks higher than their neighbours at their size and the
  sizes either side, found at σ 0.008–0.045 only: a peak at the smallest
  size is a pore, and one that still stands out at the two largest is a
  flush or shading. Ridges (creases, hairs) are rejected by SIFT's
  curvature test (ratio up to 6). A spot's extent is the largest size at
  which it stands out a third as much as at its peak (its red halo), its
  middle the peak at that size (not a highlight to one side), and it needs
  skin at its middle and all round out to 2.5 × its radius + 0.02 IOD.
  Duplicates across sizes are merged; small spots inside a big one are
  kept, since one big heal leaves them.
- **Scores.** Clear pimples score 20 and up, marks 10–20, dark marks on
  dark skin and a few shadows 7–10, texture 4–7. Sensitivity runs from 20
  (0 %) through about 9 (50 %) to 4 (100 %), on a log scale. Ice-pick
  scars score 4–12. Rolling scars and texture (MJR00685) aren't spots:
  Smooth Skin is for them.
- **Healing.** A dab 2.6 × the radius + 4 px across, hardness 0.8, so it
  covers the whole spot. The Spot Healing Brush's source choice was
  rewritten along the way, as it produced flat, pale patches on real skin:
  it compared pixels one by one, so on pored skin it chose the smoothest
  patch it could reach, and it corrected tone by adding a difference. It
  now compares blocks: the surroundings' shape (shade counts a quarter),
  the patch's texture measured a few pixels apart against the skin round
  the spot, lumps in the patch, and the patch against its own
  surroundings; candidates start one spot-width away. Tone is corrected by
  ratio, as light does, over a quarter of the dab's size.
- **Left:** long marks (scratches, some scars) need a stroke, not a dab;
  profiles need a better idea of where the nostrils and lips are; dark
  marks on dark skin often need 70–75 %.

### Denoise

As built (`omapix_engine::denoise`, `omapix_ai::denoise`). NIND is a UNet
darktable installs as `denoise-nind`: a fixed 768 × 768 input and output,
sRGB from 0 to 1 as red, green and blue planes (darktable feeds it sRGB
too). Looked at on seven high-ISO portraits and a black and white shot
from Wikimedia Commons (ISO 6400–20000: straight from the camera, camera
JPEGs, and Canon DPP and Lightroom exports), kept in
`~/Pictures/denoise-tests` with their credits, not in the repository:

```
cargo run --release -p omapix-ai --example denoise -- out ~/Pictures/denoise-tests/*.jpg
```

- **Squares.** The image, in sRGB, is cut into 768 px squares. NIND's
  outer 16 pixels come back wrong (very dark), so 32 are thrown away on
  every side and the image is reflected beyond its own edges; the kept
  704 px overlap by 32 and fade from one to the next. Without this, the
  squares' edges showed as a faint grid.
- **Only detail changes.** NIND darkens shadows a little and tints black
  and white photos magenta (by up to 4 levels in 255), which noise
  doesn't do. So only the fine part of its change is kept: the change
  less a blur of it (σ 16 px). Means before and after now match to 0.1
  levels.
- **Luminance and colour** are split in gamma-encoded sRGB by Rec. 709
  luma: Luminance scales the change in luma, Color the rest.
- **Wide gamut.** The result goes on as a change to the original, in the
  document's colours (the original plus the denoised sRGB, less the
  original sRGB, both converted back), so ProPhoto colours sRGB can't hold
  change as their clipped selves did rather than being clipped.
- **Amounts.** At ISO 12800 on an old APS-C camera, 100 % cleans it but
  starts to look smeared, and 70–80 % leaves a fine, natural grain; on
  moderate noise they look alike. Hence 80 % Luminance, 100 % Color.
  Files already noise-reduced by the camera or DPP change little, as they
  should.
- **Left:** JPEG block artifacts in heavily compressed files stay; a
  faster fp16 run or TensorRT, if 7 s at 24 MP gets in the way.

### Smooth Skin

As built (`omapix_engine::skin`, `crates/omapix/src/smooth_skin.rs`),
looked at on Michael's portraits and the acne photos in
`~/Pictures/blemish-tests` through the ignored test
`smooth_skin_in_photos`:

```
OMAPIX_FACE_PHOTO=photos OMAPIX_FACE_OUT=out \
  cargo test --release -p omapix smooth_skin_in_photos -- --ignored
```

- **Where.** Face and body skin from the segmentation, less the eyes,
  brows and lips, as Select › Skin makes it. The source is Current &
  Below of the selected layer, so it goes on after Heal Blemishes.
- **Bands.** Gaussian blurs weighted by the skin mask and divided by the
  blurred mask (the image's alpha does the same in `gaussian_blur`), so
  only skin is averaged. The layer is `image − blur(fine) + blur(coarse)`:
  at full opacity the mid band is gone, and opacity scales it back, so
  Amount is simply the layer's opacity.
- **Radii.** The design's `r_coarse` of 0.25 IOD took the face's shape
  away with the blotches: the nose's sides, the cheekbones' highlights and
  the smile lines went flat, and faces looked like masks. At 0.04–0.06 IOD
  acne redness and blotches even out and the shading stays. Smoothness
  runs from 0.025 to 0.1 IOD (0.05 at 50), and Detail from 0.01 to 0.04
  (0.02 at 50); the coarse blur is never finer than the fine one. Amount
  starts at 70 %.
- **Scale.** The distance between the eyes of the largest face, as for
  blemishes. Smaller faces in the same photo are smoothed coarser than
  they'd need.
- **Speed.** Only the skin's bounding box is blurred: 0.03–0.5 s on the
  test photos, up to 24 MP. The preview box is blurred on its own, with
  three coarse radii of the image round it, on a thread.
- **Left:** each face at its own scale; stronger smoothing that keeps the
  shading needs an edge-aware coarse blur (a guided filter), rather than
  a larger Gaussian.

### Even Tone

As built (`omapix_engine::tone`, `crates/omapix/src/even_tone.rs`), looked
at on the acne photos in `~/Pictures/blemish-tests` and Michael's portraits
through the ignored test `even_tone_in_photos`, which puts each face
before, after and as its masks side by side:

```
OMAPIX_FACE_PHOTO=photos OMAPIX_FACE_OUT=out \
  cargo test --release -p omapix even_tone_in_photos -- --ignored
```

- **Where.** The same skin as Smooth Skin, from Current & Below of the
  selected layer, so it goes on after Smooth Skin.
- **On a small copy.** Unevenness has no fine detail, so it's worked out
  on a copy of the skin's part of the image with the eyes at most 80 px
  apart (a whole number of the image's pixels to each of its own). The
  masks are scaled back up and limited to the skin at full size, so their
  edges are the skin's.
- **Luminance, not L\*.** The document's own encoded values (0.3 R +
  0.59 G + 0.11 B, as the histogram), which is what the curves work on:
  each mask is then exactly how far its curve has to be let through to
  bring that skin level. Linear documents are already converted to gamma
  1.8 on opening, so it's near enough perceptual in every document.
- **Masks.** `D` from blurs weighted by the skin, as Smooth Skin's are.
  Where `D` is negative, Dodge's mask is `−D` over how far the Dodge curve
  moves that skin's colour at full strength, and likewise Burn's where
  it's positive. So the masks are grey, mostly under a third, and painting
  on them carries on from there.
- **Amount** is the group's opacity (60 % to start), as Smooth Skin's is
  its layer's: the group is left selected, so it's there in the Layers
  panel afterwards.
- **Radii.** The design's wide band, tried first at 0.05–0.3 IOD and full
  strength, took light and shade for unevenness: cheekbone highlights were
  burned away and the shaded edge of the face dodged, and the masks were
  solid black and white. Three things keep the shape:
  - `r_small` is 0.04 IOD (just under Smooth Skin's coarse blur), and
    `r_large` 0.15 IOD at Size 50, from 0.075 to 0.3.
  - A patch is moved by 8 % of the range at most (`D` through a tanh).
    Small differences, which are the unevenness, go entirely; a highlight
    or the shadow under the jaw only moves that far.
  - Nothing's evened where less than half of what the large blur sees is
    skin, rising to all of it where it's all skin: the edge of a face is
    in shade.
- **Preview.** The box shows the small copy, so the whole face, starting
  on the nose: evenness is judged from a distance, and it costs nothing.
  Amount only re-mixes it; Size evens the copy again on a thread.
- **Speed.** 5–50 ms to shrink and even at up to 24 MP, and 3–35 ms for
  the two masks, after the 0.2–0.7 s to find the skin.
- **Left:** the shadow under the jaw is lifted a little (it's inside the
  skin); each face at its own scale; a preview on the canvas, if the box
  turns out too small to judge by; colour unevenness (redness), which is
  Smooth Skin's and a later step's.

### Auto Retouch

As built (`omapix_engine::retouch`, `crates/omapix/src/auto_retouch.rs`),
looked at on the acne photos in `~/Pictures/blemish-tests` and Michael's
group shots through the ignored test `auto_retouch_in_photos`, which puts
each face before and after side by side (`PRESET` picks the preset):

```
OMAPIX_FACE_PHOTO=photos OMAPIX_FACE_OUT=out \
  cargo test --release -p omapix auto_retouch_in_photos -- --ignored
```

- **Faces.** Every face YuNet finds, numbered from left to right, each
  measured by its own eyes, so a small face at the back of a group is
  smoothed as finely as it needs. Faces in profile count: they get Smooth
  Skin and Even Tone but no blemishes, as in Heal Blemishes. A face with
  no skin found and no spots is left out.
- **Whose skin.** Blemishes are looked for within each face's own outline.
  The rest of the skin (necks, arms, hands) goes to the nearest face,
  measured from the middle of each face's box in distances between its
  eyes (`retouch::share`), not yet by SAM as designed. One face has it
  all. In the three-person shots tried, necks and shoulders went to the
  right people; an arm across someone else would go to them.
- **Order.** For each face: Blemishes, then Smooth Skin from the image
  with them healed, then Dodge & Burn from the image smoothed, each from
  Current & Below of the face's group so far. They're the commands' own
  functions, with Smoothness, Detail and Size at 50.
- **Presets.** Sensitivity, Smooth Skin's amount and Even Tone's: Natural
  35, 40 and 40; Standard 50, 70 and 60 (each command's own default);
  Strong 65, 90 and 80. The settings last OK'd for All Faces are kept
  between runs.
- **The strip.** With more than one face: All Faces, then a thumbnail of
  each. A face follows All Faces until its own settings are changed. It
  can be switched off, and clicking it on the canvas selects it. Faces
  keep their numbers when another is off.
- **Groups.** Retouch and each face's group are Pass Through, so the
  curves see the image, and a face's group's opacity fades all of its
  retouch. A step that's off, or has nothing to do, leaves no layer, and a
  face with none leaves no group.
- **Scripts.** `AutoRetouch Natural` (or `Standard`, `Strong`, or nothing
  for the settings last used) opens the dialog and OKs it once the faces
  are found. The ignored test
  `the_auto_retouch_script_step_builds_a_group_for_each_face` runs it on
  `OMAPIX_FACE_PHOTO` and checks the layers.
- **Speed.** With CUDA, 0.2–1.4 s to find the faces, their skin and their
  spots once the models are loaded (the most at 21 MP), and 0.25–0.95 s to
  build the layers for one to three faces. Each step composites the image
  so far once, which is most of it.
- **Left:** the settings kept on the group, and redoing it from them; a
  second segmentation pass round each person (a face with 69 px between
  its eyes in a 24 MP group shot had no skin found, so it was left out);
  skin split between people with SAM; a preview of the smoothing and
  evening (the circles are all the dialog shows).

### Face-Aware Liquify

As built (`omapix_engine::reshape`, `Field::move_points` in
`omapix_engine::warp`, `crates/omapix/src/face_liquify.rs`), looked at on
Michael's portraits and group shots and the photos in
`~/Pictures/blemish-tests` (nearly all turned from the camera, which is
what showed where Symmetry goes wrong) through the ignored test
`face_liquify_in_photos`, which puts each face before and after side by
side (`SHAPE` sets the sliders by name, with the group's where several
share one, as `Left Eye Size=60`: all of Symmetry at 100 if unset):

```
SHAPE="Face Width=-50,Smile=40" OMAPIX_FACE_PHOTO=photos OMAPIX_FACE_OUT=out \
  cargo test --release -p omapix face_liquify_in_photos -- --ignored
```

- **Where it lives.** In Liquify: Face-Aware (A, the key of Photoshop's
  Face tool) in the options bar opens the panel, greyed out without the
  face models. The faces are looked for the first time it opens, in the
  document as it was before Liquify's warp, so nothing loads for someone
  who only wants the brushes. Faces the landmarker can't see (profiles)
  have no sliders.
- **Points.** From the landmarks: each eye's and brow's outline, the
  nose's (and five points down its bridge), the lips' outer and inner
  edges and the face's outline, about 150 points, as pairs that mirror
  each other and points on the midline (`reshape::Face`). The rings'
  orders give the pairs: on front-facing photos, a point's mirror image
  lands within 1–2 % of the distance between the eyes of its pair.
- **Under the brushes.** The sliders' warp is a field of its own, made
  anew from the layer as it was at each change; the brushes' field goes
  over it (`warp_area`'s `under`). So the brushes touch up what the
  sliders did, and Reconstruct and Restore All take back only the brushes'
  work, as in Photoshop: the sliders, and Reset, undo their own. A drag is
  one step for Ctrl+Z among the strokes, and the faces and their sliders
  are kept with the layer's mesh for the next Liquify.
- **The warp.** The design's rigid moving least squares was tried first.
  Weighted by the inverse square of the distance, an edge between two
  moved points sagged towards the unmoved ones, and the jaw came out
  scalloped; by the fourth power the scallops went, but the warp broke
  into a cell round each point, and a narrowed cheek came out jagged
  where the outline's points are near the eye's and the lips'. A
  thin-plate spline through the points (one small linear system for each
  change) has neither. It's worked out on Liquify's 4 px grid within an
  ellipse 1.6 times the face's outline, pinned at 32 points round it and
  faded out from 1.3, so there's room for the jaw to move and for hair
  and background to stretch.
- **Sliders**, from −100 to 100, in the face's own axes (across is from
  eye to eye). At 100: Eye Size 20 % larger about each eye's middle; Eye
  Distance 0.08 IOD further out each; Nose Length 0.1 IOD and Nose Width
  25 %, nothing at the top of the bridge and the most at the base; Smile
  the corners of the mouth up 0.1 IOD; Lip Fullness the lips' outer edge
  35 % further from their inner edge; Mouth Width 15 %; Forehead its top
  up 0.15 IOD; Chin Height the chin down 0.12 IOD; Jawline 15 % wider from
  the mouth down; Face Width 12 % wider from the eyes down. Features a
  slider doesn't move are pinned where they are, so a narrower face
  squeezes the cheeks, not the nose. Each at 100 or −100 comes out
  smooth on the portraits tried, a few at once too. Jawline and Face
  Width both at −100 is about as far as hair beside the face will
  stretch.
- **Each eye and brow** (milestone 9.5) has sliders of its own, in groups
  that start folded: Left Eye, Right Eye, Left Brow and Right Brow, left
  being the left of the picture. At 100: an eye's Size 20 % larger, on
  top of Eye Size; Height 30 % taller and Width 15 % wider, down and
  across the face; Tilt its outer corner up, turned 10° about its middle;
  Lift 0.05 IOD up. A brow's Lift 0.06 IOD up, and Tilt its outer end up,
  6°: less than an eye's, since at 0.1 IOD and 10° the brow on the far
  side of a turned face ran into the outline, which is pinned, and
  kinked. Each alone at either end comes out smooth on the portrait
  tried, and so does everything for an eye and its brow half way towards
  each other; an eye with all five at 100 under a brow at −100 runs into
  it, and that far-side brow still kinks a little at its outer end with
  both of its own at 100.
- **Symmetry**, from 0 to 100 for the eyes, brows, nose, mouth and jaw.
  - The mirror is a plane, fitted by least squares to every pair and
    midline point with the landmarks' depth, rather than a line through
    the midline. The depth is to the same scale as x and y: scaling it by
    anything but 1 made every test face less symmetrical.
  - Each side of a feature moves as a whole (shifted, turned and scaled
    half way to the other side's mirror image), and midline points go
    onto the plane. Point by point, the landmarks' noise made the jaw
    wavy.
  - On faces looking at the camera the two sides differ by 0.2–1.4 % of
    IOD, so Symmetry is subtle, as it should be. On faces turned 25° or
    more they seem to differ by 3–15 %, most of all the outline and the
    brows, because the far side's points are guesses, and at 100 that bent
    jaws and brows. So it's scaled down from a turn of 6° to nothing at
    24° (the sine of the turn, 0.1 to 0.4), and the sliders say so when
    hovered.
  - What it can't be asked for (2026-10-03, after Michael tried to make
    one eye a little larger to match the other): a slider evens the whole
    feature, its place, size and angle together, and only half way each
    side, so it's no way to change one proportion on one side; that's
    what each eye's and brow's own sliders are for. It's also nearly
    always held back, since few portraits look straight at the camera
    (86 %, 76 %, 3 % and 0 % on four faces looked at), and the eye further
    from the camera, which looks smaller in the picture (10 % narrower on
    the face at 86 %), is taken by the plane's depth for the turn, not
    for a difference. The group now says "Turned away: held back to
    86 %" or "Turned too far: nothing is done" for the face
    (`reshape::facing`).
- **While a slider's dragged**, the layer isn't warped: the canvas shows
  a pyramid level (half size, a quarter…) when zoomed out, so the layer's
  pixels before, shrunk to that level, are warped where they're on screen
  and composited with the other layers shrunk likewise
  (`Editor::preview_liquify`, `warp_area`'s `level`), and the full-size
  levels are left stale. When the drag ends (or anything else happens in
  Liquify) the layer is warped over all the drag touched. A click on a
  slider, a typed value and Reset warp it at once. At 100 % and closer
  it's the full-size warp of what's on screen.
- **Preview** in the panel, off, shows the layer without the sliders'
  warp (the brushes' stays), to compare with the faces as they were. The
  sliders keep their values and wait, and Liquify applies them whether
  it's on or off.
- **The spline** is worked out at points IOD / 100 apart (at least the
  4 px grid) and interpolated between: with every slider at its end, that
  is within a pixel of the spline everywhere, and at IOD / 50 it was up
  to 3 px out beside the eyes. For the quick look it's 2^level coarser, so
  as close in the pixels shown.
- **Speed.** Fitted in a 1600 × 1000 canvas, a step of a drag takes
  15–50 ms (the most for a face that fills the frame at 28 %, which is
  shown at half size), and the first of a drag 20–35 ms more to shrink
  the layers. Warping the layer takes 30 ms for faces 200 px across,
  0.13 s for two faces 1000 px across at 24 MP, and 0.3 s for a face that
  fills a 21 MP frame, with the canvas redrawn. Finding the faces takes
  0.2–0.5 s once the models are loaded.
- **Left:** a face cut off by the image's edge smears there when it's
  moved a lot;
  thumbnails of the faces, and clicking one to pick it; Photoshop's
  handles on the canvas; faces in profile; keeping the sliders in the
  .ora.

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
