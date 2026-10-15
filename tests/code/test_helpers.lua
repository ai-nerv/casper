-- The harness maki's index specs are written against, as their `require("maki.test_helpers")`
-- finds it here: `case` runs a block under pcall so one failure does not end the run, and
-- `report` raises with every failure once the spec has run.

local M = {}

local failures = {}
local ran = 0

function M.case(name, fn)
  ran = ran + 1
  local ok, err = pcall(fn)
  if not ok then
    failures[#failures + 1] = name .. ": " .. tostring(err)
  end
end

function M.eq(actual, expected, msg)
  if actual ~= expected then
    error((msg or "") .. "\nexpected: " .. tostring(expected) .. "\n  actual: " .. tostring(actual))
  end
end

function M.report()
  if #failures > 0 then
    error(#failures .. " of " .. ran .. " case(s) failed:\n\n" .. table.concat(failures, "\n\n"), 0)
  end
  return ran
end

return M
