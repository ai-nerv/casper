local json = require("cjson")
local x, y = require("foo"), require("bar")
require("init")

local MAX_SIZE = 100
local min_val = 10

local M = {}

function process(data, opts)
  return data
end

function M.helper(x)
end

function M:method(val)
end

return M
