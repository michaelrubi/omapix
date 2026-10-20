# The darktable round trip

Omapix is built for one workflow: the base edit in darktable, retouching in Omapix, then export. darktable gets an "edit in Omapix" export target that sends the image over and brings the retouched version back next to the raw.

## Setting it up

`make install` (or the Arch package) installs darktable's script, `omapix.lua`:

- With `make install`, it goes into `~/.config/darktable/lua/`, and a `require "omapix"` line is added to `~/.config/darktable/luarc`.
- With the package, it goes into `/usr/share/darktable/lua/`, and you add `require "omapix"` to `~/.config/darktable/luarc` yourself.

Restart darktable afterwards.

## Sending an image to Omapix

1. In darktable's **export** module, set the target storage to **edit in Omapix**. It exports a 16-bit TIFF.
2. Export. The TIFF is written beside the raw and opens in Omapix. If you export several images, they all open in one Omapix window, a tab each.
3. Retouch with as many layers as you like.
4. Press **Ctrl+S**. That saves two files:
   - the layers, to a `.ora` beside the TIFF;
   - the flattened image, back to the TIFF.
5. Quit Omapix. darktable imports the TIFF, grouped with its raw.

## Retouching it again

Select the TIFF in darktable and press **edit in Omapix** in the *selected image[s]* module. Omapix opens the layers from the `.ora`, not the flattened TIFF, so you carry on where you left off. When you quit, darktable redraws the thumbnail.

The `.ora` remembers its TIFF. However you open it (from darktable, the file manager or File › Open), saving it keeps the TIFF up to date.

## Other apps

Omapix runs as `omapix --round-trip image.tif`, which any app can use for the same round trip.

## Tips

- Keep darktable's export at 16 bits and in a wide colour space (ProPhoto or Rec. 2020). Omapix works in 16 bits and keeps the file's profile. View › Proof Colors (Ctrl+Y) shows what the web version will look like.
- Each layer in the `.ora` costs disk space: a 24 MP image with frequency separation, a Dodge & Burn layer and two masked copies is about 650 MB. Once a retouch is finished, merging its layers (Merge Down, Ctrl+E) and saving keeps the `.ora` small.
