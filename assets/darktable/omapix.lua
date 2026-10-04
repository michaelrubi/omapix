--[[
  omapix.lua: darktable → Omapix → darktable.

  Export module › target storage › "edit in Omapix": the selected images are
  exported as 16-bit TIFFs beside their raws and opened in one Omapix
  window, a tab each. Retouch, save each (Ctrl+S saves the layers to a .ora
  beside the TIFF and the flattened image to the TIFF itself) and quit
  Omapix: the TIFFs are imported, each grouped with its raw.

  To retouch some again, select their TIFFs and press "edit in Omapix" in
  the selected image[s] module. Omapix opens the layers from the .ora files,
  and darktable redraws the TIFFs when Omapix quits.

  `make install` puts this in ~/.config/darktable/lua/ and loads it from
  ~/.config/darktable/luarc.
]]

local dt = require "darktable"

local function quote(path)
  return "'" .. path:gsub("'", "'\\''") .. "'"
end

local function exists(path)
  local f = io.open(path)
  if f then
    f:close()
  end
  return f ~= nil
end

-- The installed Omapix, or whichever is on the PATH.
local function omapix()
  local installed = (os.getenv("HOME") or "") .. "/.local/bin/omapix"
  return exists(installed) and quote(installed) or "omapix"
end

-- Open `tiffs` in one Omapix window, a tab each, and wait for it to quit.
local function edit(tiffs)
  local quoted = {}
  for i, tiff in ipairs(tiffs) do
    quoted[i] = quote(tiff)
  end
  local what = #tiffs == 1 and tiffs[1]:match("[^/]+$") or #tiffs .. " images"
  dt.print("editing " .. what .. " in Omapix")
  return dt.control.execute(omapix() .. " --round-trip " .. table.concat(quoted, " ")) == 0
end

-- IMG.tif, or IMG_01.tif… if that's taken.
local function unique(path)
  local base, ext = path:match("^(.*)(%.[^./]+)$")
  local candidate, n = path, 0
  while exists(candidate) do
    n = n + 1
    candidate = string.format("%s_%02d%s", base, n, ext)
  end
  return candidate
end

-- os.rename can't move between file systems (darktable exports to /tmp).
local function move(from, to)
  return os.rename(from, to) or dt.control.execute("mv " .. quote(from) .. " " .. quote(to)) == 0
end

local function export_and_edit(storage, image_table, extra_data)
  -- Each export goes beside its raw.
  local tiffs, images = {}, {}
  for image, exported in pairs(image_table) do
    local tiff = unique(image.path .. "/" .. exported:match("[^/]+$"))
    if move(exported, tiff) then
      tiffs[#tiffs + 1] = tiff
      images[tiff] = image
    else
      dt.print("couldn't move the export to " .. tiff)
    end
  end
  if #tiffs == 0 then
    return
  end
  -- `pairs` has no order: the tabs go by file name.
  table.sort(tiffs)
  if not edit(tiffs) then
    dt.print("couldn't start Omapix")
    return
  end
  for _, tiff in ipairs(tiffs) do
    local image = images[tiff]
    local edited = dt.database.import(tiff)
    edited:group_with(image.group_leader)
    for _, tag in ipairs(dt.tags.get_tags(image)) do
      if tag.name:sub(1, 9) ~= "darktable" then
        dt.tags.attach(tag, edited)
      end
    end
  end
end

dt.register_storage(
  "omapix",
  "edit in Omapix",
  nil,
  export_and_edit,
  function(storage, format) return format.extension == "tif" end,
  function(storage, format) format.bpp = 16 end
)

dt.gui.libs.image.register_action(
  "omapix", "edit in Omapix",
  function(event, images)
    local tiffs, edited = {}, {}
    for _, image in ipairs(images) do
      local path = image.path .. "/" .. image.filename
      if path:lower():match("%.tiff?$") then
        tiffs[#tiffs + 1] = path
        edited[#edited + 1] = image
      else
        dt.print("export " .. image.filename .. " with the \"edit in Omapix\" target first")
      end
    end
    if #tiffs > 0 and edit(tiffs) then
      -- Redraw the thumbnails from the TIFFs Omapix saved.
      for _, image in ipairs(edited) do
        image:drop_cache()
      end
    end
  end,
  "retouch the selected TIFFs in Omapix again, with their layers"
)
