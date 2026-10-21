# Frequency separation

Frequency separation splits the image into two layers: colour and tone on one, texture on the other. You can then even out blotches without touching pores, or heal texture without dragging colour along with it.

## Making the layers

1. Select the layer to retouch (usually the background). The split is made from what that layer and the ones below it show.
2. Choose **Retouch › Frequency Separation…**.
3. Set the **Radius**. The canvas previews the layer you're adjusting as you drag: the texture or the colour/tone. Pick the smallest radius at which the texture layer shows pores and fine hair, and the colour/tone layer shows none. The starting radius depends on the image's size (about 8.6 px for a 24 MP frame). Once you've chosen one, it's remembered.
4. Leave **Add edit layers** ticked (more about them below), and press OK.

The layers go into a Pass Through group called **Frequency Separation**, above the layer you selected:

- **Low - color/tone**: the blurred image.
- **High - texture**: what the blur removed, in Grain Merge mode. Together the two add up to exactly the original.
- With Add edit layers ticked, a copy above each one, **Low Edit** and **High Edit**, for you to work on. High Edit is clipped to High, so it blends the same way. The originals stay as they were split: to bring part of one back, erase that part of its copy, or hide the copy.

To see the image before the retouch, hide the group. The whole split is one undo step.

## Retouching on them

- **Texture (High Edit):** use the Clone Stamp (`S`) or Healing Brush (`J`) with *Sample: Current Layer* to replace damaged texture (a scar, a big pore, a stray hair) with clean texture from nearby. Colour isn't affected.
- **Colour and tone (Low Edit):** lasso a blotchy patch, feather it, and use Filter › Blur › Gaussian Blur. Or paint over it with a soft, low-opacity brush, picking up the colour next to it with Alt+click. Texture isn't affected.

## Three bands

**Retouch › Frequency Separation (3 Bands)…** splits into three layers instead:

- **Low - color/tone**
- **Mid - blotches**: the blotchy unevenness between the two others. Blurring or lowering this one evens skin out while keeping both the overall shape and the pores.
- **High - texture**

You set the fine radius. The coarse one starts at six times the fine one, and both are remembered.

## Size on disk

At first, an edit layer shares its tiles with the layer below it, so it takes no memory. In the `.ora` file, though, it's a layer of its own: a 24 MP image split in two goes from about 520 MB to 770 MB. Untick Add edit layers if you'd rather work on Low and High directly.

## Scripting

`OMAPIX_SCRIPT="FrequencySeparation"` runs it with the remembered radius, which is handy for trying it without a mouse.
