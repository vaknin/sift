--[[
  sift.lua: applies decisions sent from sift to images already in darktable's
  library (for those, darktable's database wins over the XMP files, so sift
  can't write them directly).

  sift queues changes per film roll in ~/.cache/sift/darktable/<folder with
  / replaced by %>.todo, one line per image:
    file \t rating \t green \t add,tags \t remove,tags
  rating: keep | set:N | restore:EXPECT:TO   (restore only while still EXPECT)
  green:  keep | set | restore:0 | restore:1
  After applying, a receipt line per image is appended to <same>.done:
    ok|missing \t rating-before \t green-before(0|1) \t <the change line>
  sift reads the receipts to know what it changed and what to restore later.

  Install: copy to ~/.config/darktable/lua/sift.lua and add
    require "sift"
  to ~/.config/darktable/luarc. Adds a "sift" module to the lighttable's
  right panel; queued changes are also applied when darktable starts.
]]

local dt = require "darktable"

local cache = os.getenv("XDG_CACHE_HOME") or (os.getenv("HOME") .. "/.cache")
local queue_dir = cache .. "/sift/darktable/"

local function queue_file(film_path, ext)
  return queue_dir .. film_path:gsub("/", "%%") .. "." .. ext
end

local function split(line)
  local out = {}
  for field in (line .. "\t"):gmatch("([^\t]*)\t") do out[#out + 1] = field end
  return out
end

local function tags(list)
  local out = {}
  if list ~= "-" then
    for t in list:gmatch("[^,]+") do out[#out + 1] = t end
  end
  return out
end

-- Apply one change line to an image; returns the receipt fields.
local function apply(img, f)
  local rating, green = img.rating, img.green
  local before = rating .. "\t" .. (green and "1" or "0")

  local set = f[2]:match("^set:(%-?%d+)$")
  local expect, to = f[2]:match("^restore:(%-?%d+):(%-?%d+)$")
  if set then
    img.rating = tonumber(set)
  elseif expect and rating == tonumber(expect) then
    img.rating = tonumber(to)
  end

  if f[3] == "set" then
    img.green = true
  elseif f[3] == "restore:0" then
    img.green = false
  end

  for _, name in ipairs(tags(f[4])) do
    dt.tags.attach(dt.tags.create(name), img)
  end
  for _, name in ipairs(tags(f[5])) do
    local tag = dt.tags.find(name)
    if tag then dt.tags.detach(tag, img) end
  end
  return before
end

-- Apply one queue file (already renamed to .applying) for a film roll.
local function apply_file(film, path)
  local fh = io.open(path, "r")
  if not fh then return 0, 0 end
  local by_name = {}
  for i = 1, #film do
    local img = film[i]
    if img.duplicate_index == 0 then by_name[img.filename] = img end
  end
  local receipts, applied, missing = {}, 0, 0
  for line in fh:lines() do
    if line ~= "" and line:sub(1, 1) ~= "#" then
      local f = split(line)
      local img = by_name[f[1]]
      if img then
        receipts[#receipts + 1] = "ok\t" .. apply(img, f) .. "\t" .. line
        applied = applied + 1
      else
        receipts[#receipts + 1] = "missing\t0\t0\t" .. line
        missing = missing + 1
      end
    end
  end
  fh:close()
  local out = assert(io.open(queue_file(film.path, "done"), "a"))
  out:write(table.concat(receipts, "\n"), "\n")
  out:close()
  os.remove(path)
  return applied, missing
end

local function exists(path)
  local fh = io.open(path, "r")
  if fh then fh:close() end
  return fh ~= nil
end

local status = dt.new_widget("label") { label = "" }

local function pending()
  local n = 0
  for _, film in ipairs(dt.films) do
    for _, ext in ipairs({ "todo", "applying" }) do
      local fh = io.open(queue_file(film.path, ext), "r")
      if fh then
        for line in fh:lines() do
          if line ~= "" and line:sub(1, 1) ~= "#" then n = n + 1 end
        end
        fh:close()
      end
    end
  end
  return n
end

local function refresh()
  local n = pending()
  status.label = n == 0 and "nothing waiting" or (n .. " change(s) waiting")
end

local function apply_all(quiet)
  local applied, missing = 0, 0
  for _, film in ipairs(dt.films) do
    local applying = queue_file(film.path, "applying")
    -- A leftover .applying (darktable quit mid-way) is finished first; the
    -- changes are safe to repeat.
    if not exists(applying) and exists(queue_file(film.path, "todo")) then
      os.rename(queue_file(film.path, "todo"), applying)
    end
    if exists(applying) then
      local a, m = apply_file(film, applying)
      applied, missing = applied + a, missing + m
    end
  end
  if applied + missing > 0 then
    local msg = "sift: applied " .. applied .. " change(s)"
    if missing > 0 then msg = msg .. ", " .. missing .. " image(s) not found" end
    dt.print(msg)
  elseif not quiet then
    dt.print("sift: nothing to apply")
  end
  refresh()
end

local installed = false
local function install()
  if installed then return end
  installed = true
  dt.register_lib(
    "sift", "sift", true, false,
    { [dt.gui.views.lighttable] = { "DT_UI_CONTAINER_PANEL_RIGHT_CENTER", 100 } },
    dt.new_widget("box") {
      orientation = "vertical",
      dt.new_widget("button") {
        label = "apply sift decisions",
        tooltip = "apply ratings, labels and tags sent from sift",
        clicked_callback = function() apply_all(false) end,
      },
      status,
    },
    function() refresh() end, -- view_enter
    nil
  )
  apply_all(true)
end

-- Registering a lib before darktable's GUI is up can crash it (darktable
-- #19197); the system luarc sets darktable_gui_safe once it is. From luarc
-- at startup it isn't yet, so wait for the first switch to the lighttable.
-- The handler stays registered (install() is a no-op after the first call).
if rawget(_G, "darktable_gui_safe") and dt.gui.current_view().id == "lighttable" then
  install()
else
  dt.register_event("sift", "view-changed", function(_, _, new_view)
    if new_view.id == "lighttable" then install() end
  end)
end
