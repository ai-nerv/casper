-- The tools casper offers.
--
-- A declaration is a description and a function. The description says what the tool is, what it
-- takes, and which permission verb it acts under; the function runs it and says what it produced.
--
-- **A tool never chooses a colour.** It says what its output *means* -- `added`, `keyword`, `path`
-- -- and the harness resolves that against its own palette. That is what makes a `patch` and a
-- highlighted `cat` agree on screen instead of being two programs' idea of green.

-- Which 256-colour index means what, for programs that speak ANSI.
--
-- The join between a pager with a theme and a vocabulary with meaning, and the reason it lives
-- here: when your `bat` theme changes, this is a table you edit rather than a match in Rust you
-- cannot reach. A colour missing from it is drawn as ordinary text -- legible, never wrong.
casper.theme = {
  -- Truecolour, which is what `bat` and `delta` send by default. Read off this machine's own
  -- output rather than guessed: a theme that names colours nothing emits matches nothing, and
  -- the symptom is a file that comes out perfectly legible and completely unhighlighted.
  ["#abb2bf"] = "text",    ["#5c6370"] = "comment", ["#98c379"] = "string",
  ["#c678dd"] = "keyword", ["#e06c75"] = "error",   ["#e5c07b"] = "type",
  ["#61afef"] = "func",    ["#d19a66"] = "number",  ["#56b6c2"] = "keyword",

  -- The sixteen, for programs that still speak them.
  [1] = "error", [2] = "added",   [3] = "warn",  [4] = "path",
  [5] = "keyword", [6] = "type",  [9] = "removed", [10] = "added",
}

do -- cat
  -- `bat` when it is installed, `cat` when it is not. The fallback is not a lesser tool: it is
  -- the same file, unhighlighted, and a model reading it cannot tell the difference. Only the
  -- person can, which is exactly the half `shown` is for.
  casper.tool("cat", {
    description = "Read a file. Shown with syntax highlighting where the machine has it.",
    parameters = {
      type = "object",
      properties = {
        path = { type = "string", description = "The file to read." },
      },
      required = { "path" },
    },
    needs = "read",

    run = function(args)
      local pretty = casper.exec("bat", {
        "--color=always", "--style=plain", "--paging=never", args.path,
      })
      if pretty.code == 0 then
        -- What the model reads is the file, not the escapes: `said` is the plain text and
        -- `shown` is the painted one. Handing the model ANSI would spend its context on
        -- terminal control codes.
        local plain = casper.exec("cat", { args.path })
        return {
          said = plain.code == 0 and plain.out or pretty.out,
          shown = casper.paint.ansi(pretty.out, casper.theme),
        }
      end

      local plain = casper.exec("cat", { args.path })
      if plain.code ~= 0 then
        return { said = plain.err, failed = true }
      end
      return { said = plain.out }
    end,
  })
end

do -- patch
  -- The tool the palette argument is really about. A diff has structure a reader knows by heart,
  -- so it is read from the line's first character rather than from whatever colour some `diff`
  -- implementation chose -- and it comes out in the same green and red an `edit` already draws.
  casper.tool("patch", {
    description = "Show the difference between two files, as a unified diff.",
    parameters = {
      type = "object",
      properties = {
        old = { type = "string", description = "The file as it stands." },
        new = { type = "string", description = "The file to compare it with." },
      },
      required = { "old", "new" },
    },
    needs = "read",

    run = function(args)
      local done = casper.exec("diff", { "-u", args.old, args.new })
      -- `diff` exits 1 when the files differ, which is the ordinary case and not a failure.
      if done.code > 1 then
        return { said = done.err, failed = true }
      end
      if done.out == "" then
        return { said = "the two files are identical" }
      end
      return { said = done.out, shown = casper.paint.diff(done.out) }
    end,
  })
end

do -- ls
  casper.tool("ls", {
    description = "List a directory.",
    parameters = {
      type = "object",
      properties = {
        path = { type = "string", description = "The directory. Defaults to the current one." },
      },
    },
    needs = "read",

    run = function(args)
      local done = casper.exec("ls", { "-la", args.path or "." })
      if done.code ~= 0 then
        return { said = done.err, failed = true }
      end
      return { said = done.out }
    end,
  })
end
