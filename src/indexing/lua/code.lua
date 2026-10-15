-- What `casper code` asks of the indexer: which language a path is read as, and the skeleton of a
-- source in that language. The handler half of maki's index tool, without its view.
local indexer = require("indexer")

local M = {}

function M.language(path)
  local filename = path:match("([^/]+)$")
  local lang = indexer.FILENAME_TO_LANG[filename]
  if lang then
    return lang
  end
  local ext = path:match("%.([^%.]+)$")
  if not ext then
    return nil, "Unsupported file type: (no extension). Use the read tool instead."
  end
  lang = indexer.EXT_TO_LANG[ext]
  if not lang then
    return nil, "Unsupported file type: ." .. ext .. ". Use the read tool instead."
  end
  return lang
end

function M.index(source, lang)
  local skeleton, err = indexer.index_source(source, lang)
  if not skeleton then
    return nil, "error: " .. tostring(err)
  end
  return (skeleton:gsub("\n+$", ""))
end

return M
