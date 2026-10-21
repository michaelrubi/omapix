# Auto Retouch

**Retouch › Auto Retouch…** does a first pass of portrait retouching on every face in the image:

- heals blemishes
- reduces shine
- smooths skin
- evens tone
- lightens under the eyes
- whitens the eyes and teeth

Everything it does is an ordinary layer, so you can turn down, mask or repaint any part of it.

It runs on your own machine. It needs the face models: run `scripts/fetch-models.sh` (or `omapix-fetch-models` if you installed the Arch package). Without them, the menu item is greyed out. Help › AI Models shows what's installed, and whether it runs on the GPU or the CPU.

## Running it

1. Select the layer to retouch. Auto Retouch works from what that layer and the ones below it show.
2. Choose **Retouch › Auto Retouch…**.
3. Pick a preset: **Natural**, **Standard** or **Strong**. Or set each step yourself: each one has a switch and a strength (Sensitivity for blemishes, opacity for the rest). The dialog opens with the settings you last used.
4. The blemishes it will heal are circled on the canvas. Check them before you press OK.

With more than one face, a strip of faces appears:

- **All Faces** sets every face that doesn't have settings of its own.
- To give one face its own settings, pick it in the strip or click it on the canvas.
- You can switch a face off.

Each face is measured by its own eyes, so small faces in a group shot are smoothed as finely as they need. When the pose model is installed, skin goes to the person whose face or body it's nearest. That way an arm across someone else's shoulder is retouched as its owner's skin.

## What you get

A **Retouch** group above the selected layer, with a group for each face, numbered from the left ("Face 1", "Face 2"…). Each face's group holds:

- **Blemishes**
- **Reduce Shine**
- **Smooth Skin**
- a **Dodge & Burn** group: Even Tone's Dodge and Burn Curves layers, with grey masks you can carry on painting by hand
- **Under Eyes**
- **Whiten Eyes** and **Whiten Teeth**

Each layer is made from what the one below it shows. All of it is one undo step.

## Adjusting the result

- **Before and after:** hide the Retouch group.
- **One face too strong:** lower that face's group's opacity.
- **One step too strong:** lower that layer's opacity, or paint black on its mask (Add Layer Mask first, from the layer's right-click menu).
- **A blemish it missed:** Spot Healing Brush (`J`) on a new layer above, with *Sample All Layers*.
- **A mark that should stay** (a beauty mark, a freckle): erase that spot on the Blemishes layer, or mask it out there.

## Scripting

`OMAPIX_SCRIPT="AutoRetouch Natural"` runs it with a preset, and plain `AutoRetouch` runs it with the settings you last used.

## How long it takes

Between about 0.5 and 2.5 s at up to 24 MP once the models are loaded, on an NVIDIA GPU. It's slower on the CPU.
