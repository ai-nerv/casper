-- A tool that wraps a real program.
--
-- Copy this into `~/.config/casper/plugin/` and `casper tools` lists it in the next run -- no
-- rebuild, no edit to anything casper ships. Before P4 there was one file a person could add to;
-- before that, thirteen tools compiled into the binary and nowhere else to put one.
--
-- What a tool declaration owes:
--
--   description   what it does, in the model's terms. This is the whole of what the model knows.
--   parameters    JSON Schema for the arguments. The model is held to it before `run` is called.
--   needs         which permission verb this acts under -- `read`, `write`, `run` or `reach`.
--                 casper reports it; the harness is what asks the person.
--   run           the body. Return `{ said = ... }`, or `{ said = ..., failed = true }`.
--
-- **`said` and `shown` are two different audiences.** `said` is what the model reads and should
-- be plain; `shown` is what the person sees and may carry colour. Handing the model ANSI escapes
-- spends its context on terminal control codes.
--
-- Registering the same name twice replaces, so a file in `after/plugin/` can override anything --
-- including one of the thirteen that ship.

casper.tool("jq", {
  description = "Run a jq filter over a JSON file. Use this instead of reading a large JSON "
    .. "file whole when you only need part of it.",
  parameters = {
    type = "object",
    properties = {
      filter = { type = "string", description = "The jq program, for example `.items[].name`." },
      path = { type = "string", description = "The JSON file to read." },
    },
    required = { "filter", "path" },
  },
  needs = "read",

  run = function(args)
    -- `casper.exec` takes the program and its arguments as a list, never a command line: there
    -- is no shell in between, so a filter containing a quote or a semicolon is an argument
    -- rather than a second command. This is the whole reason tools live in casper.
    local done = casper.exec("jq", { args.filter, args.path })
    if done.code ~= 0 then
      -- jq's own message names the line and column it gave up at, which is what the model needs
      -- in order to write a better filter. Passing it through beats replacing it with "failed".
      return { said = done.err ~= "" and done.err or "jq failed", failed = true }
    end
    return { said = done.out }
  end,
})

casper.tool("json-keys", {
  description = "List the top-level keys of a JSON file. Cheap way to see the shape of one "
    .. "before deciding what to read.",
  parameters = {
    type = "object",
    properties = {
      path = { type = "string", description = "The JSON file to look at." },
    },
    required = { "path" },
  },
  needs = "read",

  run = function(args)
    local done = casper.exec("jq", { "-r", "keys[]?", args.path })
    if done.code ~= 0 then
      return { said = done.err ~= "" and done.err or "jq failed", failed = true }
    end
    if done.out == "" then
      -- An empty answer is an answer: the top level is an array or a scalar, not an object.
      -- Saying so beats returning nothing and letting the model guess which.
      return { said = "no top-level keys: the document is not an object" }
    end
    return { said = done.out }
  end,
})
